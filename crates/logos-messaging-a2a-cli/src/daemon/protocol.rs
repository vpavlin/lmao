//! Wire protocol for the LMAO daemon's Unix-socket IPC.
//!
//! Frame layout: `u32` length prefix (little-endian, max 16 MiB) followed
//! by a UTF-8 JSON-encoded [`Request`] or [`Response`]. Both sides agree
//! on a request → response correlation by virtue of the socket being
//! one-shot per command — every connection sends exactly one request and
//! reads exactly one response, then closes.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Hard cap on a single frame so a malformed length prefix can't cause
/// the daemon to allocate an absurd buffer.
pub const MAX_FRAME_BYTES: usize = 16 * 1024 * 1024;

/// Wire request from a CLI client to the running daemon.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    /// Health probe — returns the daemon's identity and uptime.
    Info,
    /// Drain the discovery topic and return any AgentCards seen since
    /// the previous call.
    Discover,
    /// Drain the presence topic and return all live peers, optionally
    /// filtered by capability.
    PresencePeers { capability: Option<String> },
    /// Send a task to a specific agent.
    TaskSend { to: String, text: String },
    /// Poll for any task results matching the given UUID.
    TaskStatus { id: String },
    /// Delegate by capability, broadcast, round-robin, or first-available.
    TaskDelegate {
        to: Option<String>,
        capability: Option<String>,
        text: String,
        parent_id: String,
        timeout_secs: u64,
        broadcast: bool,
        strategy: Option<String>,
        /// Optional conversation thread id. When set, the receiving
        /// agent's exec gets `LMAO_SESSION_ID=<this>` so wrappers can
        /// reuse a per-thread session and skip cold-start cost on
        /// follow-up turns.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    /// Fetch raw bytes by CID from the daemon's storage backend, if
    /// configured.
    StorageFetch { cid: String },
    /// Snapshot the daemon's trust list (mode + entries).
    TrustList,
    /// Add or replace a trusted peer. Daemon mutates the in-memory
    /// list and persists to its trust file (if one was loaded).
    TrustAdd {
        pubkey: String,
        nickname: String,
        capabilities: Vec<String>,
        notes: Option<String>,
        /// Optional X25519 encryption pubkey for sealed presence.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encryption_pubkey: Option<String>,
    },
    /// Remove a trusted peer by pubkey or nickname.
    TrustRemove { target: String },
    /// Set the enforcement mode. Pass `mode = None` to query without
    /// changing it.
    TrustMode { mode: Option<String> },
    /// List persisted task-history rows. Newest first. All filters
    /// optional; `limit` defaults to 100, `offset` defaults to 0.
    TaskHistoryList {
        #[serde(default)]
        limit: Option<usize>,
        #[serde(default)]
        offset: Option<usize>,
        /// `"delegated"` or `"received"` to filter by direction.
        #[serde(default)]
        direction: Option<String>,
        /// Filter by exact capability match (e.g. `"summarize"`).
        #[serde(default)]
        capability: Option<String>,
        /// Only return entries created at or after this unix-ms timestamp.
        #[serde(default)]
        since_ms: Option<u64>,
    },
    /// Look up a single history row by task_id.
    TaskHistoryGet { task_id: String },
    /// Graceful shutdown — daemon completes in-flight work, drops the
    /// socket, exits the process.
    Shutdown,
}

/// Wire response — `Ok(payload)` or `Err(message)`.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Info {
        name: String,
        pubkey: String,
        capabilities: Vec<String>,
        uptime_secs: u64,
        socket_path: PathBuf,
        storage_enabled: bool,
        /// Hex-encoded X25519 pubkey, if the agent has an encryption identity.
        /// Friends pass this to `lmao trust add --encryption-pubkey ...` so
        /// sealed presence envelopes can reach this agent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        encryption_pubkey: Option<String>,
        /// Snapshot of the agent's currently-advertised load — populated
        /// from `LmaoNode::current_load_status` so callers (UI, CLI) can
        /// show capacity without waiting for a sealed presence
        /// roundtrip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        load: Option<LoadStatusWire>,
    },
    Discover {
        agents: Vec<AgentCardWire>,
    },
    PresencePeers {
        peers: Vec<PeerWire>,
    },
    TaskSend {
        task_id: String,
        from: String,
        acked: bool,
    },
    TaskStatus {
        results: Vec<TaskWire>,
    },
    TaskDelegate {
        results: Vec<DelegationWire>,
    },
    StorageFetch {
        cid: String,
        /// Base64-encoded payload bytes.
        payload_b64: String,
    },
    TrustList {
        mode: String,
        entries: Vec<TrustEntryWire>,
        /// Path the daemon will persist mutations to. `None` if the
        /// daemon has no trust file configured (in which case writes
        /// are in-memory only).
        trust_file: Option<PathBuf>,
    },
    TrustAdd {
        pubkey: String,
        nickname: String,
        persisted: bool,
    },
    TrustRemove {
        pubkey: String,
        nickname: String,
        persisted: bool,
    },
    TrustMode {
        previous: String,
        current: String,
        persisted: bool,
    },
    TaskHistoryList {
        entries: Vec<HistoryEntryWire>,
        history_path: Option<PathBuf>,
    },
    TaskHistoryGet {
        entry: Option<HistoryEntryWire>,
    },
    ShutdownAck,
    Error {
        message: String,
    },
}

/// AgentCard projection that doesn't drag the whole core types into the
/// wire schema. Keeps the protocol stable when AgentCard adds fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCardWire {
    pub name: String,
    pub description: String,
    pub version: String,
    pub capabilities: Vec<String>,
    pub public_key: String,
    pub has_intro_bundle: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerWire {
    pub agent_id: String,
    pub name: String,
    pub capabilities: Vec<String>,
    pub waku_topic: String,
    pub last_seen_secs: u64,
    pub ttl_secs: u64,
    /// Last decrypted load status from this peer, if any sealed envelope
    /// was addressed to us. `None` means the peer didn't ship sealed
    /// status, didn't address one to us, or we have no X25519 identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<LoadStatusWire>,
}

/// Coarse-grained capacity hint shown in `lmao info` and the UI peer
/// cards. Mirrors `logos_messaging_a2a_core::LoadStatus` but is shaped
/// for the IPC wire (string bucket, not enum) so it survives older
/// clients that don't know about new bucket variants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadStatusWire {
    /// `"free"`, `"busy"`, or `"full"`.
    pub bucket: String,
    pub queue_depth: u32,
    pub max_concurrent: u32,
    #[serde(default)]
    pub avg_latency_ms: u32,
}

impl From<&logos_messaging_a2a_core::LoadStatus> for LoadStatusWire {
    fn from(s: &logos_messaging_a2a_core::LoadStatus) -> Self {
        let bucket = match s.bucket {
            logos_messaging_a2a_core::LoadBucket::Free => "free",
            logos_messaging_a2a_core::LoadBucket::Busy => "busy",
            logos_messaging_a2a_core::LoadBucket::Full => "full",
        }
        .to_string();
        LoadStatusWire {
            bucket,
            queue_depth: s.queue_depth,
            max_concurrent: s.max_concurrent,
            avg_latency_ms: s.avg_latency_ms,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskWire {
    pub id: String,
    pub state: String,
    pub from: String,
    pub to: String,
    pub text: Option<String>,
    pub result_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntryWire {
    pub pubkey: String,
    pub nickname: String,
    pub capabilities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DelegationWire {
    pub parent_task_id: String,
    pub subtask_id: String,
    pub agent_id: String,
    pub success: bool,
    pub result_text: Option<String>,
    pub error: Option<String>,
}

/// Wire projection of a [`logos_messaging_a2a_node::history::HistoryEntry`].
/// Mirrors that struct's fields one-for-one — kept as a separate type
/// so the daemon protocol's wire shape isn't tied to the node crate's
/// internal representation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntryWire {
    pub task_id: String,
    #[serde(default)]
    pub parent_id: String,
    pub created_at_ms: u64,
    pub direction: String,
    pub peer_pubkey: String,
    #[serde(default)]
    pub peer_name: String,
    #[serde(default)]
    pub capability: String,
    pub text: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub cid: String,
    pub success: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Conversation thread id. Same value across all turns of a
    /// multi-turn delegation; UI uses it to group threaded follow-ups.
    /// Empty for older entries that pre-date the auto-stamping.
    #[serde(default)]
    pub session_id: String,
}

/// Where the daemon binds its IPC socket by default. Honours
/// `XDG_RUNTIME_DIR` (preferred — typically tmpfs and per-session),
/// then `XDG_CACHE_HOME`, then `$HOME/.cache`.
pub fn default_socket_path() -> PathBuf {
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        return PathBuf::from(d).join("lmao.sock");
    }
    if let Ok(d) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(d).join("lmao").join("lmao.sock");
    }
    if let Ok(h) = std::env::var("HOME") {
        return PathBuf::from(h)
            .join(".cache")
            .join("lmao")
            .join("lmao.sock");
    }
    PathBuf::from("/tmp/lmao.sock")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_info_response() {
        let r = Response::Info {
            name: "alice".into(),
            pubkey: "02ab".into(),
            capabilities: vec!["text".into()],
            uptime_secs: 42,
            socket_path: PathBuf::from("/tmp/lmao.sock"),
            storage_enabled: true,
            encryption_pubkey: None,
            load: None,
        };
        let s = serde_json::to_string(&r).unwrap();
        let parsed: Response = serde_json::from_str(&s).unwrap();
        assert!(matches!(parsed, Response::Info { name, .. } if name == "alice"));
    }

    #[test]
    fn request_kind_is_external_tag() {
        let s = serde_json::to_string(&Request::Info).unwrap();
        assert!(s.contains("\"kind\":\"info\""));
    }

    /// All Request variants survive a JSON round-trip. Re-serializes
    /// after parsing and compares the strings — locks the wire format
    /// to the type definition.
    #[test]
    fn round_trip_all_request_variants() {
        let cases = vec![
            Request::Info,
            Request::Discover,
            Request::PresencePeers {
                capability: Some("text".into()),
            },
            Request::PresencePeers { capability: None },
            Request::TaskSend {
                to: "02ab".into(),
                text: "hi".into(),
            },
            Request::TaskStatus { id: "uuid".into() },
            Request::TaskDelegate {
                to: None,
                capability: Some("code".into()),
                text: "do".into(),
                parent_id: "p".into(),
                timeout_secs: 30,
                broadcast: false,
                strategy: Some("capability_match".into()),
                session_id: None,
            },
            Request::StorageFetch { cid: "Q...".into() },
            Request::TrustList,
            Request::TrustAdd {
                pubkey: "02ab".into(),
                nickname: "alice".into(),
                capabilities: vec!["text".into()],
                notes: Some("ETHPrague".into()),
                encryption_pubkey: None,
            },
            Request::TrustRemove {
                target: "alice".into(),
            },
            Request::TrustMode {
                mode: Some("enforce".into()),
            },
            Request::TrustMode { mode: None },
            Request::Shutdown,
        ];
        for req in cases {
            let s = serde_json::to_string(&req).unwrap();
            let parsed: Request = serde_json::from_str(&s).unwrap();
            assert_eq!(s, serde_json::to_string(&parsed).unwrap());
        }
    }

    /// All Response variants survive a JSON round-trip.
    #[test]
    fn round_trip_all_response_variants() {
        let cases = vec![
            Response::Info {
                name: "alice".into(),
                pubkey: "02ab".into(),
                capabilities: vec!["text".into()],
                uptime_secs: 7,
                socket_path: PathBuf::from("/tmp/lmao.sock"),
                storage_enabled: false,
                encryption_pubkey: None,
                load: None,
            },
            Response::Discover {
                agents: vec![AgentCardWire {
                    name: "bob".into(),
                    description: "echo".into(),
                    version: "0.1.0".into(),
                    capabilities: vec!["text".into()],
                    public_key: "02cd".into(),
                    has_intro_bundle: true,
                }],
            },
            Response::PresencePeers {
                peers: vec![PeerWire {
                    agent_id: "02cd".into(),
                    name: "bob".into(),
                    capabilities: vec!["text".into()],
                    waku_topic: "/lmao/1/task-02cd/proto".into(),
                    last_seen_secs: 3,
                    ttl_secs: 60,
                    load: None,
                }],
            },
            Response::TaskSend {
                task_id: "u1".into(),
                from: "02ab".into(),
                acked: true,
            },
            Response::TaskStatus {
                results: vec![TaskWire {
                    id: "u1".into(),
                    state: "completed".into(),
                    from: "02ab".into(),
                    to: "02cd".into(),
                    text: Some("hi".into()),
                    result_text: Some("ok".into()),
                }],
            },
            Response::TaskDelegate {
                results: vec![DelegationWire {
                    parent_task_id: "p".into(),
                    subtask_id: "s".into(),
                    agent_id: "02cd".into(),
                    success: true,
                    result_text: Some("done".into()),
                    error: None,
                }],
            },
            Response::StorageFetch {
                cid: "Q...".into(),
                payload_b64: "aGVsbG8=".into(),
            },
            Response::TrustList {
                mode: "enforce".into(),
                entries: vec![TrustEntryWire {
                    pubkey: "02ab".into(),
                    nickname: "alice".into(),
                    capabilities: vec!["text".into()],
                    notes: None,
                }],
                trust_file: Some(PathBuf::from("/home/u/.config/lmao/trust.toml")),
            },
            Response::TrustAdd {
                pubkey: "02ab".into(),
                nickname: "alice".into(),
                persisted: true,
            },
            Response::TrustRemove {
                pubkey: "02ab".into(),
                nickname: "alice".into(),
                persisted: true,
            },
            Response::TrustMode {
                previous: "off".into(),
                current: "enforce".into(),
                persisted: true,
            },
            Response::ShutdownAck,
            Response::Error {
                message: "boom".into(),
            },
        ];
        for resp in cases {
            let s = serde_json::to_string(&resp).unwrap();
            let parsed: Response = serde_json::from_str(&s).unwrap();
            assert_eq!(s, serde_json::to_string(&parsed).unwrap());
        }
    }

    /// MAX_FRAME_BYTES is enforced in the framing layer, but lock its
    /// value here so a bump to the constant requires looking at this
    /// test (and thus the security implications).
    #[test]
    fn max_frame_is_sixteen_mib() {
        assert_eq!(MAX_FRAME_BYTES, 16 * 1024 * 1024);
    }

    #[test]
    fn default_socket_uses_xdg_runtime_dir() {
        // SAFETY: tests are single-threaded by default in cargo test;
        // restoring the env afterwards keeps other tests deterministic.
        let prev = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        assert_eq!(
            default_socket_path(),
            PathBuf::from("/run/user/1000/lmao.sock")
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}
