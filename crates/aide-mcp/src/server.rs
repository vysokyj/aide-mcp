use std::path::PathBuf;

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
    let handler = AideServer::new();
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

#[derive(Clone)]
pub struct AideServer {
    registry: Registry,
    #[allow(
        dead_code,
        reason = "field is read via #[tool_handler] macro expansion"
    )]
    tool_router: ToolRouter<Self>,
}

impl AideServer {
    pub fn new() -> Self {
        Self {
            registry: Registry::builtin(),
            tool_router: Self::tool_router(),
        }
    }
}

impl Default for AideServer {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_router]
impl AideServer {
    #[tool(description = "Detect which supported languages appear in the given project root")]
    fn project_detect(&self, Parameters(args): Parameters<ProjectDetectArgs>) -> String {
        let root = match args.path {
            Some(p) => PathBuf::from(p),
            None => std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        };

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
