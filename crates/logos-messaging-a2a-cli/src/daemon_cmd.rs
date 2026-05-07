//! `lmao daemon` subcommand handlers — talk to the IPC socket directly,
//! never fall back to spinning up a new node (this command is *about*
//! the daemon, so the absence of one is a definite no).

use anyhow::{anyhow, Result};
use std::path::PathBuf;

use crate::cli::DaemonAction;
use crate::daemon::{default_socket_path, DaemonClient, Request, Response};

pub async fn handle(
    action: DaemonAction,
    daemon_socket: Option<&PathBuf>,
    json: bool,
) -> Result<()> {
    let socket = daemon_socket.cloned().unwrap_or_else(default_socket_path);
    let client = DaemonClient::new(&socket);

    match action {
        DaemonAction::Status => {
            if !client.probe().await {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "running": false,
                            "socket": socket,
                        }))?
                    );
                } else {
                    println!("Daemon: not running (no socket at {})", socket.display());
                }
                return Ok(());
            }
            let resp = client.send(Request::Info).await?;
            let Response::Info {
                name,
                pubkey,
                capabilities,
                uptime_secs,
                socket_path,
                storage_enabled,
                encryption_pubkey: _,
                load: _,
            } = resp
            else {
                return Err(anyhow!("unexpected daemon response: {resp:?}"));
            };
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "running": true,
                        "name": name,
                        "pubkey": pubkey,
                        "capabilities": capabilities,
                        "uptime_secs": uptime_secs,
                        "socket": socket_path,
                        "storage_enabled": storage_enabled,
                    }))?
                );
            } else {
                println!("Daemon:        running");
                println!("Socket:        {}", socket_path.display());
                println!("Agent name:    {name}");
                println!("Public key:    {pubkey}");
                println!("Capabilities:  {}", capabilities.join(", "));
                println!("Uptime:        {uptime_secs}s");
                println!(
                    "Storage:       {}",
                    if storage_enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
            }
            Ok(())
        }
        DaemonAction::Stop => {
            if !client.probe().await {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "stopped": false,
                            "reason": "no daemon listening",
                            "socket": socket,
                        }))?
                    );
                } else {
                    eprintln!(
                        "No daemon listening at {} — nothing to stop.",
                        socket.display()
                    );
                }
                // Not an error per se; just a no-op.
                return Ok(());
            }
            let resp = client.send(Request::Shutdown).await?;
            match resp {
                Response::ShutdownAck => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "stopped": true,
                                "socket": socket,
                            }))?
                        );
                    } else {
                        println!("Daemon at {} acknowledged shutdown.", socket.display());
                    }
                    Ok(())
                }
                Response::Error { message } => Err(anyhow!("daemon error: {message}")),
                other => Err(anyhow!("unexpected daemon response: {other:?}")),
            }
        }
    }
}
