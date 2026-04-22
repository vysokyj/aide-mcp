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

#[derive(Clone)]
pub struct AideServer {
    registry: Registry,
    paths: AidePaths,
    pool: Arc<LspPool>,
    #[allow(
        dead_code,
        reason = "field is read via #[tool_handler] macro expansion"
    )]
    tool_router: ToolRouter<Self>,
}

impl AideServer {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            registry: Registry::builtin(),
            paths: AidePaths::from_home()?,
            pool: Arc::new(LspPool::new()),
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
        description = "Install the LSP server, SCIP indexer, and debug adapter binaries for every language detected in the given project root. Idempotent — already-installed versions are skipped."
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
        description = "git status: branch, upstream divergence, and per-file working-tree + index state."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn git_status(&self, Parameters(args): Parameters<GitPathArgs>) -> String {
        let root = resolve_root(args.path);
        match aide_git::status::status(&root) {
            Ok(s) => to_json(&s),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "git log: recent commits reachable from HEAD. Returns sha, author, summary, time; newest first."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn git_log(&self, Parameters(args): Parameters<GitLogArgs>) -> String {
        let root = resolve_root(args.path);
        match aide_git::log::log(&root, args.limit) {
            Ok(entries) => to_json(&entries),
            Err(e) => error_json(e.to_string()),
        }
    }

    #[tool(
        description = "git diff: unified diff patch plus stats (files changed, insertions, deletions). Selects HEAD vs worktree by default."
    )]
    #[allow(clippy::unused_self, reason = "rmcp #[tool] methods must be &self")]
    fn git_diff(&self, Parameters(args): Parameters<GitDiffArgs>) -> String {
        let root = resolve_root(args.path);
        let mode = match args.mode.as_deref() {
            Some("index-to-worktree") => DiffMode::IndexToWorktree,
            Some("head-to-index") => DiffMode::HeadToIndex,
            None | Some("head-to-worktree") => DiffMode::HeadToWorktree,
            Some(other) => return error_json(format!("unknown diff mode: {other}")),
        };
        match aide_git::diff::diff(&root, mode, args.pathspec.as_deref()) {
            Ok(d) => to_json(&d),
            Err(e) => error_json(e.to_string()),
        }
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
