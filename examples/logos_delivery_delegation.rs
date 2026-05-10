//! Multi-agent delegation over real Logos Messaging.
//!
//! Three agents:
//! - **orchestrator** (no special capability)
//! - **alice** (capability `text`)
//! - **bob**   (capability `code`) — the intended target
//!
//! All three announce presence. The orchestrator polls presence to
//! populate its peer map, then delegates a `CapabilityMatch { "code" }`
//! subtask. The matching worker (bob) runs a small echo loop in a
//! background task and responds. The orchestrator's `delegate_task`
//! returns the success+result.
//!
//! Run:
//! ```bash
//! LIBLOGOSDELIVERY_LIB_DIR=/path/to/logos-delivery/build \
//!     LD_LIBRARY_PATH=$LIBLOGOSDELIVERY_LIB_DIR \
//!     cargo run --features logos-delivery --example logos_delivery_delegation
//! ```

use anyhow::{anyhow, Result};
use logos_messaging_a2a::{DelegationRequest, DelegationStrategy, LmaoNode, Transport};
use logos_messaging_a2a_transport::logos_delivery::{LogosDeliveryTransport, NodeConfig};
use std::sync::Arc;
use std::time::Duration;

const PEER_DISCOVERY_WAIT: Duration = Duration::from_secs(5);
const PRESENCE_PROPAGATION_WAIT: Duration = Duration::from_secs(3);
const DELEGATION_TIMEOUT_SECS: u64 = 25;

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<()> {
    eprintln!("=== LMAO delegation demo over Logos Messaging ===\n");

    let t_orch = Arc::new(make_transport(60040, 9040).await?) as Arc<dyn Transport>;
    let t_alice = Arc::new(make_transport(60041, 9041).await?) as Arc<dyn Transport>;
    let t_bob = Arc::new(make_transport(60042, 9042).await?) as Arc<dyn Transport>;

    let orchestrator = Arc::new(LmaoNode::new(
        "orchestrator",
        "delegating coordinator",
        vec![],
        t_orch.clone(),
    ));
    let alice = Arc::new(LmaoNode::new(
        "alice",
        "writes text",
        vec!["text".into()],
        t_alice.clone(),
    ));
    let bob = Arc::new(LmaoNode::new(
        "bob",
        "writes code",
        vec!["code".into()],
        t_bob.clone(),
    ));
    eprintln!(
        "  orchestrator: {}\n  alice (text): {}\n  bob   (code): {}\n",
        &orchestrator.pubkey()[..16],
        &alice.pubkey()[..16],
        &bob.pubkey()[..16]
    );

    eprintln!(
        "Step 1: waiting {}s for gossip mesh…",
        PEER_DISCOVERY_WAIT.as_secs()
    );
    tokio::time::sleep(PEER_DISCOVERY_WAIT).await;

    // Open presence subscriptions BEFORE announcing — same lesson as
    // discovery: real gossip transports don't buffer pre-subscribe.
    eprintln!("Step 2: open presence subscriptions and announce…");
    let _ = orchestrator.poll_presence().await?;
    let _ = alice.poll_presence().await?;
    let _ = bob.poll_presence().await?;
    orchestrator.announce_presence().await?;
    alice.announce_presence().await?;
    bob.announce_presence().await?;
    tokio::time::sleep(PRESENCE_PROPAGATION_WAIT).await;
    let count = orchestrator.poll_presence().await?;
    eprintln!("  orchestrator now sees {} live peer(s)", count);
    if count < 2 {
        return Err(anyhow!(
            "expected ≥2 peers after presence gossip; got {}",
            count
        ));
    }

    // Spawn an echo loop on bob — responds to any task it receives.
    eprintln!("Step 3: bob starts echo loop (responds to any task)…");
    let bob_loop = bob.clone();
    let echo = tokio::spawn(async move {
        let _ = bob_loop.poll_tasks().await; // open subscription
        let deadline = tokio::time::Instant::now() + Duration::from_secs(DELEGATION_TIMEOUT_SECS);
        loop {
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            if let Ok(tasks) = bob_loop.poll_tasks().await {
                for task in tasks {
                    if let Some(text) = task.text() {
                        let _ = bob_loop.respond(&task, &format!("Echo: {text}")).await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    });

    eprintln!("Step 4: orchestrator delegates with CapabilityMatch {{ code }}…");
    let request = DelegationRequest {
        parent_task_id: "demo-parent".to_string(),
        subtask_text: "implement quicksort".to_string(),
        strategy: DelegationStrategy::CapabilityMatch {
            capability: "code".into(),
        },
        timeout_secs: DELEGATION_TIMEOUT_SECS,
        session_id: None,
    };
    let result = orchestrator.delegate_task(&request).await?;
    eprintln!(
        "  result: success={} agent={} result={:?}",
        result.success,
        &result.agent_id[..16.min(result.agent_id.len())],
        result.result_text
    );

    if !result.success {
        return Err(anyhow!(
            "delegation failed: {}",
            result.error.unwrap_or_default()
        ));
    }
    if result.agent_id != bob.pubkey() {
        return Err(anyhow!(
            "delegated to wrong peer: expected bob ({}), got {}",
            &bob.pubkey()[..16],
            &result.agent_id[..16.min(result.agent_id.len())]
        ));
    }
    let body = result
        .result_text
        .ok_or_else(|| anyhow!("no result_text on success"))?;
    if !body.contains("implement quicksort") {
        return Err(anyhow!("unexpected result body: {body:?}"));
    }

    echo.abort();

    eprintln!("\n=== Delegation OK ===");
    Ok(())
}

async fn make_transport(tcp: u16, udp: u16) -> Result<LogosDeliveryTransport> {
    let mut cfg = NodeConfig::logos_dev();
    cfg.tcp_port = Some(tcp);
    cfg.discv5_udp_port = Some(udp);
    Ok(LogosDeliveryTransport::new(cfg).await?)
}
