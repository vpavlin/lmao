//! Streaming chunks over real Logos Messaging.
//!
//! Two embedded LogosDeliveryTransport nodes — alice (the listener) polls
//! the stream topic for incremental chunks while bob (the streamer)
//! publishes chunks via `respond_stream`. Verifies that all chunks arrive
//! in order and the reassembly produces the expected text.
//!
//! Run:
//! ```bash
//! LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build \
//!     LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR \
//!     cargo run --features logos-delivery --example logos_delivery_streaming
//! ```

use anyhow::{anyhow, Result};
use logos_messaging_a2a::{LmaoNode, Task, Transport};
use logos_messaging_a2a_transport::logos_delivery::{LogosDeliveryTransport, NodeConfig};
use std::sync::Arc;
use std::time::{Duration, Instant};

const PEER_DISCOVERY_WAIT: Duration = Duration::from_secs(5);
const SUBSCRIPTION_PROPAGATION_WAIT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const STREAM_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    eprintln!("=== LMAO streaming demo over Logos Messaging ===\n");

    let transport_a = Arc::new(make_transport(60030, 9030).await?) as Arc<dyn Transport>;
    let transport_b = Arc::new(make_transport(60031, 9031).await?) as Arc<dyn Transport>;
    let alice = LmaoNode::new(
        "alice",
        "listener",
        vec!["text".into()],
        transport_a.clone(),
    );
    let bob = LmaoNode::new(
        "bob",
        "streamer",
        vec!["text".into(), "stream".into()],
        transport_b.clone(),
    );
    eprintln!(
        "  alice: {}\n  bob:   {}\n",
        &alice.pubkey()[..16],
        &bob.pubkey()[..16]
    );

    eprintln!(
        "Step 1: waiting {}s for gossip mesh…",
        PEER_DISCOVERY_WAIT.as_secs()
    );
    tokio::time::sleep(PEER_DISCOVERY_WAIT).await;

    // Use a fixed task id so both sides agree on which stream topic to use.
    // Real flow: alice would have sent bob a Task with this id and bob
    // would respond_stream against it. Here we cut the announce/discover
    // dance and just exchange the id out-of-band for a focused demo.
    let task = Task::new(alice.pubkey(), bob.pubkey(), "stream me");
    eprintln!(
        "Step 2: alice opens the stream subscription for task {}…",
        &task.id[..8]
    );
    let _ = alice.poll_stream_chunks(&task.id).await?;
    tokio::time::sleep(SUBSCRIPTION_PROPAGATION_WAIT).await;

    let chunks = vec![
        "Once ".to_string(),
        "upon ".to_string(),
        "a time, ".to_string(),
        "two agents ".to_string(),
        "spoke ".to_string(),
        "across ".to_string(),
        "Logos ".to_string(),
        "Messaging.".to_string(),
    ];
    let expected: String = chunks.join("");
    eprintln!("Step 3: bob respond_streams {} chunk(s)…", chunks.len());
    bob.respond_stream(&task, chunks.clone()).await?;

    eprintln!(
        "Step 4: alice drains chunks until is_final=true (timeout {}s)…",
        STREAM_TIMEOUT.as_secs()
    );
    let deadline = Instant::now() + STREAM_TIMEOUT;
    loop {
        let received = alice.poll_stream_chunks(&task.id).await?;
        if received.iter().any(|c| c.is_final) {
            eprintln!("  reassembling {} chunks…", received.len());
            break;
        }
        if Instant::now() >= deadline {
            return Err(anyhow!(
                "no final chunk within {}s ({} chunks buffered)",
                STREAM_TIMEOUT.as_secs(),
                received.len()
            ));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    let assembled = alice
        .reassemble_stream(&task.id)
        .ok_or_else(|| anyhow!("reassemble_stream returned None"))?;
    eprintln!("  reassembled: \"{}\"", assembled);

    if assembled != expected {
        return Err(anyhow!(
            "reassembled mismatch: expected {:?}, got {:?}",
            expected,
            assembled
        ));
    }

    eprintln!("\n=== Streaming OK ===");
    Ok(())
}

async fn make_transport(tcp: u16, udp: u16) -> Result<LogosDeliveryTransport> {
    let mut cfg = NodeConfig::logos_dev();
    cfg.tcp_port = Some(tcp);
    cfg.discv5_udp_port = Some(udp);
    Ok(LogosDeliveryTransport::new(cfg).await?)
}
