//! Integration tests for [`StorageModuleBackend`] against a live logos_host.
//!
//! Requirements:
//!  - Binary compiled with `--features storage-module` AND `LOGOS_CPP_SDK_DIR`
//!    set at build time (logos-core-bindings in real mode).
//!  - `logoscore` (or Basecamp) running with `storage_module` loaded.
//!  - `LOGOS_INSTANCE_ID` set in the environment.
//!
//! Run with:
//!   LOGOS_INSTANCE_ID=<id> \
//!   cargo test -p logos-messaging-a2a-storage \
//!     --features storage-module \
//!     --test storage_module_integration -- --ignored

use logos_messaging_a2a_storage::{StorageBackend, StorageModuleBackend};
use logos_core_bindings::Shim;
use std::sync::Arc;

fn require_shim_env() -> Option<Arc<Shim>> {
    if !logos_core_bindings::is_real_build() {
        eprintln!("skip: logos-core-bindings is in stub mode \
                   (rebuild with LOGOS_CPP_SDK_DIR set)");
        return None;
    }
    if std::env::var("LOGOS_INSTANCE_ID").is_err() {
        eprintln!("skip: LOGOS_INSTANCE_ID not set — \
                   start logoscore with storage_module loaded first");
        return None;
    }
    match Shim::new("lmao-storage-test") {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            eprintln!("skip: Shim::new failed: {e}");
            None
        }
    }
}

/// Upload a small payload and download it back by CID.
#[tokio::test]
#[ignore]
async fn upload_download_roundtrip() {
    let Some(shim) = require_shim_env() else { return };
    let backend = StorageModuleBackend::new(shim);

    let data = b"hello from storage_module integration test".to_vec();
    let cid = backend.upload(data.clone()).await.expect("upload failed");

    assert!(!cid.is_empty(), "CID should not be empty");
    eprintln!("uploaded CID: {cid}");

    let downloaded = backend.download(&cid).await.expect("download failed");
    assert_eq!(downloaded, data);
}

/// Upload 1 MiB — exercises chunked upload path (chunk size = 1 MiB,
/// so this is exactly one chunk + finalize).
#[tokio::test]
#[ignore]
async fn upload_large_payload() {
    let Some(shim) = require_shim_env() else { return };
    let backend = StorageModuleBackend::new(shim);

    let data = vec![0xab_u8; 1024 * 1024];
    let cid = backend.upload(data.clone()).await.expect("large upload failed");
    eprintln!("large upload CID: {cid}");

    let downloaded = backend.download(&cid).await.expect("large download failed");
    assert_eq!(downloaded.len(), data.len());
    assert_eq!(downloaded, data);
}

/// Upload a payload larger than one chunk — exercises multi-chunk path.
#[tokio::test]
#[ignore]
async fn upload_multi_chunk_payload() {
    let Some(shim) = require_shim_env() else { return };
    let backend = StorageModuleBackend::new(shim);

    // 2.5 MiB → 3 chunks (1 MiB + 1 MiB + 512 KiB)
    let data = vec![0xcd_u8; 1024 * 1024 * 2 + 512 * 1024];
    let cid = backend.upload(data.clone()).await.expect("multi-chunk upload failed");
    eprintln!("multi-chunk CID: {cid}");

    let downloaded = backend.download(&cid).await.expect("multi-chunk download failed");
    assert_eq!(downloaded, data);
}

/// Two distinct payloads produce distinct CIDs.
#[tokio::test]
#[ignore]
async fn distinct_payloads_distinct_cids() {
    let Some(shim) = require_shim_env() else { return };
    let backend = StorageModuleBackend::new(shim);

    let cid1 = backend.upload(b"payload-alpha".to_vec()).await.expect("upload 1");
    let cid2 = backend.upload(b"payload-beta".to_vec()).await.expect("upload 2");
    assert_ne!(cid1, cid2, "different payloads should yield different CIDs");
}

/// Downloading a non-existent CID returns an error.
#[tokio::test]
#[ignore]
async fn download_nonexistent_cid_returns_error() {
    let Some(shim) = require_shim_env() else { return };
    let backend = StorageModuleBackend::new(shim);

    let result = backend.download("zQmNonExistentCIDForLmaoTest12345").await;
    assert!(result.is_err(), "expected error for non-existent CID");
    eprintln!("got expected error: {}", result.unwrap_err());
}

/// Initialize storage_module Codex node, then do a small upload/download roundtrip.
///
/// Use this when storage_module was never initialized by Basecamp (empty data dir).
/// Pass a writable data dir via `LMAO_TEST_STORAGE_DATA_DIR`; defaults to a
/// temporary directory under `/tmp/lmao-test-storage` (persistent across the
/// test run, removed by a separate cleanup step).
///
/// After `start()`, Codex needs ~15-30 s to initialize. We do a single blocking
/// sleep in a spawn_blocking task so the Qt event loop thread is not disturbed
/// by tokio executor re-scheduling.
#[tokio::test]
#[ignore]
async fn codex_init_then_upload_download() {
    let Some(shim) = require_shim_env() else { return };

    let data_dir: String = std::env::var("LMAO_TEST_STORAGE_DATA_DIR")
        .unwrap_or_else(|_| "/tmp/lmao-test-storage".to_string());
    std::fs::create_dir_all(&data_dir).expect("create data_dir");
    eprintln!("storage data_dir = {data_dir}");

    // init(cfgJson) — synchronous; must be called before start()
    let cfg_json = serde_json::json!({
        "data-dir": data_dir,
        "log-level": "warn",
    }).to_string();
    let init_args = serde_json::to_string(&serde_json::json!([cfg_json])).unwrap();
    eprintln!("calling init …");
    let init_result = shim.call("storage_module", "init", &init_args, 30_000);
    eprintln!("init: {:?}", init_result);
    match &init_result {
        Ok(r) if r == "true" => {}
        Ok(r) => eprintln!("WARN: unexpected init result: {r}"),
        Err(e) => eprintln!("WARN: init error: {e}"),
    }

    // start() — asynchronous internally; Codex begins initializing after this
    let start_result = shim.call("storage_module", "start", "[]", 30_000);
    eprintln!("start: {:?}", start_result);

    // Give Codex 30 s to fully initialize before making any further IPC calls.
    // Using std::thread::sleep (not tokio) so the Qt event loop thread is
    // undisturbed; the shim's Qt thread runs the event loop independently.
    eprintln!("waiting 30 s for Codex to initialize …");
    tokio::task::spawn_blocking(|| std::thread::sleep(std::time::Duration::from_secs(30)))
        .await
        .expect("sleep task");

    // Verify Codex is up with a single spr() check (long timeout)
    let spr = shim.call("storage_module", "spr", "[]", 60_000);
    eprintln!("spr: {:?}", spr);
    if spr.as_ref().map_or(true, |s| s.contains("\"error\"") || s.contains("success=false")) {
        eprintln!("SKIP: Codex SPR unavailable — Codex may not have started in time");
        return;
    }

    // Upload/download roundtrip via StorageModuleBackend
    let backend = logos_messaging_a2a_storage::StorageModuleBackend::new(shim);
    let data = b"hello from codex_init_then_upload_download".to_vec();
    let cid = backend.upload(data.clone()).await.expect("upload failed");
    eprintln!("CID: {cid}");
    assert!(!cid.is_empty());
    let downloaded = backend.download(&cid).await.expect("download failed");
    assert_eq!(downloaded, data);
}

/// Test helper: call getPluginMethods to discover the storage_module API
#[tokio::test]
#[ignore]
async fn list_plugin_methods() {
    let Some(shim) = require_shim_env() else { return };
    let result = shim.call("storage_module", "getPluginMethods", "[]", 10_000);
    eprintln!("getPluginMethods result: {:?}", result);
    match result {
        Ok(json) => eprintln!("methods JSON: {json}"),
        Err(e) => eprintln!("Error: {e}"),
    }
}

/// Diagnostic: call zero-arg methods to verify basic dispatch works
#[tokio::test]
#[ignore]
async fn zero_arg_method_calls() {
    let Some(shim) = require_shim_env() else { return };
    for method in ["spr", "version", "peerId", "dataDir"] {
        let result = shim.call("storage_module", method, "[]", 15_000);
        eprintln!("{method}: {:?}", result);
    }
    // Also call uploadInit with 1 arg (just filename, no chunkSize)
    let result = shim.call("storage_module", "uploadInit", r#"["audit.log"]"#, 15_000);
    eprintln!("uploadInit(1-arg): {:?}", result);
    // And with 2 args
    let result = shim.call("storage_module", "uploadInit", r#"["audit.log", 1048576]"#, 15_000);
    eprintln!("uploadInit(2-arg): {:?}", result);
}

/// Diagnostic: inspect package_manager methods and installed modules.
/// Useful to check if delivery_module can be installed via the AppImage.
#[tokio::test]
#[ignore]
async fn package_manager_inspect() {
    let Some(shim) = require_shim_env() else { return };

    // List package_manager methods
    let methods = shim.call("package_manager", "getPluginMethods", "[]", 10_000);
    eprintln!("package_manager getPluginMethods: {:?}", methods);

    // List installed modules
    let installed = shim.call("package_manager", "getInstalledModules", "[]", 10_000);
    eprintln!("getInstalledModules: {:?}", installed);

    // List valid variants
    let variants = shim.call("package_manager", "getValidVariants", "[]", 10_000);
    eprintln!("getValidVariants: {:?}", variants);
}

/// Install delivery_module via the AppImage package_manager, then verify
/// it's registered and check if it becomes dispatchable.
///
/// Prerequisite: copy delivery_module files to
/// ~/.local/share/Logos/LogosBasecamp/modules/delivery_module/
/// with a manifest.json using "linux-amd64" as the main variant.
#[tokio::test]
#[ignore]
async fn install_delivery_module() {
    let Some(shim) = require_shim_env() else { return };

    let dest = "/home/vpavlin/.local/share/Logos/LogosBasecamp/modules/delivery_module";
    let plugin_so = format!("{dest}/delivery_module_plugin.so");

    // Try installPlugin with directory path
    let dir_args = serde_json::to_string(&serde_json::json!([dest, false])).unwrap();
    let result = shim.call("package_manager", "installPlugin", &dir_args, 30_000);
    eprintln!("installPlugin(dir): {:?}", result);

    // Also try with .so path
    let so_args = serde_json::to_string(&serde_json::json!([plugin_so, false])).unwrap();
    let result2 = shim.call("package_manager", "installPlugin", &so_args, 30_000);
    eprintln!("installPlugin(so): {:?}", result2);

    // Check if now listed
    let installed = shim.call("package_manager", "getInstalledModules", "[]", 10_000);
    eprintln!("getInstalledModules after install: {:?}", installed);
}
