//! Wire framing: newline-delimited JSON over an async stream.

use serde::{de::DeserializeOwned, Serialize};
use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("peer closed the connection before sending a response")]
    PeerClosed,
}

/// Serialise `message` as JSON and write it plus a trailing newline.
pub async fn write_message<W, T>(writer: &mut W, message: &T) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut bytes = serde_json::to_vec(message)?;
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one newline-terminated JSON message. Returns `Ok(None)` when the
/// peer closes cleanly with no bytes pending.
pub async fn read_message<R, T>(reader: &mut BufReader<R>) -> Result<Option<T>, FrameError>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut line = String::new();
    let read = reader.read_line(&mut line).await?;
    if read == 0 {
        return Ok(None);
    }
    let parsed = serde_json::from_str(line.trim_end_matches('\n'))?;
    Ok(Some(parsed))
}
