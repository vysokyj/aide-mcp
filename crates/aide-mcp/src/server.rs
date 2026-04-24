use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use aide_core::{AidePaths, Config};
use aide_dap::DapClient;
use aide_git::diff::DiffMode;
use aide_install::{install_tool, InstallOutcome};
use aide_lang::{LanguagePlugin, Registry};
use aide_lsp::{ops as lsp_ops, LspPool};
use anyhow::Result;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars,
    service::RequestContext,
    tool, tool_handler, tool_router,
    transport::stdio,
    RoleServer, ServerHandler, ServiceExt,
};
use tokio::sync::{Mutex as AsyncMutex, RwLock};

use crate::dogfood::aggregate_coverage_gaps;
use crate::exec::{self, Progress};
use crate::indexer::Indexer;

pub async fn run() -> Result<()> {
    let handler = AideServer::new()?;
    let service = handler.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectDetectArgs {
    /// Absolute path to the project root. If omitted, uses the server's cwd.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectDetectResult {
    pub root: String,
    pub languages: Vec<DetectedLanguage>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct DetectedLanguage {
    pub id: String,
    pub lsp: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectSetupArgs {
    /// Absolute path to the project root. If omitted, uses the server's cwd.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectSetupResult {
    pub root: String,
    pub languages: Vec<String>,
    pub tools: Vec<ToolInstallReport>,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ToolInstallReport {
    pub name: String,
    pub version: String,
    pub status: &'static str,
    pub path: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectLsArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// One of "all" (default, gitignore-aware walk — includes untracked
    /// files), "tracked" (libgit2 index, tracked files only — fastest),
    /// "dirty" (files with non-clean status), or "staged" (files whose
    /// index entry differs from HEAD).
    #[serde(default)]
    pub scope: Option<String>,
    /// Glob over the repo-relative path, e.g. `crates/*/src/**/*.rs`.
    #[serde(default)]
    pub glob: Option<String>,
    /// Cap on the number of returned paths. Defaults to 500.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Include dotfiles when `scope = "all"`. Defaults to false.
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, serde::Serialize, schemars::JsonSchema)]
pub struct ProjectLsResult {
    pub root: String,
    pub scope: String,
    pub files: Vec<String>,
    pub total: usize,
    pub truncated: bool,
}

/// Indexer's view of a repo, attached to tool responses whose behaviour
/// depends on whether a Ready SCIP index exists (`project_grep`, today).
///
/// Serialised as `{state, sha?, reason?}`:
/// - `state` is one of `ready` / `pending` / `in_progress` / `failed` /
///   `no_index` — one stable set of `snake_case` strings agents can switch on.
/// - `sha` is populated for every state except `no_index` (the indexer
///   knows *some* commit for this repo, just not necessarily Ready).
/// - `reason` is only set when `state == "failed"` and carries the
///   human-readable failure string from [`aide_proto::IndexState::Failed`].
///
/// Rationale: ux-gotcha #2 — a SCIP-enriched response with no hits looked
/// identical to "SCIP not Ready, silent fallback to bare matches". Agents
/// had no signal to distinguish "try again later" from "genuinely empty".
#[derive(Debug, serde::Serialize)]
pub struct ScipMeta {
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl ScipMeta {
    fn no_index() -> Self {
        Self {
            state: "no_index",
            sha: None,
            reason: None,
        }
    }

    fn from_commit_info(info: &aide_proto::CommitInfo) -> Self {
        let (state, reason) = match &info.state {
            aide_proto::IndexState::Pending => ("pending", None),
            aide_proto::IndexState::InProgress => ("in_progress", None),
            aide_proto::IndexState::Ready => ("ready", None),
            aide_proto::IndexState::Failed(r) => ("failed", Some(r.clone())),
        };
        Self {
            state,
            sha: Some(info.sha.clone()),
            reason,
        }
    }
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectGrepArgs {
    /// Regex pattern. Smart-case by default — lowercase pattern matches
    /// case-insensitively, mixed-case matches case-sensitively.
    pub pattern: String,
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Same scopes as `project_ls`. Defaults to "all" (gitignore-aware
    /// working-tree walk — includes untracked files).
    #[serde(default)]
    pub scope: Option<String>,
    /// Glob over repo-relative paths to restrict the file set.
    #[serde(default)]
    pub glob: Option<String>,
    /// Override smart-case: `true` = sensitive, `false` = insensitive.
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    /// Lines of context before each match (capped at 10).
    #[serde(default)]
    pub before_context: Option<usize>,
    /// Lines of context after each match (capped at 10).
    #[serde(default)]
    pub after_context: Option<usize>,
    /// Total cap on matches across all files. Defaults to 200.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Cap on matches per file. Defaults to 50.
    #[serde(default)]
    pub max_results_per_file: Option<usize>,
    /// Include dotfiles when `scope = "all"`. Defaults to false.
    #[serde(default)]
    pub include_hidden: bool,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectLsAtArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA whose tree should be listed.
    pub sha: String,
    /// Glob over the repo-relative path.
    #[serde(default)]
    pub glob: Option<String>,
    /// Cap on the number of returned paths. Defaults to 500.
    #[serde(default)]
    pub max_results: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectGrepAtArgs {
    /// Regex pattern. Smart-case by default.
    pub pattern: String,
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA whose tree should be searched.
    pub sha: String,
    /// Glob over repo-relative paths to restrict the blob set.
    #[serde(default)]
    pub glob: Option<String>,
    /// Override smart-case: `true` = sensitive, `false` = insensitive.
    #[serde(default)]
    pub case_sensitive: Option<bool>,
    /// Lines of context before each match (capped at 10).
    #[serde(default)]
    pub before_context: Option<usize>,
    /// Lines of context after each match (capped at 10).
    #[serde(default)]
    pub after_context: Option<usize>,
    /// Total cap on matches across all files. Defaults to 200.
    #[serde(default)]
    pub max_results: Option<usize>,
    /// Cap on matches per file. Defaults to 50.
    #[serde(default)]
    pub max_results_per_file: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspPositionArgs {
    /// Absolute path to the source file.
    pub file: String,
    /// 0-indexed line number.
    pub line: u32,
    /// 0-indexed UTF-16 column within the line.
    pub column: u32,
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct SafeEditArgs {
    pub file: String,
    /// Must occur exactly once in `file`. Mirrors the `Edit` tool's
    /// uniqueness rule so the agent can't accidentally touch the
    /// wrong occurrence.
    pub old_string: String,
    pub new_string: String,
    /// Other files to snapshot diagnostics in — useful when the
    /// edit is expected to affect e.g. downstream call sites.
    /// Empty by default: only the edited file is measured.
    #[serde(default)]
    pub related_files: Vec<String>,
    /// Milliseconds to wait between `didChange` and the after-
    /// snapshot. Defaults to 1500. Bigger = more reliable on cold
    /// or large workspaces; smaller = faster feedback loop.
    #[serde(default)]
    pub settle_ms: Option<u64>,
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspCodeActionRangeArgs {
    pub file: String,
    /// 0-indexed line numbers for the action's range. `end_line` /
    /// `end_column` default to `line` / `column` — i.e. a point
    /// range at the cursor.
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspApplyCodeActionArgs {
    pub file: String,
    pub line: u32,
    pub column: u32,
    #[serde(default)]
    pub end_line: Option<u32>,
    #[serde(default)]
    pub end_column: Option<u32>,
    /// Case-insensitive substring match on the action's title. One
    /// of `title` / `kind` must be set; if both are set, `kind`
    /// wins.
    #[serde(default)]
    pub title: Option<String>,
    /// Exact match on the action's LSP kind (e.g.
    /// "source.organizeImports", "quickfix", "refactor.extract").
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspRenameArgs {
    pub file: String,
    pub line: u32,
    pub column: u32,
    /// The new identifier. The server refuses names that don't
    /// parse as identifiers (rust-analyzer emits an error in that
    /// case).
    pub new_name: String,
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspFileArgs {
    /// Absolute path to the source file.
    pub file: String,
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspReferencesArgs {
    /// Absolute path to the source file.
    pub file: String,
    /// 0-indexed line number.
    pub line: u32,
    /// 0-indexed UTF-16 column within the line.
    pub column: u32,
    /// Whether to include the definition site in the results. Defaults to true.
    #[serde(default = "default_true")]
    pub include_declaration: bool,
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub root: Option<String>,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct LspWorkspaceSymbolsArgs {
    /// Fuzzy query string (empty string = return all top-level symbols).
    pub query: String,
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub root: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GitPathArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GitLogArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Maximum number of commits to return. Default 20.
    #[serde(default = "default_log_limit")]
    pub limit: usize,
}

fn default_log_limit() -> usize {
    20
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GitDiffArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Which diff to produce. One of: "head-to-worktree" (default), "index-to-worktree", "head-to-index".
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional path spec to limit the diff (e.g. "src/foo.rs").
    #[serde(default)]
    pub pathspec: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GitBlameArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Path to the file to blame (absolute, or relative to the repo root).
    pub file: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexStatusArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA to query. If omitted, returns the state of the most
    /// recently enqueued commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct IndexCommitArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA to index. If omitted, uses the repo's current HEAD.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct WorkLastKnownStateArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScipDocumentsArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScipSymbolsArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Case-insensitive substring against `display_name` or the symbol id.
    /// Empty string returns every symbol in the index.
    pub query: String,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ScipReferencesArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Exact SCIP symbol id (from `scip_symbols`).
    pub symbol: String,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectMapArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Filter symbols by SCIP `SymbolInformation.kind` (exact match,
    /// case-sensitive — "Function", "Struct", "Enum", "Trait",
    /// "Class", "Method", etc.). Omit or pass an empty array to
    /// include every kind.
    #[serde(default)]
    pub kinds: Vec<String>,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ImpactOfChangeArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Exact SCIP symbol id (from `scip_symbols`).
    pub symbol: String,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PublicApiDiffArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Baseline commit (the "before"). Must have a Ready SCIP index
    /// — call `index_commit` first if needed.
    pub sha1: String,
    /// Target commit (the "after"). Must have a Ready SCIP index.
    pub sha2: String,
    /// Optional kind filter on the symbols compared (e.g.
    /// `["Function","Trait","Struct"]`). Empty = every kind.
    #[serde(default)]
    pub kinds: Vec<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TestsForSymbolArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Exact SCIP symbol id (from `scip_symbols`).
    pub symbol: String,
    /// Commit SHA whose index to query. Defaults to the most recently
    /// indexed Ready commit for this repo.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TestsForChangedFilesArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Commit SHA whose index to query when resolving callers.
    /// Defaults to the most recently indexed Ready commit.
    #[serde(default)]
    pub sha: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DogfoodCoverageGapsArgs {
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Directory (relative to `path`) where dogfood run records
    /// live. Defaults to `dogfood/runs`.
    #[serde(default)]
    pub runs_dir: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct TaskContextArgs {
    /// Absolute or repo-relative path of the file to orient around.
    pub file: String,
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub root: Option<String>,
    /// How many recent commits touching `file` to include. Defaults
    /// to 5.
    #[serde(default)]
    pub history_limit: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RunProjectArgs {
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Extra args appended to the plugin's runner (e.g. `["--release"]`).
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Wall-clock budget in seconds. Defaults to 300.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct RunTestsArgs {
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Optional test filter passed as the first positional arg
    /// (e.g. `"my_test"` → `cargo test my_test`).
    #[serde(default)]
    pub filter: Option<String>,
    /// Extra args appended to the test-runner command.
    #[serde(default)]
    pub extra_args: Vec<String>,
    /// Wall-clock budget in seconds. Defaults to 300.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct InstallPackageArgs {
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Packages to install (passed after the plugin's `install_args`).
    pub packages: Vec<String>,
    /// Wall-clock budget in seconds. Defaults to 300.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ReadExecLogArgs {
    /// Absolute path to the log file — typically `stdout_log` or
    /// `stderr_log` from a `run_project` / `run_tests` /
    /// `install_package` response.
    pub path: String,
    /// Byte offset to start reading from. Defaults to 0 (start of file).
    /// Poll a still-running tool's output by advancing this offset
    /// across calls.
    #[serde(default)]
    pub offset: u64,
    /// Maximum bytes to return per call. Defaults to 64 KiB.
    #[serde(default)]
    pub max_bytes: Option<usize>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapLaunchArgs {
    /// Project root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Absolute path to the debuggee executable (e.g. `target/debug/my_bin`).
    pub program: String,
    /// Command-line arguments passed to the debuggee.
    #[serde(default)]
    pub args: Vec<String>,
    /// Stop on program entry. Defaults to true so the agent can
    /// inspect state before the debuggee runs.
    #[serde(default)]
    pub stop_on_entry: Option<bool>,
    /// Optional environment overrides passed through to the adapter's
    /// `env` launch field (shape is adapter-specific).
    #[serde(default)]
    pub env: Option<serde_json::Value>,
    /// Session identifier. Use different names to debug two programs
    /// concurrently. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapSetBreakpointsArgs {
    /// Absolute path to the source file.
    pub source: String,
    /// 1-indexed line numbers for the breakpoints.
    pub lines: Vec<i64>,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapContinueArgs {
    /// Thread to resume. Defaults to the thread reported by the most
    /// recent `stopped` event.
    #[serde(default)]
    pub thread_id: Option<i64>,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapStepArgs {
    /// Thread to step. Defaults to the thread reported by the most
    /// recent `stopped` event.
    #[serde(default)]
    pub thread_id: Option<i64>,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapPauseArgs {
    /// Thread to pause. Must be explicit — when the debuggee is
    /// running there is no "current stopped thread" to default to.
    pub thread_id: i64,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapStackTraceArgs {
    /// Thread to read. Defaults to the thread reported by the most
    /// recent `stopped` event.
    #[serde(default)]
    pub thread_id: Option<i64>,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapScopesArgs {
    pub frame_id: i64,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapVariablesArgs {
    /// `variablesReference` from a scope or variable returned earlier.
    pub variables_reference: i64,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapEvaluateArgs {
    /// Expression in the adapter's native syntax (e.g. gdb/lldb syntax
    /// for codelldb).
    pub expression: String,
    /// Frame to evaluate in. Omit to evaluate in the "global" context.
    #[serde(default)]
    pub frame_id: Option<i64>,
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct DapTerminateArgs {
    /// Session identifier. Defaults to `"default"`.
    #[serde(default)]
    pub session: Option<String>,
}

#[derive(Copy, Clone)]
enum StepKind {
    Over,
    In,
    Out,
}

const DEFAULT_DAP_SESSION: &str = "default";

#[derive(Clone)]
pub struct AideServer {
    registry: Registry,
    paths: AidePaths,
    /// Live config — swapped by a background reloader every
    /// [`CONFIG_RELOAD_INTERVAL`] on mtime change. Tool methods take
    /// a short read lock when they need a value.
    config: Arc<RwLock<Config>>,
    pool: Arc<LspPool>,
    indexer: Indexer,
    /// Active DAP sessions keyed by a user-chosen name (default
    /// `"default"`). A fresh launch uses the name supplied in the
    /// request; existing names are refused until the caller explicitly
    /// terminates the session.
    dap_sessions: Arc<AsyncMutex<HashMap<String, Arc<DapClient>>>>,
    #[allow(
        dead_code,
        reason = "field is read via #[tool_handler] macro expansion"
    )]
    tool_router: ToolRouter<Self>,
}

/// How often the background reloader polls `~/.aide/config.toml`.
const CONFIG_RELOAD_INTERVAL: Duration = Duration::from_secs(5);

impl AideServer {
    pub fn new() -> anyhow::Result<Self> {
        let paths = AidePaths::from_home()?;
        let config = Config::load(&paths.config_file())?;
        let indexer = Indexer::start(&paths, &config.scip)?;
        let config = Arc::new(RwLock::new(config));
        let config_path = paths.config_file();
        spawn_config_reloader(config_path, config.clone(), indexer.retention_handle());
        Ok(Self {
            registry: Registry::builtin(),
            paths,
            config,
            pool: Arc::new(LspPool::new()),
            indexer,
            dap_sessions: Arc::new(AsyncMutex::new(HashMap::new())),
            tool_router: Self::tool_router(),
        })
    }

    /// Current exec default timeout (config is live-reloaded).
    async fn exec_default_timeout(&self) -> u64 {
        self.config.read().await.exec.default_timeout_secs
    }

    /// Current DAP wait-for-stopped timeout (config is live-reloaded).
    async fn dap_stop_timeout(&self) -> Duration {
        Duration::from_secs(self.config.read().await.dap.stop_timeout_secs)
    }

    /// Build a [`Progress`] if the MCP client attached a `progressToken`
    /// to the request meta. No token → no heartbeat.
    fn progress_for(&self, ctx: &RequestContext<RoleServer>, label: &str) -> Option<Progress> {
        let _ = self;
        ctx.meta.get_progress_token().map(|token| Progress {
            token,
            peer: ctx.peer.clone(),
            label: label.to_string(),
        })
    }

    /// Clone the Arc to the DAP client for `session` (default `"default"`).
    async fn dap_client(&self, session: Option<&str>) -> Result<Arc<DapClient>, String> {
        let name = session.unwrap_or(DEFAULT_DAP_SESSION);
        self.dap_sessions
            .lock()
            .await
            .get(name)
            .cloned()
            .ok_or_else(|| {
                format!("no DAP session named `{name}`; call dap_launch with `session` first")
            })
    }

    async fn run_step(
        &self,
        session: Option<&str>,
        thread_id: Option<i64>,
        kind: StepKind,
    ) -> String {
        let client = match self.dap_client(session).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        let thread_id = match thread_id {
            Some(t) => t,
            None => match client.current_stopped().await.and_then(|s| s.thread_id) {
                Some(t) => t,
                None => return error_json("no stopped thread; pass thread_id explicitly"),
            },
        };
        let op = match kind {
            StepKind::Over => client.next(thread_id).await,
            StepKind::In => client.step_in(thread_id).await,
            StepKind::Out => client.step_out(thread_id).await,
        };
        if let Err(e) = op {
            return error_json(e.to_string());
        }
        match client.wait_for_stopped(self.dap_stop_timeout().await).await {
            Ok(info) => to_json(&info),
            Err(e) => error_json(format!("no stop after step: {e}")),
        }
    }

    /// Select a language plugin that claims `root`, together with the
    /// path to its LSP binary in `~/.aide/bin/` and the plugin-supplied
    /// launch args (empty for servers that take no arguments).
    fn language_for(
        &self,
        root: &std::path::Path,
    ) -> Option<(Arc<dyn LanguagePlugin>, PathBuf, Vec<OsString>)> {
        let plugin = self.registry.detect(root).into_iter().next()?;
        let binary = self.paths.bin().join(plugin.lsp().executable);
        let args = plugin.lsp_spawn_args(root, &self.paths);
        Some((plugin, binary, args))
    }

    /// Find the `.scip` file to query for `repo_root`, preferring the
    /// explicit `sha` when given, else the most recently indexed Ready
    /// commit. Returns a human-readable error message if no Ready index
    /// is available yet (e.g. the worker is still indexing).
    async fn resolve_scip_path(
        &self,
        repo_root: &str,
        sha: Option<&str>,
    ) -> Result<PathBuf, String> {
        let info = match sha {
            Some(s) => self
                .indexer
                .status(repo_root, Some(s))
                .await
                .ok_or_else(|| format!("no indexer state for {repo_root}@{s}"))?,
            None => self
                .indexer
                .last_ready(repo_root)
                .await
                .ok_or_else(|| format!("no ready index for {repo_root}"))?,
        };
        match info.state {
            aide_proto::IndexState::Ready => info
                .index_path
                .map(PathBuf::from)
                .ok_or_else(|| "ready commit is missing an index_path".to_string()),
            other => Err(format!("index for {} is {:?}, not Ready", info.sha, other)),
        }
    }

    /// Best-effort enrichment: for each grep hit, attach the enclosing
    /// definition's display name to every line, using the most recent
    /// Ready SCIP index for `root`. Skips annotation (returns meta with
    /// non-ready state) when nothing is Ready, the `.scip` file cannot be
    /// loaded, or the hit's path is not covered. Hits whose file is
    /// dirty relative to the indexed commit will see best-effort
    /// annotations that may point at a neighbouring symbol — good enough
    /// for orientation, not a substitute for `lsp_*` tools on the live
    /// tree.
    ///
    /// Returns a [`ScipMeta`] describing the indexer's view of this repo
    /// so `project_grep` can surface `scip.state` to the caller — silent
    /// downgrade from "annotated" to "bare matches" used to look the same
    /// as "no matching symbol", see ux-gotcha #2.
    async fn annotate_hits_with_scip(
        &self,
        root: &std::path::Path,
        hits: &mut [aide_search::GrepHit],
    ) -> ScipMeta {
        let root_str = root.display().to_string();
        let Some(info) = self.indexer.last_known(&root_str).await else {
            return ScipMeta::no_index();
        };
        let meta = ScipMeta::from_commit_info(&info);
        if !matches!(info.state, aide_proto::IndexState::Ready) {
            return meta;
        }
        let Some(index_path) = info.index_path.as_ref() else {
            return meta;
        };
        let Ok(index) = aide_scip::load(std::path::Path::new(index_path)) else {
            return meta;
        };
        for hit in hits {
            for line in &mut hit.lines {
                let line_0based = i32::try_from(line.line.saturating_sub(1)).unwrap_or(i32::MAX);
                line.symbol = aide_scip::enclosing_definition(&index, &hit.path, line_0based);
            }
        }
        meta
    }

    /// Best-effort enrichment: for each diagnostic with a source span,
    /// fill `enclosing_symbol` from the most recent Ready SCIP index
    /// for `root`. Rules mirror [`Self::annotate_hits_with_scip`] —
    /// silent no-op when no SCIP is available or the path is not
    /// covered. Diagnostic line numbers are 1-indexed as reported by
    /// cargo / rustc; SCIP is 0-indexed, so we subtract one.
    async fn annotate_diagnostics_with_scip(
        &self,
        root: &std::path::Path,
        diagnostics: &mut [aide_proto::Diagnostic],
    ) {
        if diagnostics.is_empty() {
            return;
        }
        let root_str = root.display().to_string();
        let Ok(scip_path) = self.resolve_scip_path(&root_str, None).await else {
            return;
        };
        let Ok(index) = aide_scip::load(&scip_path) else {
            return;
        };
        for d in diagnostics {
            let (Some(file), Some(line)) = (d.file.as_deref(), d.line_start) else {
                continue;
            };
            let rel = relativize_path(root, file);
            let line_0based = i32::try_from(line.saturating_sub(1)).unwrap_or(i32::MAX);
            d.enclosing_symbol = aide_scip::enclosing_definition(&index, &rel, line_0based);
        }
    }
}

/// Turn a span path reported by a build tool into a repo-relative path
/// that can be looked up in a SCIP index. cargo usually emits paths
/// relative to the package manifest, which for single-crate repos
/// already matches SCIP's `relative_path`; for workspaces or when a
/// tool emits absolute paths we strip the `root` prefix. Returns the
/// input unchanged when neither transformation applies.
fn relativize_path(root: &std::path::Path, file: &str) -> String {
    let p = std::path::Path::new(file);
    if let Ok(stripped) = p.strip_prefix(root) {
        return stripped.display().to_string();
    }
    file.to_string()
}

fn range_from_args(
    line: u32,
    column: u32,
    end_line: Option<u32>,
    end_column: Option<u32>,
) -> lsp_types::Range {
    let start = lsp_types::Position {
        line,
        character: column,
    };
    let end = lsp_types::Position {
        line: end_line.unwrap_or(line),
        character: end_column.unwrap_or(column),
    };
    lsp_types::Range { start, end }
}

#[tool_router]
impl AideServer {
    #[tool(description = "Detect which supported languages appear in the given project root")]
    fn project_detect(&self, Parameters(args): Parameters<ProjectDetectArgs>) -> String {
        let root = resolve_root(args.path);
        let languages: Vec<DetectedLanguage> = self
            .registry
            .detect(&root)
            .into_iter()
            .map(|p| DetectedLanguage {
                id: p.id().as_str().to_string(),
                lsp: p.lsp().name.to_string(),
            })
            .collect();

        let result = ProjectDetectResult {
            root: root.display().to_string(),
            languages,
        };

        to_json(&result)
    }

    #[tool(
        description = "Install the LSP server, SCIP indexer, and debug adapter binaries for every language detected in the given project root. Idempotent — already-installed versions are skipped. Also enqueues the current HEAD commit for SCIP indexing in the background."
    )]
    async fn project_setup(&self, Parameters(args): Parameters<ProjectSetupArgs>) -> String {
        let root = resolve_root(args.path);
        let plugins = self.registry.detect(&root);

        let languages: Vec<String> = plugins
            .iter()
            .map(|p| p.id().as_str().to_string())
            .collect();

        let mut reports = Vec::new();
        for plugin in &plugins {
            for spec in plugin.tools() {
                let report = match install_tool(&self.paths, &spec).await {
                    Ok(InstallOutcome::AlreadyInstalled { path, version }) => ToolInstallReport {
                        name: spec.name.clone(),
                        version,
                        status: "already-installed",
                        path: Some(path.display().to_string()),
                        error: None,
                    },
                    Ok(InstallOutcome::Installed { path, version }) => ToolInstallReport {
                        name: spec.name.clone(),
                        version,
                        status: "installed",
                        path: Some(path.display().to_string()),
                        error: None,
                    },
                    Err(e) => ToolInstallReport {
                        name: spec.name.clone(),
                        version: spec.version.clone(),
                        status: "error",
                        path: None,
                        error: Some(e.to_string()),
                    },
                };
                reports.push(report);
            }
        }

        // Kick off SCIP indexing for the current HEAD in the background.
        self.indexer.enqueue_head(&root).await;

        let result = ProjectSetupResult {
            root: root.display().to_string(),
            languages,
            tools: reports,
        };

        to_json(&result)
    }

    #[tool(
        description = "List files under the project root. Replaces `ls`/`find` with a gitignore-aware, git-scoped enumerator. Scope: \"all\" (default — gitignore-aware working-tree walk, INCLUDES untracked files so newly-created files are visible), \"tracked\" (libgit2 index, tracked files only — fastest but silent on untracked), \"dirty\" (anything non-clean), \"staged\" (index vs HEAD). Optional glob filter applies to repo-relative paths."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn project_ls(&self, Parameters(args): Parameters<ProjectLsArgs>) -> String {
        let root = resolve_root(args.path);
        let scope = match parse_scope(args.scope.as_deref()) {
            Ok(s) => s,
            Err(e) => return error_json(e),
        };
        let max_results = args.max_results.unwrap_or(500);
        let options = aide_search::LsOptions {
            glob: args.glob,
            max_results: Some(max_results),
            include_hidden: args.include_hidden,
        };
        match aide_search::list_files(&root, &scope, &options) {
            Ok(files) => {
                let total = files.len();
                let truncated = total == max_results;
                let result = ProjectLsResult {
                    root: root.display().to_string(),
                    scope: scope_label(&scope).to_string(),
                    files,
                    total,
                    truncated,
                };
                to_json(&result)
            }
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Regex search across project files. Replaces `grep`/`rg` with a gitignore-aware, git-scoped searcher powered by the ripgrep engine (grep-regex + grep-searcher). Smart-case by default, binary files skipped, per-file and total result caps. Default scope = \"all\" (working-tree walk incl. untracked files); pass `scope: \"tracked\"` to restrict to committed files. Returns match lines with optional before/after context tagged by kind. When a SCIP index is Ready for the project, each line is annotated with `symbol` (the enclosing definition's display name) — a semantic layer no plain grep can provide; the response also carries a top-level `scip: {state, sha?}` field so callers know whether enrichment was applied."
    )]
    async fn project_grep(&self, Parameters(args): Parameters<ProjectGrepArgs>) -> String {
        let root = resolve_root(args.path);
        let scope = match parse_scope(args.scope.as_deref()) {
            Ok(s) => s,
            Err(e) => return error_json(e),
        };
        let options = aide_search::GrepOptions {
            glob: args.glob,
            case_sensitive: args.case_sensitive,
            before_context: args.before_context.unwrap_or(0),
            after_context: args.after_context.unwrap_or(0),
            max_results_per_file: args.max_results_per_file.unwrap_or(50),
            max_results: args.max_results.unwrap_or(200),
            include_hidden: args.include_hidden,
        };
        match aide_search::grep(&root, &args.pattern, &scope, &options) {
            Ok(mut result) => {
                let scip_meta = self.annotate_hits_with_scip(&root, &mut result.hits).await;
                let mut value = serde_json::to_value(&result).unwrap_or_default();
                if let Some(obj) = value.as_object_mut() {
                    if let Ok(meta_value) = serde_json::to_value(&scip_meta) {
                        obj.insert("scip".to_string(), meta_value);
                    }
                }
                value.to_string()
            }
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Like project_ls, but against the tree of a specific commit. Reads the git tree object directly — no worktree checkout, no TempDir, no filesystem side effects. Useful for auditing historical state without disturbing the working tree. Always uses the committed file set as scope (git scopes Tracked/Dirty/Staged don't apply)."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn project_ls_at(&self, Parameters(args): Parameters<ProjectLsAtArgs>) -> String {
        let root = resolve_root(args.path);
        let max_results = args.max_results.unwrap_or(500);
        let options = aide_search::LsOptions {
            glob: args.glob,
            max_results: Some(max_results),
            include_hidden: false,
        };
        match aide_search::list_files_at(&root, &args.sha, &options) {
            Ok(files) => {
                let total = files.len();
                let truncated = total == max_results;
                let result = ProjectLsResult {
                    root: root.display().to_string(),
                    scope: format!("at:{}", short_sha(&args.sha)),
                    files,
                    total,
                    truncated,
                };
                to_json(&result)
            }
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Like project_grep, but against the tree of a specific commit. Reads blob bytes directly from libgit2 (no worktree export) and runs the ripgrep engine over them. Smart-case, binary skip, context, and result caps work identically to project_grep."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn project_grep_at(&self, Parameters(args): Parameters<ProjectGrepAtArgs>) -> String {
        let root = resolve_root(args.path);
        let options = aide_search::GrepOptions {
            glob: args.glob,
            case_sensitive: args.case_sensitive,
            before_context: args.before_context.unwrap_or(0),
            after_context: args.after_context.unwrap_or(0),
            max_results_per_file: args.max_results_per_file.unwrap_or(50),
            max_results: args.max_results.unwrap_or(200),
            include_hidden: false,
        };
        match aide_search::grep_at(&root, &args.sha, &args.pattern, &options) {
            Ok(result) => to_json(&result),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP hover: summary of the symbol at (file, line, column). Returns null if no symbol is at that position. Requires project_setup to have installed the language server."
    )]
    async fn lsp_hover(&self, Parameters(args): Parameters<LspPositionArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::hover(&client, &file, args.line, args.column).await {
            Ok(Some(h)) => to_json(&h),
            Ok(None) => "null".to_string(),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP goto-definition: return the source location(s) where the symbol at (file, line, column) is defined."
    )]
    async fn lsp_definition(&self, Parameters(args): Parameters<LspPositionArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::definition(&client, &file, args.line, args.column).await {
            Ok(hits) => to_json(&hits),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Apply a unique `old_string` → `new_string` replacement in `file` and return the LSP diagnostic delta: new errors, new warnings, resolved findings, and the count of unchanged ones. `old_string` must occur exactly once. Optional `related_files` expands which files get snapshotted (use when an edit is expected to propagate to callers). `settle_ms` (default 1500) controls how long to wait between the edit and the after-snapshot — longer is more reliable on slower servers. The `confidence` field is always `\"best_effort\"`: this tool is a fast feedback loop, not a replacement for `run_tests`/`cargo check` when the stakes are high."
    )]
    async fn safe_edit(&self, Parameters(args): Parameters<SafeEditArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        let related: Vec<PathBuf> = args.related_files.iter().map(PathBuf::from).collect();
        let settle = Duration::from_millis(args.settle_ms.unwrap_or(1500));

        match lsp_ops::safe_edit(
            &client,
            &file,
            &args.old_string,
            &args.new_string,
            &related,
            settle,
        )
        .await
        {
            Ok(report) => to_json(&report),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "List LSP code actions (quick-fixes, refactorings, \"organize imports\", \"fill match arms\", …) offered at a range in `file`. When `end_line`/`end_column` are omitted, the range collapses to the cursor position. Each entry has `title`, optional `kind`, and a `disabled` flag. Pair with `lsp_apply_code_action` to actually run one."
    )]
    async fn lsp_list_code_actions(
        &self,
        Parameters(args): Parameters<LspCodeActionRangeArgs>,
    ) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        let range = range_from_args(args.line, args.column, args.end_line, args.end_column);
        match lsp_ops::list_code_actions(&client, &file, range).await {
            Ok(hits) => to_json(&hits),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Run a single LSP code action at a range in `file`, selected by `kind` (exact, e.g. \"source.organizeImports\") or `title` (case-insensitive substring). Resolves the action if the server returned a lazy stub, applies its WorkspaceEdit (both `changes` and `document_changes` shapes), and dispatches any attached `workspace/executeCommand`. Returns `{title, kind, applied_edit: {files, total_edits}, ran_command}` — null if no offered action matches the selector."
    )]
    async fn lsp_apply_code_action(
        &self,
        Parameters(args): Parameters<LspApplyCodeActionArgs>,
    ) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let selector = if let Some(kind) = args.kind {
            lsp_ops::CodeActionSelector::Kind(kind)
        } else if let Some(title) = args.title {
            lsp_ops::CodeActionSelector::Title(title)
        } else {
            return error_json("one of `title` or `kind` must be set".to_string());
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        let range = range_from_args(args.line, args.column, args.end_line, args.end_column);
        match lsp_ops::apply_code_action(&client, &file, range, &selector).await {
            Ok(Some(applied)) => to_json(&applied),
            Ok(None) => "null".to_string(),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Rename the symbol at (file, line, column) to `new_name` via LSP `textDocument/rename`. Applies the resulting WorkspaceEdit to disk across every touched file and keeps the language server's in-memory buffers in sync. Returns `{files, total_edits}` — per-file change counts. Unlike shell find-and-replace, this respects scope, traits, and reexports. Returns null if the symbol at that position is not renameable."
    )]
    async fn lsp_rename_symbol(&self, Parameters(args): Parameters<LspRenameArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::rename(&client, &file, args.line, args.column, args.new_name).await {
            Ok(Some(summary)) => to_json(&summary),
            Ok(None) => "null".to_string(),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Recursively expand the macro invocation at (file, line, column) using rust-analyzer's `rust-analyzer/expandMacro` extension. Returns the macro's display name and the expanded source, or null when the position is not inside a macro. Currently only wired for Rust — other languages ignore the request."
    )]
    async fn lsp_expand_macro(&self, Parameters(args): Parameters<LspPositionArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::expand_macro(&client, &file, args.line, args.column).await {
            Ok(Some(h)) => to_json(&h),
            Ok(None) => "null".to_string(),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP diagnostics for a single file (errors, warnings). Waits briefly for the server to finish analysing the file, then returns the published diagnostics."
    )]
    async fn lsp_diagnostics(&self, Parameters(args): Parameters<LspFileArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::diagnostics(&client, &file, std::time::Duration::from_millis(500)).await {
            Ok(d) => to_json(&d),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP references: return every source location that references the symbol at (file, line, column)."
    )]
    async fn lsp_references(&self, Parameters(args): Parameters<LspReferencesArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::references(
            &client,
            &file,
            args.line,
            args.column,
            args.include_declaration,
        )
        .await
        {
            Ok(hits) => to_json(&hits),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP document symbols: a hierarchical outline of every symbol (function, struct, method, …) in a single file."
    )]
    async fn lsp_document_symbols(&self, Parameters(args): Parameters<LspFileArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::document_symbols(&client, &file).await {
            Ok(symbols) => to_json(&symbols),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "LSP workspace symbols: fuzzy-search every symbol defined anywhere in the project. Empty query returns top-level symbols."
    )]
    async fn lsp_workspace_symbols(
        &self,
        Parameters(args): Parameters<LspWorkspaceSymbolsArgs>,
    ) -> String {
        let root = resolve_root(args.root);
        let Some((plugin, binary, lsp_args)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };

        match lsp_ops::workspace_symbols(&client, &args.query).await {
            Ok(hits) => to_json(&hits),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "git status: branch, upstream divergence, and per-file working-tree + index state. Also nudges the SCIP indexer to keep up with new commits."
    )]
    async fn git_status(&self, Parameters(args): Parameters<GitPathArgs>) -> String {
        let root = resolve_root(args.path);
        let body = match aide_git::status::status(&root) {
            Ok(s) => to_json(&s),
            Err(e) => error_json(e.to_string()),
        };
        self.indexer.enqueue_head(&root).await;
        body
    }

    #[tool(
        description = "git log: recent commits reachable from HEAD. Returns sha, author, summary, time; newest first. Also nudges the SCIP indexer to keep up with new commits."
    )]
    async fn git_log(&self, Parameters(args): Parameters<GitLogArgs>) -> String {
        let root = resolve_root(args.path);
        let body = match aide_git::log::log(&root, args.limit) {
            Ok(entries) => to_json(&entries),
            Err(e) => error_json(e.to_string()),
        };
        self.indexer.enqueue_head(&root).await;
        body
    }

    #[tool(
        description = "git diff: unified diff patch plus stats (files changed, insertions, deletions). Selects HEAD vs worktree by default. Also nudges the SCIP indexer to keep up with new commits."
    )]
    async fn git_diff(&self, Parameters(args): Parameters<GitDiffArgs>) -> String {
        let root = resolve_root(args.path);
        let mode = match args.mode.as_deref() {
            Some("index-to-worktree") => DiffMode::IndexToWorktree,
            Some("head-to-index") => DiffMode::HeadToIndex,
            None | Some("head-to-worktree") => DiffMode::HeadToWorktree,
            Some(other) => return error_json(format!("unknown diff mode: {other}")),
        };
        let body = match aide_git::diff::diff(&root, mode, args.pathspec.as_deref()) {
            Ok(d) => to_json(&d),
            Err(e) => error_json(e.to_string()),
        };
        self.indexer.enqueue_head(&root).await;
        body
    }

    #[tool(
        description = "git blame: per-line authorship for a single file. Each entry gives the commit, author, time, and summary introducing that line."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn git_blame(&self, Parameters(args): Parameters<GitBlameArgs>) -> String {
        let root = resolve_root(args.path);
        let file = PathBuf::from(&args.file);
        match aide_git::blame::blame(&root, &file) {
            Ok(lines) => to_json(&lines),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Explicitly enqueue a commit for SCIP indexing. Defaults to the repo's current HEAD when no sha is given. Returns the current CommitInfo; indexing continues in the background."
    )]
    async fn index_commit(&self, Parameters(args): Parameters<IndexCommitArgs>) -> String {
        let root = resolve_root(args.path);
        let (repo_root, sha) = match args.sha {
            Some(sha) => (root.display().to_string(), sha),
            None => match aide_git::resolve_head(&root) {
                Ok((rr, sha)) => (rr.display().to_string(), sha),
                Err(e) => return error_json(e.to_string()),
            },
        };
        match self.indexer.enqueue(repo_root, sha).await {
            Ok(info) => to_json(&info),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "State of a commit in the in-process SCIP indexer. Defaults to the most recently enqueued commit for this repo. Returns null when nothing is known yet."
    )]
    async fn index_status(&self, Parameters(args): Parameters<IndexStatusArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        match self.indexer.status(&repo_root, args.sha.as_deref()).await {
            Some(info) => to_json(&info),
            None => "null".to_string(),
        }
    }

    #[tool(
        description = "Last commit the indexer knows about for this repo. Use this to recover an agent's last stable view of 'completed work' across sessions. Returns null when nothing has been enqueued."
    )]
    async fn work_last_known_state(
        &self,
        Parameters(args): Parameters<WorkLastKnownStateArgs>,
    ) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        match self.indexer.last_known(&repo_root).await {
            Some(info) => to_json(&info),
            None => "null".to_string(),
        }
    }

    #[tool(
        description = "List every document (file path relative to the repo root) covered by the SCIP index for a commit. Defaults to the most recently indexed Ready commit."
    )]
    async fn scip_documents(&self, Parameters(args): Parameters<ScipDocumentsArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        match aide_scip::load(&index_path) {
            Ok(idx) => to_json(&aide_scip::documents(&idx)),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Fuzzy-search SCIP symbols by display name or symbol id (case-insensitive substring). Empty query returns everything. Queries the most recently indexed Ready commit by default."
    )]
    async fn scip_symbols(&self, Parameters(args): Parameters<ScipSymbolsArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        match aide_scip::load(&index_path) {
            Ok(idx) => to_json(&aide_scip::find_symbols(&idx, &args.query)),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Every occurrence of a SCIP symbol id across the index, with `is_definition` flag. Pair with `scip_symbols` to discover the symbol id first."
    )]
    async fn scip_references(&self, Parameters(args): Parameters<ScipReferencesArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        match aide_scip::load(&index_path) {
            Ok(idx) => to_json(&aide_scip::references(&idx, &args.symbol)),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Call sites of a SCIP symbol — every occurrence except the definition itself. Shorthand for `scip_references` filtered to non-definitions; use it when the question is 'who uses this?'."
    )]
    async fn scip_callers(&self, Parameters(args): Parameters<ScipReferencesArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        match aide_scip::load(&index_path) {
            Ok(idx) => to_json(&aide_scip::callers(&idx, &args.symbol)),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Per-document digest of the public API surface from the SCIP index — every document that defines at least one matching symbol, with its top-level symbols (name, kind, definition line). Replaces 'grep for `pub fn`/`class`/`interface`' reflexes. Defaults to the most recently indexed Ready commit; filter by `kinds` (e.g. [\"Function\",\"Trait\"]) to narrow the view."
    )]
    async fn project_map(&self, Parameters(args): Parameters<ProjectMapArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        match aide_scip::load(&index_path) {
            Ok(idx) => {
                let kinds: Vec<&str> = args.kinds.iter().map(String::as_str).collect();
                to_json(&aide_scip::project_map(&idx, &kinds))
            }
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Callers of `symbol` classified by the plugin's path heuristic (test / bin / lib / example / bench). Same scan as `scip_callers` but each hit carries a `category` field so an agent can see at a glance how much of the fallout is test-only vs production code. Use before a risky rename / signature change."
    )]
    async fn impact_of_change(&self, Parameters(args): Parameters<ImpactOfChangeArgs>) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        let idx = match aide_scip::load(&index_path) {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };
        let callers = aide_scip::enclosing_defs_of_callers(&idx, &args.symbol);
        let entries: Vec<serde_json::Value> = callers
            .into_iter()
            .map(|c| {
                let category = if plugin.is_test_symbol(&c.relative_path, &c.display_name) {
                    "test"
                } else {
                    plugin.classify_path(&c.relative_path)
                };
                let mut v = serde_json::to_value(&c).unwrap_or_default();
                if let Some(obj) = v.as_object_mut() {
                    obj.insert(
                        "category".to_string(),
                        serde_json::Value::String(category.to_string()),
                    );
                }
                v
            })
            .collect();
        to_json(&entries)
    }

    #[tool(
        description = "Structured diff of the public API surface between two commits. Both must have a Ready SCIP index — call `index_commit` for either SHA that hasn't been indexed yet. Returns `{added: [...], removed: [...]}` lists of symbols (by SCIP id). Use `kinds` to narrow to e.g. `[\"Function\",\"Trait\"]` when checking semver impact of a refactor."
    )]
    async fn public_api_diff(&self, Parameters(args): Parameters<PublicApiDiffArgs>) -> String {
        let root = resolve_root(args.path);
        let repo_root = root.display().to_string();

        let path1 = match self.resolve_scip_path(&repo_root, Some(&args.sha1)).await {
            Ok(p) => p,
            Err(e) => return error_json(format!("sha1 not Ready: {e}")),
        };
        let path2 = match self.resolve_scip_path(&repo_root, Some(&args.sha2)).await {
            Ok(p) => p,
            Err(e) => return error_json(format!("sha2 not Ready: {e}")),
        };
        let idx1 = match aide_scip::load(&path1) {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };
        let idx2 = match aide_scip::load(&path2) {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };

        let kinds: Vec<&str> = args.kinds.iter().map(String::as_str).collect();
        let surface = |idx: &aide_scip::ScipIndex| -> std::collections::BTreeMap<String, aide_scip::LocatedSymbol> {
            aide_scip::project_map(idx, &kinds)
                .into_iter()
                .flat_map(|entry| {
                    let path = entry.relative_path.clone();
                    entry.symbols.into_iter().map(move |s| {
                        (
                            s.symbol.clone(),
                            aide_scip::LocatedSymbol {
                                symbol: s.symbol,
                                display_name: s.display_name,
                                kind: s.kind,
                                relative_path: path.clone(),
                                line: s.line,
                            },
                        )
                    })
                })
                .collect()
        };
        let before = surface(&idx1);
        let after = surface(&idx2);

        let added: Vec<_> = after
            .iter()
            .filter(|(k, _)| !before.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();
        let removed: Vec<_> = before
            .iter()
            .filter(|(k, _)| !after.contains_key(*k))
            .map(|(_, v)| v.clone())
            .collect();

        to_json(&serde_json::json!({
            "sha1": args.sha1,
            "sha2": args.sha2,
            "added": added,
            "removed": removed,
        }))
    }

    #[tool(
        description = "Tests (by the language plugin's test heuristic) that transitively reference `symbol`. Walks every non-definition occurrence of the symbol in the SCIP index, resolves the enclosing function, and keeps those the plugin recognises as tests (Rust: `tests/` paths, `*_test.rs`, `test_*` / `*_test` names). Returns the test symbols; pair with `run_tests`."
    )]
    async fn tests_for_symbol(&self, Parameters(args): Parameters<TestsForSymbolArgs>) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        let idx = match aide_scip::load(&index_path) {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };
        let tests: Vec<_> = aide_scip::enclosing_defs_of_callers(&idx, &args.symbol)
            .into_iter()
            .filter(|c| plugin.is_test_symbol(&c.relative_path, &c.display_name))
            .collect();
        to_json(&tests)
    }

    #[tool(
        description = "Tests worth running after the current working-tree changes. Collects every symbol defined in dirty or staged files, then combines (a) tests that directly reference any of those symbols with (b) tests defined in the changed files themselves. Deduplicated, tagged with the file and line where each test lives. Agent can feed the display names into `run_tests` as filters."
    )]
    async fn tests_for_changed_files(
        &self,
        Parameters(args): Parameters<TestsForChangedFilesArgs>,
    ) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let changed_paths: Vec<String> = match aide_git::status::status(&root) {
            Ok(s) => s
                .files
                .into_iter()
                .filter(|f| !f.is_ignored && (f.staged.is_some() || f.working.is_some()))
                .map(|f| f.path)
                .collect(),
            Err(e) => return error_json(e.to_string()),
        };
        if changed_paths.is_empty() {
            return to_json(&Vec::<aide_scip::LocatedSymbol>::new());
        }
        let repo_root = root.display().to_string();
        let index_path = match self
            .resolve_scip_path(&repo_root, args.sha.as_deref())
            .await
        {
            Ok(p) => p,
            Err(e) => return error_json(e),
        };
        let idx = match aide_scip::load(&index_path) {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut out: Vec<aide_scip::LocatedSymbol> = Vec::new();

        // (a) tests defined directly inside changed files.
        for path in &changed_paths {
            for sym in aide_scip::defs_in_path(&idx, path) {
                if plugin.is_test_symbol(&sym.relative_path, &sym.display_name)
                    && seen.insert(sym.symbol.clone())
                {
                    out.push(sym);
                }
            }
        }

        // (b) tests that reference any symbol defined in a changed file.
        for path in &changed_paths {
            for sym in aide_scip::defs_in_path(&idx, path) {
                for cand in aide_scip::enclosing_defs_of_callers(&idx, &sym.symbol) {
                    if plugin.is_test_symbol(&cand.relative_path, &cand.display_name)
                        && seen.insert(cand.symbol.clone())
                    {
                        out.push(cand);
                    }
                }
            }
        }

        to_json(&out)
    }

    #[tool(
        description = "Aggregate `Coverage gaps` bullets from `dogfood/runs/*.md` (or `runs_dir` override) and rank them by frequency. Each gap record lists the missing capability, the agent's proposed tool name (text after '→'), and which run files mentioned it. Drives the dogfood → roadmap feedback loop — the most-frequent gaps point at what the benchmark keeps catching the aide agent lacking."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn dogfood_coverage_gaps(
        &self,
        Parameters(args): Parameters<DogfoodCoverageGapsArgs>,
    ) -> String {
        let root = resolve_root(args.path);
        let runs_rel = args.runs_dir.unwrap_or_else(|| "dogfood/runs".to_string());
        let runs_dir = root.join(&runs_rel);
        match aggregate_coverage_gaps(&runs_dir) {
            Ok(report) => to_json(&report),
            Err(e) => error_json(e),
        }
    }

    #[tool(
        description = "Aggregate orientation data for a single file: LSP document symbols, LSP diagnostics, head-to-worktree diff for just this file, the last `history_limit` commits that touched it, and SCIP top-level symbols. One call replaces the five agents typically make when picking up work on a file. Any sub-query that fails (LSP warming up, no SCIP Ready yet) contributes a null/empty field — the rest of the response stays valid."
    )]
    async fn task_context(&self, Parameters(args): Parameters<TaskContextArgs>) -> String {
        let root = resolve_root(args.root);
        let file = PathBuf::from(&args.file);
        let history_limit = args.history_limit.unwrap_or(5);

        let relative = file
            .strip_prefix(&root)
            .map_or_else(|_| file.clone(), std::path::Path::to_path_buf);
        let relative_str = relative.display().to_string();

        let document_symbols = serde_json::Value::Null;
        let diagnostics = serde_json::Value::Null;
        let mut ctx = serde_json::json!({
            "file": file.display().to_string(),
            "relative_path": relative_str.clone(),
            "document_symbols": document_symbols,
            "diagnostics": diagnostics,
            "head_diff": serde_json::Value::Null,
            "recent_commits": serde_json::Value::Array(Vec::new()),
            "scip_symbols": serde_json::Value::Array(Vec::new()),
        });

        if let Some((plugin, binary, lsp_args)) = self.language_for(&root) {
            ctx["language"] = serde_json::Value::String(plugin.id().as_str().to_string());
            if let Ok(client) = self
                .pool
                .get_or_spawn(plugin.id().as_str(), &root, &binary, &lsp_args)
                .await
            {
                if let Ok(syms) = lsp_ops::document_symbols(&client, &file).await {
                    ctx["document_symbols"] = serde_json::to_value(syms).unwrap_or_default();
                }
                if let Ok(diag) =
                    lsp_ops::diagnostics(&client, &file, std::time::Duration::from_millis(500))
                        .await
                {
                    ctx["diagnostics"] = serde_json::to_value(diag).unwrap_or_default();
                }
            }
        }

        if let Ok(diff) = aide_git::diff::diff(&root, DiffMode::HeadToWorktree, Some(&relative_str))
        {
            ctx["head_diff"] = serde_json::to_value(diff).unwrap_or_default();
        }

        if let Ok(commits) = aide_git::log::log_for_path(&root, &relative_str, history_limit) {
            ctx["recent_commits"] = serde_json::to_value(commits).unwrap_or_default();
        }

        let repo_root = root.display().to_string();
        if let Ok(index_path) = self.resolve_scip_path(&repo_root, None).await {
            if let Ok(idx) = aide_scip::load(&index_path) {
                ctx["scip_symbols"] =
                    serde_json::to_value(aide_scip::defs_in_path(&idx, &relative_str))
                        .unwrap_or_default();
            }
        }

        to_json(&ctx)
    }

    #[tool(
        description = "Run the project via the language plugin's runner (e.g. `cargo run`) and return the full stdout + stderr. Captures up to 1 MB per stream. Default timeout 300s; override with timeout_secs. When the plugin has a structured-output parser (Rust: cargo JSON), the response also contains a `diagnostics` array — each entry tagged with its enclosing SCIP symbol when an index is Ready."
    )]
    async fn run_project(
        &self,
        Parameters(args): Parameters<RunProjectArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let runner = plugin.runner();
        let mut argv: Vec<OsString> = runner.args.iter().map(OsString::from).collect();
        argv.extend(plugin.structured_output_args().iter().map(OsString::from));
        argv.extend(args.extra_args.into_iter().map(OsString::from));
        let duration = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(self.exec_default_timeout().await),
        );
        let progress = self.progress_for(&ctx, runner.executable);

        match exec::run(
            runner.executable,
            &argv,
            &root,
            duration,
            Some(&self.paths.logs()),
            progress,
        )
        .await
        {
            Ok(mut result) => {
                result.diagnostics = plugin.parse_diagnostics(&result.stdout);
                self.annotate_diagnostics_with_scip(&root, &mut result.diagnostics)
                    .await;
                to_json(&result)
            }
            Err(e) => error_json(format!("failed to spawn {}: {e}", runner.executable)),
        }
    }

    #[tool(
        description = "Run the project's tests via the language plugin's test_runner (e.g. `cargo test [filter]`). Captures stdout/stderr and exit code. Default timeout 300s. Same structured-diagnostic enrichment as `run_project` when the plugin supports it — compile errors surface as a `diagnostics` array with enclosing-symbol tags."
    )]
    async fn run_tests(
        &self,
        Parameters(args): Parameters<RunTestsArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let runner = plugin.test_runner();
        let mut argv: Vec<OsString> = runner.args.iter().map(OsString::from).collect();
        argv.extend(plugin.structured_output_args().iter().map(OsString::from));
        if let Some(filter) = args.filter {
            argv.push(OsString::from(filter));
        }
        argv.extend(args.extra_args.into_iter().map(OsString::from));
        let duration = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(self.exec_default_timeout().await),
        );
        let progress = self.progress_for(&ctx, runner.executable);

        match exec::run(
            runner.executable,
            &argv,
            &root,
            duration,
            Some(&self.paths.logs()),
            progress,
        )
        .await
        {
            Ok(mut result) => {
                result.diagnostics = plugin.parse_diagnostics(&result.stdout);
                self.annotate_diagnostics_with_scip(&root, &mut result.diagnostics)
                    .await;
                to_json(&result)
            }
            Err(e) => error_json(format!("failed to spawn {}: {e}", runner.executable)),
        }
    }

    #[tool(
        description = "Install packages via the language plugin's package manager (e.g. `cargo add <pkg>`). Each string in `packages` becomes a positional argument after the install subcommand."
    )]
    async fn install_package(
        &self,
        Parameters(args): Parameters<InstallPackageArgs>,
        ctx: RequestContext<RoleServer>,
    ) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        if args.packages.is_empty() {
            return error_json("`packages` must be non-empty");
        }
        let pm = plugin.package_manager();
        let mut argv: Vec<OsString> = pm.install_args.iter().map(OsString::from).collect();
        argv.extend(args.packages.into_iter().map(OsString::from));
        let duration = Duration::from_secs(
            args.timeout_secs
                .unwrap_or(self.exec_default_timeout().await),
        );
        let progress = self.progress_for(&ctx, pm.executable);

        match exec::run(
            pm.executable,
            &argv,
            &root,
            duration,
            Some(&self.paths.logs()),
            progress,
        )
        .await
        {
            Ok(result) => to_json(&result),
            Err(e) => error_json(format!("failed to spawn {}: {e}", pm.executable)),
        }
    }

    #[tool(
        description = "Read `max_bytes` from an exec log file starting at `offset`. Returns `{bytes_read, eof, content, next_offset, total_size}`. Poll a running tool by advancing `offset = next_offset` across calls; stop when `eof` is true AND the producing tool has returned."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    async fn read_exec_log(&self, Parameters(args): Parameters<ReadExecLogArgs>) -> String {
        use std::io::{Read, Seek, SeekFrom};
        let cap = args.max_bytes.unwrap_or(64 * 1024).max(1);
        let path = PathBuf::from(&args.path);
        let mut file = match std::fs::File::open(&path) {
            Ok(f) => f,
            Err(e) => return error_json(format!("open {}: {e}", path.display())),
        };
        let total_size = match file.metadata() {
            Ok(m) => m.len(),
            Err(e) => return error_json(format!("stat {}: {e}", path.display())),
        };
        if args.offset > total_size {
            return to_json(&serde_json::json!({
                "bytes_read": 0,
                "eof": true,
                "content": "",
                "next_offset": total_size,
                "total_size": total_size,
            }));
        }
        if let Err(e) = file.seek(SeekFrom::Start(args.offset)) {
            return error_json(format!("seek {}: {e}", path.display()));
        }
        let mut buf = vec![0u8; cap];
        let n = match file.read(&mut buf) {
            Ok(n) => n,
            Err(e) => return error_json(format!("read {}: {e}", path.display())),
        };
        buf.truncate(n);
        let next_offset = args.offset.saturating_add(n as u64);
        let eof = next_offset >= total_size;
        let content = String::from_utf8_lossy(&buf).into_owned();
        to_json(&serde_json::json!({
            "bytes_read": n,
            "eof": eof,
            "content": content,
            "next_offset": next_offset,
            "total_size": total_size,
        }))
    }

    #[tool(
        description = "Start a DAP debug session for `program` under an optional `session` name (default `\"default\"`). Launch multiple debuggees concurrently by passing distinct session names. Runs the full initialize → launch → configurationDone → first-stop handshake and returns `{ session, stopped }`."
    )]
    async fn dap_launch(&self, Parameters(args): Parameters<DapLaunchArgs>) -> String {
        let root = resolve_root(args.path);
        let Some(plugin) = self.registry.detect(&root).into_iter().next() else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };
        let Some(dap_spec) = plugin.dap() else {
            return error_json(format!(
                "language `{}` does not declare a DAP adapter",
                plugin.id().as_str()
            ));
        };

        let pinned = self.paths.bin().join(dap_spec.executable);
        let adapter_path: PathBuf = if pinned.exists() {
            pinned
        } else {
            // Fall back to a bare executable name so a system-installed
            // adapter (e.g. pacman -S lldb) still works without
            // project_setup having been able to auto-install it.
            PathBuf::from(dap_spec.executable)
        };

        let session_name = args
            .session
            .unwrap_or_else(|| DEFAULT_DAP_SESSION.to_string());

        {
            let sessions = self.dap_sessions.lock().await;
            if sessions.contains_key(&session_name) {
                return error_json(format!(
                    "session `{session_name}` already exists; call dap_terminate first"
                ));
            }
        }

        let client = match DapClient::spawn(&adapter_path, &[], &root).await {
            Ok(c) => Arc::new(c),
            Err(e) => {
                return error_json(format!("failed to spawn {}: {e}", adapter_path.display()));
            }
        };

        if let Err(e) = client.initialize("aide-mcp").await {
            let _ = client.disconnect().await;
            return error_json(format!("initialize: {e}"));
        }

        let mut launch_args = serde_json::json!({
            "program": args.program,
            "args": args.args,
            "stopOnEntry": args.stop_on_entry.unwrap_or(true),
            "cwd": root.display().to_string(),
        });
        if let Some(env) = args.env {
            launch_args["env"] = env;
        }

        let launch_rx = match client.launch_start(launch_args).await {
            Ok(rx) => rx,
            Err(e) => {
                let _ = client.disconnect().await;
                return error_json(format!("launch dispatch: {e}"));
            }
        };

        if let Err(e) = client.wait_for_initialized(Duration::from_secs(30)).await {
            let _ = client.disconnect().await;
            return error_json(format!("adapter did not signal initialized: {e}"));
        }

        if let Err(e) = client.configuration_done().await {
            let _ = client.disconnect().await;
            return error_json(format!("configurationDone: {e}"));
        }

        if let Err(e) = client.await_response(launch_rx).await {
            let _ = client.disconnect().await;
            return error_json(format!("launch: {e}"));
        }

        let stopped = match client.wait_for_stopped(self.dap_stop_timeout().await).await {
            Ok(info) => info,
            Err(e) => {
                let _ = client.disconnect().await;
                return error_json(format!("no stop after launch: {e}"));
            }
        };

        self.dap_sessions
            .lock()
            .await
            .insert(session_name.clone(), client);
        to_json(&serde_json::json!({
            "session": session_name,
            "stopped": stopped,
        }))
    }

    #[tool(
        description = "Set line breakpoints in `source` for the active DAP session. Replaces any previous breakpoints in that file. Returns the adapter's verified-breakpoint list."
    )]
    async fn dap_set_breakpoints(
        &self,
        Parameters(args): Parameters<DapSetBreakpointsArgs>,
    ) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        match client.set_breakpoints(&args.source, &args.lines).await {
            Ok(v) => to_json(&v),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Resume a paused thread and wait for the next stop. Defaults to the thread reported by the most recent `stopped` event. Returns the new StoppedInfo once the debuggee pauses again (or errors on timeout / program exit)."
    )]
    async fn dap_continue(&self, Parameters(args): Parameters<DapContinueArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        let thread_id = match args.thread_id {
            Some(t) => t,
            None => match client.current_stopped().await.and_then(|s| s.thread_id) {
                Some(t) => t,
                None => return error_json("no stopped thread; pass thread_id explicitly"),
            },
        };
        if let Err(e) = client.continue_thread(thread_id).await {
            return error_json(e.to_string());
        }
        match client.wait_for_stopped(self.dap_stop_timeout().await).await {
            Ok(info) => to_json(&info),
            Err(e) => error_json(format!("no stop after continue: {e}")),
        }
    }

    #[tool(
        description = "Step over: run until the next source line in the same frame, then stop. Returns the new StoppedInfo."
    )]
    async fn dap_step_over(&self, Parameters(args): Parameters<DapStepArgs>) -> String {
        self.run_step(args.session.as_deref(), args.thread_id, StepKind::Over)
            .await
    }

    #[tool(
        description = "Step in: if the current line contains a call, enter it; otherwise step to the next line."
    )]
    async fn dap_step_in(&self, Parameters(args): Parameters<DapStepArgs>) -> String {
        self.run_step(args.session.as_deref(), args.thread_id, StepKind::In)
            .await
    }

    #[tool(description = "Step out: run until the current frame returns, then stop.")]
    async fn dap_step_out(&self, Parameters(args): Parameters<DapStepArgs>) -> String {
        self.run_step(args.session.as_deref(), args.thread_id, StepKind::Out)
            .await
    }

    #[tool(
        description = "Pause a running thread. Returns the StoppedInfo the adapter publishes once it suspends."
    )]
    async fn dap_pause(&self, Parameters(args): Parameters<DapPauseArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        if let Err(e) = client.pause(args.thread_id).await {
            return error_json(e.to_string());
        }
        match client.wait_for_stopped(self.dap_stop_timeout().await).await {
            Ok(info) => to_json(&info),
            Err(e) => error_json(format!("no stop after pause: {e}")),
        }
    }

    #[tool(
        description = "Return the current call stack for `thread_id` (defaults to the last-stopped thread). Up to 50 frames."
    )]
    async fn dap_stack_trace(&self, Parameters(args): Parameters<DapStackTraceArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        let thread_id = match args.thread_id {
            Some(t) => t,
            None => match client.current_stopped().await.and_then(|s| s.thread_id) {
                Some(t) => t,
                None => return error_json("no stopped thread"),
            },
        };
        match client.stack_trace(thread_id).await {
            Ok(frames) => to_json(&frames),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "List scopes (e.g. Locals, Registers) for a stack frame. Use the `variables_reference` returned here with `dap_variables` to read actual values."
    )]
    async fn dap_scopes(&self, Parameters(args): Parameters<DapScopesArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        match client.scopes(args.frame_id).await {
            Ok(s) => to_json(&s),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Read variables for a `variables_reference` (from a scope or a structured variable). Useful for expanding composite values."
    )]
    async fn dap_variables(&self, Parameters(args): Parameters<DapVariablesArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        match client.variables(args.variables_reference).await {
            Ok(v) => to_json(&v),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Evaluate an expression in the debuggee. When `frame_id` is given, the expression resolves in that frame's scope."
    )]
    async fn dap_evaluate(&self, Parameters(args): Parameters<DapEvaluateArgs>) -> String {
        let client = match self.dap_client(args.session.as_deref()).await {
            Ok(c) => c,
            Err(e) => return error_json(e),
        };
        match client.evaluate(&args.expression, args.frame_id).await {
            Ok(v) => to_json(&v),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Disconnect from the DAP adapter and tear down the named session (default `\"default\"`)."
    )]
    async fn dap_terminate(&self, Parameters(args): Parameters<DapTerminateArgs>) -> String {
        let name = args
            .session
            .unwrap_or_else(|| DEFAULT_DAP_SESSION.to_string());
        let mut sessions = self.dap_sessions.lock().await;
        match sessions.remove(&name) {
            None => to_json(&serde_json::json!({
                "terminated": false,
                "session": name,
                "reason": "no such session",
            })),
            Some(client) => {
                let _ = client.disconnect().await;
                to_json(&serde_json::json!({ "terminated": true, "session": name }))
            }
        }
    }

    // ---------- v0.19 GitHub integration ----------

    #[tool(
        description = "Report the current GitHub auth state. Walks $GITHUB_TOKEN → `gh auth token` → ~/.aide/auth/github.token, then hits /user with the resolved token to produce `{source, login, scopes}`. Scopes come from the `x-oauth-scopes` response header — empty for fine-grained tokens. When no source resolves, returns `{source: \"none\", remediation: <three-step actionable message>}` instead of erroring, so agents can branch on it."
    )]
    async fn gh_auth_status(&self, Parameters(_args): Parameters<GhAuthStatusArgs>) -> String {
        let token_file = self.paths.github_token();
        match aide_github::resolve_token(&token_file).await {
            Err(e) => error_json(e.to_string()),
            Ok(None) => to_json(&serde_json::json!({
                "source": "none",
                "login": serde_json::Value::Null,
                "scopes": Vec::<String>::new(),
                "remediation": aide_github::NO_AUTH_REMEDIATION,
            })),
            Ok(Some(resolved)) => {
                let client = match aide_github::GithubClient::new(resolved.token) {
                    Ok(c) => c,
                    Err(e) => return error_json(e.to_string()),
                };
                match client.current_user_with_scopes().await {
                    Ok((user, scopes)) => to_json(&serde_json::json!({
                        "source": resolved.source.as_str(),
                        "login": user.login,
                        "scopes": scopes,
                    })),
                    Err(e) => to_json(&serde_json::json!({
                        "source": resolved.source.as_str(),
                        "login": serde_json::Value::Null,
                        "scopes": Vec::<String>::new(),
                        "error": e.to_string(),
                    })),
                }
            }
        }
    }

    #[tool(
        description = "Create an issue on the GitHub repo whose `origin` remote resolves to `:owner/:repo`. Requires a token via the waterfall — call `gh_auth_status` first if unsure. For dogfood-gotcha reports prefer `gh_ux_gotcha`, which enforces the `ux-gotcha` label and the CLAUDE.md body template."
    )]
    async fn gh_issue_create(&self, Parameters(args): Parameters<GhIssueCreateArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        let payload = aide_github::IssueCreate {
            title: args.title,
            body: args.body,
            labels: args.labels.unwrap_or_default(),
        };
        match client.create_issue(&slug.owner, &slug.repo, &payload).await {
            Ok(issue) => to_json(&issue),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "List issues on the repo attached to `origin`. Filters: `state` (open / closed / all — default open on GitHub's side), `labels` (AND-joined), `limit` mapped to `per_page` (max 100). Returns `[{number, title, state, html_url, labels}]` — same shape `gh_issue_create` returns for the single-issue case."
    )]
    async fn gh_issue_list(&self, Parameters(args): Parameters<GhIssueListArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let state = match args.state.as_deref() {
            None => None,
            Some(s) => match aide_github::IssueState::parse(s) {
                Some(v) => Some(v),
                None => {
                    return error_json(format!(
                        "unknown state {s:?}; expected one of: open, closed, all"
                    ))
                }
            },
        };
        let filter = aide_github::IssueListFilter {
            state,
            labels: args.labels.unwrap_or_default(),
            limit: args.limit,
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        match client.list_issues(&slug.owner, &slug.repo, &filter).await {
            Ok(issues) => to_json(&issues),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "File a UX-gotcha issue with the CLAUDE.md policy baked in. Policy (all automatic): (1) label `ux-gotcha` is always added; (2) `title` is prefixed with the implicated `tool` in backticks unless already so; (3) a provenance footer — `Filed via gh_ux_gotcha from <tool>[/<param>] per CLAUDE.md § \"Reporting UX gotchas\"` — is appended to `body`. Agent supplies the narrative (Repro / Why it bites / Suggested fix), the policy handles the shell."
    )]
    async fn gh_ux_gotcha(&self, Parameters(args): Parameters<GhUxGotchaArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        let payload = aide_github::ux_gotcha::build(
            &args.title,
            &args.body,
            &args.tool,
            args.param.as_deref(),
        );
        match client.create_issue(&slug.owner, &slug.repo, &payload).await {
            Ok(issue) => to_json(&issue),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "View a single GitHub issue on the repo detected from `origin`. Returns `{issue, comments}` — issue includes the full `body` (unlike `gh_issue_list` which omits it from some endpoints) and `state_reason`; comments are every reply in chronological order (capped at GitHub's 100-per-page, no pagination beyond that yet)."
    )]
    async fn gh_issue_view(&self, Parameters(args): Parameters<GhIssueViewArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        let issue = match client.get_issue(&slug.owner, &slug.repo, args.number).await {
            Ok(i) => i,
            Err(e) => return error_json(e.to_string()),
        };
        let comments = match client
            .list_comments(&slug.owner, &slug.repo, args.number)
            .await
        {
            Ok(c) => c,
            Err(e) => return error_json(e.to_string()),
        };
        to_json(&serde_json::json!({
            "issue": issue,
            "comments": comments,
        }))
    }

    #[tool(
        description = "Post a comment on a GitHub issue. Returns the created `Comment` (id, body, user, timestamps, html_url) so the caller can link to it. Use for follow-up notes — \"also hit this in commit abc123\", \"duplicate of #N\" — instead of opening a second issue."
    )]
    async fn gh_issue_comment(&self, Parameters(args): Parameters<GhIssueCommentArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        match client
            .create_comment(&slug.owner, &slug.repo, args.number, &args.body)
            .await
        {
            Ok(comment) => to_json(&comment),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "Close a GitHub issue. Optional `reason` is `completed` (default intent — the underlying bug is fixed) or `not_planned` (wontfix). Returns the updated Issue including its `state_reason`. Prefer `Closes #N` in a commit message's footer when the close is driven by a merge — GitHub auto-closes and this tool becomes redundant. Use this tool for human-driven closes without a commit or when reason matters."
    )]
    async fn gh_issue_close(&self, Parameters(args): Parameters<GhIssueCloseArgs>) -> String {
        let root = resolve_root(args.path);
        let slug = match aide_github::detect_github_slug(&root) {
            Ok(s) => s,
            Err(e) => return error_json(e.to_string()),
        };
        let reason = match args.reason.as_deref() {
            None => None,
            Some(s) => match aide_github::CloseReason::parse(s) {
                Some(r) => Some(r),
                None => {
                    return error_json(format!(
                        "unknown reason {s:?}; expected one of: completed, not_planned"
                    ))
                }
            },
        };
        let client = match self.gh_client().await {
            Ok(c) => c,
            Err(msg) => return error_json(msg),
        };
        match client
            .close_issue(&slug.owner, &slug.repo, args.number, reason)
            .await
        {
            Ok(issue) => to_json(&issue),
            Err(e) => error_json(e.to_string()),
        }
    }

    async fn gh_client(&self) -> Result<aide_github::GithubClient, String> {
        let token_file = self.paths.github_token();
        let resolved = aide_github::resolve_token(&token_file)
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| aide_github::NO_AUTH_REMEDIATION.to_string())?;
        aide_github::GithubClient::new(resolved.token).map_err(|e| e.to_string())
    }
}

#[derive(Debug, Default, serde::Deserialize, schemars::JsonSchema)]
pub struct GhAuthStatusArgs {}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhIssueCreateArgs {
    /// Project root whose `origin` remote is parsed for `owner/repo`. If
    /// omitted, the server cwd is used.
    #[serde(default)]
    pub path: Option<String>,
    pub title: String,
    pub body: String,
    /// Optional labels to attach. The repo must already have them; GitHub
    /// does not auto-create labels on issue create.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhIssueListArgs {
    #[serde(default)]
    pub path: Option<String>,
    /// One of `open` (default on GitHub) / `closed` / `all`.
    #[serde(default)]
    pub state: Option<String>,
    /// AND-filter: only issues carrying every label in this list.
    #[serde(default)]
    pub labels: Option<Vec<String>>,
    /// `per_page`. GitHub caps at 100; above that the API returns 422.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhIssueViewArgs {
    #[serde(default)]
    pub path: Option<String>,
    /// Issue number (as shown in the URL / `gh_issue_list` output).
    pub number: u64,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhIssueCommentArgs {
    #[serde(default)]
    pub path: Option<String>,
    pub number: u64,
    pub body: String,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhIssueCloseArgs {
    #[serde(default)]
    pub path: Option<String>,
    pub number: u64,
    /// One of `completed` or `not_planned`. Omit to close without a
    /// reason (GitHub leaves `state_reason` null).
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct GhUxGotchaArgs {
    #[serde(default)]
    pub path: Option<String>,
    pub title: String,
    pub body: String,
    /// The aide MCP tool whose behaviour surfaced the gotcha —
    /// `project_ls` / `project_grep` / etc. Used for title prefix and
    /// provenance footer.
    pub tool: String,
    /// Optional parameter name (e.g. `scope` on `project_ls`) to narrow
    /// the provenance when the gotcha is specific to one argument.
    #[serde(default)]
    pub param: Option<String>,
}

#[tool_handler]
impl ServerHandler for AideServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(rmcp::model::Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions("aide-mcp: IDE-grade tools (LSP/SCIP/GIT/exec/DAP) for AI agents")
    }
}

fn parse_scope(raw: Option<&str>) -> Result<aide_search::Scope, String> {
    // Default: `all` (gitignore-aware walk of the working tree). Previously
    // defaulted to `tracked` (libgit2 index), which silently hid newly-
    // created files — see ux-gotcha #1. `all` matches the `ls`/`find`
    // intuition agents bring in from shell.
    match raw.unwrap_or("all") {
        "tracked" => Ok(aide_search::Scope::Tracked),
        "all" => Ok(aide_search::Scope::All),
        "dirty" => Ok(aide_search::Scope::Dirty),
        "staged" => Ok(aide_search::Scope::Staged),
        other => Err(format!(
            "unknown scope {other:?}; expected one of: tracked, all, dirty, staged"
        )),
    }
}

fn short_sha(sha: &str) -> String {
    let len = sha.len().min(12);
    sha[..len].to_string()
}

fn scope_label(scope: &aide_search::Scope) -> &'static str {
    match scope {
        aide_search::Scope::Tracked => "tracked",
        aide_search::Scope::All => "all",
        aide_search::Scope::Dirty => "dirty",
        aide_search::Scope::Staged => "staged",
    }
}

fn resolve_root(path: Option<String>) -> PathBuf {
    match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
    }
}

fn to_json<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|e| error_json(e.to_string()))
}

fn error_json(message: impl Into<String>) -> String {
    let message = message.into();
    serde_json::json!({ "error": message }).to_string()
}

/// Background reloader: poll `config_path` every
/// [`CONFIG_RELOAD_INTERVAL`] and swap the live [`Config`] when the
/// file's mtime changes. Updates the [`Indexer`] retention atomic
/// side-by-side so a bumped `scip.retention_ready` takes effect at
/// the next `mark_ready`.
fn spawn_config_reloader(
    path: PathBuf,
    config: Arc<RwLock<Config>>,
    retention: Arc<std::sync::atomic::AtomicUsize>,
) {
    tokio::spawn(async move {
        let mut last_mtime = file_mtime(&path);
        loop {
            tokio::time::sleep(CONFIG_RELOAD_INTERVAL).await;
            let current_mtime = file_mtime(&path);
            if current_mtime == last_mtime {
                continue;
            }
            last_mtime = current_mtime;
            match Config::load(&path) {
                Ok(new_cfg) => {
                    retention.store(new_cfg.scip.retention_ready.max(1), Ordering::Relaxed);
                    *config.write().await = new_cfg;
                    tracing::info!(path = %path.display(), "config reloaded");
                }
                Err(e) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %e,
                        "config reload failed; keeping previous values"
                    );
                }
            }
        }
    });
}

fn file_mtime(path: &PathBuf) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}
