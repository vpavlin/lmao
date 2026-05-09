//! Sealed presence end-to-end test.
//!
//! Two encrypted nodes (alice, bob) trust each other with X25519 pubkeys.
//! Bob is at capacity (in_flight = 1, max_concurrent = 1, bucket = Full).
//! Alice receives Bob's presence, decrypts the envelope addressed to
//! her, and her PeerInfo for Bob shows the Full bucket. Routing then
//! prefers a non-Full peer when one exists.

use k256::ecdsa::SigningKey;
use logos_messaging_a2a_core::{
    DelegationRequest, DelegationStrategy, LoadBucket, TrustEntry, TrustList, TrustMode,
};
use logos_messaging_a2a_crypto::AgentIdentity;
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_transport::memory::InMemoryTransport;
use std::sync::Arc;
use std::time::SystemTime;

fn trust_entry_with_x25519(
    pubkey: &str,
    nickname: &str,
    encryption_pubkey: Option<String>,
) -> TrustEntry {
    TrustEntry {
        pubkey: pubkey.into(),
        nickname: nickname.into(),
        capabilities: vec![],
        notes: None,
        added_at: SystemTime::UNIX_EPOCH,
        encryption_pubkey,
    }
}

fn pubkey_hex_of(key: &SigningKey) -> String {
    hex::encode(key.verifying_key().to_encoded_point(true).as_bytes())
}

fn make_node(
    name: &str,
    capability: &str,
    transport: InMemoryTransport,
    signing_key: SigningKey,
    x25519: AgentIdentity,
    peers: Vec<(String, &str, Option<String>)>,
    max_concurrent: u32,
) -> Arc<LmaoNode<InMemoryTransport>> {
    let mut trust = TrustList::with_mode(TrustMode::Enforce);
    for (pubkey, nick, x_pub) in peers {
        trust.add(trust_entry_with_x25519(&pubkey, nick, x_pub));
    }
    Arc::new(
        LmaoNode::from_key(
            name,
            name,
            vec![capability.into()],
            transport,
            signing_key,
        )
        .with_identity(x25519)
        .with_trust_list(trust)
        .with_max_concurrent(max_concurrent),
    )
}

fn fresh_signing_key() -> SigningKey {
    SigningKey::random(&mut k256::elliptic_curve::rand_core::OsRng)
}

#[tokio::test]
async fn alice_decrypts_envelope_addressed_to_her() {
    let transport = InMemoryTransport::new();

    let alice_sk = fresh_signing_key();
    let bob_sk = fresh_signing_key();
    let alice_pk = pubkey_hex_of(&alice_sk);
    let bob_pk = pubkey_hex_of(&bob_sk);

    let alice_x = AgentIdentity::generate();
    let bob_x = AgentIdentity::generate();
    let alice_x_pub = alice_x.public_key_hex();
    let bob_x_pub = bob_x.public_key_hex();

    let alice = make_node(
        "alice",
        "text",
        transport.clone(),
        alice_sk,
        alice_x,
        vec![(bob_pk.clone(), "bob", Some(bob_x_pub))],
        1,
    );
    let bob = make_node(
        "bob",
        "text",
        transport.clone(),
        bob_sk,
        bob_x,
        vec![(alice_pk.clone(), "alice", Some(alice_x_pub))],
        1,
    );

    alice.poll_presence().await.unwrap();
    bob.poll_presence().await.unwrap();

    bob.load_inc();
    assert!(bob.is_at_capacity());
    assert_eq!(bob.current_load_status().bucket, LoadBucket::Full);

    bob.announce_presence().await.unwrap();
    let count = alice.poll_presence().await.unwrap();
    assert_eq!(count, 1);

    let bob_info = alice.peers().get(&bob_pk).expect("bob in peer map");
    let load = bob_info.load.expect(
        "alice must decrypt bob's envelope — sealed_status should have been addressed to her",
    );
    assert_eq!(load.bucket, LoadBucket::Full);
    assert_eq!(load.queue_depth, 1);
    assert_eq!(load.max_concurrent, 1);
}

#[tokio::test]
async fn delegation_skips_full_peer_when_free_alternative_exists() {
    let transport = InMemoryTransport::new();

    let alice_sk = fresh_signing_key();
    let bob_sk = fresh_signing_key();
    let carol_sk = fresh_signing_key();
    let alice_pk = pubkey_hex_of(&alice_sk);
    let bob_pk = pubkey_hex_of(&bob_sk);
    let carol_pk = pubkey_hex_of(&carol_sk);

    let alice_x = AgentIdentity::generate();
    let bob_x = AgentIdentity::generate();
    let carol_x = AgentIdentity::generate();
    let alice_x_pub = alice_x.public_key_hex();
    let bob_x_pub = bob_x.public_key_hex();
    let carol_x_pub = carol_x.public_key_hex();

    let alice = make_node(
        "alice",
        "text",
        transport.clone(),
        alice_sk,
        alice_x,
        vec![
            (bob_pk.clone(), "bob", Some(bob_x_pub)),
            (carol_pk.clone(), "carol", Some(carol_x_pub)),
        ],
        1,
    );
    let bob = make_node(
        "bob",
        "code",
        transport.clone(),
        bob_sk,
        bob_x,
        vec![(alice_pk.clone(), "alice", Some(alice_x_pub.clone()))],
        1,
    );
    let carol = make_node(
        "carol",
        "code",
        transport.clone(),
        carol_sk,
        carol_x,
        vec![(alice_pk, "alice", Some(alice_x_pub))],
        1,
    );

    alice.poll_presence().await.unwrap();
    bob.poll_presence().await.unwrap();
    carol.poll_presence().await.unwrap();

    bob.load_inc();
    bob.announce_presence().await.unwrap();
    carol.announce_presence().await.unwrap();

    let n = alice.poll_presence().await.unwrap();
    assert_eq!(n, 2);

    let bob_info = alice.peers().get(&bob_pk).unwrap();
    let carol_info = alice.peers().get(&carol_pk).unwrap();
    assert_eq!(bob_info.load.unwrap().bucket, LoadBucket::Full);
    assert_eq!(carol_info.load.unwrap().bucket, LoadBucket::Free);

    // CapabilityMatch sorts by load_rank; carol (Free) must come before
    // bob (Full). delegate_task awaits a response — we don't have a
    // worker for carol so it will time out — but the picked peer ends
    // up in result.agent_id either way.
    let request = DelegationRequest {
        parent_task_id: "p1".into(),
        subtask_text: "do code".into(),
        strategy: DelegationStrategy::CapabilityMatch {
            capability: "code".into(),
        },
        timeout_secs: 1,
        session_id: None,
    };
    let result = alice.delegate_task(&request).await.unwrap();
    assert_eq!(
        result.agent_id, carol_pk,
        "Free carol must be preferred over Full bob"
    );
}
