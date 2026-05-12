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
