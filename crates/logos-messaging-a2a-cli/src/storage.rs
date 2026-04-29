//! Storage subcommands. Today: `lmao storage fetch <cid>`.
//!
//! Fetching a CID requires a daemon to be running with
//! `--storage libstorage`, because libstorage's peer-discovered
//! blockstore lives in that long-lived process. A fresh ephemeral
//! storage node started just for `fetch` would not know about the
//! producer's blocks.

use anyhow::{anyhow, Context, Result};
use base64::Engine;
use std::path::{Path, PathBuf};

use crate::cli::StorageAction;
use crate::daemon::{default_socket_path, DaemonClient, Request, Response};

pub async fn handle(
    action: StorageAction,
    daemon_socket: Option<&PathBuf>,
    json: bool,
) -> Result<()> {
    match action {
        StorageAction::Fetch { cid, output } => {
            fetch(&cid, output.as_deref(), daemon_socket, json).await
        }
    }
}

async fn fetch(
    cid: &str,
    output: Option<&Path>,
    daemon_socket: Option<&PathBuf>,
    json: bool,
) -> Result<()> {
    let socket = daemon_socket.cloned().unwrap_or_else(default_socket_path);
    let client = DaemonClient::new(&socket);
    if !client.probe().await {
        return Err(anyhow!(
            "no daemon listening at {}\n\nstorage fetch requires a running `lmao agent run` \
             daemon with --storage libstorage; the embedded blockstore lives in that process \
             and isn't reachable by a one-shot CLI invocation.",
            socket.display()
        ));
    }

    let resp = client
        .send(Request::StorageFetch {
            cid: cid.to_string(),
        })
        .await?;
    let Response::StorageFetch {
        cid: _,
        payload_b64,
    } = resp
    else {
        return Err(anyhow!("unexpected daemon response: {resp:?}"));
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&payload_b64)
        .context("decoding daemon's base64 payload")?;

    if let Some(path) = output {
        std::fs::write(path, &bytes).with_context(|| format!("writing to {}", path.display()))?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "cid": cid,
                    "bytes": bytes.len(),
                    "output": path.to_string_lossy(),
                }))?
            );
        } else {
            eprintln!("Wrote {} bytes to {}", bytes.len(), path.display());
        }
    } else {
        // Stdout. JSON wraps the (possibly binary) payload in base64
        // so callers don't have to handle truncation or encoding.
        if json {
            println!(
                "{}",
                serde_json::to_string(&serde_json::json!({
                    "cid": cid,
                    "bytes": bytes.len(),
                    "payload_b64": payload_b64,
                }))?
            );
        } else {
            use std::io::Write;
            std::io::stdout()
                .write_all(&bytes)
                .context("writing payload to stdout")?;
        }
    }
    Ok(())
}
