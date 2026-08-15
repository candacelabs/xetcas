//! Download backpressure: stalled reconstruction downloads must not starve the
//! shared blocking pool that sqlite (and /health) depend on.

mod common;

use std::time::Duration;

use common::{pseudo_random_bytes, upload_all, TestServer};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

async fn wait_for_permits(server: &TestServer, want: usize) {
    for _ in 0..250 {
        if server.state.download_permits.available_permits() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let have = server.state.download_permits.available_permits();
    panic!("permits never reached {want}; stuck at {have}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stalled_downloads_do_not_block_health() {
    let server = TestServer::start_with_download_permits(2).await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(71, 4 * 1024 * 1024);
    let oid = hex::encode(Sha256::digest(&data));
    upload_all(&server.base_url, cache.path(), &[("s.bin", data)]).await;
    let url = format!("{}/lfs/objects/{oid}", server.base_url);
    // Two downloads whose bodies are never read: each fills the 8-slot channel
    // and parks a decode thread, taking one of the two permits.
    let a = reqwest::Client::new().get(&url).send().await.unwrap();
    let b = reqwest::Client::new().get(&url).send().await.unwrap();
    wait_for_permits(&server, 0).await;

    // Both permits are held, yet /health (the sqlite pool) still answers.
    let health = tokio::time::timeout(
        Duration::from_secs(5),
        reqwest::Client::new()
            .get(format!("{}/health", server.base_url))
            .send(),
    )
    .await
    .expect("health timed out")
    .unwrap();
    assert_eq!(health.status(), 200);

    // Hanging up frees the permits.
    drop(a);
    drop(b);
    wait_for_permits(&server, 2).await;
}
