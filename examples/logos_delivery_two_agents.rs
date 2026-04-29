//! Two LMAO agents on logos.dev — full A2A loop over real Logos Messaging.
//!
//! This is the Day-2 milestone: announce → discover → send → respond,
//! all over real-network gossip. Each agent runs its own embedded
//! liblogosdelivery node connected to the `logos.dev` fleet.
//!
//! Run:
//! ```bash
//! LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build \
//!     LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR \
//!     cargo run --features logos-delivery --example logos_delivery_two_agents
//! ```
//!
//! Prints `=== Demo OK ===` on success; non-zero exit on any step
//! that fails or times out.

use anyhow::{anyhow, Result};
use logos_messaging_a2a::{LmaoNode, Task, Transport};
use logos_messaging_a2a_transport::logos_delivery::{LogosDeliveryTransport, NodeConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PEER_DISCOVERY_WAIT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const TASK_DELIVERY_TIMEOUT: Duration = Duration::from_secs(20);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    eprintln!("=== Two LMAO agents over real Logos Messaging ===\n");

    eprintln!("Step 1: spinning up two embedded Logos Messaging nodes…");
    let transport_a = Arc::new(make_transport(60020, 9020).await?) as Arc<dyn Transport>;
    let transport_b = Arc::new(make_transport(60021, 9021).await?) as Arc<dyn Transport>;
    eprintln!("  • alice-node up on tcp/60020 udp/9020");
    eprintln!("  • bob-node   up on tcp/60021 udp/9021");

    let alice = LmaoNode::new(
        "alice",
        "greeting agent",
        vec!["text".into()],
        transport_a.clone(),
    );
    let bob = LmaoNode::new(
        "bob",
        "echo agent",
        vec!["text".into(), "echo".into()],
        transport_b.clone(),
    );
    eprintln!("  alice pubkey: {}", &alice.pubkey()[..16]);
    eprintln!("  bob   pubkey: {}", &bob.pubkey()[..16]);

    eprintln!(
        "\nStep 2: waiting {}s for gossip mesh to form…",
        PEER_DISCOVERY_WAIT.as_secs()
    );
    tokio::time::sleep(PEER_DISCOVERY_WAIT).await;

    // Open discovery subscriptions BEFORE announce — the gossip mesh
    // doesn't buffer messages, so a subscribe-after-announce misses them.
    eprintln!("\nStep 3a: opening discovery subscriptions…");
    let _ = alice.discover().await?;
    let _ = bob.discover().await?;

    eprintln!("\nStep 3b: announcing on /lmao/1/discovery/proto…");
    alice.announce().await?;
    bob.announce().await?;
    eprintln!("  both AgentCards published; waiting for gossip propagation…");
    tokio::time::sleep(Duration::from_secs(3)).await;

    eprintln!("\nStep 4: alice drains the discovery topic…");
    let cards = poll_until(
        || async { alice.discover().await.map(|cards| (cards.len() >= 1, cards)) },
        TASK_DELIVERY_TIMEOUT,
        "alice did not discover bob in time",
    )
    .await?;
    let bob_card = cards
        .iter()
        .find(|c| c.public_key == bob.pubkey())
        .ok_or_else(|| anyhow!("alice discovered {} cards but none was bob", cards.len()))?;
    eprintln!(
        "  alice found {} agent(s); bob is {} with caps {:?}",
        cards.len(),
        bob_card.name,
        bob_card.capabilities
    );

    eprintln!("\nStep 5: alice sends a task to bob…");
    // bob must be subscribed to its task topic before alice publishes
    bob.poll_tasks().await?;
    let task = Task::new(alice.pubkey(), bob.pubkey(), "Hello from alice");
    let acked = alice.send_task(&task).await?;
    eprintln!(
        "  task {} sent (acked={})",
        &task.id[..8.min(task.id.len())],
        acked
    );

    eprintln!("\nStep 6: bob receives + responds…");
    let received = poll_until(
        || async {
            bob.poll_tasks()
                .await
                .map(|tasks| (!tasks.is_empty(), tasks))
        },
        TASK_DELIVERY_TIMEOUT,
        "bob did not receive the task in time",
    )
    .await?;
    let received_task = received
        .iter()
        .find(|t| t.id == task.id)
        .ok_or_else(|| anyhow!("bob received tasks but not the one alice sent"))?;
    let body = received_task.text().unwrap_or("(no text)");
    eprintln!("  bob received task {}: \"{}\"", &received_task.id[..8], body);
    let response = format!("Echo: {}", body);
    bob.respond(received_task, &response).await?;
    eprintln!("  bob responded: \"{}\"", response);

    eprintln!("\nStep 7: alice polls for the response…");
    let alice_results = poll_until(
        || async {
            alice
                .poll_tasks()
                .await
                .map(|tasks| (tasks.iter().any(|t| t.result_text().is_some()), tasks))
        },
        RESPONSE_TIMEOUT,
        "alice did not receive bob's response in time",
    )
    .await?;
    let result = alice_results
        .iter()
        .find_map(|t| t.result_text())
        .ok_or_else(|| anyhow!("alice polled but no result text on any task"))?;
    eprintln!("  alice got: \"{}\"", result);

    if result != response {
        return Err(anyhow!(
            "response mismatch: bob sent {:?}, alice got {:?}",
            response,
            result
        ));
    }

    eprintln!("\n=== Demo OK ===");
    Ok(())
}

async fn make_transport(tcp: u16, udp: u16) -> Result<LogosDeliveryTransport> {
    let mut cfg = NodeConfig::logos_dev();
    cfg.tcp_port = Some(tcp);
    cfg.discv5_udp_port = Some(udp);
    Ok(LogosDeliveryTransport::new(cfg).await?)
}

/// Poll a fallible operation until it returns `(true, value)` or the
/// deadline elapses. Returns the final `value` once the predicate is
/// satisfied.
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
