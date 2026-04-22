use std::path::PathBuf;

use aide_core::AidePaths;
use aide_install::{install_tool, InstallOutcome};
use aide_lang::Registry;
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

#[derive(Clone)]
pub struct AideServer {
    registry: Registry,
    paths: AidePaths,
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
            tool_router: Self::tool_router(),
        })
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

        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
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

        serde_json::to_string(&result).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
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
