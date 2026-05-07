use anyhow::{Context, Result};
use logos_messaging_a2a_crypto::AgentIdentity;
use logos_messaging_a2a_node::LmaoNode;
use logos_messaging_a2a_transport::Transport;
use std::path::{Path, PathBuf};
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
        let node = LmaoNode::from_keyfile(name, description, capabilities, transport, path)?;
        // Sealed presence needs an X25519 identity. When the user has
        // a keyfile, auto-load/generate a sidecar `.x25519` next to it
        // so encryption pubkey is stable across restarts. This makes
        // the friend-keyring exchange one-time: nicknames don't change
        // identity hex.
        let x25519_path = sidecar_x25519_path(path);
        let x_identity = load_or_create_x25519(&x25519_path)?;
        node.with_identity(x_identity)
    } else if identity.encrypt {
        LmaoNode::new_encrypted(name, description, capabilities, transport)
    } else {
        LmaoNode::new(name, description, capabilities, transport)
    };
    Ok(node)
}

fn sidecar_x25519_path(keyfile: &Path) -> PathBuf {
    let mut p = keyfile.to_path_buf();
    let ext = match keyfile.extension().and_then(|e| e.to_str()) {
        Some(e) => format!("{e}.x25519"),
        None => "x25519".to_string(),
    };
    p.set_extension(ext);
    p
}

fn load_or_create_x25519(path: &Path) -> Result<AgentIdentity> {
    if path.exists() {
        let hex = std::fs::read_to_string(path)
            .with_context(|| format!("reading X25519 sidecar {}", path.display()))?;
        AgentIdentity::from_hex(hex.trim())
            .with_context(|| format!("parsing X25519 secret in {}", path.display()))
    } else {
        let id = AgentIdentity::generate();
        write_secret_file(path, &id.secret_hex())
            .with_context(|| format!("writing X25519 sidecar {}", path.display()))?;
        Ok(id)
    }
}

fn write_secret_file(path: &Path, hex_secret: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::File::create(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(hex_secret.as_bytes())?;
    Ok(())
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
