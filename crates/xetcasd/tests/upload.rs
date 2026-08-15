//! Xorb upload admission control.
//!
//! A xorb body is capped at ~68 MiB and the verifier holds a second copy while
//! it stores one, so the number of bodies resident at once has to be bounded
//! before they are buffered -- not after.

mod common;

use std::time::Duration;

use common::TestServer;
use tokio::io::AsyncWriteExt;
use xet_core_structures::xorb_object::{
    reconstruct_xorb_with_footer, serialize_chunk, CompressionScheme,
};

/// A valid single-frame xorb body and the hash it is addressed by.
fn xorb_body(content: &[u8]) -> (Vec<u8>, String) {
    let mut body = Vec::new();
    serialize_chunk(content, &mut body, CompressionScheme::None).unwrap();
    let (_object, hash) = reconstruct_xorb_with_footer(&mut Vec::new(), &body).unwrap();
    (body, hash.hex())
}

/// Poll until the semaphore reaches `want`, or fail loudly. The budget is
/// deliberately generous: `cargo test` runs every test binary at once, and this
/// only has to outlast scheduling noise, never a real regression.
async fn wait_for_upload_permits(server: &TestServer, want: usize) {
    for _ in 0..1200 {
        if server.state.upload_permits.available_permits() == want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let have = server.state.upload_permits.available_permits();
    panic!("upload permits never reached {want}; stuck at {have}");
}

/// Two things are asserted here, and the first is the load-bearing one.
///
/// 1. The permit is taken BEFORE the body is buffered. A request whose body is
///    still in flight already holds a permit; if the permit were taken after
///    the body was read, the semaphore would still be full at this point and
///    the cap would bound verification work rather than resident bytes.
/// 2. With the cap exhausted, a second complete upload is not admitted, and
///    nothing is stored until the first one releases its permit.
#[tokio::test(flavor = "multi_thread")]
async fn a_xorb_upload_holds_its_permit_before_its_body_is_buffered() {
    let server = TestServer::start_with_upload_permits(1).await;
    let addr = server
        .base_url
        .strip_prefix("http://")
        .expect("http base url")
        .to_string();

    let (slow_body, slow_hash) = xorb_body(&vec![0xA5u8; 64 * 1024]);
    let (fast_body, fast_hash) = xorb_body(b"a different, complete xorb");

    // Send the headers and half the body on a raw socket. hyper hands the
    // request to the handler as soon as the headers are complete, so the
    // handler runs, takes the only permit, and then waits for the rest.
    let mut socket = tokio::net::TcpStream::connect(&addr).await.unwrap();
    let head = format!(
        "POST /v1/xorbs/default/{slow_hash} HTTP/1.1\r\n\
         Host: {addr}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\r\n",
        slow_body.len()
    );
    socket.write_all(head.as_bytes()).await.unwrap();
    let split = slow_body.len() / 2;
    socket.write_all(&slow_body[..split]).await.unwrap();
    socket.flush().await.unwrap();

    // (1) The permit is held while the body is demonstrably incomplete.
    wait_for_upload_permits(&server, 0).await;

    // (2) A complete, valid upload that would otherwise finish immediately.
    let url = format!("{}/v1/xorbs/default/{fast_hash}", server.base_url);
    let queued = tokio::spawn({
        let body = fast_body.clone();
        async move { reqwest::Client::new().post(&url).body(body).send().await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !queued.is_finished(),
        "the second xorb upload was admitted while the cap was exhausted"
    );
    assert_eq!(
        server.counts().await.0,
        0,
        "nothing may be stored yet: the first body is incomplete and the second is queued"
    );

    // Finishing the first body releases the permit and the queued upload runs.
    socket.write_all(&slow_body[split..]).await.unwrap();
    socket.flush().await.unwrap();

    let response = tokio::time::timeout(Duration::from_secs(60), queued)
        .await
        .expect("the queued upload never ran after the permit was released")
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 200);
    wait_for_upload_permits(&server, 1).await;
    assert_eq!(
        server.counts().await.0,
        2,
        "both xorbs should be stored once the cap admitted them in turn"
    );
}
