use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_transport::Transport;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::cli::AgentAction;
use crate::common::{build_node, parse_capabilities, IdentityConfig};

/// Output of an executor invocation: trimmed stdout (the response sent
/// back over LMAO) and full stderr (the audit log, retained for upload
/// to Logos Storage when configured).
struct ExecOutput {
    response: String,
    log: String,
}

/// Run the user's `--exec` command with the task text on stdin.
///
/// The command runs through `sh -c` so quoting and pipes work the way the
/// user wrote them. stdout becomes the agent's response; stderr is kept
/// as the audit-log payload. A non-zero exit is surfaced as an error so
/// the caller can decide whether to respond with a graceful "[error]"
/// message or skip the task entirely.
async fn run_exec(cmd: &str, task_text: &str) -> Result<ExecOutput> {
    let mut child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .with_context(|| format!("failed to spawn `sh -c {cmd:?}` — is the command on PATH?"))?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(task_text.as_bytes())
            .await
            .context("writing task text to exec stdin")?;
        // Drop stdin so the executor sees EOF — many CLI agents (Goose
        // included) wait on stdin close rather than fixed-length reads.
        drop(stdin);
    }

    let out = child
        .wait_with_output()
        .await
        .context("waiting for exec to finish")?;

    let response = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let log = String::from_utf8_lossy(&out.stderr).into_owned();

    if !out.status.success() {
        return Err(anyhow!(
            "exec exited with {}: {}",
            out.status,
            log.lines().rev().find(|l| !l.is_empty()).unwrap_or("(no stderr)")
        ));
    }
    if response.is_empty() {
        return Err(anyhow!(
            "exec produced empty stdout (stderr last line: {})",
            log.lines().rev().find(|l| !l.is_empty()).unwrap_or("(no stderr)")
        ));
    }

    Ok(ExecOutput { response, log })
}

/// Presence announcements are valid for this long; the agent re-announces
/// well before TTL so a peer that joins the mesh during the window still
/// sees us. Override with `LMAO_PRESENCE_TTL_SECS` (the matching re-announce
/// interval is `LMAO_PRESENCE_REANNOUNCE_SECS`).
const PRESENCE_TTL_SECS_DEFAULT: u64 = 300;
/// How often `agent run` re-announces presence. Short enough that a
/// freshly-started peer waiting on the presence topic catches us inside
/// a normal demo window, long enough that we don't flood the network.
const PRESENCE_REANNOUNCE_SECS_DEFAULT: u64 = 15;
/// How long the inbox poll loop sleeps between drains.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
/// Initial wait for gossip mesh to form before announcing. Without this,
/// the first announce is published before any peer is subscribed and is
/// effectively dropped on the floor.
const STARTUP_GOSSIP_WAIT: Duration = Duration::from_secs(3);

pub async fn handle(
    action: AgentAction,
    transport: Arc<dyn Transport>,
    identity: &IdentityConfig,
    json: bool,
) -> Result<()> {
    match action {
        AgentAction::Run {
            name,
            capabilities,
            exec,
        } => {
            let caps = parse_capabilities(&capabilities);
            let node = Arc::new(build_node(
                &name,
                &format!("{} agent", name),
                caps,
                transport,
                identity,
            )?);

            if json {
                let mut info = serde_json::json!({
                    "event": "agent_started",
                    "name": node.card.name,
                    "pubkey": node.pubkey(),
                    "capabilities": node.card.capabilities,
                });
                if identity.encrypt {
                    if let Some(ref bundle) = node.card.intro_bundle {
                        info["encryption"] = serde_json::json!({
                            "enabled": true,
                            "x25519_pubkey": bundle.agent_pubkey,
                        });
                    }
                }
                if let Some(ref kf) = identity.keyfile {
                    info["keyfile"] = serde_json::json!(kf.display().to_string());
                }
                println!("{}", serde_json::to_string(&info)?);
            } else {
                if let Some(ref kf) = identity.keyfile {
                    println!("Using keyfile: {}", kf.display());
                }
                println!("Agent: {}", node.card.name);
                println!("Pubkey: {}", node.pubkey());
                println!("Capabilities: {}", node.card.capabilities.join(", "));
                if identity.encrypt {
                    if let Some(ref bundle) = node.card.intro_bundle {
                        println!("Encryption: ENABLED (X25519+ChaCha20-Poly1305)");
                        println!("X25519 pubkey: {}", bundle.agent_pubkey);
                    }
                }
                println!("Listening for tasks...\n");
            }

            // Open the inbox subscription before announcing, so we don't
            // miss tasks sent in the moment between announce and the first
            // poll loop iteration.
            let _ = node.poll_tasks().await;

            // Wait briefly for the gossip mesh to form. Announcing into a
            // mesh with zero subscribed peers is silently dropped.
            tokio::time::sleep(STARTUP_GOSSIP_WAIT).await;

            let ttl_secs: u64 = std::env::var("LMAO_PRESENCE_TTL_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(PRESENCE_TTL_SECS_DEFAULT);
            let reannounce_secs: u64 = std::env::var("LMAO_PRESENCE_REANNOUNCE_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(PRESENCE_REANNOUNCE_SECS_DEFAULT);

            if let Err(e) = node.announce().await {
                eprintln!("Warning: discovery announce failed: {}", e);
            }
            if let Err(e) = node.announce_presence_with_ttl(ttl_secs).await {
                eprintln!("Warning: presence announce failed: {}", e);
            }

            // Background re-announce so peers that join later still see us.
            // The presence map evicts entries whose TTL has elapsed, so
            // missing one re-announce window means we go offline to peers.
            let presence_node = node.clone();
            tokio::spawn(async move {
                let mut interval = tokio::time::interval(Duration::from_secs(reannounce_secs));
                interval.tick().await; // skip the immediate first tick
                loop {
                    interval.tick().await;
                    if let Err(e) = presence_node.announce_presence_with_ttl(ttl_secs).await {
                        eprintln!("Warning: presence re-announce failed: {}", e);
                    }
                }
            });

            // Inbox loop.
            loop {
                match node.poll_tasks().await {
                    Ok(tasks) => {
                        for task in tasks {
                            if json {
                                let mut event = serde_json::json!({
                                    "event": "task_received",
                                    "task_id": task.id,
                                    "from": task.from,
                                });
                                if let Some(text) = task.text() {
                                    event["message"] = serde_json::json!(text);
                                    let response = match run_exec(&exec, text).await {
                                        Ok(out) => {
                                            event["log_bytes"] = serde_json::json!(out.log.len());
                                            out.response
                                        }
                                        Err(e) => {
                                            event["exec_error"] = serde_json::json!(e.to_string());
                                            format!("[error] {e}")
                                        }
                                    };
                                    match node.respond(&task, &response).await {
                                        Ok(()) => {
                                            event["response"] = serde_json::json!(response);
                                        }
                                        Err(e) => {
                                            event["error"] = serde_json::json!(e.to_string());
                                        }
                                    }
                                }
                                println!("{}", serde_json::to_string(&event)?);
                            } else {
                                println!("Received task {} from {}", task.id, task.from);
                                if let Some(text) = task.text() {
                                    println!("  Message: {}", text);
                                    let response = match run_exec(&exec, text).await {
                                        Ok(out) => {
                                            if !out.log.is_empty() {
                                                println!("  Exec log: {} bytes", out.log.len());
                                            }
                                            out.response
                                        }
                                        Err(e) => {
                                            eprintln!("  Exec failed: {e}");
                                            format!("[error] {e}")
                                        }
                                    };
                                    if let Err(e) = node.respond(&task, &response).await {
                                        eprintln!("  Failed to respond: {}", e);
                                    } else {
                                        // Truncate the printed response so a long agent
                                        // answer doesn't dominate the terminal.
                                        let preview = if response.len() > 200 {
                                            format!("{}…", &response[..200])
                                        } else {
                                            response.clone()
                                        };
                                        println!("  Responded: {}", preview);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Poll error: {}", e);
                    }
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
        }
        AgentAction::Discover => {
            let node = build_node("discovery-client", "temporary", vec![], transport, identity)?;
            match node.discover().await {
                Ok(cards) => {
                    if json {
                        let agents: Vec<_> = cards
                            .iter()
                            .map(|card| {
                                let mut obj = serde_json::json!({
                                    "name": card.name,
                                    "description": card.description,
                                    "capabilities": card.capabilities,
                                    "pubkey": card.public_key,
                                });
                                if let Some(ref bundle) = card.intro_bundle {
                                    obj["encryption"] = serde_json::json!({
                                        "enabled": true,
                                        "x25519_pubkey": bundle.agent_pubkey,
                                    });
                                }
                                obj
                            })
                            .collect();
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({ "agents": agents }))?
                        );
                    } else if cards.is_empty() {
                        println!("No agents found. (Are agents announcing on the network?)");
                    } else {
                        println!("Discovered {} agent(s):\n", cards.len());
                        for card in cards {
                            println!("  Name: {}", card.name);
                            println!("  Description: {}", card.description);
                            println!("  Capabilities: {}", card.capabilities.join(", "));
                            println!("  Pubkey: {}", card.public_key);
                            if let Some(ref bundle) = card.intro_bundle {
                                println!("  Encryption: YES (X25519: {})", bundle.agent_pubkey);
                            }
                            println!();
                        }
                    }
                }
                Err(e) => {
                    eprintln!("Discovery failed: {}", e);
                }
            }
        }
        AgentAction::Bundle => {
            let encrypt_id = IdentityConfig {
                keyfile: identity.keyfile.clone(),
                encrypt: true,
            };
            let node = build_node("bundle-gen", "temporary", vec![], transport, &encrypt_id)?;
            let bundle = node.card.intro_bundle.as_ref().unwrap();
            let json_str = serde_json::to_string_pretty(bundle)?;
            println!("{}", json_str);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn discover_json_output_is_parseable() {
        // Mirrors the JSON structure produced by `agent discover --json`
        let agents = vec![serde_json::json!({
            "name": "echo-agent",
            "description": "An echo agent",
            "capabilities": ["text"],
            "pubkey": "02abcdef1234567890",
        })];
        let output = serde_json::to_string(&serde_json::json!({ "agents": agents })).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let arr = parsed["agents"].as_array().unwrap();
        assert_eq!(arr.len(), 1);
        assert_eq!(arr[0]["name"], "echo-agent");
        assert_eq!(arr[0]["capabilities"][0], "text");
    }

    #[test]
    fn agent_started_json_output_is_parseable() {
        let info = serde_json::json!({
            "event": "agent_started",
            "name": "my-agent",
            "pubkey": "02deadbeef",
        });
        let output = serde_json::to_string(&info).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["event"], "agent_started");
        assert_eq!(parsed["name"], "my-agent");
        assert!(parsed["pubkey"].is_string());
    }
}
