use std::path::PathBuf;

use aide_core::AidePaths;
use aide_proto::default_indexer_socket;
use anyhow::{Context, Result};
use tracing_subscriber::EnvFilter;

use crate::state::Store;
use crate::worker::Job;

mod server;
mod state;
mod worker;

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
    std::fs::create_dir_all(paths.scip())?;

    let socket = default_indexer_socket(&paths);
    let state_path: PathBuf = paths.queue().join("indexer_state.json");

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        socket = %socket.display(),
        state = %state_path.display(),
        "starting aide-indexer"
    );

    let store = Store::load(&state_path).context("loading indexer state")?;
    let jobs = worker::spawn(paths.clone(), store.clone());

    // Re-enqueue anything that was still in flight when we were last stopped.
    for (repo_root, sha) in store.recoverable_jobs().await {
        tracing::info!(repo = %repo_root, sha = %sha, "recovering interrupted job");
        let _ = jobs.send(Job { repo_root, sha });
    }

    server::run(&socket, store, jobs).await
}
