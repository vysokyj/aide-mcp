use anyhow::Result;
use clap::Parser;
use tracing_subscriber::EnvFilter;

mod dogfood;
mod exec;
mod indexer;
mod jobs;
mod memory;
mod processes;
mod server;

#[derive(Debug, Parser)]
#[command(
    name = "mcp-aide",
    version,
    about = "AIDE — MCP server (LSP/SCIP/git/exec/DAP)"
)]
struct Cli {
    /// Serve over HTTP instead of stdio. Accepts `:PORT` (binds 127.0.0.1)
    /// or a full `host:port`. Default transport is stdio.
    #[arg(long, value_name = "ADDR")]
    http: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(
            // NB: the binary target is `mcp-aide`, so this crate's
            // tracing target is `mcp_aide` — the old `aide_mcp`
            // directive matched nothing and silently ate every log
            // line from the server itself.
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("mcp_aide=info,aide_lsp=info,rmcp=warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting aide-mcp");

    match cli.http {
        Some(addr) => server::run_http(&addr).await,
        None => server::run().await,
    }
}
