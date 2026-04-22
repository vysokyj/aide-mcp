//! Thin async client for talking to the `aide-indexer` daemon.
//!
//! One request, one response per connection. If the daemon is not running
//! the error surfaces as a connect failure — callers decide whether to
//! ignore it (post-commit hook) or report it (MCP tool).

use std::path::Path;

use aide_proto::framing::{read_message, write_message, FrameError};
use aide_proto::{Request, Response};
use thiserror::Error;
use tokio::io::BufReader;
use tokio::net::UnixStream;

#[derive(Debug, Error)]
pub enum IndexerClientError {
    #[error("cannot reach aide-indexer at {socket}: {source}")]
    Connect {
        socket: String,
        source: std::io::Error,
    },
    #[error("framing error: {0}")]
    Frame(#[from] FrameError),
    #[error("daemon closed connection without a response")]
    NoResponse,
}

pub async fn send(socket: &Path, request: &Request) -> Result<Response, IndexerClientError> {
    let stream =
        UnixStream::connect(socket)
            .await
            .map_err(|source| IndexerClientError::Connect {
                socket: socket.display().to_string(),
                source,
            })?;
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);

    write_message(&mut write_half, request).await?;
    read_message(&mut reader)
        .await?
        .ok_or(IndexerClientError::NoResponse)
}
