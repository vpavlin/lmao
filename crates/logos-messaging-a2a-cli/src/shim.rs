//! Per-process LogosAPI shim construction.
//!
//! Both `--transport delivery-module` and `--storage storage-module` need
//! the same `Arc<Shim>`. Boot it once in `main` and pass the handle to
//! the two builders — that keeps the shim's Qt event-loop thread to a
//! single-instance and makes both backends share the same LogosAPI
//! consumer identity in the registry.

use anyhow::{Context, Result};
use std::sync::Arc;

use logos_core_bindings::Shim;

use crate::cli::{Cli, StorageKind, TransportKind};

/// Whether any selected backend would need the shim. Lets `main` skip
/// construction (and the noisy Qt-thread spin-up) when neither
/// shim-based variant is active.
pub fn cli_needs_shim(cli: &Cli) -> bool {
    let transport_needs = matches!(cli.transport, TransportKind::DeliveryModule);
    let storage_needs = matches!(cli.storage, StorageKind::StorageModule);
    transport_needs || storage_needs
}

/// Apply logos-core-native defaults when running inside a logos_host.
///
/// If `LOGOS_INSTANCE_ID` is set (the variable logos_host injects into
/// every child process) and the binary was compiled with real shim
/// support, switch any still-at-default transport/storage to the shim
/// variants so the spawned agent shares the host's Waku + Codex nodes
/// instead of spinning up its own.
///
/// Explicit `--transport` / `--storage` flags always win; this only
/// fires when the user left those at their compiled defaults.
pub fn apply_logos_core_defaults(cli: &mut Cli) {
    if !logos_core_bindings::is_real_build() {
        return;
    }
    if std::env::var("LOGOS_INSTANCE_ID").is_err() {
        return;
    }
    if cli.transport == TransportKind::default() {
        cli.transport = TransportKind::DeliveryModule;
    }
    if cli.storage == StorageKind::None {
        cli.storage = StorageKind::StorageModule;
    }
}

/// Boot the shim. The module name is what the LogosAPI registry shows
/// for this consumer in logs — using the binary name keeps Basecamp's
/// own log streams readable when both sides are running.
pub fn build(_cli: &Cli) -> Result<Arc<Shim>> {
    if !logos_core_bindings::is_real_build() {
        anyhow::bail!(
            "shim backend requested but `logos-core-bindings` was built in stub mode — \
             rebuild this crate with `LOGOS_CPP_SDK_DIR` pointing at a logos-cpp-sdk checkout"
        );
    }
    let shim = Shim::new("lmao-cli")
        .with_context(|| "failed to construct LogosAPI shim — is logos-core / Basecamp running?")?;
    Ok(Arc::new(shim))
}
