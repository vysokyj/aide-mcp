use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod server;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("aide_mcp=info,aide_lsp=info,rmcp=warn")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting aide-mcp");

    server::run().await
}
