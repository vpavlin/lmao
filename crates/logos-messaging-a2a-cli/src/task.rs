use anyhow::{anyhow, Result};
use logos_messaging_a2a_core::{DelegationRequest, DelegationStrategy, Task};
use logos_messaging_a2a_transport::Transport;
use std::path::PathBuf;
use std::sync::Arc;

use crate::cli::TaskAction;
use crate::common::{build_node, IdentityConfig};
use crate::daemon::{default_socket_path, DaemonClient, Request, Response};

/// Try to dispatch the task action via a running daemon. Returns
/// `Ok(true)` if the daemon handled it, `Ok(false)` if no daemon was
/// listening (caller should fall back to building its own node).
async fn try_via_daemon(
    action: &TaskAction,
    daemon_socket: Option<&PathBuf>,
    json: bool,
) -> Result<bool> {
    let socket = daemon_socket.cloned().unwrap_or_else(default_socket_path);
    let client = DaemonClient::new(socket);
    if !client.probe().await {
        return Ok(false);
    }

    let request = match action {
        TaskAction::Send { to, text } => Request::TaskSend {
            to: to.clone(),
            text: text.clone(),
        },
        TaskAction::Status { id } => Request::TaskStatus { id: id.clone() },
        TaskAction::Delegate {
            to,
            capability,
            text,
            parent_id,
            timeout,
            broadcast,
            strategy,
        } => Request::TaskDelegate {
            to: to.clone(),
            capability: capability.clone(),
            text: text.clone(),
            parent_id: parent_id.clone(),
            timeout_secs: *timeout,
            broadcast: *broadcast,
            strategy: strategy.clone(),
            // CLI delegate doesn't take --session-id today; daemons
            // started by basecamp populate it from the QML follow-up
            // path. Plumb through when we add `lmao task delegate
            // --session <id>`.
            session_id: None,
        },
        // Streaming isn't on the daemon protocol yet — fall through to
        // the local poll loop.
        TaskAction::Stream { .. } => return Ok(false),
        TaskAction::History {
            limit,
            offset,
            direction,
            capability,
        } => Request::TaskHistoryList {
            limit: Some(*limit),
            offset: Some(*offset),
            direction: direction.clone(),
            capability: capability.clone(),
            since_ms: None,
        },
        TaskAction::Get { task_id } => Request::TaskHistoryGet {
            task_id: task_id.clone(),
        },
    };

    let resp = client.send(request).await?;
    print_daemon_response(resp, json)?;
    Ok(true)
}

fn print_daemon_response(resp: Response, json: bool) -> Result<()> {
    match resp {
        Response::TaskSend {
            task_id,
            from,
            acked,
        } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "via": "daemon",
                        "task_id": task_id,
                        "from": from,
                        "status": if acked { "acked" } else { "sent" },
                    }))?
                );
            } else {
                eprintln!("Source: daemon");
                println!("Task ID: {task_id}");
                println!("From:    {from}");
                println!(
                    "Status:  {}",
                    if acked {
                        "ACKed by recipient"
                    } else {
                        "Sent (no ACK yet)"
                    }
                );
            }
            Ok(())
        }
        Response::TaskStatus { results } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "via": "daemon",
                        "results": results,
                    }))?
                );
            } else if results.is_empty() {
                println!("No matching task in this daemon's inbox yet.");
            } else {
                eprintln!("Source: daemon");
                for t in results {
                    println!("Task:   {}", t.id);
                    println!("State:  {}", t.state);
                    if let Some(text) = t.result_text {
                        println!("Result: {text}");
                    }
                }
            }
            Ok(())
        }
        Response::TaskDelegate { results } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "via": "daemon",
                        "results": results,
                    }))?
                );
                return Ok(());
            }
            eprintln!("Source: daemon");
            if results.is_empty() {
                println!("Delegation produced no results.");
            }
            for r in results {
                let status = if r.success { "OK" } else { "FAIL" };
                let agent = &r.agent_id[..12.min(r.agent_id.len())];
                println!("[{status}] agent={agent} subtask={}", r.subtask_id);
                if let Some(text) = r.result_text {
                    println!("  Result: {text}");
                }
                if let Some(err) = r.error {
                    println!("  Error:  {err}");
                }
            }
            Ok(())
        }
        Response::TaskHistoryList { entries, history_path } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "via": "daemon",
                        "entries": entries,
                        "history_path": history_path,
                    }))?
                );
                return Ok(());
            }
            eprintln!("Source: daemon");
            if let Some(p) = history_path {
                eprintln!("History: {}", p.display());
            }
            if entries.is_empty() {
                println!("No history entries yet.");
                return Ok(());
            }
            for e in entries {
                let status = if e.success { "OK  " } else { "FAIL" };
                let dir = match e.direction.as_str() {
                    "delegated" => ">",
                    "received" => "<",
                    _ => "?",
                };
                let peer = if e.peer_name.is_empty() {
                    e.peer_pubkey[..12.min(e.peer_pubkey.len())].to_string()
                } else {
                    format!(
                        "{} ({})",
                        e.peer_name,
                        &e.peer_pubkey[..8.min(e.peer_pubkey.len())]
                    )
                };
                println!(
                    "{ts} [{status}] {dir} {peer}  cap={cap}  {ms}ms",
                    ts = format_unix_ms(e.created_at_ms),
                    cap = if e.capability.is_empty() { "-" } else { &e.capability },
                    ms = e.elapsed_ms,
                );
                let preview: String = e.text.chars().take(80).collect();
                println!("    text: {preview}");
                if !e.body.is_empty() {
                    let body: String = e.body.chars().take(80).collect();
                    println!("    body: {body}");
                }
                if let Some(err) = e.error {
                    println!("    err:  {err}");
                }
                println!("    id:   {}", e.task_id);
            }
            Ok(())
        }
        Response::TaskHistoryGet { entry } => {
            if json {
                println!(
                    "{}",
                    serde_json::to_string(&serde_json::json!({
                        "via": "daemon",
                        "entry": entry,
                    }))?
                );
                return Ok(());
            }
            let Some(e) = entry else {
                println!("No history entry with that id.");
                return Ok(());
            };
            println!("Task ID:   {}", e.task_id);
            println!("Direction: {}", e.direction);
            println!("Peer:      {} ({})", e.peer_name, e.peer_pubkey);
            if !e.capability.is_empty() {
                println!("Capability: {}", e.capability);
            }
            println!("When:      {}", format_unix_ms(e.created_at_ms));
            println!("Elapsed:   {}ms", e.elapsed_ms);
            println!("Success:   {}", e.success);
            if let Some(err) = e.error {
                println!("Error:     {err}");
            }
            if !e.cid.is_empty() {
                println!("Audit CID: codex://{}", e.cid);
            }
            println!("Text:");
            println!("{}", e.text);
            if !e.body.is_empty() {
                println!("\nBody:");
                println!("{}", e.body);
            }
            Ok(())
        }
        Response::Error { message } => Err(anyhow!("daemon error: {message}")),
        other => Err(anyhow!("unexpected daemon response: {other:?}")),
    }
}

/// Format a unix-millisecond timestamp as `YYYY-MM-DD HH:MM:SS` UTC.
/// Avoids pulling chrono/time; the demo display doesn't need locale.
fn format_unix_ms(ms: u64) -> String {
    let secs = ms / 1000;
    // Days since 1970-01-01 (proleptic Gregorian, Julian-day style).
    let days = (secs / 86_400) as i64;
    let sod = secs % 86_400;
    let h = sod / 3600;
    let m = (sod % 3600) / 60;
    let s = sod % 60;
    let (y, mo, d) = ymd_from_days(days);
    format!("{y:04}-{mo:02}-{d:02} {h:02}:{m:02}:{s:02}Z")
}

/// Convert days-since-1970-01-01 → (year, month, day). Algorithm from
/// Howard Hinnant's date library: avoids leap-second / leap-day
/// branches by transforming through a March-anchored era.
fn ymd_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719468; // days since 0000-03-01
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe as i32 + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

pub async fn handle(
    action: TaskAction,
    transport: Arc<dyn Transport>,
    daemon_socket: Option<&PathBuf>,
    identity: &IdentityConfig,
    json: bool,
) -> Result<()> {
    if try_via_daemon(&action, daemon_socket, json).await? {
        return Ok(());
    }
    match action {
        TaskAction::Send { to, text } => {
            let node = build_node("cli-sender", "CLI client", vec![], transport, identity)?;
            if !json {
                println!("Sending task to {}...", &to[..12.min(to.len())]);
                println!("From pubkey: {}", node.pubkey());
            }
            let task = Task::new(node.pubkey(), &to, &text);
            match node.send_task(&task).await {
                Ok(acked) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "task_id": task.id,
                                "from": node.pubkey(),
                                "to": to,
                                "status": if acked { "acked" } else { "sent" },
                            }))?
                        );
                    } else {
                        println!("Task ID: {}", task.id);
                        if acked {
                            println!("Status: ACKed by recipient");
                        } else {
                            println!("Status: Sent (no ACK — recipient may be offline)");
                        }
                    }
                }
                Err(e) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "task_id": task.id,
                                "from": node.pubkey(),
                                "to": to,
                                "status": "failed",
                                "error": e.to_string(),
                            }))?
                        );
                    } else {
                        eprintln!("Failed to send task: {}", e);
                        println!("Task ID: {} (failed)", task.id);
                    }
                }
            }
        }
        TaskAction::Status { id } => {
            let node = build_node("cli-poller", "CLI client", vec![], transport, identity)?;
            if !json {
                println!("Polling for task {} responses...", id);
                println!("Listening as: {}", node.pubkey());
            }
            match node.poll_tasks().await {
                Ok(tasks) => {
                    let found: Vec<_> = tasks.iter().filter(|t| t.id == id).collect();
                    if json {
                        let results: Vec<_> = found
                            .iter()
                            .map(|t| {
                                let mut obj = serde_json::json!({
                                    "task_id": t.id,
                                    "state": format!("{:?}", t.state),
                                });
                                if let Some(text) = t.result_text() {
                                    obj["result"] = serde_json::json!(text);
                                }
                                obj
                            })
                            .collect();
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "task_id": id,
                                "listener": node.pubkey(),
                                "results": results,
                            }))?
                        );
                    } else if found.is_empty() {
                        println!("No response yet for task {}", id);
                    } else {
                        for task in found {
                            println!("Task: {}", task.id);
                            println!("State: {:?}", task.state);
                            if let Some(text) = task.result_text() {
                                println!("Result: {}", text);
                            }
                        }
                    }
                }
                Err(e) => {
                    if json {
                        println!(
                            "{}",
                            serde_json::to_string(&serde_json::json!({
                                "task_id": id,
                                "error": e.to_string(),
                            }))?
                        );
                    } else {
                        eprintln!("Failed to poll: {}", e);
                    }
                }
            }
        }
        TaskAction::Stream { id, timeout } => {
            let node = build_node("cli-stream", "CLI client", vec![], transport, identity)?;
            if !json {
                println!(
                    "Following stream for task {} (timeout {}s)...\n",
                    id, timeout
                );
            }

            let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(timeout);
            let mut last_index: Option<u32> = None;

            while tokio::time::Instant::now() < deadline {
                match node.poll_stream_chunks(&id).await {
                    Ok(chunks) => {
                        for chunk in &chunks {
                            if last_index.is_none() || chunk.chunk_index > last_index.unwrap() {
                                if json {
                                    println!(
                                        "{}",
                                        serde_json::to_string(&serde_json::json!({
                                            "event": "chunk",
                                            "task_id": id,
                                            "chunk_index": chunk.chunk_index,
                                            "text": chunk.text,
                                            "is_final": chunk.is_final,
                                        }))?
                                    );
                                } else {
                                    print!("{}", chunk.text);
                                }
                                last_index = Some(chunk.chunk_index);
                            }
                        }
                        if chunks.iter().any(|c| c.is_final) {
                            if json {
                                println!(
                                    "{}",
                                    serde_json::to_string(&serde_json::json!({
                                        "event": "stream_complete",
                                        "task_id": id,
                                        "total_chunks": chunks.len(),
                                    }))?
                                );
                            } else {
                                println!();
                                println!("\n--- Stream complete ({} chunks) ---", chunks.len());
                            }
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("Stream poll error: {}", e);
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }

            if last_index.is_none() {
                if json {
                    println!(
                        "{}",
                        serde_json::to_string(&serde_json::json!({
                            "event": "stream_timeout",
                            "task_id": id,
                            "total_chunks": 0,
                        }))?
                    );
                } else {
                    println!("No stream chunks received for task {}", id);
                }
            }
        }
        TaskAction::Delegate {
            to,
            capability,
            text,
            parent_id,
            timeout,
            broadcast,
            strategy,
        } => {
            let node = build_node(
                "cli-delegator",
                "CLI delegation client",
                vec![],
                transport,
                identity,
            )?;
            if !json {
                println!("From pubkey: {}", node.pubkey());
            }

            // Build strategy from flags
            let strategy = if let Some(ref agent_key) = to {
                // Direct delegation — send task directly, skip presence lookup
                if !json {
                    println!(
                        "Delegating directly to {}...",
                        &agent_key[..12.min(agent_key.len())]
                    );
                }
                let task = Task::new(node.pubkey(), agent_key, &text);
                if !json {
                    println!("Subtask ID: {}", task.id);
                }
                match node.send_task(&task).await {
                    Ok(acked) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "subtask_id": task.id,
                                    "from": node.pubkey(),
                                    "to": agent_key,
                                    "parent_id": parent_id,
                                    "status": if acked { "acked" } else { "sent" },
                                }))?
                            );
                        } else {
                            if acked {
                                println!("Status: ACKed by recipient");
                            } else {
                                println!("Status: Sent (no ACK)");
                            }
                            println!("Parent task: {}", parent_id);
                        }
                    }
                    Err(e) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "subtask_id": task.id,
                                    "from": node.pubkey(),
                                    "to": agent_key,
                                    "parent_id": parent_id,
                                    "status": "failed",
                                    "error": e.to_string(),
                                }))?
                            );
                        } else {
                            eprintln!("Failed to delegate: {}", e);
                        }
                    }
                }
                return Ok(());
            } else if let Some(ref s) = strategy {
                match s.as_str() {
                    "round-robin" => DelegationStrategy::RoundRobin,
                    "broadcast" => DelegationStrategy::BroadcastCollect,
                    "first-available" => DelegationStrategy::FirstAvailable,
                    other => {
                        if let Some(ref cap) = capability {
                            DelegationStrategy::CapabilityMatch {
                                capability: cap.clone(),
                            }
                        } else {
                            eprintln!("Unknown strategy '{other}', using first-available");
                            DelegationStrategy::FirstAvailable
                        }
                    }
                }
            } else if let Some(ref cap) = capability {
                DelegationStrategy::CapabilityMatch {
                    capability: cap.clone(),
                }
            } else {
                DelegationStrategy::FirstAvailable
            };

            // Presence-based delegation
            if !json {
                println!("Discovering peers via presence...");
            }
            // 25s gives a freshly-spawned client time to dial the gossip
            // mesh AND comfortably exceeds the agent re-announce interval
            // (default 15s) so a single missed cycle doesn't fail the
            // delegation. Override with LMAO_DELEGATE_DISCOVERY_SECS.
            let poll_secs: u64 = std::env::var("LMAO_DELEGATE_DISCOVERY_SECS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(25);
            let poll_deadline =
                tokio::time::Instant::now() + std::time::Duration::from_secs(poll_secs);
            while tokio::time::Instant::now() < poll_deadline {
                node.poll_presence().await?;
                if !node.peers().all_live().is_empty() {
                    // Keep polling briefly even after first hit so
                    // capability-match delegation has multiple peers to
                    // choose from when the mesh delivers them in waves.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                    node.poll_presence().await?;
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let live = node.peers().all_live();
            if !json {
                println!("Found {} live peer(s)", live.len());
            }

            let request = DelegationRequest {
                parent_task_id: parent_id.clone(),
                subtask_text: text,
                strategy,
                timeout_secs: timeout,
                session_id: None,
            };

            if broadcast {
                if !json {
                    println!("Broadcasting to all matching peers...");
                }
                match node.delegate_broadcast(&request).await {
                    Ok(results) => {
                        if json {
                            let items: Vec<_> = results
                                .iter()
                                .map(|r| {
                                    serde_json::json!({
                                        "subtask_id": r.subtask_id,
                                        "agent_id": r.agent_id,
                                        "success": r.success,
                                        "result_text": r.result_text,
                                        "error": r.error,
                                    })
                                })
                                .collect();
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "parent_id": parent_id,
                                    "broadcast": true,
                                    "results": items,
                                }))?
                            );
                        } else {
                            println!("Received {} result(s):", results.len());
                            for r in &results {
                                let status = if r.success { "OK" } else { "FAIL" };
                                let agent = &r.agent_id[..12.min(r.agent_id.len())];
                                println!("  [{status}] agent={agent} subtask={}", r.subtask_id);
                                if let Some(ref text) = r.result_text {
                                    println!("    Result: {text}");
                                }
                                if let Some(ref err) = r.error {
                                    println!("    Error: {err}");
                                }
                            }
                        }
                    }
                    Err(e) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "parent_id": parent_id,
                                    "broadcast": true,
                                    "error": e.to_string(),
                                }))?
                            );
                        } else {
                            eprintln!("Broadcast delegation failed: {}", e);
                        }
                    }
                }
            } else {
                if !json {
                    println!("Delegating to single peer...");
                }
                match node.delegate_task(&request).await {
                    Ok(r) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "parent_id": parent_id,
                                    "subtask_id": r.subtask_id,
                                    "agent_id": r.agent_id,
                                    "success": r.success,
                                    "result_text": r.result_text,
                                    "error": r.error,
                                }))?
                            );
                        } else {
                            let status = if r.success { "OK" } else { "FAIL" };
                            let agent = &r.agent_id[..12.min(r.agent_id.len())];
                            println!("[{status}] agent={agent} subtask={}", r.subtask_id);
                            if let Some(ref text) = r.result_text {
                                println!("Result: {text}");
                            }
                            if let Some(ref err) = r.error {
                                println!("Error: {err}");
                            }
                        }
                    }
                    Err(e) => {
                        if json {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "parent_id": parent_id,
                                    "error": e.to_string(),
                                }))?
                            );
                        } else {
                            eprintln!("Delegation failed: {}", e);
                        }
                    }
                }
            }
        }
        TaskAction::History { .. } | TaskAction::Get { .. } => {
            return Err(anyhow!(
                "task history requires a running `lmao agent run` daemon — \
                history is daemon-side state. Set --daemon-socket or run \
                `lmao agent run` first."
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn task_send_json_output_is_parseable() {
        // Mirrors the JSON structure produced by `task send --json`
        let output = serde_json::to_string(&serde_json::json!({
            "task_id": "550e8400-e29b-41d4-a716-446655440000",
            "from": "02aabbcc",
            "to": "02ddeeff",
            "status": "acked",
        }))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["status"], "acked");
        assert!(parsed["task_id"].is_string());
        assert!(parsed["from"].is_string());
        assert!(parsed["to"].is_string());
    }

    #[test]
    fn task_status_json_output_is_parseable() {
        let output = serde_json::to_string(&serde_json::json!({
            "task_id": "task-42",
            "listener": "02aabbcc",
            "results": [
                {
                    "task_id": "task-42",
                    "state": "Completed",
                    "result": "Echo: hello",
                }
            ],
        }))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["task_id"], "task-42");
        let results = parsed["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["state"], "Completed");
    }

    #[test]
    fn stream_chunk_json_output_is_parseable() {
        let output = serde_json::to_string(&serde_json::json!({
            "event": "chunk",
            "task_id": "task-42",
            "chunk_index": 0,
            "text": "Hello ",
            "is_final": false,
        }))
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(parsed["event"], "chunk");
        assert_eq!(parsed["chunk_index"], 0);
        assert_eq!(parsed["is_final"], false);
    }
}
