//! Multi-turn conversation sessions between agents.

use serde::Serialize;
use std::time::{SystemTime, UNIX_EPOCH};

/// A multi-turn conversation session between two agents.
#[derive(Debug, Clone, Serialize)]
pub struct Session {
    /// Unique session identifier (UUID v4).
    pub id: String,
    /// Public key of the remote peer in this session.
    pub peer: String,
    /// Task IDs exchanged within this session, in chronological order.
    /// Bounded to the last `MAX_TASK_IDS` entries so a long-running
    /// conversation can't grow this vec without bound.
    pub task_ids: Vec<String>,
    /// Unix timestamp (seconds) when this session was created.
    pub created_at: u64,
    /// Unix timestamp (seconds) of the most recent task observed in
    /// this session. Used for idle-session eviction.
    pub last_seen: u64,
}

impl Session {
    /// Cap the per-session task_ids history. Beyond this we drop the
    /// oldest entries — task history lives in the JSONL log anyway.
    pub const MAX_TASK_IDS: usize = 64;

    pub(crate) fn new(peer: &str) -> Self {
        let id = uuid::Uuid::new_v4().to_string();
        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        Self {
            id,
            peer: peer.to_string(),
            task_ids: Vec::new(),
            created_at,
            last_seen: created_at,
        }
    }

    /// Append a task id to the session, capping length at
    /// [`MAX_TASK_IDS`](Self::MAX_TASK_IDS) and refreshing `last_seen`.
    pub(crate) fn push_task(&mut self, task_id: String) {
        if !self.task_ids.contains(&task_id) {
            self.task_ids.push(task_id);
            if self.task_ids.len() > Self::MAX_TASK_IDS {
                let drop = self.task_ids.len() - Self::MAX_TASK_IDS;
                self.task_ids.drain(..drop);
            }
        }
        self.last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
    }
}
