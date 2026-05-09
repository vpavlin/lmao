//! Daemon-side IPC server: listens on a Unix socket, dispatches incoming
//! [`Request`]s against a shared LmaoNode + storage backend.

use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_core::{
    DelegationRequest, DelegationStrategy, Task, TrustEntry, TrustMode,
};
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_storage::StorageBackend;
use logos_messaging_a2a_transport::Transport;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Instant, SystemTime};
use tokio::net::{UnixListener, UnixStream};

use super::frame::{read_frame, write_frame};
use super::protocol::{
    AgentCardWire, DelegationWire, PeerWire, Request, Response, TaskWire, TrustEntryWire,
};

/// Background server attached to an `lmao agent run` process. Holds an
/// `Arc<LmaoNode>` (shared with the inbox loop) and an optional storage
/// backend, accepts Unix-socket connections and runs each one as a one-
/// request → one-response exchange.
pub struct DaemonServer {
    socket_path: PathBuf,
    node: Arc<LmaoNode<Arc<dyn Transport>>>,
    storage: Option<Arc<dyn StorageBackend>>,
    started_at: Instant,
    /// Display name from the agent — surfaced via the `info` response so
    /// clients can confirm they're talking to the right daemon.
    name: String,
    /// Path to the trust file the daemon was started with. Trust
    /// mutations write back to this path so a restart picks up the
    /// changes. `None` if no file was configured (in-memory only).
    trust_file: Option<PathBuf>,
}

impl DaemonServer {
    pub fn new(
        socket_path: PathBuf,
        node: Arc<LmaoNode<Arc<dyn Transport>>>,
        storage: Option<Arc<dyn StorageBackend>>,
        name: String,
        trust_file: Option<PathBuf>,
    ) -> Self {
        Self {
            socket_path,
            node,
            storage,
            started_at: Instant::now(),
            name,
            trust_file,
        }
    }

    /// Bind the socket and run the accept loop forever. Designed to be
    /// `tokio::spawn`ed — it returns a Result but typically only does so
    /// on a fatal bind error.
    pub async fn serve(self: Arc<Self>) -> Result<()> {
        // Best-effort cleanup of a stale socket file from a previous run.
        // If a *running* daemon already owns this path, the bind() below
        // will fail, surfacing the conflict to the user.
        let _ = std::fs::remove_file(&self.socket_path);
        if let Some(parent) = self.socket_path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("creating daemon socket parent dir {}", parent.display())
            })?;
        }

        let listener = UnixListener::bind(&self.socket_path)
            .with_context(|| format!("binding daemon socket {}", self.socket_path.display()))?;
        // Tighten permissions so only this user can talk to the daemon.
        // Best-effort — on filesystems without unix mode bits we accept
        // whatever the OS gives us.
        let _ = set_socket_perms(&self.socket_path);

        eprintln!("[daemon] listening on {}", self.socket_path.display());

        loop {
            let (stream, _) = match listener.accept().await {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[daemon] accept failed: {e}");
                    continue;
                }
            };
            let server = self.clone();
            tokio::spawn(async move {
                if let Err(e) = server.handle_connection(stream).await {
                    eprintln!("[daemon] connection error: {e}");
                }
            });
        }
    }

    /// Persist the current trust list to the configured file, if any.
    /// Returns true if a write happened, false if no file is configured.
    /// Logs (but does not surface) write errors — a failed persist
    /// shouldn't block the in-memory mutation from taking effect, since
    /// the next restart can be re-seeded from CLI.
    fn persist_trust(&self) -> bool {
        let Some(ref path) = self.trust_file else {
            return false;
        };
        if let Err(e) = self.node.trust_save_to(path) {
            eprintln!(
                "[daemon] persisting trust list to {} failed: {e}",
                path.display()
            );
            return false;
        }
        true
    }

    async fn handle_connection(self: Arc<Self>, mut stream: UnixStream) -> Result<()> {
        let req: Request = read_frame(&mut stream).await?;
        let resp = match self.dispatch(req).await {
            Ok(r) => r,
            Err(e) => Response::Error {
                message: e.to_string(),
            },
        };
        write_frame(&mut stream, &resp).await?;
        Ok(())
    }

    async fn dispatch(&self, req: Request) -> Result<Response> {
        match req {
            Request::Info => Ok(Response::Info {
                name: self.name.clone(),
                pubkey: self.node.pubkey().to_string(),
                capabilities: self.node.card.capabilities.clone(),
                uptime_secs: self.started_at.elapsed().as_secs(),
                socket_path: self.socket_path.clone(),
                storage_enabled: self.storage.is_some(),
                encryption_pubkey: self
                    .node
                    .identity()
                    .map(|id| id.public_key_hex()),
                load: Some((&self.node.current_load_status()).into()),
            }),
            Request::Discover => {
                let cards = self.node.discover().await?;
                Ok(Response::Discover {
                    agents: cards
                        .into_iter()
                        .map(|c| AgentCardWire {
                            name: c.name,
                            description: c.description,
                            version: c.version,
                            capabilities: c.capabilities,
                            public_key: c.public_key,
                            has_intro_bundle: c.intro_bundle.is_some(),
                        })
                        .collect(),
                })
            }
            Request::PresencePeers { capability } => {
                self.node.poll_presence().await?;
                let peers = match capability {
                    Some(ref cap) => self.node.find_peers_by_capability(cap),
                    None => self.node.peers().all_live(),
                };
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Ok(Response::PresencePeers {
                    peers: peers
                        .into_iter()
                        .map(|(id, info)| PeerWire {
                            agent_id: id,
                            name: info.name,
                            capabilities: info.capabilities,
                            waku_topic: info.waku_topic,
                            last_seen_secs: info.last_seen.min(now_secs),
                            ttl_secs: info.ttl_secs,
                            load: info.load.as_ref().map(|l| l.into()),
                        })
                        .collect(),
                })
            }
            Request::TaskSend { to, text } => {
                let task = Task::new(self.node.pubkey(), &to, &text);
                let acked = self.node.send_task(&task).await?;
                Ok(Response::TaskSend {
                    task_id: task.id,
                    from: self.node.pubkey().to_string(),
                    acked,
                })
            }
            Request::TaskStatus { id } => {
                let tasks = self.node.poll_tasks().await?;
                Ok(Response::TaskStatus {
                    results: tasks
                        .into_iter()
                        .filter(|t| t.id == id)
                        .map(|t| TaskWire {
                            id: t.id.clone(),
                            state: format!("{:?}", t.state),
                            from: t.from.clone(),
                            to: t.to.clone(),
                            text: t.text().map(String::from),
                            result_text: t.result_text().map(String::from),
                        })
                        .collect(),
                })
            }
            Request::TaskDelegate {
                to,
                capability,
                text,
                parent_id,
                timeout_secs,
                broadcast,
                strategy,
                session_id,
            } => {
                let strategy =
                    build_strategy(to.as_deref(), capability.as_deref(), strategy.as_deref());
                let request = DelegationRequest {
                    parent_task_id: parent_id,
                    subtask_text: text,
                    strategy,
                    timeout_secs,
                    session_id,
                };
                // `--to <pubkey>` short-circuits strategy selection: send
                // the subtask directly to the named peer, regardless of
                // capability list / load / round-robin counter. Trust
                // list still applies — direct delegation to an untrusted
                // peer surfaces a diagnostic error rather than silently
                // failing strategy fall-through (the previous behaviour
                // dropped `to` on the floor and ran FirstAvailable).
                let results = if let Some(ref pk) = to {
                    if broadcast {
                        return Ok(Response::Error {
                            message:
                                "--broadcast cannot be combined with --to (direct delegation \
                                 targets a single peer)"
                                    .into(),
                        });
                    }
                    vec![self.node.delegate_direct(&request, pk).await?]
                } else if broadcast {
                    self.node.delegate_broadcast(&request).await?
                } else {
                    vec![self.node.delegate_task(&request).await?]
                };
                Ok(Response::TaskDelegate {
                    results: results
                        .into_iter()
                        .map(|r| DelegationWire {
                            parent_task_id: r.parent_task_id,
                            subtask_id: r.subtask_id,
                            agent_id: r.agent_id,
                            success: r.success,
                            result_text: r.result_text,
                            error: r.error,
                        })
                        .collect(),
                })
            }
            Request::StorageFetch { cid } => {
                let backend = self
                    .storage
                    .as_ref()
                    .ok_or_else(|| anyhow!("daemon has no storage backend configured"))?;
                let bytes = backend
                    .download(&cid)
                    .await
                    .with_context(|| format!("downloading {cid} from storage"))?;
                use base64::Engine;
                let payload_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
                Ok(Response::StorageFetch { cid, payload_b64 })
            }
            Request::TrustList => {
                let (mode, entries) = self.node.trust_snapshot();
                Ok(Response::TrustList {
                    mode: mode_to_str(mode).to_string(),
                    entries: entries
                        .into_iter()
                        .map(|e| TrustEntryWire {
                            pubkey: e.pubkey,
                            nickname: e.nickname,
                            capabilities: e.capabilities,
                            notes: e.notes,
                        })
                        .collect(),
                    trust_file: self.trust_file.clone(),
                })
            }
            Request::TrustAdd {
                pubkey,
                nickname,
                capabilities,
                notes,
                encryption_pubkey,
            } => {
                // First add: bump Off → Enforce so the list is actually
                // applied. Mirrors the `lmao trust add` CLI behaviour.
                let (current_mode, before_count) = {
                    let snap = self.node.trust_snapshot();
                    (snap.0, snap.1.len())
                };
                if matches!(current_mode, TrustMode::Off) && before_count == 0 {
                    self.node.trust_set_mode(TrustMode::Enforce);
                }
                self.node.trust_add(TrustEntry {
                    pubkey: pubkey.clone(),
                    nickname: nickname.clone(),
                    capabilities,
                    notes,
                    added_at: SystemTime::now(),
                    encryption_pubkey,
                });
                let persisted = self.persist_trust();
                Ok(Response::TrustAdd {
                    pubkey,
                    nickname,
                    persisted,
                })
            }
            Request::TrustRemove { target } => {
                let dropped = self
                    .node
                    .trust_remove(&target)
                    .ok_or_else(|| anyhow!("no entry matched {target}"))?;
                let persisted = self.persist_trust();
                Ok(Response::TrustRemove {
                    pubkey: dropped.pubkey,
                    nickname: dropped.nickname,
                    persisted,
                })
            }
            Request::TrustMode { mode } => {
                let previous = self.node.trust_mode();
                let next = match mode.as_deref() {
                    None => previous,
                    Some(s) => parse_trust_mode(s)?,
                };
                let persisted = if next != previous {
                    self.node.trust_set_mode(next);
                    self.persist_trust()
                } else {
                    false
                };
                Ok(Response::TrustMode {
                    previous: mode_to_str(previous).to_string(),
                    current: mode_to_str(next).to_string(),
                    persisted,
                })
            }
            Request::TaskHistoryList {
                limit,
                offset,
                direction,
                capability,
                since_ms,
            } => {
                let Some(history) = self.node.history() else {
                    return Ok(Response::TaskHistoryList {
                        entries: Vec::new(),
                        history_path: None,
                    });
                };
                let filter = logos_messaging_a2a_node::history::HistoryFilter {
                    direction,
                    capability,
                    since_ms,
                };
                let limit = limit.unwrap_or(100).min(10_000);
                let offset = offset.unwrap_or(0);
                let entries = history
                    .list(limit, offset, &filter)
                    .await
                    .map_err(|e| anyhow::anyhow!("history list failed: {e}"))?
                    .into_iter()
                    .map(history_to_wire)
                    .collect();
                Ok(Response::TaskHistoryList {
                    entries,
                    history_path: Some(history.path().to_path_buf()),
                })
            }
            Request::TaskHistoryGet { task_id } => {
                let Some(history) = self.node.history() else {
                    return Ok(Response::TaskHistoryGet { entry: None });
                };
                let entry = history
                    .get(&task_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("history get failed: {e}"))?
                    .map(history_to_wire);
                Ok(Response::TaskHistoryGet { entry })
            }
            Request::Shutdown => {
                // Reply first; the spawned task will exit the process
                // after we return. The accept loop will then exit on
                // the next iteration when the socket closes.
                tokio::spawn(async {
                    // Give the response a moment to flush over the socket
                    // before the process exits.
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                    std::process::exit(0);
                });
                Ok(Response::ShutdownAck)
            }
        }
    }
}

fn history_to_wire(
    e: logos_messaging_a2a_node::history::HistoryEntry,
) -> super::protocol::HistoryEntryWire {
    super::protocol::HistoryEntryWire {
        task_id: e.task_id,
        parent_id: e.parent_id,
        created_at_ms: e.created_at_ms,
        direction: e.direction,
        peer_pubkey: e.peer_pubkey,
        peer_name: e.peer_name,
        capability: e.capability,
        text: e.text,
        body: e.body,
        cid: e.cid,
        success: e.success,
        error: e.error,
        elapsed_ms: e.elapsed_ms,
        session_id: e.session_id,
    }
}

fn mode_to_str(mode: TrustMode) -> &'static str {
    match mode {
        TrustMode::Off => "off",
        TrustMode::Enforce => "enforce",
        TrustMode::Log => "log",
    }
}

fn parse_trust_mode(s: &str) -> Result<TrustMode> {
    match s.to_ascii_lowercase().as_str() {
        "off" => Ok(TrustMode::Off),
        "enforce" => Ok(TrustMode::Enforce),
        "log" => Ok(TrustMode::Log),
        other => Err(anyhow!(
            "unknown trust mode: {other} (expected off|enforce|log)"
        )),
    }
}

fn build_strategy(
    to: Option<&str>,
    capability: Option<&str>,
    strategy_name: Option<&str>,
) -> DelegationStrategy {
    if to.is_some() {
        // Direct delegation is dispatched separately (see `delegate_direct`
        // call site). The strategy field on the request is unused in that
        // path; we still return *some* value here to satisfy the type.
        return DelegationStrategy::FirstAvailable;
    }
    match strategy_name {
        Some("round-robin") => DelegationStrategy::RoundRobin,
        Some("broadcast") => DelegationStrategy::BroadcastCollect,
        Some("first-available") => DelegationStrategy::FirstAvailable,
        _ => match capability {
            Some(c) => DelegationStrategy::CapabilityMatch {
                capability: c.to_string(),
            },
            None => DelegationStrategy::FirstAvailable,
        },
    }
}

#[cfg(unix)]
fn set_socket_perms(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = std::fs::Permissions::from_mode(0o600);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
fn set_socket_perms(_path: &Path) -> std::io::Result<()> {
    Ok(())
}
