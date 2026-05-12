//! Integration tests for [`DeliveryModuleTransport`] against a live logos_host.
//!
//! Requirements:
//!  - Binary compiled with `--features delivery-module` AND `LOGOS_CPP_SDK_DIR`
//!    set at build time (logos-core-bindings in real mode).
//!  - `logoscore` (or Basecamp) running with `delivery_module` loaded.
//!  - `LOGOS_INSTANCE_ID` set in the environment (logoscore sets this
//!    automatically for all child processes; set it manually for bare shells).
//!
//! Run with:
//!   LOGOS_INSTANCE_ID=<id> \
//!   cargo test -p logos-messaging-a2a-transport \
//!     --features delivery-module \
//!     --test shim_integration -- --ignored

use logos_messaging_a2a_transport::{DeliveryModuleTransport, Transport};
use logos_core_bindings::Shim;
use std::sync::Arc;

/// Returns `(shim, delivery_cfg_json)` when the environment is suitable for
/// a shim integration test, or prints a skip message and returns `None`.
fn require_shim_env() -> Option<(Arc<Shim>, String)> {
    if !logos_core_bindings::is_real_build() {
        eprintln!("skip: logos-core-bindings is in stub mode \
                   (rebuild with LOGOS_CPP_SDK_DIR set)");
        return None;
    }
    if std::env::var("LOGOS_INSTANCE_ID").is_err() {
        eprintln!("skip: LOGOS_INSTANCE_ID not set — \
                   start logoscore with delivery_module loaded first");
        return None;
    }
    // Allow the test to use an explicit config; fall back to the logos.dev preset.
    let cfg = std::env::var("LMAO_TEST_DELIVERY_CFG")
        .unwrap_or_else(|_| {
            r#"{"logLevel":"WARN","mode":"Core","preset":"logos.dev"}"#.to_string()
        });
    match Shim::new("lmao-transport-test") {
        Ok(s) => Some((Arc::new(s), cfg)),
        Err(e) => {
            eprintln!("skip: Shim::new failed: {e} \
                       (is logos_host / logoscore running?)");
            None
        }
    }
}

/// Publish a message and receive it on the same transport instance.
///
/// delivery_module routes messages through the Waku gossip mesh, so the
/// subscriber must already be registered before publish fires (no in-process
/// loopback). The test waits up to 30 s — typical Waku round-trip on
/// logos.dev is <1 s once the node has peers.
#[tokio::test]
#[ignore]
async fn publish_subscribe_roundtrip() {
    let Some((shim, cfg)) = require_shim_env() else { return };

    let transport = DeliveryModuleTransport::new(shim, &cfg)
        .await
        .expect("DeliveryModuleTransport::new failed");

    let topic = "/lmao/test/shim-roundtrip/1";
    let payload = b"hello from shim integration test";

    let mut rx = transport.subscribe(topic).await.expect("subscribe failed");
    transport.publish(topic, payload).await.expect("publish failed");

    let received = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        rx.recv(),
    )
    .await
    .expect("timed out waiting for published message — is delivery_module connected to peers?")
    .expect("channel closed unexpectedly");

    assert_eq!(received.as_slice(), payload.as_slice());
}

/// Subscribe to a topic, publish two distinct messages, verify both arrive
/// in order.
#[tokio::test]
#[ignore]
async fn publish_multiple_messages_received_in_order() {
    let Some((shim, cfg)) = require_shim_env() else { return };

    let transport = DeliveryModuleTransport::new(shim, &cfg)
        .await
        .expect("DeliveryModuleTransport::new failed");

    let topic = "/lmao/test/shim-multi/1";
    let mut rx = transport.subscribe(topic).await.expect("subscribe failed");

    transport.publish(topic, b"first").await.expect("publish 1 failed");
    transport.publish(topic, b"second").await.expect("publish 2 failed");

    let timeout = std::time::Duration::from_secs(30);

    let msg1 = tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("timed out on first message")
        .expect("channel closed");
    assert_eq!(msg1.as_slice(), b"first");

    let msg2 = tokio::time::timeout(timeout, rx.recv())
        .await
        .expect("timed out on second message")
        .expect("channel closed");
    assert_eq!(msg2.as_slice(), b"second");
}

/// Unsubscribe and verify no further messages arrive on the closed channel.
#[tokio::test]
#[ignore]
async fn unsubscribe_stops_delivery() {
    let Some((shim, cfg)) = require_shim_env() else { return };

    let transport = DeliveryModuleTransport::new(shim, &cfg)
        .await
        .expect("DeliveryModuleTransport::new failed");

    let topic = "/lmao/test/shim-unsub/1";
    let mut rx = transport.subscribe(topic).await.expect("subscribe failed");
    transport.unsubscribe(topic).await.expect("unsubscribe failed");

    // A message published after unsubscribe should NOT arrive.
    transport.publish(topic, b"should-not-arrive").await.expect("publish failed");

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        rx.recv(),
    )
    .await;

    // Either timeout (no message) or channel-closed is acceptable;
    // a received message means unsubscribe didn't stop delivery.
    match result {
        Err(_timeout) => { /* good — nothing arrived */ }
        Ok(None) => { /* channel closed — also acceptable */ }
        Ok(Some(msg)) => {
            panic!("received message after unsubscribe: {:?}", String::from_utf8_lossy(&msg));
        }
    }
}
