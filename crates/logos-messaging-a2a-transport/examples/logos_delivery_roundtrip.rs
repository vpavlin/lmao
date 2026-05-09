//! End-of-Day-1 smoke test: two embedded Logos Messaging nodes connected
//! to the `logos.dev` fleet via `liblogosdelivery`, exchanging one message
//! over a unique content topic.
//!
//! Run:
//!
//! ```bash
//! LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build \
//!     LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR \
//!     cargo run -p logos-messaging-a2a-transport \
//!       --features logos-delivery \
//!       --example logos_delivery_roundtrip
//! ```
//!
//! Expected output ends with `✓ roundtrip OK`. Exits non-zero on failure.

use logos_messaging_a2a_transport::logos_delivery::{LogosDeliveryTransport, NodeConfig};
use logos_messaging_a2a_transport::Transport;
use std::time::Duration;
use tokio::time::timeout;
use uuid::Uuid;

const PEER_DISCOVERY_WAIT: Duration = Duration::from_secs(5);
const SUBSCRIPTION_PROPAGATION_WAIT: Duration = Duration::from_secs(2);
const RECV_TIMEOUT: Duration = Duration::from_secs(20);

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    eprintln!("[smoke] starting two LogosDeliveryTransport nodes on logos.dev preset");

    let mut cfg_a = NodeConfig::logos_dev();
    cfg_a.tcp_port = Some(60010);
    cfg_a.discv5_udp_port = Some(9010);

    let mut cfg_b = NodeConfig::logos_dev();
    cfg_b.tcp_port = Some(60011);
    cfg_b.discv5_udp_port = Some(9011);

    eprintln!("[smoke] creating alice (tcp=60010, udp=9010)");
    let alice = LogosDeliveryTransport::new(cfg_a).await?;
    eprintln!("[smoke] alice up");

    eprintln!("[smoke] creating bob (tcp=60011, udp=9011)");
    let bob = LogosDeliveryTransport::new(cfg_b).await?;
    eprintln!("[smoke] bob up");

    eprintln!(
        "[smoke] waiting {}s for peer discovery / mesh formation…",
        PEER_DISCOVERY_WAIT.as_secs()
    );
    tokio::time::sleep(PEER_DISCOVERY_WAIT).await;

    // Format: /{app}/{generation}/{name}/{encoding} — generation must be numeric.
    let topic = format!("/lmao/1/smoke-{}/proto", Uuid::new_v4());
    eprintln!("[smoke] topic: {}", topic);

    let mut rx = bob.subscribe(&topic).await?;
    eprintln!("[smoke] bob subscribed");

    tokio::time::sleep(SUBSCRIPTION_PROPAGATION_WAIT).await;

    let payload = b"hello from alice";
    alice.publish(&topic, payload).await?;
    eprintln!("[smoke] alice published {} bytes", payload.len());

    match timeout(RECV_TIMEOUT, rx.recv()).await {
        Ok(Some(received)) => {
            eprintln!("[smoke] bob received {} bytes", received.len());
            if received != payload {
                return Err(format!(
                    "payload mismatch: expected {:?}, got {:?}",
                    payload, received
                )
                .into());
            }
            eprintln!("[smoke] ✓ roundtrip OK");
        }
        Ok(None) => return Err("subscription channel closed unexpectedly".into()),
        Err(_) => {
            return Err(format!(
                "no message received within {}s — peer discovery likely failed",
                RECV_TIMEOUT.as_secs()
            )
            .into())
        }
    }

    alice.shutdown().await?;
    bob.shutdown().await?;
    eprintln!("[smoke] shutdown complete");

    Ok(())
}
