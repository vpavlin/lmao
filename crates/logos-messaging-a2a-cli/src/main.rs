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
#[cfg(feature = "shim")]
mod shim;
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

    // Die when the parent process dies. Without this, when a host (e.g.
    // Basecamp's logos_host) gets `kill -9`'d before its destructor can
    // tear down our QProcess child cleanly, the lmao subprocess is
    // orphaned and keeps holding its libp2p ports — blocking the next
    // launch with "Address already in use". Linux only; harmless on
    // other platforms (NOP).
    #[cfg(target_os = "linux")]
    unsafe {
        // PR_SET_PDEATHSIG = 1, SIGTERM = 15. Posix const not in libc
        // crate's stable surface, hard-coded values are fine here.
        libc::prctl(
            1,  /* PR_SET_PDEATHSIG */
            15, /* SIGTERM */
            0, 0, 0,
        );
    }

    #[allow(unused_mut)]
    let mut cli = Cli::parse();
    // When running inside a logos_host (LOGOS_INSTANCE_ID is set), prefer
    // the shim backends so this process shares the host's Waku + Codex
    // nodes instead of spinning up its own. Explicit --transport / --storage
    // flags on the command line override this.
    #[cfg(feature = "shim")]
    shim::apply_logos_core_defaults(&mut cli);
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

    // Optional shim — only built when a shim-backed transport or
    // storage variant is selected. Constructed once and shared so both
    // backends drive the same LogosAPI consumer / Qt event loop.
    #[cfg(feature = "shim")]
    let shim_handle = if !daemon_can_handle && shim::cli_needs_shim(&cli) {
        Some(shim::build(&cli)?)
    } else {
        None
    };

    let transport: Arc<dyn Transport> = if daemon_can_handle {
        // Placeholder. None of the daemon-aware code paths actually
        // touch this transport — the handlers route through IPC and
        // return before falling back to it.
        Arc::new(logos_messaging_a2a_transport::memory::InMemoryTransport::new())
    } else {
        build_transport(
            &cli,
            #[cfg(feature = "shim")]
            shim_handle.clone(),
        )
        .await?
    };
    let storage: Option<Arc<dyn StorageBackend>> = if daemon_can_handle {
        None
    } else {
        build_storage(
            &cli,
            #[cfg(feature = "shim")]
            shim_handle.clone(),
        )
        .await?
    };

    match cli.command {
        Commands::Agent { action } => {
            agent::handle(
                action,
                transport,
                storage,
                daemon_socket,
                &identity,
                cli.trust_file.clone(),
                cli.storage_data_dir.clone(),
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
pub(crate) async fn build_transport(
    cli: &Cli,
    #[cfg(feature = "shim")] shim: Option<Arc<logos_core_bindings::Shim>>,
) -> Result<Arc<dyn Transport>> {
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
            if !cli.entry_nodes.is_empty() {
                config.entry_nodes = cli.entry_nodes.clone();
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
        #[cfg(feature = "shim")]
        TransportKind::DeliveryModule => {
            use logos_messaging_a2a_transport::DeliveryModuleTransport;
            let shim = shim.ok_or_else(|| {
                anyhow::anyhow!("--transport delivery-module requires the shim to be available")
            })?;
            // No explicit cfg? Fall back to delivery_module's preset
            // catalog. Tries `--preset` first, then the first preset
            // the module reports; if none, errors with the catalog so
            // the user can pick.
            let cfg_json = match cli.delivery_module_cfg.clone() {
                Some(c) => c,
                None => shim_delivery_cfg_from_preset(&shim, &cli.preset).await?,
            };
            let t = DeliveryModuleTransport::new(shim, &cfg_json)
                .await
                .map_err(|e| anyhow::anyhow!("delivery_module transport: {e}"))?;
            Ok(Arc::new(t))
        }
    }
}

/// Ask `delivery_module.getAvailableConfigs()` for its preset catalog
/// and return the JSON config for `preset`. Used when the user picks
/// `--transport delivery-module` without an explicit `--delivery-module-cfg`.
#[cfg(feature = "shim")]
async fn shim_delivery_cfg_from_preset(
    shim: &logos_core_bindings::Shim,
    preset: &str,
) -> Result<String> {
    // 60 s timeout: delivery_module's first call after startup can be
    // slow if the host process is still warming up its QtRO links.
    // getAvailableConfigs itself is cheap once the wire is open.
    let raw = shim
        .call("delivery_module", "getAvailableConfigs", "[]", 60_000)
        .map_err(|e| anyhow::anyhow!("delivery_module getAvailableConfigs: {e}"))?;
    let map: serde_json::Value = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("getAvailableConfigs returned non-JSON: {raw}: {e}"))?;
    // Surface daemon-side error shapes first so the user sees the real
    // reason (timeout, module-not-loaded, etc.) instead of a wrong-
    // looking "preset not in catalog" message.
    if let Some(err) = map.get("error").and_then(serde_json::Value::as_str) {
        anyhow::bail!("delivery_module getAvailableConfigs: {err}");
    }
    if map.get("kind").and_then(serde_json::Value::as_str) == Some("error") {
        if let Some(msg) = map.get("message").and_then(serde_json::Value::as_str) {
            anyhow::bail!("delivery_module getAvailableConfigs: {msg}");
        }
    }
    // The catalog may arrive as either a {"value": {...}} envelope or
    // a bare map — accept both.
    let catalog = map.get("value").unwrap_or(&map);
    if let Some(cfg) = catalog.get(preset) {
        // Each entry is itself JSON-encoded (delivery_module historically
        // returned strings). Tolerate both shapes.
        return Ok(match cfg {
            serde_json::Value::String(s) => s.clone(),
            other => other.to_string(),
        });
    }
    let presets: Vec<String> = catalog
        .as_object()
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default();
    anyhow::bail!(
        "preset `{preset}` not in delivery_module catalog (available: {presets:?}). \
         Override with --delivery-module-cfg <json>."
    )
}

/// Construct the chosen storage backend, if any. Returns `Ok(None)` when
/// the user picked `--storage none` so callers don't need to thread a
/// dummy backend through the call sites.
#[allow(dead_code)] // also re-exported to daemon-aware fallback paths
pub(crate) async fn build_storage(
    cli: &Cli,
    #[cfg(feature = "shim")] shim: Option<Arc<logos_core_bindings::Shim>>,
) -> Result<Option<Arc<dyn StorageBackend>>> {
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
            let backend =
                LibstorageBackend::with_config(&data_dir, port, None, &cli.storage_bootstrap)
                    .await?;
            // Surface our own SPR so a peer can dial us. Best-effort —
            // a startup hiccup shouldn't break agent_run.
            if let Ok(spr) = backend.spr().await {
                eprintln!("[storage] SPR: {spr}");
            }
            Ok(Some(Arc::new(backend)))
        }
        #[cfg(feature = "shim")]
        StorageKind::StorageModule => {
            use logos_messaging_a2a_storage::StorageModuleBackend;
            let shim = shim.ok_or_else(|| {
                anyhow::anyhow!("--storage storage-module requires the shim to be available")
            })?;
            Ok(Some(Arc::new(StorageModuleBackend::new(shim))))
        }
    }
}
