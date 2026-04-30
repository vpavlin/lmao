//! Friend-keyring trust filter — incoming + outgoing integration tests.
//!
//! Three nodes share one InMemoryTransport bus:
//! - **alice**  (the operator) — runs the trust list
//! - **bob**    (a friend, in alice's trust list)
//! - **charlie** (a stranger, NOT in alice's trust list)
//!
//! Verifies:
//! - In `TrustMode::Enforce`, alice drops charlie's tasks; bob's still arrive.
//! - `tasks_dropped_untrusted` metric is bumped per drop.
//! - In `TrustMode::Log`, alice still surfaces charlie's task (with a warn).
//! - In `TrustMode::Off` (default), both bob and charlie are accepted.
//! - Outgoing: alice's `delegate_task(CapabilityMatch)` selects only
//!   trusted peers, even when an untrusted one would have been the
//!   first match.

use logos_messaging_a2a_core::{
    DelegationRequest, DelegationStrategy, PresenceAnnouncement, TrustEntry, TrustList, TrustMode,
};
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_transport::memory::InMemoryTransport;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

fn entry(pubkey: &str, nickname: &str, caps: &[&str]) -> TrustEntry {
    TrustEntry {
        pubkey: pubkey.into(),
        nickname: nickname.into(),
        capabilities: caps.iter().map(|s| s.to_string()).collect(),
        notes: None,
        added_at: SystemTime::UNIX_EPOCH,
    }
}

type SharedNode = Arc<LmaoNode<InMemoryTransport>>;

fn make_three_nodes() -> (SharedNode, SharedNode, SharedNode) {
    let transport = InMemoryTransport::new();
    let alice = Arc::new(LmaoNode::new(
        "alice",
        "alice",
        vec!["text".into()],
        transport.clone(),
    ));
    let bob = Arc::new(LmaoNode::new(
        "bob",
        "bob",
        vec!["text".into()],
        transport.clone(),
    ));
    let charlie = Arc::new(LmaoNode::new(
        "charlie",
        "charlie",
        vec!["text".into()],
        transport,
    ));
    (alice, bob, charlie)
}

/// Drive `alice.poll_tasks()` continuously for `total` collecting every
/// surfaced task, while the body of `senders` runs concurrently. Returns
/// once `expected` tasks have arrived OR the deadline elapses.
async fn collect_with_polling<T, F>(
    alice: Arc<LmaoNode<T>>,
    senders: F,
    expected: usize,
    total: Duration,
) -> Vec<logos_messaging_a2a_core::Task>
where
    T: logos_messaging_a2a_transport::Transport + Clone + 'static,
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let collected = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let alice_poll = alice.clone();
    let collected_poll = collected.clone();
    let poll_handle = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + total;
        while std::time::Instant::now() < deadline {
            for t in alice_poll.poll_tasks().await.unwrap_or_default() {
                let mut c = collected_poll.lock().await;
                c.push(t);
                if c.len() >= expected {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    senders.await;
    let _ = poll_handle.await;
    let mut out = collected.lock().await;
    std::mem::take(&mut *out)
}

#[tokio::test]
async fn enforce_mode_drops_untrusted_sender() {
    let (alice, bob, charlie) = make_three_nodes();

    // Build alice with a trust list that contains bob but not charlie.
    let mut list = TrustList::with_mode(TrustMode::Enforce);
    list.add(entry(bob.pubkey(), "bob", &[]));
    let alice = Arc::new(
        LmaoNode::new(
            "alice",
            "alice",
            vec!["text".into()],
            alice.channel().transport().clone(),
        )
        .with_trust_list(list),
    );

    // alice opens her inbox; the cell stays sticky for the polling loop.
    alice.poll_tasks().await.unwrap();

    let bob_pk = alice.pubkey().to_string();
    let charlie_pk = alice.pubkey().to_string();
    let bob_send = bob.clone();
    let charlie_send = charlie.clone();
    let senders = async move {
        let _ = tokio::join!(
            bob_send.send_text(&bob_pk, "hi from bob"),
            charlie_send.send_text(&charlie_pk, "hi from charlie"),
        );
    };

    let received = collect_with_polling(alice.clone(), senders, 1, Duration::from_secs(5)).await;
    assert_eq!(
        received.len(),
        1,
        "only bob's task should survive the filter"
    );
    assert_eq!(received[0].from, bob.pubkey());
    assert_eq!(received[0].text(), Some("hi from bob"));

    let snap = alice.metrics();
    assert_eq!(
        snap.tasks_dropped_untrusted, 1,
        "charlie's drop should have bumped the metric"
    );
}

#[tokio::test]
async fn log_mode_accepts_untrusted_but_counts_them() {
    let (alice, _bob, charlie) = make_three_nodes();
    let list = TrustList::with_mode(TrustMode::Log);
    let alice = Arc::new(
        LmaoNode::new(
            "alice",
            "alice",
            vec!["text".into()],
            alice.channel().transport().clone(),
        )
        .with_trust_list(list),
    );

    alice.poll_tasks().await.unwrap();
    let alice_pk = alice.pubkey().to_string();
    let charlie_send = charlie.clone();
    let senders = async move {
        let _ = charlie_send.send_text(&alice_pk, "hi").await;
    };
    let received = collect_with_polling(alice.clone(), senders, 1, Duration::from_secs(3)).await;
    assert_eq!(received.len(), 1, "Log mode surfaces untrusted senders");
    assert_eq!(received[0].from, charlie.pubkey());

    let snap = alice.metrics();
    assert_eq!(
        snap.tasks_dropped_untrusted, 1,
        "Log mode still bumps the counter so operators notice"
    );
}

#[tokio::test]
async fn off_mode_accepts_everyone() {
    let (alice, bob, charlie) = make_three_nodes();
    // alice has no with_trust_list call → default Off, empty list.
    alice.poll_tasks().await.unwrap();

    let alice_pk = alice.pubkey().to_string();
    let alice_pk2 = alice_pk.clone();
    let bob_send = bob.clone();
    let charlie_send = charlie.clone();
    let senders = async move {
        let _ = tokio::join!(
            bob_send.send_text(&alice_pk, "from bob"),
            charlie_send.send_text(&alice_pk2, "from charlie"),
        );
    };
    let received = collect_with_polling(alice.clone(), senders, 2, Duration::from_secs(5)).await;
    assert_eq!(received.len(), 2);

    let snap = alice.metrics();
    assert_eq!(
        snap.tasks_dropped_untrusted, 0,
        "no drops in Off mode regardless of who sent what"
    );
}

#[tokio::test]
async fn outgoing_capability_match_filters_to_trusted_peers() {
    let (alice, bob, charlie) = make_three_nodes();

    // Both bob and charlie advertise the "code" capability via presence.
    // alice trusts only bob for that capability.
    let mut list = TrustList::with_mode(TrustMode::Enforce);
    list.add(entry(bob.pubkey(), "bob", &["code"]));
    // charlie is NOT in the list at all.
    let alice = Arc::new(
        LmaoNode::new(
            "alice",
            "alice",
            vec!["text".into()],
            alice.channel().transport().clone(),
        )
        .with_trust_list(list),
    );

    // Hand-inject presence into alice's PeerMap for both candidates so
    // we don't have to wait for gossip propagation. delegate_task reads
    // the live PeerMap; what's in it is what gets considered.
    for peer in [&bob, &charlie] {
        let announcement = PresenceAnnouncement {
            agent_id: peer.pubkey().to_string(),
            name: peer.card.name.clone(),
            capabilities: vec!["code".into()],
            waku_topic: format!("/lmao/1/task-{}/proto", peer.pubkey()),
            ttl_secs: 60,
            signature: None,
        };
        alice.peers().update(&announcement);
    }

    // bob's inbox open so he can ACK the delegation. delegate_task will
    // time out without it; charlie's would too (good — proves we never
    // even tried him).
    bob.poll_tasks().await.unwrap();

    // Spawn bob's responder loop so the delegation completes.
    let bob_responder = bob.clone();
    let responder = tokio::spawn(async move {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if let Some(task) = bob_responder
                .poll_tasks()
                .await
                .unwrap_or_default()
                .into_iter()
                .next()
            {
                let _ = bob_responder.respond(&task, "code review ok").await;
                return task;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("bob never received a subtask — outgoing trust filter likely picked charlie");
    });

    let req = DelegationRequest {
        parent_task_id: "parent".into(),
        subtask_text: "review this".into(),
        strategy: DelegationStrategy::CapabilityMatch {
            capability: "code".into(),
        },
        timeout_secs: 3,
    };
    let result = alice.delegate_task(&req).await.unwrap();

    let received_by_bob = responder.await.unwrap();
    assert_eq!(received_by_bob.from, alice.pubkey());
    assert_eq!(
        result.agent_id,
        bob.pubkey(),
        "alice routed only to the trusted peer (bob), not charlie"
    );
    assert!(result.success);
}

#[tokio::test]
async fn delegate_errors_when_no_trusted_peer_advertises_capability() {
    let (alice, _bob, charlie) = make_three_nodes();

    // alice trusts no one. charlie alone advertises "code".
    let alice = Arc::new(
        LmaoNode::new(
            "alice",
            "alice",
            vec!["text".into()],
            alice.channel().transport().clone(),
        )
        .with_trust_list(TrustList::with_mode(TrustMode::Enforce)),
    );
    alice.peers().update(&PresenceAnnouncement {
        agent_id: charlie.pubkey().to_string(),
        name: "charlie".into(),
        capabilities: vec!["code".into()],
        waku_topic: format!("/lmao/1/task-{}/proto", charlie.pubkey()),
        ttl_secs: 60,
        signature: None,
    });

    let req = DelegationRequest {
        parent_task_id: "parent".into(),
        subtask_text: "review".into(),
        strategy: DelegationStrategy::CapabilityMatch {
            capability: "code".into(),
        },
        timeout_secs: 1,
    };
    let err = alice.delegate_task(&req).await.unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("no live peers with capability"),
        "expected capability-not-found error, got: {msg}"
    );
}
