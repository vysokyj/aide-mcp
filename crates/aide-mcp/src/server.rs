use std::path::PathBuf;
use std::sync::Arc;

use aide_core::AidePaths;
use aide_git::diff::DiffMode;
use aide_install::{install_tool, InstallOutcome};
use aide_lang::{LanguagePlugin, Registry};
use aide_lsp::{ops as lsp_ops, LspPool};
use anyhow::Result;
use rmcp::{
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    model::{ServerCapabilities, ServerInfo},
    schemars, tool, tool_handler, tool_router,
    transport::stdio,
    ServerHandler, ServiceExt,
};

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

#[derive(Clone)]
pub struct AideServer {
    registry: Registry,
    paths: AidePaths,
    pool: Arc<LspPool>,
    indexer: Indexer,
    #[allow(
        dead_code,
        reason = "field is read via #[tool_handler] macro expansion"
    )]
    tool_router: ToolRouter<Self>,
}

impl AideServer {
    pub fn new() -> anyhow::Result<Self> {
        let paths = AidePaths::from_home()?;
        let indexer = Indexer::start(&paths)?;
        Ok(Self {
            registry: Registry::builtin(),
            paths,
            pool: Arc::new(LspPool::new()),
            indexer,
            tool_router: Self::tool_router(),
        })
    }

    /// Select a language plugin that claims `root`, together with the path to
    /// its LSP binary in `~/.aide/bin/`.
    fn language_for(&self, root: &std::path::Path) -> Option<(Arc<dyn LanguagePlugin>, PathBuf)> {
        let plugin = self.registry.detect(root).into_iter().next()?;
        let binary = self.paths.bin().join(plugin.lsp().executable);
        Some((plugin, binary))
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
        description = "LSP hover: summary of the symbol at (file, line, column). Returns null if no symbol is at that position. Requires project_setup to have installed the language server."
    )]
    async fn lsp_hover(&self, Parameters(args): Parameters<LspPositionArgs>) -> String {
        let file = PathBuf::from(&args.file);
        let root = resolve_root(args.root);
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
        let Some((plugin, binary)) = self.language_for(&root) else {
            return error_json(format!("no language plugin claims root {}", root.display()));
        };

        let client = match self
            .pool
            .get_or_spawn(plugin.id().as_str(), &root, &binary)
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
