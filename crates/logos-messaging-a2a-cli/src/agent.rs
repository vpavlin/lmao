use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_storage::StorageBackend;
use logos_messaging_a2a_transport::Transport;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

use crate::cli::AgentAction;
use crate::common::{build_node, parse_capabilities, IdentityConfig};
use crate::daemon::{default_socket_path, DaemonServer};

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
/// the caller can decide whether to respond with a graceful `[error]`
/// message or skip the task entirely.
async fn run_exec(
    cmd: &str,
    task_text: &str,
    session_id: Option<&str>,
    sender: &str,
) -> Result<ExecOutput> {
    let mut command = tokio::process::Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // Pass conversation context to the exec via env so wrappers
        // (pi-exec.sh, lemonade-summarizer.sh) can keep per-thread
        // state — pi `--session $LMAO_SESSION_ID`, lemonade conversation
        // history file — instead of cold-starting every follow-up.
        .env("LMAO_SENDER_PUBKEY", sender);
    if let Some(sid) = session_id {
        if !sid.is_empty() {
            command.env("LMAO_SESSION_ID", sid);
        }
    }
    let mut child = command
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
            log.lines()
                .rev()
                .find(|l| !l.is_empty())
                .unwrap_or("(no stderr)")
        ));
    }
    if response.is_empty() {
        return Err(anyhow!(
            "exec produced empty stdout (stderr last line: {})",
            log.lines()
                .rev()
                .find(|l| !l.is_empty())
                .unwrap_or("(no stderr)")
        ));
    }

    Ok(ExecOutput { response, log })
}

/// Try to upload the audit-log payload to the configured storage backend.
/// Returns `Some(cid)` on success, `None` if no backend is configured or
/// the log is empty. Upload errors are logged and swallowed — a failed
/// upload should never block delivery of the agent's actual response.
async fn upload_log(storage: &Option<Arc<dyn StorageBackend>>, log: &str) -> Option<String> {
    let backend = storage.as_ref()?;
    if log.is_empty() {
        return None;
    }
    match backend.upload(log.as_bytes().to_vec()).await {
        Ok(cid) => Some(cid),
        Err(e) => {
            eprintln!("  Storage upload failed: {e}");
            None
        }
    }
}

/// Format the LMAO response text — agent's answer plus, when storage is
/// configured, a content-addressed pointer to the full execution log so
/// the receiver can audit the run after the fact.
fn format_response(answer: &str, log_cid: Option<&str>) -> String {
    match log_cid {
        Some(cid) => format!("{answer}\n\n---\nexecution log: codex://{cid}"),
        None => answer.to_string(),
    }
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

// One entry-point that wires daemon socket, identity, transport, storage,
// trust file, history dir, and CLI flags. Could be a builder, but the
// call site is a single match arm — splitting just to satisfy clippy
// pushes the same args into a struct without adding clarity.
#[allow(clippy::too_many_arguments)]
pub async fn handle(
    action: AgentAction,
    transport: Arc<dyn Transport>,
    storage: Option<Arc<dyn StorageBackend>>,
    daemon_socket: Option<PathBuf>,
    identity: &IdentityConfig,
    trust_file: Option<PathBuf>,
    storage_data_dir: Option<PathBuf>,
    json: bool,
) -> Result<()> {
    match action {
        AgentAction::Run {
            name,
            capabilities,
            exec,
        } => {
            let caps = parse_capabilities(&capabilities);
            let trust_file_explicit = trust_file.is_some();
            let trust_path =
                trust_file.unwrap_or_else(logos_messaging_a2a_core::TrustList::default_path);
            let trust_list = logos_messaging_a2a_core::TrustList::load_from(&trust_path)
                .with_context(|| format!("loading trust file {}", trust_path.display()))?;
            let trust_mode = trust_list.mode();
            let trust_count = trust_list.len();
            // Persist history alongside the storage data dir so wiping
            // libstorage state and wiping history are independent ops.
            // No history when storage_data_dir is unset (ephemeral CLI),
            // since we don't have a stable home for the JSONL file.
            let history = storage_data_dir.as_ref().map(|d| {
                let path = d.join("history.jsonl");
                logos_messaging_a2a_node::history::History::open(path)
            });

            let max_concurrent: u32 = std::env::var("LMAO_AGENT_MAX_CONCURRENT")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(1)
                .max(1);
            let mut builder =
                build_node(&name, &format!("{} agent", name), caps, transport, identity)?
                    .with_trust_list(trust_list)
                    .with_max_concurrent(max_concurrent);
            if let Some(h) = history {
                builder = builder.with_history(h);
            }
            let node = Arc::new(builder);

            if json {
                let mut info = serde_json::json!({
                    "event": "agent_started",
                    "name": node.card.name,
                    "pubkey": node.pubkey(),
                    "capabilities": node.card.capabilities,
                    "trust": {
                        "mode": trust_mode,
                        "entries": trust_count,
                        "file": trust_path.display().to_string(),
                    },
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
                println!(
                    "Trust: {trust_mode:?} ({trust_count} entries from {})",
                    trust_path.display()
                );
                // Loud warning when an *implicit* default trust file is
                // active in Enforce mode — incoming presence + delegation
                // from peers not on the list will be silently filtered,
                // and operators have been bitten by stale lists from a
                // previous demo carrying over into a fresh run.
                if !trust_file_explicit
                    && matches!(trust_mode, logos_messaging_a2a_core::TrustMode::Enforce)
                    && trust_count > 0
                {
                    eprintln!(
                        "warning: trust list is in `enforce` mode with {trust_count} entries \
                         loaded from the default path ({}). Peers not on the list will be \
                         filtered out of presence and delegation. Pass `--trust-file <path>` \
                         to use a different list, or run `lmao trust mode off` to disable \
                         filtering for this identity.",
                        trust_path.display()
                    );
                }
                if identity.encrypt {
                    if let Some(ref bundle) = node.card.intro_bundle {
                        println!("Encryption: ENABLED (X25519+ChaCha20-Poly1305)");
                        println!("X25519 pubkey: {}", bundle.agent_pubkey);
                    }
                }
                println!("Listening for tasks...\n");
            }

            // Bind the IPC socket so other CLI commands on this host can
            // share this process's already-connected node + storage.
            let socket = daemon_socket.unwrap_or_else(default_socket_path);
            let server = Arc::new(DaemonServer::new(
                socket,
                node.clone(),
                storage.clone(),
                name.clone(),
                Some(trust_path.clone()),
            ));
            tokio::spawn(server.serve());

            // Open the inbox + presence subscriptions before announcing,
            // so we don't miss tasks (or peer announcements) sent in the
            // moment between announce and the first poll loop iteration.
            // Both subscriptions stay open for the lifetime of the agent.
            let _ = node.poll_tasks().await;
            let _ = node.poll_presence().await;

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

            // Inbox loop. Also drains presence so the agent's PeerMap
            // sees other agents' announcements — required for any
            // capability-routed delegation through this agent's daemon.
            //
            // Long-running agents accumulate state without periodic
            // eviction: stale presence entries, idle conversation
            // sessions. We tick the eviction every `EVICT_EVERY_TICKS`
            // poll iterations so the agent's RSS doesn't climb
            // monotonically over hours of uptime.
            const EVICT_EVERY_TICKS: u32 = 30; // ~ every 60 s with POLL_INTERVAL=2s
            const SESSION_IDLE_MAX_SECS: u64 = 60 * 60; // 1 h
            let mut tick: u32 = 0;
            loop {
                tick = tick.wrapping_add(1);
                if tick.is_multiple_of(EVICT_EVERY_TICKS) {
                    let evicted_peers = node.peers().evict_expired();
                    let evicted_sessions = node.evict_idle_sessions(SESSION_IDLE_MAX_SECS);
                    if evicted_peers + evicted_sessions > 0 {
                        eprintln!("[evict] peers={evicted_peers} sessions={evicted_sessions}");
                    }
                }
                let _ = node.poll_presence().await;
                match node.poll_tasks().await {
                    Ok(tasks) => {
                        // Track in-batch acceptance so we can short-circuit
                        // once we hit `max_concurrent`. With sequential
                        // exec this only matters when `poll_tasks` drains
                        // multiple tasks at once — in that case all but
                        // `max_concurrent` get an immediate "rejected"
                        // response so the sender can retry elsewhere
                        // instead of timing out.
                        let mut accepted_this_batch: u32 = 0;
                        let max_in_batch = node.max_concurrent();
                        for task in tasks {
                            if accepted_this_batch >= max_in_batch {
                                let reject_payload =
                                    "[rejected: at capacity, retry later]".to_string();
                                if json {
                                    let event = serde_json::json!({
                                        "event": "task_rejected",
                                        "task_id": task.id,
                                        "from": task.from,
                                        "reason": "at_capacity",
                                        "max_concurrent": max_in_batch,
                                    });
                                    println!("{}", serde_json::to_string(&event)?);
                                } else {
                                    eprintln!(
                                        "  Rejected task {} from {} — at capacity",
                                        task.id, task.from
                                    );
                                }
                                let _ = node.respond(&task, &reject_payload).await;
                                continue;
                            }
                            if json {
                                let mut event = serde_json::json!({
                                    "event": "task_received",
                                    "task_id": task.id,
                                    "from": task.from,
                                });
                                if let Some(text) = task.text() {
                                    event["message"] = serde_json::json!(text);
                                    if let Some(ref sid) = task.session_id {
                                        event["session_id"] = serde_json::json!(sid);
                                    }
                                    accepted_this_batch += 1;
                                    node.load_inc();
                                    let exec_result = run_exec(
                                        &exec,
                                        text,
                                        task.session_id.as_deref(),
                                        &task.from,
                                    )
                                    .await;
                                    let (response, failed) = match exec_result {
                                        Ok(out) => {
                                            event["log_bytes"] = serde_json::json!(out.log.len());
                                            let cid = upload_log(&storage, &out.log).await;
                                            if let Some(ref c) = cid {
                                                event["log_cid"] = serde_json::json!(c);
                                            }
                                            (format_response(&out.response, cid.as_deref()), false)
                                        }
                                        Err(e) => {
                                            event["exec_error"] = serde_json::json!(e.to_string());
                                            (format!("[error] {e}"), true)
                                        }
                                    };
                                    node.load_dec();
                                    let respond_outcome = if failed {
                                        node.respond_failed(&task, &response).await
                                    } else {
                                        node.respond(&task, &response).await
                                    };
                                    match respond_outcome {
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
                                    if let Some(ref sid) = task.session_id {
                                        println!("  Session: {sid}");
                                    }
                                    accepted_this_batch += 1;
                                    node.load_inc();
                                    let (response, failed) = match run_exec(
                                        &exec,
                                        text,
                                        task.session_id.as_deref(),
                                        &task.from,
                                    )
                                    .await
                                    {
                                        Ok(out) => {
                                            if !out.log.is_empty() {
                                                println!("  Exec log: {} bytes", out.log.len());
                                            }
                                            let cid = upload_log(&storage, &out.log).await;
                                            if let Some(ref c) = cid {
                                                println!("  Uploaded log → codex://{c}");
                                            }
                                            (format_response(&out.response, cid.as_deref()), false)
                                        }
                                        Err(e) => {
                                            eprintln!("  Exec failed: {e}");
                                            (format!("[error] {e}"), true)
                                        }
                                    };
                                    node.load_dec();
                                    let respond_outcome = if failed {
                                        node.respond_failed(&task, &response).await
                                    } else {
                                        node.respond(&task, &response).await
                                    };
                                    if let Err(e) = respond_outcome {
                                        eprintln!("  Failed to respond: {}", e);
                                    } else {
                                        let preview = if response.len() > 200 {
                                            format!("{}…", &response[..200])
                                        } else {
                                            response.clone()
                                        };
                                        println!(
                                            "  Responded ({}): {}",
                                            if failed { "failed" } else { "ok" },
                                            preview
                                        );
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
