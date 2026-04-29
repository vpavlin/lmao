//! Encrypted A2A task over real Logos Messaging.
//!
//! Both agents are created with `LmaoNode::new_encrypted`, which embeds
//! an X25519 identity and an [`IntroBundle`](
//! logos_messaging_a2a::IntroBundle) in the agent card. After mutual
//! discovery, alice sends a task with `send_task_to(&task, Some(&bob_card))`,
//! which auto-derives a ChaCha20-Poly1305 session key via ECDH and ships
//! the task as an `EncryptedTask` envelope. Bob's `poll_tasks()` decrypts
//! transparently. The round-tripped result is verified against the
//! plaintext sent.
//!
//! Run:
//! ```bash
//! LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build \
//!     LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR \
//!     cargo run --features logos-delivery --example logos_delivery_encrypted
//! ```

use anyhow::{anyhow, Result};
use logos_messaging_a2a::{LmaoNode, Task, Transport};
use logos_messaging_a2a_transport::logos_delivery::{LogosDeliveryTransport, NodeConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PEER_DISCOVERY_WAIT: Duration = Duration::from_secs(5);
const GOSSIP_PROPAGATION_WAIT: Duration = Duration::from_secs(3);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    eprintln!("=== LMAO encrypted task over Logos Messaging ===\n");

    let t_a = Arc::new(make_transport(60050, 9050).await?) as Arc<dyn Transport>;
    let t_b = Arc::new(make_transport(60051, 9051).await?) as Arc<dyn Transport>;
    let alice = LmaoNode::new_encrypted("alice", "encrypted requester", vec!["text".into()], t_a.clone());
    let bob = LmaoNode::new_encrypted("bob", "encrypted echo", vec!["text".into()], t_b.clone());
    eprintln!(
        "  alice X25519 bundle: {}…",
        &alice
            .card
            .intro_bundle
            .as_ref()
            .map(|b| b.agent_pubkey.as_str())
            .unwrap_or("<none>")[..16]
    );
    eprintln!(
        "  bob   X25519 bundle: {}…\n",
        &bob.card
            .intro_bundle
            .as_ref()
            .map(|b| b.agent_pubkey.as_str())
            .unwrap_or("<none>")[..16]
    );

    eprintln!(
        "Step 1: waiting {}s for gossip mesh…",
        PEER_DISCOVERY_WAIT.as_secs()
    );
    tokio::time::sleep(PEER_DISCOVERY_WAIT).await;

    eprintln!("Step 2: opening discovery subscriptions and announcing…");
    let _ = alice.discover().await?;
    let _ = bob.discover().await?;
    alice.announce().await?;
    bob.announce().await?;
    tokio::time::sleep(GOSSIP_PROPAGATION_WAIT).await;
    let cards = alice.discover().await?;
    let bob_card = cards
        .iter()
        .find(|c| c.public_key == bob.pubkey())
        .ok_or_else(|| anyhow!("alice did not see bob's AgentCard"))?
        .clone();
    if bob_card.intro_bundle.is_none() {
        return Err(anyhow!(
            "bob's discovered AgentCard has no intro_bundle — encryption can't be derived"
        ));
    }
    eprintln!("  alice has bob's intro_bundle — ready to encrypt");

    eprintln!("Step 3: bob opens task subscription…");
    bob.poll_tasks().await?;

    let plaintext = "secret: rendezvous at the Codex node";
    let task = Task::new(alice.pubkey(), bob.pubkey(), plaintext);
    let task_id = task.id.clone();
    eprintln!(
        "Step 4: alice sends encrypted task {}…",
        &task_id[..8.min(task_id.len())]
    );
    alice.send_task_to(&task, Some(&bob_card)).await?;

    eprintln!("Step 5: bob polls + decrypts…");
    let received = poll_until(
        || async {
            bob.poll_tasks()
                .await
                .map(|tasks| (tasks.iter().any(|t| t.id == task_id), tasks))
        },
        TIMEOUT,
        "bob did not receive the encrypted task in time",
    )
    .await?;
    let bob_task = received
        .iter()
        .find(|t| t.id == task_id)
        .ok_or_else(|| anyhow!("encrypted task not found in bob's inbox"))?;
    let body = bob_task.text().unwrap_or("");
    if body != plaintext {
        return Err(anyhow!(
            "bob got {:?} after decrypt, expected {:?}",
            body,
            plaintext
        ));
    }
    eprintln!("  bob decrypted: \"{}\"", body);

    eprintln!("Step 6: bob responds (also encrypted, since he has alice's card)…");
    let alice_card = bob
        .discover()
        .await?
        .into_iter()
        .find(|c| c.public_key == alice.pubkey());
    if let Some(ref card) = alice_card {
        bob.respond_to(bob_task, "Acknowledged.", Some(card)).await?;
    } else {
        // Fallback: plaintext respond. Less interesting; flag it loudly.
        eprintln!("  (warn) bob did not see alice's card; responding plaintext");
        bob.respond(bob_task, "Acknowledged.").await?;
    }

    eprintln!("Step 7: alice polls for the encrypted response…");
    let alice_results = poll_until(
        || async {
            alice
                .poll_tasks()
                .await
                .map(|tasks| (tasks.iter().any(|t| t.result_text().is_some()), tasks))
        },
        TIMEOUT,
        "alice did not receive bob's response in time",
    )
    .await?;
    let result = alice_results
        .iter()
        .find_map(|t| t.result_text())
        .ok_or_else(|| anyhow!("no result text on any task"))?;
    eprintln!("  alice decrypted response: \"{}\"", result);
    if result != "Acknowledged." {
        return Err(anyhow!("response mismatch: got {:?}", result));
    }

    eprintln!("\n=== Encrypted OK ===");
    Ok(())
}

async fn make_transport(tcp: u16, udp: u16) -> Result<LogosDeliveryTransport> {
    let mut cfg = NodeConfig::logos_dev();
    cfg.tcp_port = Some(tcp);
    cfg.discv5_udp_port = Some(udp);
    Ok(LogosDeliveryTransport::new(cfg).await?)
}

async fn poll_until<T, E, F, Fut>(mut op: F, timeout: Duration, on_timeout: &str) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = std::result::Result<(bool, T), E>>,
    E: Into<anyhow::Error>,
{
    let deadline = Instant::now() + timeout;
    loop {
        let (done, value) = op().await.map_err(Into::into)?;
        if done {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(on_timeout.to_string()));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}
