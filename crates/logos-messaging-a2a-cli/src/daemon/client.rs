//! Client-side IPC: connect to the daemon's Unix socket, send a
//! [`Request`], read a [`Response`].
//!
//! Each connection handles exactly one request/response. Callers are
//! expected to test for daemon availability with [`DaemonClient::probe`]
//! and fall back to the spin-up-own-node path if it fails — that lets
//! the CLI keep working when no daemon is running.

use anyhow::{anyhow, Context, Result};
use std::path::PathBuf;
use tokio::net::UnixStream;

use super::frame::{read_frame, write_frame};
use super::protocol::{Request, Response};

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
}
