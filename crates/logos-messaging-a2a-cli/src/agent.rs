use anyhow::Result;
use logos_messaging_a2a_transport::Transport;
use std::sync::Arc;
use std::time::Duration;

use crate::cli::AgentAction;
use crate::common::{build_node, parse_capabilities, IdentityConfig};

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
        AgentAction::Run { name, capabilities } => {
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
                                    let response = format!("Echo: {}", text);
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
                                    let response = format!("Echo: {}", text);
                                    if let Err(e) = node.respond(&task, &response).await {
                                        eprintln!("  Failed to respond: {}", e);
                                    } else {
                                        println!("  Responded: {}", response);
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
