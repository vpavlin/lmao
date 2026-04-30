//! Friend-keyring trust list — SSH `known_hosts` for LMAO agents.
//!
//! A local TOML file enumerates the pubkeys this operator is willing to
//! talk to, optionally restricted to specific capabilities. The trust
//! list is consulted at two points in the node:
//!
//! - **Outgoing**: `delegate_task` peer selection only considers peers
//!   whose pubkey is in the list (and, for capability-matched
//!   delegation, who are trusted *for that capability*).
//! - **Incoming**: `poll_tasks` filters tasks by `task.from`. In
//!   [`TrustMode::Enforce`] untrusted senders are dropped silently;
//!   in [`TrustMode::Log`] they are surfaced but logged for triage;
//!   in [`TrustMode::Off`] no filtering happens at all (current default
//!   for unconfigured nodes).
//!
//! See `docs/TRUST.md` for the design discussion and rationale.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Trust enforcement mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrustMode {
    /// No filtering — every sender is trusted, every peer is a delegation
    /// candidate. Default for legacy / unconfigured nodes so behaviour is
    /// unchanged when no trust file exists.
    Off,
    /// Drop untrusted senders silently; only consider trusted peers for
    /// delegation. The "I know what I'm doing" mode.
    Enforce,
    /// Accept everything but log untrusted senders. Useful while building
    /// up the trust list — you can see who's trying to talk to you and
    /// decide whether to add them.
    Log,
}

impl Default for TrustMode {
    fn default() -> Self {
        Self::Off
    }
}

/// One trusted peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustEntry {
    /// secp256k1 compressed-hex pubkey, same format as `AgentCard.public_key`.
    pub pubkey: String,
    /// Human-readable name for CLI / UI surfaces.
    pub nickname: String,
    /// Capabilities this peer is trusted for. **Empty list trusts the
    /// peer for any capability.**
    #[serde(default)]
    pub capabilities: Vec<String>,
    /// Free-form note ("met at ETHPrague 2026", "alice's laptop", …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// Wall-clock time the entry was added. Used for sort/audit.
    #[serde(default = "SystemTime::now")]
    pub added_at: SystemTime,
}

/// On-disk schema. Kept separate from [`TrustList`] so the public API
/// can stay opinionated about lookups while the file format stays
/// straightforward TOML.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TrustFile {
    #[serde(default)]
    mode: TrustMode,
    #[serde(default, rename = "peer")]
    peers: Vec<TrustEntry>,
}

/// Loaded trust list. Lookups are O(log n) over the entry map, but n is
/// expected to be small (tens to low hundreds), so this is plenty.
#[derive(Debug, Clone, Default)]
pub struct TrustList {
    mode: TrustMode,
    entries: BTreeMap<String, TrustEntry>,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("io error reading trust file {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parse error in trust file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("serialize error: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("duplicate pubkey: {0}")]
    DuplicatePubkey(String),
}

impl TrustList {
    /// Empty list in [`TrustMode::Off`] — equivalent to "no trust file".
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build a list at a specific mode (mostly useful in tests).
    pub fn with_mode(mode: TrustMode) -> Self {
        Self {
            mode,
            entries: BTreeMap::new(),
        }
    }

    pub fn mode(&self) -> TrustMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: TrustMode) {
        self.mode = mode;
    }

    /// Load from disk. **A missing file is not an error** — it returns
    /// an empty list in [`TrustMode::Off`], which means "behave as if no
    /// trust list were configured" (today's default).
    pub fn load_from(path: &Path) -> Result<Self, TrustError> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::from_toml_str(&text).map_err(|e| TrustError::Parse {
                path: path.to_path_buf(),
                source: e,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(e) => Err(TrustError::Io {
                path: path.to_path_buf(),
                source: e,
            }),
        }
    }

    /// Parse from a TOML string. Useful for tests and `lmao trust import`
    /// when reading from stdin.
    pub fn from_toml_str(text: &str) -> Result<Self, toml::de::Error> {
        let file: TrustFile = toml::from_str(text)?;
        let mut entries = BTreeMap::new();
        for entry in file.peers {
            entries.insert(entry.pubkey.clone(), entry);
        }
        Ok(Self {
            mode: file.mode,
            entries,
        })
    }

    /// Serialise to a TOML string in canonical form. Sorted by pubkey so
    /// VCS diffs are stable.
    pub fn to_toml_string(&self) -> Result<String, toml::ser::Error> {
        let file = TrustFile {
            mode: self.mode,
            peers: self.entries.values().cloned().collect(),
        };
        toml::to_string_pretty(&file)
    }

    /// Write the trust list to disk, creating parent directories if
    /// necessary. Tightens permissions to 0600 on Unix so other users
    /// on a shared host can't read who you trust.
    pub fn save_to(&self, path: &Path) -> Result<(), TrustError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|e| TrustError::Io {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
            }
        }
        let text = self.to_toml_string()?;
        std::fs::write(path, text).map_err(|e| TrustError::Io {
            path: path.to_path_buf(),
            source: e,
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
        Ok(())
    }

    /// `$XDG_CONFIG_HOME/lmao/trust.toml`, falling back to
    /// `$HOME/.config/lmao/trust.toml`, finally `./trust.toml` if neither
    /// env var is set.
    pub fn default_path() -> PathBuf {
        if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
            return PathBuf::from(d).join("lmao").join("trust.toml");
        }
        if let Ok(h) = std::env::var("HOME") {
            return PathBuf::from(h)
                .join(".config")
                .join("lmao")
                .join("trust.toml");
        }
        PathBuf::from("trust.toml")
    }

    /// Is this pubkey trusted for *any* capability? In [`TrustMode::Off`]
    /// returns true unconditionally.
    pub fn is_trusted(&self, pubkey: &str) -> bool {
        if matches!(self.mode, TrustMode::Off) {
            return true;
        }
        self.entries.contains_key(pubkey)
    }

    /// Is this pubkey trusted for the named capability? An entry with an
    /// empty `capabilities` list trusts the peer for *any* capability;
    /// a non-empty list trusts only the listed ones. In [`TrustMode::Off`]
    /// returns true unconditionally.
    pub fn is_trusted_for(&self, pubkey: &str, capability: &str) -> bool {
        if matches!(self.mode, TrustMode::Off) {
            return true;
        }
        match self.entries.get(pubkey) {
            None => false,
            Some(entry) => {
                entry.capabilities.is_empty() || entry.capabilities.iter().any(|c| c == capability)
            }
        }
    }

    /// Insert (or replace) an entry. The pubkey is the key, so adding
    /// a second entry with the same pubkey overwrites silently.
    pub fn add(&mut self, entry: TrustEntry) {
        self.entries.insert(entry.pubkey.clone(), entry);
    }

    /// Insert; error out if a pubkey already exists. Useful for
    /// `lmao trust import` where silent overwrites would be surprising.
    pub fn add_unique(&mut self, entry: TrustEntry) -> Result<(), TrustError> {
        if self.entries.contains_key(&entry.pubkey) {
            return Err(TrustError::DuplicatePubkey(entry.pubkey));
        }
        self.entries.insert(entry.pubkey.clone(), entry);
        Ok(())
    }

    pub fn remove(&mut self, pubkey: &str) -> Option<TrustEntry> {
        self.entries.remove(pubkey)
    }

    /// Remove by nickname (case-sensitive exact match). Returns None if
    /// no entry has that nickname; useful for `lmao trust remove alice`.
    pub fn remove_by_nickname(&mut self, nickname: &str) -> Option<TrustEntry> {
        let pubkey = self
            .entries
            .iter()
            .find(|(_, e)| e.nickname == nickname)
            .map(|(k, _)| k.clone())?;
        self.entries.remove(&pubkey)
    }

    pub fn iter(&self) -> impl Iterator<Item = &TrustEntry> {
        self.entries.values()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Merge another list into this one. Existing pubkeys are
    /// **preserved** — incoming entries with the same pubkey are
    /// dropped. Used by `lmao trust import`.
    pub fn merge(&mut self, other: TrustList) -> usize {
        let mut added = 0;
        for entry in other.entries.into_values() {
            if !self.entries.contains_key(&entry.pubkey) {
                self.entries.insert(entry.pubkey.clone(), entry);
                added += 1;
            }
        }
        added
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn entry(pubkey: &str, nickname: &str, caps: &[&str]) -> TrustEntry {
        TrustEntry {
            pubkey: pubkey.into(),
            nickname: nickname.into(),
            capabilities: caps.iter().map(|s| s.to_string()).collect(),
            notes: None,
            added_at: SystemTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn off_mode_trusts_everyone() {
        let list = TrustList::with_mode(TrustMode::Off);
        assert!(list.is_trusted("anything"));
        assert!(list.is_trusted_for("anything", "code"));
    }

    #[test]
    fn enforce_mode_rejects_unknown_pubkeys() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(entry("02ab", "alice", &[]));
        assert!(list.is_trusted("02ab"));
        assert!(!list.is_trusted("03cd"));
    }

    #[test]
    fn empty_capability_list_trusts_for_any_capability() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(entry("02ab", "alice", &[]));
        assert!(list.is_trusted_for("02ab", "code"));
        assert!(list.is_trusted_for("02ab", "anything-else"));
    }

    #[test]
    fn non_empty_capability_list_scopes_trust() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(entry("02ab", "alice", &["code", "review"]));
        assert!(list.is_trusted_for("02ab", "code"));
        assert!(list.is_trusted_for("02ab", "review"));
        assert!(!list.is_trusted_for("02ab", "summarize"));
        // is_trusted (no capability scope) is true as long as the pubkey
        // is present, regardless of capability list.
        assert!(list.is_trusted("02ab"));
    }

    #[test]
    fn toml_round_trip_preserves_entries_and_mode() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(TrustEntry {
            pubkey: "02ab".into(),
            nickname: "alice".into(),
            capabilities: vec!["code".into()],
            notes: Some("met at ETHPrague".into()),
            added_at: SystemTime::UNIX_EPOCH,
        });
        list.add(entry("03cd", "bob", &["text"]));

        let text = list.to_toml_string().unwrap();
        let parsed = TrustList::from_toml_str(&text).unwrap();

        assert_eq!(parsed.mode(), TrustMode::Enforce);
        assert_eq!(parsed.len(), 2);
        assert!(parsed.is_trusted_for("02ab", "code"));
        assert!(parsed.is_trusted_for("03cd", "text"));
        assert_eq!(
            parsed.entries.get("02ab").unwrap().notes.as_deref(),
            Some("met at ETHPrague")
        );
    }

    #[test]
    fn save_then_load_preserves_state() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested").join("trust.toml");

        let mut list = TrustList::with_mode(TrustMode::Log);
        list.add(entry("02ab", "alice", &["code"]));
        list.save_to(&path).unwrap();

        let loaded = TrustList::load_from(&path).unwrap();
        assert_eq!(loaded.mode(), TrustMode::Log);
        assert!(loaded.is_trusted_for("02ab", "code"));
    }

    #[test]
    fn missing_file_loads_as_off_mode_empty_list() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("does-not-exist.toml");
        let loaded = TrustList::load_from(&path).unwrap();
        assert_eq!(loaded.mode(), TrustMode::Off);
        assert!(loaded.is_empty());
        // And — critically — it trusts everything, so nodes without a
        // trust file behave exactly as before.
        assert!(loaded.is_trusted("any-pubkey"));
    }

    #[test]
    fn malformed_toml_returns_parse_error_with_path() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("trust.toml");
        std::fs::write(&path, "this is not valid toml = =").unwrap();
        let err = TrustList::load_from(&path).unwrap_err();
        match err {
            TrustError::Parse { path: p, .. } => assert_eq!(p, path),
            other => panic!("expected Parse error, got {other:?}"),
        }
    }

    #[test]
    fn add_unique_rejects_duplicate_pubkey() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(entry("02ab", "alice", &[]));
        let err = list.add_unique(entry("02ab", "alice2", &[])).unwrap_err();
        assert!(matches!(err, TrustError::DuplicatePubkey(_)));
    }

    #[test]
    fn remove_by_nickname_returns_dropped_entry() {
        let mut list = TrustList::with_mode(TrustMode::Enforce);
        list.add(entry("02ab", "alice", &[]));
        list.add(entry("03cd", "bob", &[]));
        let dropped = list.remove_by_nickname("alice").unwrap();
        assert_eq!(dropped.pubkey, "02ab");
        assert_eq!(list.len(), 1);
        assert!(list.is_trusted("03cd"));
        assert!(!list.is_trusted("02ab"));
    }

    #[test]
    fn merge_preserves_existing_entries_on_conflict() {
        let mut a = TrustList::with_mode(TrustMode::Enforce);
        a.add(entry("02ab", "alice-canonical", &["code"]));

        let mut b = TrustList::with_mode(TrustMode::Enforce);
        b.add(entry("02ab", "alice-from-import", &["text"])); // conflicts
        b.add(entry("03cd", "bob", &[]));

        let added = a.merge(b);
        assert_eq!(added, 1); // only bob was new
        assert_eq!(a.len(), 2);
        // Existing alice entry is preserved — capabilities still ["code"].
        assert!(a.is_trusted_for("02ab", "code"));
        assert!(!a.is_trusted_for("02ab", "text"));
    }

    #[test]
    fn default_path_honours_xdg_config_home() {
        // SAFETY: cargo test runs single-threaded by default; restoring
        // the previous value keeps other tests deterministic.
        let prev = std::env::var("XDG_CONFIG_HOME").ok();
        unsafe {
            std::env::set_var("XDG_CONFIG_HOME", "/run/user/1000/config");
        }
        assert_eq!(
            TrustList::default_path(),
            PathBuf::from("/run/user/1000/config/lmao/trust.toml")
        );
        unsafe {
            match prev {
                Some(v) => std::env::set_var("XDG_CONFIG_HOME", v),
                None => std::env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }
}
