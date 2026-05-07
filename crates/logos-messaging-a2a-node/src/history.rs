//! Append-only JSONL task-history log.
//!
//! A daemon writes one row per task it sees: every delegation it sends
//! (success, error, or timeout) and every task it receives + responds to.
//! The CLI and Basecamp UI read this log to reconstruct the operator's
//! task history across daemon restarts.
//!
//! Format choice rationale: JSON-Lines (one JSON object per line) is
//! cheap to append, survives a partial write at the tail (the partial
//! line is just discarded on read), and is grepable with the usual
//! shell tools — useful for live debugging at the demo. We don't need
//! random-access reads or large-table queries, so SQLite would be
//! overkill for the volumes we care about (operator-scale, not
//! service-scale).
//!
//! The file lives at `<storage_dir>/history.jsonl` — co-located with
//! the libstorage data dir but outside the DHT subtree, so wiping
//! libstorage state doesn't blow away history (and vice versa).

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::fs::{File, OpenOptions};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;

/// One persisted task-history row.
///
/// Field naming matches the Basecamp QML model so the daemon → IPC →
/// QML pipe is a thin pass-through.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HistoryEntry {
    /// The subtask id (delegated) or the task's own id (received).
    pub task_id: String,
    /// Parent task id (delegated) or empty (received).
    #[serde(default)]
    pub parent_id: String,
    /// Unix milliseconds when the entry was written.
    pub created_at_ms: u64,
    /// `"delegated"` (we sent it out) or `"received"` (a peer sent it to us).
    pub direction: String,
    /// Counterparty pubkey (recipient for delegated, sender for received).
    pub peer_pubkey: String,
    /// Counterparty display name when known. Empty if unknown.
    #[serde(default)]
    pub peer_name: String,
    /// Capability requested (delegated) or empty (received).
    #[serde(default)]
    pub capability: String,
    /// Task input text.
    pub text: String,
    /// Task response text. Empty on timeout/error.
    #[serde(default)]
    pub body: String,
    /// Codex CID of the audit log, if storage is configured. Empty otherwise.
    #[serde(default)]
    pub cid: String,
    /// True if the task completed end-to-end.
    pub success: bool,
    /// Error message when `success == false`.
    #[serde(default)]
    pub error: Option<String>,
    /// End-to-end elapsed time (sender-side delegate roundtrip,
    /// receiver-side exec duration).
    #[serde(default)]
    pub elapsed_ms: u64,
    /// Conversation thread id. Tasks with the same `session_id` belong
    /// to the same multi-turn thread. Empty for single-shot delegations
    /// from clients that don't auto-stamp (older Basecamp builds, raw
    /// CLI without `--session-id`). Used by the UI to group threaded
    /// follow-ups visually.
    #[serde(default)]
    pub session_id: String,
}

/// Filters for `History::list`. All fields default to "no filter".
#[derive(Debug, Default, Clone)]
pub struct HistoryFilter {
    pub direction: Option<String>,
    pub capability: Option<String>,
    pub since_ms: Option<u64>,
}

/// Append-only history log. Cloneable (`Arc`-backed) so the daemon can
/// share one instance across delegate / respond / receive paths.
pub struct History {
    path: PathBuf,
    /// Serializes appends so concurrent delegates from independent
    /// tasks don't interleave half-lines into the file.
    write_lock: Mutex<()>,
}

impl History {
    /// Open (or create) a history log at the given path. The parent
    /// directory must already exist; the daemon's storage setup is
    /// responsible for that.
    pub fn open(path: impl AsRef<Path>) -> Arc<Self> {
        Arc::new(Self {
            path: path.as_ref().to_path_buf(),
            write_lock: Mutex::new(()),
        })
    }

    /// Path to the log file. Useful for IPC responses + tests.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one entry. Returns Ok even if the entry can't be
    /// flushed yet (buffered) — the next read will sync.
    pub async fn append(&self, entry: &HistoryEntry) -> std::io::Result<()> {
        let line = serde_json::to_string(entry).unwrap_or_default();
        let _g = self.write_lock.lock().await;
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .await?;
        f.write_all(line.as_bytes()).await?;
        f.write_all(b"\n").await?;
        f.flush().await?;
        Ok(())
    }

    /// Read everything in the log, applying optional filters. Newest
    /// entries first, capped at `limit` after filtering. `offset`
    /// skips entries from the newest end before applying limit.
    ///
    /// We load the whole file into memory and sort. For the operator-
    /// scale volumes this is fine — a typical daemon will have under
    /// a thousand entries even after weeks of use.
    pub async fn list(
        &self,
        limit: usize,
        offset: usize,
        filter: &HistoryFilter,
    ) -> std::io::Result<Vec<HistoryEntry>> {
        let f = match File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };

        let mut rdr = BufReader::new(f).lines();
        let mut all = Vec::new();
        while let Some(line) = rdr.next_line().await? {
            if line.trim().is_empty() {
                continue;
            }
            // Skip rows that fail to parse (partial trailing write,
            // forward-compat schema drift). Don't fail the whole
            // listing — operator should still see whatever's intact.
            let Ok(entry) = serde_json::from_str::<HistoryEntry>(&line) else {
                continue;
            };
            if let Some(d) = &filter.direction {
                if &entry.direction != d {
                    continue;
                }
            }
            if let Some(c) = &filter.capability {
                if &entry.capability != c {
                    continue;
                }
            }
            if let Some(s) = filter.since_ms {
                if entry.created_at_ms < s {
                    continue;
                }
            }
            all.push(entry);
        }

        all.sort_by_key(|e| std::cmp::Reverse(e.created_at_ms));
        Ok(all.into_iter().skip(offset).take(limit).collect())
    }

    /// Look up a single entry by task_id. Returns None if not found.
    pub async fn get(&self, task_id: &str) -> std::io::Result<Option<HistoryEntry>> {
        let f = match File::open(&self.path).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut rdr = BufReader::new(f).lines();
        let mut found: Option<HistoryEntry> = None;
        while let Some(line) = rdr.next_line().await? {
            let Ok(entry) = serde_json::from_str::<HistoryEntry>(line.trim()) else {
                continue;
            };
            if entry.task_id == task_id {
                // Keep scanning — last match wins, since later writes
                // (e.g. a "received" entry that gets a follow-up "responded"
                // update) may supersede earlier ones.
                found = Some(entry);
            }
        }
        Ok(found)
    }
}

/// Convenience: current time in unix milliseconds. Saturates to 0 if
/// the system clock is set before the unix epoch (it shouldn't be).
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(task_id: &str, direction: &str, ts: u64) -> HistoryEntry {
        HistoryEntry {
            task_id: task_id.into(),
            parent_id: String::new(),
            created_at_ms: ts,
            direction: direction.into(),
            peer_pubkey: "02ab".into(),
            peer_name: "alice".into(),
            capability: "text".into(),
            text: "hi".into(),
            body: "bye".into(),
            cid: String::new(),
            success: true,
            error: None,
            elapsed_ms: 42,
            session_id: String::new(),
        }
    }

    #[tokio::test]
    async fn append_and_list_returns_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path().join("h.jsonl"));
        h.append(&entry("a", "delegated", 100)).await.unwrap();
        h.append(&entry("b", "delegated", 300)).await.unwrap();
        h.append(&entry("c", "delegated", 200)).await.unwrap();

        let out = h.list(10, 0, &HistoryFilter::default()).await.unwrap();
        assert_eq!(
            out.iter().map(|e| e.task_id.as_str()).collect::<Vec<_>>(),
            vec!["b", "c", "a"]
        );
    }

    #[tokio::test]
    async fn list_filters_by_direction() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path().join("h.jsonl"));
        h.append(&entry("a", "delegated", 100)).await.unwrap();
        h.append(&entry("b", "received", 200)).await.unwrap();
        let f = HistoryFilter {
            direction: Some("received".into()),
            ..Default::default()
        };
        let out = h.list(10, 0, &f).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].task_id, "b");
    }

    #[tokio::test]
    async fn list_paginates_with_offset_and_limit() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path().join("h.jsonl"));
        for i in 0..5 {
            h.append(&entry(&format!("t{i}"), "delegated", i)).await.unwrap();
        }
        let out = h.list(2, 1, &HistoryFilter::default()).await.unwrap();
        // newest first: t4, t3, t2, t1, t0 — skip 1 → t3, take 2 → t3, t2
        assert_eq!(out.iter().map(|e| e.task_id.as_str()).collect::<Vec<_>>(), vec!["t3", "t2"]);
    }

    #[tokio::test]
    async fn get_returns_latest_match() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path().join("h.jsonl"));
        let mut a = entry("a", "received", 100);
        a.body = "first".into();
        h.append(&a).await.unwrap();
        let mut a2 = entry("a", "received", 200);
        a2.body = "second".into();
        h.append(&a2).await.unwrap();
        let got = h.get("a").await.unwrap().unwrap();
        assert_eq!(got.body, "second");
    }

    #[tokio::test]
    async fn missing_file_yields_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let h = History::open(dir.path().join("nonexistent.jsonl"));
        let out = h.list(10, 0, &HistoryFilter::default()).await.unwrap();
        assert!(out.is_empty());
    }

    #[tokio::test]
    async fn corrupt_line_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let h = History::open(&path);
        h.append(&entry("a", "delegated", 100)).await.unwrap();
        // Manually append a partial garbage line.
        tokio::fs::write(&path, b"{\"task_id\":\"a\",\"created_at_ms\":100,\"direction\":\"delegated\",\"peer_pubkey\":\"02ab\",\"text\":\"hi\",\"success\":true}\nNOT VALID JSON\n").await.unwrap();
        let out = h.list(10, 0, &HistoryFilter::default()).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].task_id, "a");
    }
}
