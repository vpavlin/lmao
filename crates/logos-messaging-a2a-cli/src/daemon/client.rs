//! Client-side IPC: connect to the daemon's Unix socket, send a
//! [`Request`], read a [`Response`].
//!
//! Each connection handles exactly one request/response. Callers are
//! expected to test for daemon availability with [`DaemonClient::probe`]
//! and fall back to the spin-up-own-node path if it fails — that lets
//! the CLI keep working when no daemon is running.

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use super::protocol::{Request, Response, MAX_FRAME_BYTES};

/// Lightweight client that connects per-request. There's no connection
/// pool because the CLI commands are short-lived and each issues at most
/// one request.
pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    /// Returns true if a daemon is reachable at the configured socket
    /// path. Cheap — just a `connect()` then immediate close. Used by
    /// CLI commands to decide between IPC and spinning up a fresh node.
    pub async fn probe(&self) -> bool {
        UnixStream::connect(&self.socket_path).await.is_ok()
    }

    pub async fn send(&self, request: Request) -> Result<Response> {
        let mut stream = UnixStream::connect(&self.socket_path)
            .await
            .with_context(|| format!("connecting to daemon at {}", self.socket_path.display()))?;
        write_frame(&mut stream, &request).await?;
        let response: Response = read_frame(&mut stream).await?;
        if let Response::Error { message } = &response {
            return Err(anyhow!("daemon error: {message}"));
        }
        Ok(response)
    }

    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }
}

async fn read_frame<T: serde::de::DeserializeOwned>(stream: &mut UnixStream) -> Result<T> {
    let mut len_buf = [0u8; 4];
    stream
        .read_exact(&mut len_buf)
        .await
        .context("reading frame length")?;
    let len = u32::from_le_bytes(len_buf) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(anyhow!("frame too large: {len} bytes"));
    }
    let mut body = vec![0u8; len];
    stream
        .read_exact(&mut body)
        .await
        .context("reading frame body")?;
    serde_json::from_slice(&body).context("parsing frame body")
}

async fn write_frame<T: serde::Serialize>(stream: &mut UnixStream, value: &T) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    let len = body.len() as u32;
    stream.write_all(&len.to_le_bytes()).await?;
    stream.write_all(&body).await?;
    stream.flush().await?;
    Ok(())
}
