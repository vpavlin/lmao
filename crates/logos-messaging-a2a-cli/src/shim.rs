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
