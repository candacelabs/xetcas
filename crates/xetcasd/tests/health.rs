//! `/health` is a liveness check, not a store audit.
//!
//! The container health-checks it every five seconds with a three-second
//! timeout, so it must stay constant time as the store grows -- and it must
//! still notice that the object tree is gone, which a pure index read would
//! not.

mod common;

use common::{pseudo_random_bytes, upload_all, TestServer};
use serde_json::Value;
use tempfile::TempDir;

async fn health(server: &TestServer) -> (reqwest::StatusCode, Value) {
    let response = reqwest::Client::new()
        .get(format!("{}/health", server.base_url))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.json().await.unwrap_or(Value::Null);
    (status, body)
}

/// `stored_bytes` comes from the index, so bytes that appear in the object tree
/// without being indexed do not move it. The old implementation recursively
/// stat'd every stored object on every request, which is exactly the walk this
/// asserts is gone.
#[tokio::test(flavor = "multi_thread")]
async fn health_reports_indexed_bytes_without_walking_the_object_tree() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(81, 512 * 1024);
    upload_all(&server.base_url, cache.path(), &[("h.bin", data)]).await;

    let (status, body) = health(&server).await;
    assert_eq!(status, 200);
    let indexed = body["stored_bytes"].as_u64().expect("stored_bytes");
    assert!(indexed > 0, "a real upload must be accounted for");
    assert_eq!(
        indexed,
        server.stored_bytes().await,
        "the indexed total must agree with the object tree it describes"
    );

    // Something in the object tree that no xorb record points at: a crash
    // between write_atomic and put_xorb, a half-copied restore, an operator's
    // stray file. A walking /health would fold it in; a constant-time one
    // reports what the index actually knows.
    let stray = server.state.xorbs.root().join("ff");
    std::fs::create_dir_all(&stray).unwrap();
    std::fs::write(stray.join("stray.bin"), vec![0u8; 4096]).unwrap();

    let (status, body) = health(&server).await;
    assert_eq!(status, 200);
    assert_eq!(
        body["stored_bytes"].as_u64().unwrap(),
        indexed,
        "/health walked the object tree instead of reading the index"
    );
    assert_eq!(
        server.stored_bytes().await,
        indexed + 4096,
        "the auditing walk should see the stray file even though /health does not"
    );
}

/// A dead object root means every indexed object is unreachable. Reporting
/// `"status":"ok"` there keeps the container marked healthy while every
/// download 500s, so the probe must fail the request.
#[tokio::test(flavor = "multi_thread")]
async fn health_fails_when_the_object_root_is_unreadable() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(82, 256 * 1024);
    upload_all(&server.base_url, cache.path(), &[("g.bin", data)]).await;

    let (status, _) = health(&server).await;
    assert_eq!(status, 200, "healthy before the store goes away");

    // Simulate the failed mount: the index still holds every record, but the
    // objects those records describe cannot be reached.
    std::fs::remove_dir_all(server.state.xorbs.root()).unwrap();

    let (status, body) = health(&server).await;
    assert_ne!(
        status, 200,
        "/health reported {body} with no object store behind it"
    );
    assert!(status.is_server_error(), "unexpected status {status}");
}
