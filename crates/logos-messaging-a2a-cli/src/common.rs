use anyhow::Result;
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_transport::Transport;
use std::path::PathBuf;
use std::sync::Arc;

pub fn parse_capabilities(capabilities: &str) -> Vec<String> {
    capabilities
        .split(',')
        .map(|s| s.trim().to_string())
        .collect()
}

/// Identity configuration extracted from global CLI flags.
#[derive(Debug, Clone)]
pub struct IdentityConfig {
    pub keyfile: Option<PathBuf>,
    pub encrypt: bool,
}

/// Build a [`LmaoNode`] using the global identity flags.
///
/// When `--keyfile` is provided, the node loads (or creates) a persistent
/// signing key so that every CLI invocation shares the same pubkey and
/// can therefore poll for responses to tasks it previously sent.
///
/// When `--encrypt` is set, the node generates an X25519 keypair for
/// end-to-end encryption.
pub fn build_node(
    name: &str,
    description: &str,
    capabilities: Vec<String>,
    transport: Arc<dyn Transport>,
    identity: &IdentityConfig,
) -> Result<LmaoNode<Arc<dyn Transport>>> {
    let node = if let Some(ref path) = identity.keyfile {
        LmaoNode::from_keyfile(name, description, capabilities, transport, path)?
    } else if identity.encrypt {
        LmaoNode::new_encrypted(name, description, capabilities, transport)
    } else {
        LmaoNode::new(name, description, capabilities, transport)
    };
    Ok(node)
}

#[cfg(test)]
mod tests {
    use super::*;
    use logos_messaging_a2a_transport::memory::InMemoryTransport;

    #[test]
    fn parse_single_capability() {
        assert_eq!(parse_capabilities("text"), vec!["text"]);
    }

    #[test]
    fn parse_multiple_capabilities() {
        assert_eq!(
            parse_capabilities("text, code, search"),
            vec!["text", "code", "search"]
        );
    }

    #[test]
    fn build_node_ephemeral() {
        let transport: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let id = IdentityConfig {
            keyfile: None,
            encrypt: false,
        };
        let node = build_node("test", "test node", vec![], transport, &id).unwrap();
        assert!(!node.pubkey().is_empty());
    }

    #[test]
    fn build_node_encrypted() {
        let transport: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let id = IdentityConfig {
            keyfile: None,
            encrypt: true,
        };
        let node = build_node("test", "test node", vec![], transport, &id).unwrap();
        assert!(node.card.intro_bundle.is_some());
    }

    #[test]
    fn build_node_with_keyfile() {
        let dir = tempfile::tempdir().unwrap();
        let keypath = dir.path().join("test.key");
        let transport: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let id = IdentityConfig {
            keyfile: Some(keypath.clone()),
            encrypt: false,
        };
        let node1 = build_node("test", "test node", vec![], transport, &id).unwrap();
        let pk1 = node1.pubkey().to_string();

        // Second build with same keyfile should produce same pubkey
        let transport2: Arc<dyn Transport> = Arc::new(InMemoryTransport::new());
        let node2 = build_node("test", "test node", vec![], transport2, &id).unwrap();
        assert_eq!(pk1, node2.pubkey());
    }
}
