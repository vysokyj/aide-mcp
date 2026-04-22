//! LSP uses `Content-Length`-framed JSON-RPC over stdio:
//!
//! ```text
//! Content-Length: 42\r\n
//! \r\n
//! {"jsonrpc":"2.0", ... 42 bytes ...}
//! ```
//!
//! This module handles the framing — nothing else.

use std::io;

use thiserror::Error;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Error)]
pub enum FramingError {
    #[error("I/O: {0}")]
    Io(#[from] io::Error),
    #[error("unexpected EOF")]
    Eof,
    #[error("malformed header: {0}")]
    BadHeader(String),
    #[error("missing Content-Length header")]
    NoContentLength,
    #[error("invalid Content-Length: {0}")]
    BadContentLength(String),
}

/// Write a single framed JSON message to `writer`.
pub async fn write_message<W>(writer: &mut W, bytes: &[u8]) -> Result<(), FramingError>
where
    W: AsyncWriteExt + Unpin,
{
    let header = format!("Content-Length: {}\r\n\r\n", bytes.len());
    writer.write_all(header.as_bytes()).await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one framed JSON message from `reader`, returning the raw body bytes.
pub async fn read_message<R>(reader: &mut BufReader<R>) -> Result<Vec<u8>, FramingError>
where
    R: AsyncReadExt + Unpin,
{
    let mut content_length: Option<usize> = None;
    let mut header_buf = String::new();

    loop {
        header_buf.clear();
        let n = reader.read_line(&mut header_buf).await?;
        if n == 0 {
            return Err(FramingError::Eof);
        }
        let line = header_buf.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| FramingError::BadHeader(line.to_string()))?;
        if name.eq_ignore_ascii_case("content-length") {
            let parsed = value
                .trim()
                .parse::<usize>()
                .map_err(|_| FramingError::BadContentLength(value.trim().to_string()))?;
            content_length = Some(parsed);
        }
    }

    let len = content_length.ok_or(FramingError::NoContentLength)?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body).await?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    #[tokio::test]
    async fn roundtrip() {
        let payload = br#"{"jsonrpc":"2.0","id":1,"method":"hi"}"#;
        let mut buf = Vec::new();
        write_message(&mut buf, payload).await.unwrap();

        let mut reader = BufReader::new(&buf[..]);
        let body = read_message(&mut reader).await.unwrap();
        assert_eq!(body, payload);
    }

    #[tokio::test]
    async fn lowercase_header_accepted() {
        let mut buf = Vec::new();
        buf.extend_from_slice(b"content-length: 2\r\n\r\n{}");
        let mut reader = BufReader::new(&buf[..]);
        let body = read_message(&mut reader).await.unwrap();
        assert_eq!(body, b"{}");
    }
}
