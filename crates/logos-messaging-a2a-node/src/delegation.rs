//! Multi-agent task delegation: decompose tasks into subtasks and forward
//! them to capable peers discovered via presence.

use logos_messaging_a2a_core::{
    topics, A2AEnvelope, DelegationRequest, DelegationResult, DelegationStrategy, LoadBucket, Task,
    TaskState, TrustMode,
};
use logos_messaging_a2a_transport::Transport;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crate::metrics::Metrics;
use crate::presence::PeerInfo;
use crate::{LmaoNode, NodeError, Result};

/// Default timeout for delegation when none is specified (30 seconds).
const DEFAULT_DELEGATION_TIMEOUT_SECS: u64 = 30;

/// Rank peers by their last-known load: known Free first, known Busy
/// second, unknown (no sealed envelope addressed to us) third, known
/// Full last. Routing prefers higher-ranked peers — Free peers are
/// chosen before Busy ones, Full peers are skipped if any alternative
/// exists.
fn load_rank(info: &PeerInfo) -> u8 {
    match info.load.as_ref().map(|l| l.bucket) {
        Some(LoadBucket::Free) => 0,
        Some(LoadBucket::Busy) => 1,
        None => 2,
        Some(LoadBucket::Full) => 3,
    }
}

/// Build a delegation error that distinguishes "no candidates exist" from
/// "candidates exist but trust filtered them all out". The second case is
/// confusing in practice — operators see "no live peers" while
/// `presence peers` shows live peers — so name it explicitly.
fn no_trusted_peers_err<T: Transport>(
    node: &LmaoNode<T>,
    total_candidates: usize,
    capability: Option<&str>,
) -> NodeError {
    let mode = node.trust_mode();
    let what = match capability {
        Some(cap) => format!("with capability '{cap}'"),
        None => String::from("available"),
    };
    if total_candidates > 0 && !matches!(mode, TrustMode::Off) {
        NodeError::Other(format!(
            "no trusted peers {what} for delegation: {total_candidates} live peer(s) all filtered \
             by trust list (mode={mode:?}). Add them with `lmao trust add <pubkey>` or pass \
             `--trust-file <path>` to use a different list."
        ))
    } else {
        NodeError::Other(format!("no live peers {what} for delegation"))
    }
}

impl<T: Transport> LmaoNode<T> {
    /// Delegate a subtask to a single peer based on the delegation strategy.
    ///
    /// Looks up peers from the live peer map, picks one according to the
    /// strategy, sends the subtask, and waits for a response within the
    /// specified timeout.
    pub async fn delegate_task(&self, request: &DelegationRequest) -> Result<DelegationResult> {
        let timeout_secs = if request.timeout_secs == 0 {
            DEFAULT_DELEGATION_TIMEOUT_SECS
        } else {
            request.timeout_secs
        };

        // Find a suitable peer based on strategy. The trust list filters
        // the candidate set in `TrustMode::Enforce` / `Log`; in
        // `TrustMode::Off` `is_trusted*` returns true for every pubkey
        // and the closure is a no-op. Within each strategy, peers are
        // sorted by `load_rank` so Free peers are picked before Busy
        // ones and Full peers are tried last.
        let peer_id = match &request.strategy {
            DelegationStrategy::FirstAvailable => {
                let mut peers = self.peers().all_live();
                peers.sort_by_key(|(_, info)| load_rank(info));
                let total_live = peers.len();
                peers
                    .into_iter()
                    .map(|(id, _)| id)
                    .find(|id| self.is_trusted(id))
                    .ok_or_else(|| no_trusted_peers_err(self, total_live, None))?
            }
            DelegationStrategy::CapabilityMatch { capability } => {
                let mut peers = self.find_peers_by_capability(capability);
                peers.sort_by_key(|(_, info)| load_rank(info));
                let total_live = peers.len();
                peers
                    .into_iter()
                    .map(|(id, _)| id)
                    .find(|id| self.is_trusted_for(id, capability))
                    .ok_or_else(|| no_trusted_peers_err(self, total_live, Some(capability)))?
            }
            DelegationStrategy::BroadcastCollect => {
                // For single delegation, broadcast acts like first-available
                let mut peers = self.peers().all_live();
                peers.sort_by_key(|(_, info)| load_rank(info));
                peers
                    .into_iter()
                    .map(|(id, _)| id)
                    .find(|id| self.is_trusted(id))
                    .ok_or_else(|| {
                        NodeError::Other("no live peers available for broadcast delegation".into())
                    })?
            }
            DelegationStrategy::RoundRobin => {
                let live = self.peers().all_live();
                // Round-robin only among the best-load tier — once that
                // tier saturates we naturally walk to the next tier.
                let best_rank = live
                    .iter()
                    .map(|(_, info)| load_rank(info))
                    .min()
                    .unwrap_or(2);
                let peers: Vec<String> = live
                    .into_iter()
                    .filter(|(_, info)| load_rank(info) == best_rank)
                    .map(|(id, _)| id)
                    .filter(|id| self.is_trusted(id))
                    .collect();
                if peers.is_empty() {
                    return Err(NodeError::Other(
                        "no live peers available for round-robin delegation".into(),
                    ));
                }
                let idx = self.round_robin_counter.fetch_add(1, Ordering::Relaxed) % peers.len();
                peers.into_iter().nth(idx).unwrap()
            }
        };

        Metrics::inc(&self.metrics.delegations_sent);
        self.delegate_to_peer(request, &peer_id, timeout_secs).await
    }

    /// Delegate a subtask directly to a specific peer by pubkey, bypassing
    /// strategy-based selection. Used when the caller already knows which
    /// peer should handle the task (e.g. `lmao task delegate --to <pk>`
    /// through the daemon, or any UI that has a peer handle).
    ///
    /// Honors the trust list — direct delegation still respects the
    /// configured `TrustMode`. In `Off` mode this is a no-op; in `Log` /
    /// `Enforce` an untrusted target produces a diagnostic error rather
    /// than silently falling back to strategy-based routing.
    pub async fn delegate_direct(
        &self,
        request: &DelegationRequest,
        peer_pubkey: &str,
    ) -> Result<DelegationResult> {
        let timeout_secs = if request.timeout_secs == 0 {
            DEFAULT_DELEGATION_TIMEOUT_SECS
        } else {
            request.timeout_secs
        };
        if !self.is_trusted(peer_pubkey) {
            return Err(NodeError::Other(format!(
                "direct delegation to {peer_pubkey} blocked: peer not on trust list (mode={:?}). \
                 Add it with `lmao trust add` or pass an alternate `--trust-file`.",
                self.trust_mode()
            )));
        }
        Metrics::inc(&self.metrics.delegations_sent);
        self.delegate_to_peer(request, peer_pubkey, timeout_secs)
            .await
    }

    /// Delegate a subtask to all matching peers and collect responses.
    ///
    /// Distributes the subtask to every peer that matches the delegation
    /// strategy and waits up to `timeout_secs` for responses from all of them.
    pub async fn delegate_broadcast(
        &self,
        request: &DelegationRequest,
    ) -> Result<Vec<DelegationResult>> {
        let timeout_secs = if request.timeout_secs == 0 {
            DEFAULT_DELEGATION_TIMEOUT_SECS
        } else {
            request.timeout_secs
        };

        let peer_ids: Vec<String> = match &request.strategy {
            DelegationStrategy::CapabilityMatch { capability } => self
                .find_peers_by_capability(capability)
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| self.is_trusted_for(id, capability))
                .collect(),
            // RoundRobin, BroadcastCollect, FirstAvailable all broadcast to every peer
            _ => self
                .peers()
                .all_live()
                .into_iter()
                .map(|(id, _)| id)
                .filter(|id| self.is_trusted(id))
                .collect(),
        };

        if peer_ids.is_empty() {
            return Err(NodeError::Other(
                "no live peers available for broadcast delegation".into(),
            ));
        }

        let mut results = Vec::new();
        Metrics::inc_by(&self.metrics.delegations_sent, peer_ids.len() as u64);
        for peer_id in peer_ids {
            let result = self.delegate_to_peer(request, &peer_id, timeout_secs).await;
            match result {
                Ok(r) => results.push(r),
                Err(e) => results.push(DelegationResult {
                    parent_task_id: request.parent_task_id.clone(),
                    subtask_id: String::new(),
                    agent_id: peer_id,
                    result_text: None,
                    success: false,
                    error: Some(e.to_string()),
                }),
            }
        }

        Ok(results)
    }

    /// Send a subtask to a specific peer and wait for a response.
    ///
    /// This is an internal helper used by both [`delegate_task`] and
    /// [`delegate_broadcast`].
    ///
    /// Uses fire-and-forget send (like `respond`) rather than SDS reliable
    /// delivery, since delegation already has its own response-based timeout.
    async fn delegate_to_peer(
        &self,
        request: &DelegationRequest,
        peer_id: &str,
        timeout_secs: u64,
    ) -> Result<DelegationResult> {
        let task = match request.session_id.as_deref() {
            Some(sid) if !sid.is_empty() => {
                Task::new_in_session(self.pubkey(), peer_id, &request.subtask_text, sid)
            }
            _ => Task::new(self.pubkey(), peer_id, &request.subtask_text),
        };
        let subtask_id = task.id.clone();

        // Publish directly to transport (bypassing SDS reliable delivery)
        // since delegation already polls for the response with its own timeout.
        let topic = topics::task_topic(peer_id);
        let envelope = A2AEnvelope::Task(task);
        let payload = serde_json::to_vec(&envelope)?;
        self.channel().transport().publish(&topic, &payload).await?;

        // Poll for response with timeout
        let started_at = tokio::time::Instant::now();
        let deadline = started_at + Duration::from_secs(timeout_secs);

        while tokio::time::Instant::now() < deadline {
            let tasks = self.poll_tasks().await?;
            for received in &tasks {
                if received.id == subtask_id {
                    // Inspect the response state — receivers signal exec
                    // failure via `TaskState::Failed` (paired with the
                    // error message in `result`). Treating that as a
                    // successful response would have the UI render an
                    // error log as if it were a normal answer.
                    let body = received.result_text().map(String::from);
                    let success = !matches!(received.state, TaskState::Failed);
                    let result = DelegationResult {
                        parent_task_id: request.parent_task_id.clone(),
                        subtask_id: subtask_id.clone(),
                        agent_id: peer_id.to_string(),
                        result_text: if success { body.clone() } else { None },
                        success,
                        error: if success { None } else { body },
                    };
                    self.record_delegation_history(
                        &result,
                        &request.subtask_text,
                        capability_of(&request.strategy),
                        started_at.elapsed(),
                        request.session_id.clone(),
                    )
                    .await;
                    return Ok(result);
                }
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let result = DelegationResult {
            parent_task_id: request.parent_task_id.clone(),
            subtask_id,
            agent_id: peer_id.to_string(),
            result_text: None,
            success: false,
            error: Some("delegation timed out".to_string()),
        };
        self.record_delegation_history(
            &result,
            &request.subtask_text,
            capability_of(&request.strategy),
            started_at.elapsed(),
            request.session_id.clone(),
        )
        .await;
        Ok(result)
    }

    /// Persist a finished delegation (success or timeout) into the
    /// task-history log, if one is attached. Look up the peer's
    /// display name from the live presence map best-effort. Extract
    /// the Codex CID from the result_text trailer when present so the
    /// UI can render the audit-log link without re-parsing.
    async fn record_delegation_history(
        &self,
        result: &DelegationResult,
        subtask_text: &str,
        capability: String,
        elapsed: Duration,
        session_id: Option<String>,
    ) {
        let Some(history) = self.history.as_ref() else {
            return;
        };
        let body = result.result_text.clone().unwrap_or_default();
        let cid = extract_codex_cid(&body);
        let peer_name = self
            .peer_map
            .get(&result.agent_id)
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let entry = crate::history::HistoryEntry {
            task_id: result.subtask_id.clone(),
            parent_id: result.parent_task_id.clone(),
            created_at_ms: crate::history::now_ms(),
            direction: "delegated".to_string(),
            peer_pubkey: result.agent_id.clone(),
            peer_name,
            capability,
            text: subtask_text.to_string(),
            body,
            cid,
            success: result.success,
            error: result.error.clone(),
            elapsed_ms: elapsed.as_millis() as u64,
            session_id: session_id.unwrap_or_default(),
        };
        if let Err(e) = history.append(&entry).await {
            tracing::warn!(err=%e, "failed to append delegation to history");
        }
    }
}

/// Best-effort capability extraction for history rows. `CapabilityMatch`
/// is the only strategy that carries one; the others get an empty
/// string ("any").
fn capability_of(strategy: &DelegationStrategy) -> String {
    match strategy {
        DelegationStrategy::CapabilityMatch { capability } => capability.clone(),
        _ => String::new(),
    }
}

/// Pull the trailing `codex://<cid>` reference (if any) out of a
/// receiver's response text. Receivers add this trailer when they
/// upload the exec audit log to libstorage; the UI uses the CID to
/// fetch + display the log on demand.
pub(crate) fn extract_codex_cid(body: &str) -> String {
    // Look for `codex://` prefix, take alphanumeric chars after it.
    let Some(idx) = body.find("codex://") else {
        return String::new();
    };
    let tail = &body[idx + "codex://".len()..];
    let end = tail
        .find(|c: char| !c.is_ascii_alphanumeric())
        .unwrap_or(tail.len());
    tail[..end].to_string()
}

#[cfg(test)]
mod tests {
    use crate::LmaoNode;
    use logos_messaging_a2a_core::{DelegationRequest, DelegationStrategy};
    use logos_messaging_a2a_transport::memory::InMemoryTransport;
    use logos_messaging_a2a_transport::sds::ChannelConfig;
    use std::time::Duration;

    /// Fast SDS config that skips ACK waits — suitable for in-process tests.
    fn fast_config() -> ChannelConfig {
        ChannelConfig {
            ack_timeout: Duration::from_millis(1),
            max_retries: 0,
            ..Default::default()
        }
    }

    /// Create a node with fast SDS config, announce presence, and subscribe
    /// to its own task topic (lazy-init).
    async fn make_announced_node(
        name: &str,
        capabilities: Vec<&str>,
        transport: InMemoryTransport,
    ) -> LmaoNode<InMemoryTransport> {
        let caps = capabilities.into_iter().map(String::from).collect();
        let node = LmaoNode::with_config(
            name,
            &format!("{name} agent"),
            caps,
            transport,
            fast_config(),
        );
        node.announce_presence().await.unwrap();
        node.poll_tasks().await.unwrap();
        node
    }

    /// Poll for one incoming task and respond with an echo.
    async fn echo_once(node: &LmaoNode<InMemoryTransport>) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while tokio::time::Instant::now() < deadline {
            let tasks = node.poll_tasks().await.unwrap();
            for task in &tasks {
                if task.result.is_none() {
                    let reply = format!("echo: {}", task.text().unwrap_or(""));
                    node.respond(task, &reply).await.unwrap();
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("echo_once timed out waiting for a task");
    }

    /// Poll for exactly `n` incoming tasks and respond to each.
    async fn echo_n(node: &LmaoNode<InMemoryTransport>, n: usize) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let mut count = 0;
        while count < n && tokio::time::Instant::now() < deadline {
            let tasks = node.poll_tasks().await.unwrap();
            for task in &tasks {
                if task.result.is_none() {
                    let reply = format!(
                        "echo from {}: {}",
                        node.card.name,
                        task.text().unwrap_or("")
                    );
                    node.respond(task, &reply).await.unwrap();
                    count += 1;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(count, n, "echo_n: expected {n} tasks but got {count}");
    }

    // ── 1. FirstAvailable — success ──

    #[tokio::test]
    async fn delegate_task_first_available_success() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let worker = make_announced_node("worker", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-001".into(),
            subtask_text: "Hello worker".into(),
            strategy: DelegationStrategy::FirstAvailable,
            timeout_secs: 5,
            session_id: None,
        };

        let wh = tokio::spawn(async move { echo_once(&worker).await });
        let result = orchestrator.delegate_task(&request).await.unwrap();
        wh.await.unwrap();

        assert!(result.success);
        assert_eq!(result.parent_task_id, "p-001");
        assert!(result.result_text.unwrap().contains("echo: Hello worker"));
        assert!(result.error.is_none());
    }

    // ── 2. CapabilityMatch — correct peer selected ──

    #[tokio::test]
    async fn delegate_task_capability_match_finds_correct_peer() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let summarizer =
            make_announced_node("summarizer", vec!["summarize"], transport.clone()).await;
        let _coder = make_announced_node("coder", vec!["code"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let summarizer_pubkey = summarizer.pubkey().to_string();

        let request = DelegationRequest {
            parent_task_id: "p-002".into(),
            subtask_text: "Summarize this".into(),
            strategy: DelegationStrategy::CapabilityMatch {
                capability: "summarize".into(),
            },
            timeout_secs: 5,
            session_id: None,
        };

        let sh = tokio::spawn(async move { echo_once(&summarizer).await });
        let result = orchestrator.delegate_task(&request).await.unwrap();
        sh.await.unwrap();

        assert!(result.success);
        assert_eq!(result.agent_id, summarizer_pubkey);
        assert!(result.result_text.unwrap().contains("echo: Summarize this"));
    }

    // ── 3. CapabilityMatch — no matching peers ──

    #[tokio::test]
    async fn delegate_task_capability_match_no_matching_peers() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let _worker = make_announced_node("worker", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-003".into(),
            subtask_text: "Need image processing".into(),
            strategy: DelegationStrategy::CapabilityMatch {
                capability: "image".into(),
            },
            timeout_secs: 1,
            session_id: None,
        };

        let err = orchestrator.delegate_task(&request).await.unwrap_err();
        assert!(
            err.to_string().contains("no live peers with capability"),
            "unexpected error: {err}"
        );
    }

    // ── 4. No live peers at all ──

    #[tokio::test]
    async fn delegate_task_no_live_peers_error() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;

        // No other nodes — peer map is empty
        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-004".into(),
            subtask_text: "Nobody home".into(),
            strategy: DelegationStrategy::FirstAvailable,
            timeout_secs: 1,
            session_id: None,
        };

        let err = orchestrator.delegate_task(&request).await.unwrap_err();
        assert!(
            err.to_string().contains("no live peers"),
            "unexpected error: {err}"
        );
    }

    // ── 5. RoundRobin — distributes across peers ──

    #[tokio::test]
    async fn delegate_task_round_robin_distributes() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let worker_a = make_announced_node("worker-a", vec!["text"], transport.clone()).await;
        let worker_b = make_announced_node("worker-b", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let peer_ids: Vec<String> = orchestrator
            .peers()
            .all_live()
            .into_iter()
            .map(|(id, _)| id)
            .collect();
        assert_eq!(peer_ids.len(), 2);

        // Each worker handles 2 tasks (4 total / 2 workers)
        let ha = tokio::spawn(async move { echo_n(&worker_a, 2).await });
        let hb = tokio::spawn(async move { echo_n(&worker_b, 2).await });

        let mut agent_ids = Vec::new();
        for i in 0..4 {
            let request = DelegationRequest {
                parent_task_id: format!("rr-{i}"),
                subtask_text: format!("round-robin task {i}"),
                strategy: DelegationStrategy::RoundRobin,
                timeout_secs: 5,
                session_id: None,
            };
            let result = orchestrator.delegate_task(&request).await.unwrap();
            assert!(result.success, "task {i} should succeed");
            agent_ids.push(result.agent_id);
        }

        ha.await.unwrap();
        hb.await.unwrap();

        // Should alternate: peer0, peer1, peer0, peer1
        assert_eq!(agent_ids[0], peer_ids[0]);
        assert_eq!(agent_ids[1], peer_ids[1]);
        assert_eq!(agent_ids[2], peer_ids[0]);
        assert_eq!(agent_ids[3], peer_ids[1]);
    }

    // ── 6. Timeout — peer does not respond ──

    #[tokio::test]
    async fn delegate_task_timeout_returns_failure() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        // Silent worker announces but never responds to tasks
        let _silent = make_announced_node("silent", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-006".into(),
            subtask_text: "No reply expected".into(),
            strategy: DelegationStrategy::FirstAvailable,
            timeout_secs: 1,
            session_id: None,
        };

        let result = orchestrator.delegate_task(&request).await.unwrap();
        assert!(!result.success);
        assert_eq!(result.error.as_deref(), Some("delegation timed out"));
        assert!(result.result_text.is_none());
    }

    // ── 7. Broadcast — collects multiple responses ──

    #[tokio::test]
    async fn delegate_broadcast_collects_results() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let worker_a = make_announced_node("worker-a", vec!["text"], transport.clone()).await;
        let worker_b = make_announced_node("worker-b", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-007".into(),
            subtask_text: "Broadcast task".into(),
            strategy: DelegationStrategy::BroadcastCollect,
            timeout_secs: 10,
            session_id: None,
        };

        let ha = tokio::spawn(async move { echo_once(&worker_a).await });
        let hb = tokio::spawn(async move { echo_once(&worker_b).await });

        let results = orchestrator.delegate_broadcast(&request).await.unwrap();
        ha.await.unwrap();
        hb.await.unwrap();

        assert_eq!(results.len(), 2);
        let successes: Vec<_> = results.iter().filter(|r| r.success).collect();
        assert!(
            !successes.is_empty(),
            "at least one broadcast result should succeed"
        );
        for r in &successes {
            assert_eq!(r.parent_task_id, "p-007");
            assert!(r
                .result_text
                .as_ref()
                .unwrap()
                .contains("echo: Broadcast task"));
        }
    }

    // ── 8. Broadcast — no peers ──

    #[tokio::test]
    async fn delegate_broadcast_no_peers_error() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-008".into(),
            subtask_text: "Nobody".into(),
            strategy: DelegationStrategy::BroadcastCollect,
            timeout_secs: 1,
            session_id: None,
        };

        let err = orchestrator.delegate_broadcast(&request).await.unwrap_err();
        assert!(
            err.to_string().contains("no live peers"),
            "unexpected error: {err}"
        );
    }

    // ── 9. Broadcast — mixed success/failure ──

    #[tokio::test]
    async fn delegate_broadcast_mixed_success_failure() {
        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let worker = make_announced_node("worker", vec!["text"], transport.clone()).await;
        // silent announces but never responds
        let _silent = make_announced_node("silent", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-009".into(),
            subtask_text: "Partial broadcast".into(),
            strategy: DelegationStrategy::CapabilityMatch {
                capability: "text".into(),
            },
            timeout_secs: 2,
            session_id: None,
        };

        let wh = tokio::spawn(async move { echo_once(&worker).await });
        let results = orchestrator.delegate_broadcast(&request).await.unwrap();
        wh.await.unwrap();

        assert_eq!(results.len(), 2);
        let successes = results.iter().filter(|r| r.success).count();
        let failures = results.iter().filter(|r| !r.success).count();
        assert_eq!(successes, 1);
        assert_eq!(failures, 1);

        // The failing result should carry a timeout error
        let fail = results.iter().find(|r| !r.success).unwrap();
        assert_eq!(fail.error.as_deref(), Some("delegation timed out"));
    }

    // ── 10. Default timeout when request.timeout_secs == 0 ──

    #[tokio::test]
    async fn default_timeout_used_when_request_timeout_is_zero() {
        // Verify the constant is 30 seconds.
        assert_eq!(super::DEFAULT_DELEGATION_TIMEOUT_SECS, 30);

        let transport = InMemoryTransport::new();
        let orchestrator =
            make_announced_node("orchestrator", vec!["coordinate"], transport.clone()).await;
        let worker = make_announced_node("worker", vec!["text"], transport.clone()).await;

        orchestrator.poll_presence().await.unwrap();

        let request = DelegationRequest {
            parent_task_id: "p-010".into(),
            subtask_text: "Quick task".into(),
            strategy: DelegationStrategy::FirstAvailable,
            timeout_secs: 0, // should fall back to DEFAULT_DELEGATION_TIMEOUT_SECS
            session_id: None,
        };

        let wh = tokio::spawn(async move { echo_once(&worker).await });
        let result = orchestrator.delegate_task(&request).await.unwrap();
        wh.await.unwrap();

        // If the default were 0, this would have timed out immediately.
        assert!(result.success);
    }
}
