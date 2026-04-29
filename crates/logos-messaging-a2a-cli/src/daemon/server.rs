//! Daemon-side IPC server: listens on a Unix socket, dispatches incoming
//! [`Request`]s against a shared LmaoNode + storage backend.

use anyhow::{anyhow, Context, Result};
use logos_messaging_a2a_core::{DelegationRequest, DelegationStrategy, Task};
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_storage::StorageBackend;
use logos_messaging_a2a_transport::Transport;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use super::protocol::{
    AgentCardWire, DelegationWire, PeerWire, Request, Response, TaskWire, MAX_FRAME_BYTES,
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
}

impl DaemonServer {
    pub fn new(
        socket_path: PathBuf,
        node: Arc<LmaoNode<Arc<dyn Transport>>>,
        storage: Option<Arc<dyn StorageBackend>>,
        name: String,
    ) -> Self {
        Self {
            socket_path,
            node,
            storage,
            started_at: Instant::now(),
            name,
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

    async fn handle_connection(self: Arc<Self>, mut stream: UnixStream) -> Result<()> {
        let req = read_frame::<Request>(&mut stream).await?;
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
            } => {
                let strategy = build_strategy(to.as_deref(), capability.as_deref(), strategy.as_deref());
                let request = DelegationRequest {
                    parent_task_id: parent_id,
                    subtask_text: text,
                    strategy,
                    timeout_secs,
                };
                let results = if broadcast {
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

fn build_strategy(
    to: Option<&str>,
    capability: Option<&str>,
    strategy_name: Option<&str>,
) -> DelegationStrategy {
    if let Some(_pk) = to {
        // Direct delegation goes through `to` on the request — the
        // strategy field is unused. Use FirstAvailable as a no-op.
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
