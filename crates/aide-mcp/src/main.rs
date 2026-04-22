use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod hook;
mod indexer;
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

    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("post-commit") => hook::run_post_commit().await,
        Some(other) => {
            eprintln!("aide-mcp: unknown subcommand '{other}'");
            std::process::exit(2);
        }
        None => {
            tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting aide-mcp");
            server::run().await
        }
    }
}
