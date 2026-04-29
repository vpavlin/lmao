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
    },
    /// Fetch raw bytes by CID from the daemon's storage backend, if
    /// configured.
    StorageFetch { cid: String },
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
pub struct DelegationWire {
    pub parent_task_id: String,
    pub subtask_id: String,
    pub agent_id: String,
    pub success: bool,
    pub result_text: Option<String>,
    pub error: Option<String>,
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
        return PathBuf::from(h).join(".cache").join("lmao").join("lmao.sock");
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

    #[test]
    fn default_socket_uses_xdg_runtime_dir() {
        // SAFETY: tests are single-threaded by default in cargo test;
        // restoring the env afterwards keeps other tests deterministic.
        let prev = std::env::var("XDG_RUNTIME_DIR").ok();
        unsafe {
            std::env::set_var("XDG_RUNTIME_DIR", "/run/user/1000");
        }
        assert_eq!(default_socket_path(), PathBuf::from("/run/user/1000/lmao.sock"));
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_RUNTIME_DIR", v),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }
}
