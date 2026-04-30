mod agent;
mod cli;
mod common;
mod completion;
mod daemon;
mod daemon_cmd;
mod health;
mod info;
mod metrics;
mod presence;
mod session;
mod storage;
mod task;
mod trust;

use anyhow::Result;
use clap::Parser;
use logos_messaging_a2a_storage::StorageBackend;
use logos_messaging_a2a_transport::Transport;
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Commands, StorageKind, TransportKind};
use common::IdentityConfig;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let json = cli.json;
    let identity = IdentityConfig {
        keyfile: cli.keyfile.clone(),
        encrypt: cli.encrypt,
    };

    let daemon_socket = cli.daemon_socket.clone();

    // `info` and `storage` go straight to the daemon — no point paying
    // the cost of building a transport eagerly when the daemon will (or
    // must) handle the request.
    if matches!(cli.command, Commands::Info) {
        return info::handle(&cli).await;
    }
    if let Commands::Storage { action } = cli.command {
        return storage::handle(action, daemon_socket.as_ref(), cli.json).await;
    }
    if let Commands::Daemon { action } = cli.command {
        return daemon_cmd::handle(action, daemon_socket.as_ref(), cli.json).await;
    }
    // Trust list commands are pure file IO — no transport, no daemon.
    if let Commands::Trust { action } = cli.command {
        return trust::handle(action, cli.trust_file, daemon_socket, cli.keyfile, cli.json).await;
    }

    // Daemon-aware commands (task / presence / agent discover) probe
    // the daemon socket first and short-circuit there. If a daemon is
    // listening, building an embedded transport here is just expensive
    // log noise. Skip the transport entirely in that case — handlers
    // will fall through to their own ephemeral path on a socket-miss.
    let agent_action_uses_daemon = matches!(cli.command, Commands::Agent { .. });
    let daemon_can_handle = !agent_action_uses_daemon
        && matches!(
            cli.command,
            Commands::Task { .. } | Commands::Presence { .. }
        )
        && daemon::DaemonClient::new(
            daemon_socket
                .clone()
                .unwrap_or_else(daemon::default_socket_path),
        )
        .probe()
        .await;

    let transport: Arc<dyn Transport> = if daemon_can_handle {
        // Placeholder. None of the daemon-aware code paths actually
        // touch this transport — the handlers route through IPC and
        // return before falling back to it.
        Arc::new(logos_messaging_a2a_transport::memory::InMemoryTransport::new())
    } else {
        build_transport(&cli).await?
    };
    let storage: Option<Arc<dyn StorageBackend>> = if daemon_can_handle {
        None
    } else {
        build_storage(&cli).await?
    };

    match cli.command {
        Commands::Agent { action } => {
            agent::handle(
                action,
                transport,
                storage,
                daemon_socket,
                &identity,
                cli.trust_file,
                json,
            )
            .await
        }
        Commands::Task { action } => {
            task::handle(action, transport, daemon_socket.as_ref(), &identity, json).await
        }
        Commands::Presence { action } => {
            presence::handle(action, transport, daemon_socket.as_ref(), &identity, json).await
        }
        Commands::Session { action } => session::handle(action, transport, &identity, json).await,
        Commands::Health => health::handle(&cli.waku, json).await,
        Commands::Metrics => metrics::handle(transport, &identity, json).await,
        Commands::Completion { shell } => {
            completion::handle(shell);
            Ok(())
        }
        Commands::Info => unreachable!("handled above"),
        Commands::Storage { .. } => unreachable!("handled above"),
        Commands::Daemon { .. } => unreachable!("handled above"),
        Commands::Trust { .. } => unreachable!("handled above"),
    }
}

/// Construct the chosen transport, boxed as `Arc<dyn Transport>` so all
/// command handlers can share a single signature. Also used by daemon-
/// aware handlers (e.g. `info`) on the fallback path when no daemon is
/// listening.
pub(crate) async fn build_transport(cli: &Cli) -> Result<Arc<dyn Transport>> {
    match cli.transport {
        #[cfg(feature = "logos-delivery")]
        TransportKind::LogosDelivery => {
            use logos_messaging_a2a_transport::logos_delivery::{
                LogosDeliveryTransport, NodeConfig,
            };
            let mut config = NodeConfig::logos_dev();
            config.preset = Some(cli.preset.clone());
            if cli.tcp_port != 0 {
                config.tcp_port = Some(cli.tcp_port);
            }
            if cli.udp_port != 0 {
                config.discv5_udp_port = Some(cli.udp_port);
            }
            // Quieter by default for CLI users — the libp2p / nim-waku INFO
            // stream is great for debugging and noisy on stdout. Override
            // with LMAO_NODE_LOG_LEVEL when triaging connection issues.
            config.log_level =
                Some(std::env::var("LMAO_NODE_LOG_LEVEL").unwrap_or_else(|_| "WARN".to_string()));
            let t = LogosDeliveryTransport::new(config).await?;
            Ok(Arc::new(t))
        }
        #[cfg(feature = "rest")]
        TransportKind::Rest => {
            use logos_messaging_a2a_transport::nwaku_rest::LogosMessagingTransport;
            Ok(Arc::new(LogosMessagingTransport::new(&cli.waku)))
        }
    }
}

/// Construct the chosen storage backend, if any. Returns `Ok(None)` when
/// the user picked `--storage none` so callers don't need to thread a
/// dummy backend through the call sites.
#[allow(dead_code)] // also re-exported to daemon-aware fallback paths
pub(crate) async fn build_storage(cli: &Cli) -> Result<Option<Arc<dyn StorageBackend>>> {
    match cli.storage {
        StorageKind::None => Ok(None),
        #[cfg(feature = "libstorage")]
        StorageKind::Libstorage => {
            use logos_messaging_a2a_storage::LibstorageBackend;
            // If the user didn't pin a data dir, scope an ephemeral one
            // to this process so concurrent agents on one host don't
            // clobber each other's blockstore.
            let data_dir = match cli.storage_data_dir.clone() {
                Some(p) => p,
                None => std::env::temp_dir().join(format!("lmao-storage-{}", std::process::id())),
            };
            let port = if cli.storage_port == 0 {
                None
            } else {
                Some(cli.storage_port)
            };
            let backend = LibstorageBackend::with_config(&data_dir, port, None).await?;
            Ok(Some(Arc::new(backend)))
        }
    }
}
