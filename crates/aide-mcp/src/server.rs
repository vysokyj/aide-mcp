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
    /// One of "tracked" (default, reads libgit2 index), "all" (gitignore-aware
    /// walk of the working tree), "dirty" (files with non-clean status), or
    /// "staged" (files whose index entry differs from HEAD).
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

#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ProjectGrepArgs {
    /// Regex pattern. Smart-case by default — lowercase pattern matches
    /// case-insensitively, mixed-case matches case-sensitively.
    pub pattern: String,
    /// Repository root. If omitted, falls back to the server cwd.
    #[serde(default)]
    pub path: Option<String>,
    /// Same scopes as `project_ls`. Defaults to "tracked".
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
        description = "List files under the project root. Replaces `ls`/`find` with a gitignore-aware, git-scoped enumerator. Scope: \"tracked\" (default, fastest — reads libgit2 index), \"all\" (working tree minus .gitignore), \"dirty\" (anything non-clean), \"staged\" (index vs HEAD). Optional glob filter applies to repo-relative paths."
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
        description = "Regex search across project files. Replaces `grep`/`rg` with a gitignore-aware, git-scoped searcher powered by the ripgrep engine (grep-regex + grep-searcher). Smart-case by default, binary files skipped, per-file and total result caps. Returns match lines with optional before/after context tagged by kind."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn project_grep(&self, Parameters(args): Parameters<ProjectGrepArgs>) -> String {
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
        description = "Run the project via the language plugin's runner (e.g. `cargo run`) and return the full stdout + stderr. Captures up to 1 MB per stream. Default timeout 300s; override with timeout_secs."
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
            Ok(result) => to_json(&result),
            Err(e) => error_json(format!("failed to spawn {}: {e}", runner.executable)),
        }
    }

    #[tool(
        description = "Run the project's tests via the language plugin's test_runner (e.g. `cargo test [filter]`). Captures stdout/stderr and exit code. Default timeout 300s."
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
            Ok(result) => to_json(&result),
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
    match raw.unwrap_or("tracked") {
        "tracked" => Ok(aide_search::Scope::Tracked),
        "all" => Ok(aide_search::Scope::All),
        "dirty" => Ok(aide_search::Scope::Dirty),
        "staged" => Ok(aide_search::Scope::Staged),
        other => Err(format!(
            "unknown scope {other:?}; expected one of: tracked, all, dirty, staged"
        )),
    }
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
