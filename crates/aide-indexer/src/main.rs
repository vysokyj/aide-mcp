use std::path::PathBuf;

use aide_core::AidePaths;
use aide_proto::default_indexer_socket;
use anyhow::Result;
use tracing_subscriber::EnvFilter;

mod server;
mod state;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("aide_indexer=info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let paths = AidePaths::from_home()?;
    std::fs::create_dir_all(paths.sock())?;
    std::fs::create_dir_all(paths.queue())?;

    let socket = default_indexer_socket(&paths);
    let state_path: PathBuf = paths.queue().join("indexer_state.json");

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        socket = %socket.display(),
        state = %state_path.display(),
        "starting aide-indexer"
    );

    server::run(&socket, &state_path).await
}
