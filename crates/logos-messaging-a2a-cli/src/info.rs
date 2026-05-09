use anyhow::Result;
use logos_messaging_a2a_core::topics;

use crate::cli::Cli;
use crate::common::{build_node, IdentityConfig};
use crate::daemon::{default_socket_path, DaemonClient, Request, Response};

/// Display agent identity and topic configuration.
///
/// Prefers the running daemon when available — that way `lmao info`
/// returns the *daemon's* identity (the one actually broadcasting on the
/// network) rather than spinning up an ephemeral node with a different
/// pubkey. Falls back to the ephemeral path when no daemon is listening.
pub async fn handle(cli: &Cli) -> Result<()> {
    let socket = cli
        .daemon_socket
        .clone()
        .unwrap_or_else(default_socket_path);
    let client = DaemonClient::new(socket);

    if client.probe().await {
        return print_via_daemon(&client, cli.json).await;
    }

    print_via_ephemeral(cli).await
}

async fn print_via_daemon(client: &DaemonClient, json: bool) -> Result<()> {
    let resp = client.send(Request::Info).await?;
    let Response::Info {
        name,
        pubkey,
        capabilities,
        uptime_secs,
        socket_path,
        storage_enabled,
        encryption_pubkey,
        load,
    } = resp
    else {
        anyhow::bail!("unexpected response variant from daemon: {resp:?}");
    };

    let task_topic = topics::task_topic(&pubkey);
    if json {
        let mut obj = serde_json::json!({
            "source": "daemon",
            "socket": socket_path,
            "name": name,
            "public_key": pubkey,
            "capabilities": capabilities,
            "uptime_secs": uptime_secs,
            "storage_enabled": storage_enabled,
            "task_topic": task_topic,
            "discovery_topic": topics::DISCOVERY,
            "presence_topic": topics::PRESENCE,
        });
        if let Some(ref ep) = encryption_pubkey {
            obj["encryption_pubkey"] = serde_json::json!(ep);
        }
        if let Some(ref l) = load {
            obj["load"] = serde_json::json!({
                "bucket": l.bucket,
                "queue_depth": l.queue_depth,
                "max_concurrent": l.max_concurrent,
            });
        }
        println!("{}", serde_json::to_string(&obj)?);
    } else {
        eprintln!("Source: daemon ({})", socket_path.display());
        println!("Agent name:      {name}");
        println!("Public key:      {pubkey}");
        if let Some(ref ep) = encryption_pubkey {
            println!("X25519 pubkey:   {ep}");
        }
        println!("Capabilities:    {}", capabilities.join(", "));
        println!("Task topic:      {task_topic}");
        println!("Discovery topic: {}", topics::DISCOVERY);
        println!("Presence topic:  {}", topics::PRESENCE);
        println!(
            "Storage:         {}",
            if storage_enabled {
                "enabled"
            } else {
                "disabled"
            }
        );
        if let Some(ref l) = load {
            println!(
                "Load:            {} (queue {}/{})",
                l.bucket, l.queue_depth, l.max_concurrent
            );
        }
        println!("Uptime:          {uptime_secs}s");
    }
    Ok(())
}

async fn print_via_ephemeral(cli: &Cli) -> Result<()> {
    let transport = crate::build_transport(cli).await?;
    let identity = IdentityConfig {
        keyfile: cli.keyfile.clone(),
        encrypt: cli.encrypt,
    };
    let node = build_node("info", "info command", vec![], transport, &identity)?;
    let pubkey = node.pubkey().to_string();
    let task_topic = topics::task_topic(&pubkey);
    let encrypt = identity.encrypt || node.card.intro_bundle.is_some();

    if cli.json {
        let obj = serde_json::json!({
            "source": "ephemeral",
            "public_key": pubkey,
            "task_topic": task_topic,
            "discovery_topic": topics::DISCOVERY,
            "presence_topic": topics::PRESENCE,
            "encryption": encrypt,
        });
        println!("{}", serde_json::to_string(&obj)?);
    } else {
        eprintln!("Source: ephemeral (no daemon listening)");
        if let Some(ref kf) = identity.keyfile {
            eprintln!("Keyfile: {}", kf.display());
        } else {
            eprintln!("Identity: ephemeral (use --keyfile for persistent identity)");
        }
        println!("Public key:      {pubkey}");
        println!("Task topic:      {task_topic}");
        println!("Discovery topic: {}", topics::DISCOVERY);
        println!("Presence topic:  {}", topics::PRESENCE);
        println!(
            "Encryption:      {}",
            if encrypt { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use logos_messaging_a2a_transport::memory::InMemoryTransport;
    use logos_messaging_a2a_transport::Transport;
    use std::sync::Arc;

    /// Helper: build a node from an in-memory transport and return the
    /// info fields the ephemeral path would print, as JSON.
    fn info_json(encrypt: bool) -> serde_json::Value {
        let transport: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let identity = IdentityConfig {
            keyfile: None,
            encrypt,
        };
        let node = build_node("info", "info command", vec![], transport, &identity).unwrap();
        let pubkey = node.pubkey().to_string();
        serde_json::json!({
            "source": "ephemeral",
            "public_key": pubkey,
            "task_topic": topics::task_topic(&pubkey),
            "discovery_topic": topics::DISCOVERY,
            "presence_topic": topics::PRESENCE,
            "encryption": encrypt || node.card.intro_bundle.is_some(),
        })
    }

    #[test]
    fn json_output_contains_all_fields() {
        let obj = info_json(false);
        assert!(obj["public_key"].is_string());
        assert!(obj["task_topic"]
            .as_str()
            .unwrap()
            .contains("/lmao/1/task-"));
        assert_eq!(obj["discovery_topic"], topics::DISCOVERY);
        assert_eq!(obj["presence_topic"], topics::PRESENCE);
        assert_eq!(obj["encryption"], false);
    }

    #[test]
    fn json_output_encryption_enabled() {
        let obj = info_json(true);
        assert_eq!(obj["encryption"], true);
    }

    #[test]
    fn json_output_is_parseable() {
        let obj = info_json(false);
        let serialized = serde_json::to_string(&obj).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["public_key"], obj["public_key"]);
    }

    #[test]
    fn task_topic_includes_pubkey() {
        let obj = info_json(false);
        let pubkey = obj["public_key"].as_str().unwrap();
        let task_topic = obj["task_topic"].as_str().unwrap();
        assert!(task_topic.contains(pubkey));
    }

    #[test]
    fn keyfile_identity_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let keypath = dir.path().join("test.key");
        let transport1: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let identity = IdentityConfig {
            keyfile: Some(keypath.clone()),
            encrypt: false,
        };
        let node1 = build_node("info", "test", vec![], transport1, &identity).unwrap();
        let pk1 = node1.pubkey().to_string();

        let transport2: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let node2 = build_node("info", "test", vec![], transport2, &identity).unwrap();
        let pk2 = node2.pubkey().to_string();

        assert_eq!(pk1, pk2);
    }
}
