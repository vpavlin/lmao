//! Main A2A node implementation for the Logos Messaging protocol.
//!
//! This crate combines crypto ([`logos_messaging_a2a_crypto`]), transport
//! ([`logos_messaging_a2a_transport`]), storage ([`logos_messaging_a2a_storage`]),
//! and execution ([`logos_messaging_a2a_execution`]) into a single high-level
//! [`LmaoNode`] that can announce, discover, send/receive tasks, manage
//! sessions, stream responses, and handle payments over a Waku-compatible
//! transport layer.

pub mod delegation;
pub mod discovery;
pub mod history;
pub mod metrics;
pub mod payment;
pub mod presence;
pub mod retry;
pub mod session;
pub mod storage;
mod streaming;
mod tasks;

/// Errors that can occur in node operations.
#[derive(Debug, thiserror::Error)]
pub enum NodeError {
    /// An error originating from the transport layer.
    #[error("transport error: {0}")]
    Transport(#[from] logos_messaging_a2a_transport::TransportError),

    /// An error originating from the crypto layer.
    #[error("crypto error: {0}")]
    Crypto(#[from] logos_messaging_a2a_crypto::CryptoError),

    /// A JSON serialization or deserialization error.
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    /// An error originating from the execution backend.
    #[error("execution error: {0}")]
    Execution(#[from] logos_messaging_a2a_execution::ExecutionError),

    /// An error originating from presence verification.
    #[error("presence error: {0}")]
    Presence(#[from] logos_messaging_a2a_core::PresenceError),

    /// An I/O error (e.g. reading a keyfile).
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    /// A catch-all error with a freeform message.
    #[error("{0}")]
    Other(String),
}

/// Alias for results returned by node operations.
pub type Result<T> = std::result::Result<T, NodeError>;

use k256::ecdsa::SigningKey;
use logos_messaging_a2a_core::registry::AgentRegistry;
use logos_messaging_a2a_core::AgentCard;
use logos_messaging_a2a_core::RetryConfig;
/// Re-export of [`logos_messaging_a2a_core::Task`] for convenience.
pub use logos_messaging_a2a_core::Task as TaskType;
use logos_messaging_a2a_core::{TrustEntry, TrustList, TrustMode};
use logos_messaging_a2a_crypto::{AgentIdentity, IntroBundle};
use logos_messaging_a2a_transport::sds::{ChannelConfig, MessageChannel};
use logos_messaging_a2a_transport::Transport;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::sync::mpsc;

/// Re-export of [`session::Session`] for convenient access from crate root.
pub use session::Session;

/// Re-export of [`storage::StorageOffloadConfig`] for convenient access from crate root.
pub use storage::StorageOffloadConfig;

/// Re-export of [`payment::PaymentConfig`] for convenient access from crate root.
pub use payment::PaymentConfig;

/// Re-export of [`metrics::Metrics`] for convenient access from crate root.
pub use metrics::Metrics;

/// Re-export of [`metrics::MetricsSnapshot`] for convenient access from crate root.
pub use metrics::MetricsSnapshot;

/// A2A node: announce, discover, send/receive tasks over Waku.
///
/// Uses SDS MessageChannel for reliable, causally-ordered delivery with
/// bloom filter deduplication and implicit ACK via remote bloom filters.
pub struct LmaoNode<T: Transport> {
    /// This agent's public identity card, including name, capabilities, and public key.
    pub card: AgentCard,
    channel: MessageChannel<T>,
    signing_key: SigningKey,
    /// Optional X25519 identity for encrypted sessions.
    identity: Option<AgentIdentity>,
    /// Persistent subscription to this node's task topic (lazy-initialized).
    task_rx: tokio::sync::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// Persistent subscription to the discovery topic (lazy-initialized).
    /// Kept open between `discover()` calls so messages arriving on a
    /// real-network gossip mesh aren't missed in the gap between
    /// subscribe and unsubscribe.
    discover_rx: tokio::sync::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// Per-task persistent subscription to stream topics. Same rationale
    /// as `discover_rx` — real gossip transports don't buffer pre-subscribe.
    stream_rx: tokio::sync::Mutex<HashMap<String, mpsc::Receiver<Vec<u8>>>>,
    /// Active conversation sessions.
    sessions: std::sync::Mutex<HashMap<String, Session>>,
    /// Optional storage offload for large payloads.
    storage_offload: Option<StorageOffloadConfig>,
    /// Optional x402-style payment configuration.
    payment: Option<PaymentConfig>,
    /// Set of already-seen payment tx hashes to prevent replay attacks.
    seen_tx_hashes: std::sync::Mutex<HashSet<String>>,
    /// Live peer map built from presence announcements.
    peer_map: presence::PeerMap,
    /// Persistent subscription to the presence topic (lazy-initialized).
    presence_rx: tokio::sync::Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
    /// Optional persistent agent registry (e.g. LEZ on-chain).
    registry: Option<Arc<dyn AgentRegistry>>,
    /// Buffered stream chunks keyed by task_id, sorted by chunk_index.
    stream_chunks:
        std::sync::Mutex<HashMap<String, Vec<logos_messaging_a2a_core::TaskStreamChunk>>>,
    /// Optional retry configuration for exponential-backoff retries of
    /// transport send failures.
    retry_config: Option<RetryConfig>,
    /// Atomic counter for round-robin delegation peer selection.
    round_robin_counter: AtomicUsize,
    /// Operational metrics counters.
    metrics: Metrics,
    /// Friend-keyring filter applied at delegation peer selection and
    /// incoming-task acceptance. Default = empty list in `TrustMode::Off`,
    /// which matches pre-trust behaviour: no filtering at all.
    ///
    /// Wrapped in `Arc<RwLock<…>>` so the daemon's IPC handlers can
    /// mutate the list at runtime (Basecamp Trust pane, `lmao trust add`
    /// against a live agent) without restarting the agent. Reads are
    /// frequent (every poll_tasks / delegate_task call); writes are rare.
    trust_list: Arc<std::sync::RwLock<TrustList>>,
    /// Optional task-history log. When set, every delegation result and
    /// every received-task response gets persisted as a JSONL row, so
    /// the CLI / Basecamp UI can show history across daemon restarts.
    /// `None` for ephemeral nodes (tests, ad-hoc CLI calls without
    /// `agent run`).
    pub(crate) history: Option<Arc<history::History>>,
    /// In-flight task counter — incremented when accepting a task and
    /// decremented when the response is published. Drives the public
    /// `LoadBucket` shipped in sealed presence envelopes.
    in_flight: Arc<AtomicUsize>,
    /// Max concurrent tasks this agent will accept before reporting `Full`.
    /// Senders use this to route around saturated peers. `1` is the safe
    /// default for a single-process agent backing one external executor
    /// (the typical case for the demo).
    max_concurrent: u32,
}

impl<T: Transport> LmaoNode<T> {
    /// Create a new node with a random keypair (no encryption).
    pub fn new(name: &str, description: &str, capabilities: Vec<String>, transport: T) -> Self {
        let signing_key = SigningKey::random(&mut rand_core());
        let public_key = hex::encode(
            signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes(),
        );

        let card = AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            version: "0.1.0".to_string(),
            capabilities,
            public_key: public_key.clone(),
            intro_bundle: None,
        };

        Self {
            card,
            channel: MessageChannel::new(
                format!("node-{}", &public_key[..8]),
                public_key,
                transport,
            ),
            signing_key,
            identity: None,
            task_rx: tokio::sync::Mutex::new(None),
            discover_rx: tokio::sync::Mutex::new(None),
            stream_rx: tokio::sync::Mutex::new(HashMap::new()),
            sessions: std::sync::Mutex::new(HashMap::new()),
            storage_offload: None,
            payment: None,
            seen_tx_hashes: std::sync::Mutex::new(HashSet::new()),
            peer_map: presence::PeerMap::new(),
            presence_rx: tokio::sync::Mutex::new(None),
            registry: None,
            stream_chunks: std::sync::Mutex::new(HashMap::new()),
            retry_config: None,
            round_robin_counter: AtomicUsize::new(0),
            metrics: Metrics::new(),
            trust_list: Arc::new(std::sync::RwLock::new(TrustList::empty())),
            history: None,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_concurrent: 1,
        }
    }

    /// Create a new node with encryption enabled.
    pub fn new_encrypted(
        name: &str,
        description: &str,
        capabilities: Vec<String>,
        transport: T,
    ) -> Self {
        let signing_key = SigningKey::random(&mut rand_core());
        let public_key = hex::encode(
            signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes(),
        );

        let identity = AgentIdentity::generate();
        let intro_bundle = IntroBundle::new(&identity.public_key_hex());

        let card = AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            version: "0.1.0".to_string(),
            capabilities,
            public_key: public_key.clone(),
            intro_bundle: Some(intro_bundle),
        };

        Self {
            card,
            channel: MessageChannel::new(
                format!("node-{}", &public_key[..8]),
                public_key,
                transport,
            ),
            signing_key,
            identity: Some(identity),
            task_rx: tokio::sync::Mutex::new(None),
            discover_rx: tokio::sync::Mutex::new(None),
            stream_rx: tokio::sync::Mutex::new(HashMap::new()),
            sessions: std::sync::Mutex::new(HashMap::new()),
            storage_offload: None,
            payment: None,
            seen_tx_hashes: std::sync::Mutex::new(HashSet::new()),
            peer_map: presence::PeerMap::new(),
            presence_rx: tokio::sync::Mutex::new(None),
            registry: None,
            stream_chunks: std::sync::Mutex::new(HashMap::new()),
            retry_config: None,
            round_robin_counter: AtomicUsize::new(0),
            metrics: Metrics::new(),
            trust_list: Arc::new(std::sync::RwLock::new(TrustList::empty())),
            history: None,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_concurrent: 1,
        }
    }

    /// Create a node from an existing signing key (no encryption).
    pub fn from_key(
        name: &str,
        description: &str,
        capabilities: Vec<String>,
        transport: T,
        signing_key: SigningKey,
    ) -> Self {
        let public_key = hex::encode(
            signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes(),
        );

        let card = AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            version: "0.1.0".to_string(),
            capabilities,
            public_key: public_key.clone(),
            intro_bundle: None,
        };

        Self {
            card,
            channel: MessageChannel::new(
                format!("node-{}", &public_key[..8]),
                public_key,
                transport,
            ),
            signing_key,
            identity: None,
            task_rx: tokio::sync::Mutex::new(None),
            discover_rx: tokio::sync::Mutex::new(None),
            stream_rx: tokio::sync::Mutex::new(HashMap::new()),
            sessions: std::sync::Mutex::new(HashMap::new()),
            storage_offload: None,
            payment: None,
            seen_tx_hashes: std::sync::Mutex::new(HashSet::new()),
            peer_map: presence::PeerMap::new(),
            presence_rx: tokio::sync::Mutex::new(None),
            registry: None,
            stream_chunks: std::sync::Mutex::new(HashMap::new()),
            retry_config: None,
            round_robin_counter: AtomicUsize::new(0),
            metrics: Metrics::new(),
            trust_list: Arc::new(std::sync::RwLock::new(TrustList::empty())),
            history: None,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_concurrent: 1,
        }
    }

    /// Alias for [`from_key`](Self::from_key) — create a node from an existing signing key.
    pub fn new_with_key(
        name: &str,
        description: &str,
        capabilities: Vec<String>,
        transport: T,
        signing_key: SigningKey,
    ) -> Self {
        Self::from_key(name, description, capabilities, transport, signing_key)
    }

    /// Load a signing key from a file (hex-encoded 32 bytes), or generate and
    /// save one if the file does not exist. File is created with mode 0600.
    pub fn from_keyfile(
        name: &str,
        description: &str,
        capabilities: Vec<String>,
        transport: T,
        path: &Path,
    ) -> Result<Self> {
        let signing_key = if path.exists() {
            let hex_str = std::fs::read_to_string(path).map_err(|e| {
                NodeError::Other(format!("failed to read keyfile {}: {}", path.display(), e))
            })?;
            let bytes = hex::decode(hex_str.trim()).map_err(|e| {
                NodeError::Other(format!("invalid hex in keyfile {}: {}", path.display(), e))
            })?;
            if bytes.len() != 32 {
                return Err(NodeError::Other(format!(
                    "keyfile {} contains {} bytes, expected 32",
                    path.display(),
                    bytes.len()
                )));
            }
            SigningKey::from_bytes(bytes.as_slice().into()).map_err(|e| {
                NodeError::Other(format!("invalid signing key in {}: {}", path.display(), e))
            })?
        } else {
            let key = SigningKey::random(&mut rand_core());
            let hex_str = hex::encode(key.to_bytes());

            // Write atomically: create file, set perms, write content
            {
                use std::io::Write;
                let mut file = std::fs::File::create(path).map_err(|e| {
                    NodeError::Other(format!(
                        "failed to create keyfile {}: {}",
                        path.display(),
                        e
                    ))
                })?;

                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    file.set_permissions(std::fs::Permissions::from_mode(0o600))
                        .map_err(|e| {
                            NodeError::Other(format!(
                                "failed to set permissions on {}: {}",
                                path.display(),
                                e
                            ))
                        })?;
                }

                file.write_all(hex_str.as_bytes()).map_err(|e| {
                    NodeError::Other(format!("failed to write keyfile {}: {}", path.display(), e))
                })?;
            }

            key
        };

        Ok(Self::from_key(
            name,
            description,
            capabilities,
            transport,
            signing_key,
        ))
    }

    /// Create a node with custom SDS channel configuration.
    pub fn with_config(
        name: &str,
        description: &str,
        capabilities: Vec<String>,
        transport: T,
        config: ChannelConfig,
    ) -> Self {
        let signing_key = SigningKey::random(&mut rand_core());
        let public_key = hex::encode(
            signing_key
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes(),
        );

        let card = AgentCard {
            name: name.to_string(),
            description: description.to_string(),
            version: "0.1.0".to_string(),
            capabilities,
            public_key: public_key.clone(),
            intro_bundle: None,
        };

        Self {
            card,
            channel: MessageChannel::with_config(
                format!("node-{}", &public_key[..8]),
                public_key,
                transport,
                config,
            ),
            signing_key,
            identity: None,
            task_rx: tokio::sync::Mutex::new(None),
            discover_rx: tokio::sync::Mutex::new(None),
            stream_rx: tokio::sync::Mutex::new(HashMap::new()),
            sessions: std::sync::Mutex::new(HashMap::new()),
            storage_offload: None,
            payment: None,
            seen_tx_hashes: std::sync::Mutex::new(HashSet::new()),
            peer_map: presence::PeerMap::new(),
            presence_rx: tokio::sync::Mutex::new(None),
            registry: None,
            stream_chunks: std::sync::Mutex::new(HashMap::new()),
            retry_config: None,
            round_robin_counter: AtomicUsize::new(0),
            metrics: Metrics::new(),
            trust_list: Arc::new(std::sync::RwLock::new(TrustList::empty())),
            history: None,
            in_flight: Arc::new(AtomicUsize::new(0)),
            max_concurrent: 1,
        }
    }

    /// Enable CID-based offloading of large payloads to Logos Storage.
    ///
    /// When configured, payloads exceeding the threshold are automatically
    /// uploaded to storage. Only the CID is sent over Waku. Receivers with
    /// the same config auto-fetch the full payload by CID.
    pub fn with_storage_offload(mut self, config: StorageOffloadConfig) -> Self {
        self.storage_offload = Some(config);
        self
    }

    /// Enable x402-style payment flow via an [`ExecutionBackend`](logos_messaging_a2a_execution::ExecutionBackend).
    ///
    /// When configured, outgoing tasks can auto-pay and incoming tasks can
    /// require payment proof before processing.
    pub fn with_payment(mut self, config: PaymentConfig) -> Self {
        self.payment = Some(config);
        self
    }

    /// Enable exponential-backoff retry for transport send failures.
    ///
    /// When configured, `send_task` / `send_task_to` will retry on transport
    /// errors up to `config.max_attempts` times with exponential backoff.
    pub fn with_retry(mut self, config: RetryConfig) -> Self {
        self.retry_config = Some(config);
        self
    }

    /// Attach a persistent agent registry for on-chain discovery.
    ///
    /// When set, [`discover_all`](Self::discover_all) merges results from both
    /// Waku presence and the registry. The node can also
    /// [`register`](Self::register_in_registry) itself for permanent discovery.
    pub fn with_registry(mut self, registry: Arc<dyn AgentRegistry>) -> Self {
        self.registry = Some(registry);
        self
    }

    /// Attach a friend-keyring trust list. The list is consulted at two
    /// points: outgoing delegation (peer selection filters to trusted
    /// peers) and incoming task acceptance (untrusted senders are dropped
    /// in `TrustMode::Enforce`, surfaced-with-warning in `TrustMode::Log`).
    ///
    /// When the list is in `TrustMode::Off` (the default for unconfigured
    /// nodes) both filters are no-ops and behaviour is identical to a
    /// node without a trust list.
    pub fn with_trust_list(mut self, list: TrustList) -> Self {
        self.trust_list = Arc::new(std::sync::RwLock::new(list));
        self
    }

    /// Attach a persistent task-history log. Every delegation result
    /// and every received-task response will be appended as a JSONL
    /// row, so callers can reconstruct task history across daemon
    /// restarts (typically via the IPC `task_history_list` request).
    pub fn with_history(mut self, history: Arc<history::History>) -> Self {
        self.history = Some(history);
        self
    }

    /// Borrow the task-history log if one is attached. The daemon's
    /// IPC handlers use this to serve `task_history_list` and
    /// `task_history_get` requests; everything else uses the internal
    /// `record_history` helpers in the delegate / respond paths.
    pub fn history(&self) -> Option<&Arc<history::History>> {
        self.history.as_ref()
    }

    /// Configure how many tasks this agent will accept in parallel.
    /// Used both for backpressure (rejecting incoming tasks at capacity)
    /// and for the `LoadBucket` shipped in sealed presence envelopes.
    pub fn with_max_concurrent(mut self, n: u32) -> Self {
        self.max_concurrent = n.max(1);
        self
    }

    /// Inject a pre-existing X25519 identity. Used by daemon callers that
    /// want a stable encryption pubkey across restarts (the `agent run`
    /// command persists the X25519 secret in a `.x25519` sidecar next
    /// to the secp256k1 keyfile). Sets the agent card's `intro_bundle`
    /// so peers can pick up the X25519 pubkey through normal discovery.
    pub fn with_identity(mut self, identity: AgentIdentity) -> Self {
        self.card.intro_bundle = Some(IntroBundle::new(&identity.public_key_hex()));
        self.identity = Some(identity);
        self
    }

    /// Current advertised max-concurrent ceiling.
    pub fn max_concurrent(&self) -> u32 {
        self.max_concurrent
    }

    /// Increment the in-flight counter. Returns the new value.
    /// Pair with [`load_dec`](Self::load_dec) when the task completes.
    pub fn load_inc(&self) -> usize {
        self.in_flight
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    /// Decrement the in-flight counter (saturating at zero).
    pub fn load_dec(&self) -> usize {
        // Compare-and-swap to avoid wraparound when called more times
        // than `load_inc` (defensive — paths that reject early shouldn't
        // both inc and dec).
        let mut current = self.in_flight.load(std::sync::atomic::Ordering::SeqCst);
        loop {
            if current == 0 {
                return 0;
            }
            match self.in_flight.compare_exchange(
                current,
                current - 1,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            ) {
                Ok(_) => return current - 1,
                Err(actual) => current = actual,
            }
        }
    }

    /// Current in-flight task count.
    pub fn in_flight_count(&self) -> usize {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Build the [`logos_messaging_a2a_core::LoadStatus`] this agent
    /// advertises right now. Inspected when sealing presence envelopes
    /// for trusted peers.
    pub fn current_load_status(&self) -> logos_messaging_a2a_core::LoadStatus {
        let queue_depth = self.in_flight_count() as u32;
        logos_messaging_a2a_core::LoadStatus {
            bucket: logos_messaging_a2a_core::LoadBucket::from_load(
                queue_depth,
                self.max_concurrent,
            ),
            queue_depth,
            max_concurrent: self.max_concurrent,
            avg_latency_ms: 0,
        }
    }

    /// Whether this agent is at or above its advertised concurrency
    /// ceiling. Receivers use this in `tasks::respond` to immediately
    /// reject incoming tasks instead of letting them queue silently.
    pub fn is_at_capacity(&self) -> bool {
        (self.in_flight_count() as u32) >= self.max_concurrent
    }

    /// Is this pubkey on the local trust list? Read-locks briefly. In
    /// `TrustMode::Off` returns true for every pubkey.
    pub fn is_trusted(&self, pubkey: &str) -> bool {
        self.trust_list.read().unwrap().is_trusted(pubkey)
    }

    /// Is this pubkey trusted for the named capability? Read-locks briefly.
    /// Empty per-entry capability list trusts the peer for any capability.
    pub fn is_trusted_for(&self, pubkey: &str, capability: &str) -> bool {
        self.trust_list
            .read()
            .unwrap()
            .is_trusted_for(pubkey, capability)
    }

    /// Current enforcement mode.
    pub fn trust_mode(&self) -> TrustMode {
        self.trust_list.read().unwrap().mode()
    }

    /// A snapshot of the trust list — useful for IPC handlers and CLI
    /// `lmao trust list`. Cheap because the list is small (tens to low
    /// hundreds of entries).
    pub fn trust_snapshot(&self) -> (TrustMode, Vec<TrustEntry>) {
        let g = self.trust_list.read().unwrap();
        (g.mode(), g.iter().cloned().collect())
    }

    /// Insert (or replace) a peer entry. Persistent storage (TOML file)
    /// is the caller's responsibility — typically the daemon writes the
    /// file after this returns successfully.
    pub fn trust_add(&self, entry: TrustEntry) {
        self.trust_list.write().unwrap().add(entry);
    }

    /// Remove a peer by pubkey first, then by nickname. Returns the
    /// dropped entry on success, None if no entry matched.
    pub fn trust_remove(&self, target: &str) -> Option<TrustEntry> {
        let mut g = self.trust_list.write().unwrap();
        g.remove(target).or_else(|| g.remove_by_nickname(target))
    }

    /// Change the enforcement mode at runtime.
    pub fn trust_set_mode(&self, mode: TrustMode) {
        self.trust_list.write().unwrap().set_mode(mode);
    }

    /// Persist the current trust list to disk. Used by the daemon after
    /// each mutation so a restart picks up the same state.
    pub fn trust_save_to(&self, path: &std::path::Path) -> Result<()> {
        self.trust_list
            .read()
            .unwrap()
            .save_to(path)
            .map_err(|e| NodeError::Other(format!("trust save failed: {e}")))
    }

    /// Get this agent's public key hex string.
    pub fn pubkey(&self) -> &str {
        &self.card.public_key
    }

    /// Get the signing key (for testing or advanced use).
    pub fn signing_key(&self) -> &SigningKey {
        &self.signing_key
    }

    /// Get the encryption identity (if encryption is enabled).
    pub fn identity(&self) -> Option<&AgentIdentity> {
        self.identity.as_ref()
    }

    /// Access the underlying SDS MessageChannel.
    pub fn channel(&self) -> &MessageChannel<T> {
        &self.channel
    }

    /// Get the current retry configuration, if any.
    pub fn retry_config(&self) -> Option<&RetryConfig> {
        self.retry_config.as_ref()
    }

    /// Get a reference to the round-robin counter (for delegation).
    pub fn round_robin_counter(&self) -> &AtomicUsize {
        &self.round_robin_counter
    }

    /// Get a snapshot of the current operational metrics.
    pub fn metrics(&self) -> metrics::MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Create a new conversation session with a peer.
    pub fn create_session(&self, peer_pubkey: &str) -> Session {
        let session = Session::new(peer_pubkey);
        let id = session.id.clone();
        self.sessions
            .lock()
            .unwrap()
            .insert(id.clone(), session.clone());
        Metrics::inc(&self.metrics.sessions_created);
        session
    }

    /// Get a session by ID.
    pub fn get_session(&self, session_id: &str) -> Option<Session> {
        self.sessions.lock().unwrap().get(session_id).cloned()
    }

    /// List all active sessions.
    pub fn list_sessions(&self) -> Vec<Session> {
        self.sessions.lock().unwrap().values().cloned().collect()
    }

    /// Drop sessions that haven't seen a task in `max_age_secs` seconds.
    /// Returns the number of sessions evicted. Long-running agents
    /// should call this periodically so an unused session doesn't pin
    /// its `task_ids` vec forever. Cheap when there are no idle entries.
    pub fn evict_idle_sessions(&self, max_age_secs: u64) -> usize {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut sessions = self.sessions.lock().unwrap();
        let before = sessions.len();
        sessions.retain(|_, s| now.saturating_sub(s.last_seen) <= max_age_secs);
        before - sessions.len()
    }
}

/// Platform-appropriate RNG.
fn rand_core() -> k256::elliptic_curve::rand_core::OsRng {
    k256::elliptic_curve::rand_core::OsRng
}

#[cfg(test)]
mod keyfile_tests {
    use super::*;
    use logos_messaging_a2a_transport::memory::InMemoryTransport;
    use tempfile::TempDir;

    fn make_transport() -> InMemoryTransport {
        InMemoryTransport::new()
    }

    #[test]
    fn new_with_key_creates_node_with_specified_key() {
        let key = SigningKey::random(&mut rand_core());
        let expected_pubkey = hex::encode(key.verifying_key().to_encoded_point(true).as_bytes());

        let node = LmaoNode::new_with_key(
            "test",
            "test agent",
            vec!["text".into()],
            make_transport(),
            key,
        );

        assert_eq!(node.pubkey(), expected_pubkey);
        assert_eq!(node.card.name, "test");
    }

    #[test]
    fn from_keyfile_creates_file_when_missing() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.key");

        assert!(!path.exists());

        let node = LmaoNode::from_keyfile(
            "test",
            "test agent",
            vec!["text".into()],
            make_transport(),
            &path,
        )
        .unwrap();

        assert!(path.exists());
        // File should contain 64 hex chars (32 bytes)
        let contents = std::fs::read_to_string(&path).unwrap();
        assert_eq!(contents.len(), 64);
        hex::decode(&contents).expect("file should be valid hex");

        // Node should have a valid pubkey
        assert!(!node.pubkey().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn from_keyfile_sets_permissions_0600() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.key");

        LmaoNode::from_keyfile("test", "test agent", vec![], make_transport(), &path).unwrap();

        let perms = std::fs::metadata(&path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    #[test]
    fn from_keyfile_loads_existing_key_same_pubkey() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.key");

        let node1 = LmaoNode::from_keyfile(
            "agent-a",
            "first",
            vec!["text".into()],
            make_transport(),
            &path,
        )
        .unwrap();
        let pubkey1 = node1.pubkey().to_string();

        let node2 = LmaoNode::from_keyfile(
            "agent-b",
            "second",
            vec!["code".into()],
            make_transport(),
            &path,
        )
        .unwrap();
        let pubkey2 = node2.pubkey().to_string();

        assert_eq!(pubkey1, pubkey2, "reloaded key should produce same pubkey");
    }

    #[test]
    fn from_keyfile_roundtrip_restart_same_identity() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("agent.key");

        // "First run" — generates key
        let pubkey_first = {
            let node = LmaoNode::from_keyfile(
                "bot",
                "my bot",
                vec!["text".into()],
                make_transport(),
                &path,
            )
            .unwrap();
            node.pubkey().to_string()
        };
        // node is dropped — simulates process exit

        // "Second run" — loads existing key
        let pubkey_second = {
            let node = LmaoNode::from_keyfile(
                "bot",
                "my bot",
                vec!["text".into()],
                make_transport(),
                &path,
            )
            .unwrap();
            node.pubkey().to_string()
        };

        assert_eq!(pubkey_first, pubkey_second, "identity must survive restart");
    }

    #[test]
    fn from_keyfile_rejects_wrong_length() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.key");

        // Write 16 bytes (too short)
        std::fs::write(&path, "aabbccdd00112233aabbccdd00112233").unwrap();

        match LmaoNode::from_keyfile("test", "test", vec![], make_transport(), &path) {
            Err(e) => assert!(
                e.to_string().contains("16 bytes, expected 32"),
                "error should mention length: {}",
                e
            ),
            Ok(_) => panic!("expected error for wrong-length key"),
        }
    }

    #[test]
    fn from_keyfile_rejects_invalid_hex() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("bad.key");

        std::fs::write(&path, "not-valid-hex!!").unwrap();

        match LmaoNode::from_keyfile("test", "test", vec![], make_transport(), &path) {
            Err(e) => assert!(
                e.to_string().contains("invalid hex"),
                "error should mention hex: {}",
                e
            ),
            Ok(_) => panic!("expected error for invalid hex"),
        }
    }

    #[test]
    fn from_keyfile_with_known_key() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("known.key");

        // Generate a key, write it manually, then load via from_keyfile
        let key = SigningKey::random(&mut rand_core());
        let hex_str = hex::encode(key.to_bytes());
        let expected_pubkey = hex::encode(key.verifying_key().to_encoded_point(true).as_bytes());
        std::fs::write(&path, &hex_str).unwrap();

        let node = LmaoNode::from_keyfile("test", "test", vec![], make_transport(), &path).unwrap();

        assert_eq!(node.pubkey(), expected_pubkey);
    }
}
