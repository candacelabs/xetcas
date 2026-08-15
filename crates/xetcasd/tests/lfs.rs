//! Git LFS bridge: batch negotiation, exact headers, and object download.

mod common;

use common::{pseudo_random_bytes, upload_all, TestServer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const BATCH_PATH: &str = "/git/t/r.git/info/lfs/objects/batch";
const LFS_CONTENT_TYPE: &str = "application/vnd.git-lfs+json";

/// An object's `authenticated` claim. The field is omitted when false, which
/// the Git LFS spec defines as equivalent ("if omitted or false, Git LFS will
/// attempt to find credentials for this URL").
fn authenticated(object: &Value) -> bool {
    object
        .get("authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

async fn batch(server: &TestServer, body: Value) -> (reqwest::StatusCode, Value) {
    batch_as(server, body, None).await
}

/// Issue a batch request, optionally carrying the `Authorization` an
/// authenticating reverse proxy in front of xetcasd would have accepted.
async fn batch_as(
    server: &TestServer,
    body: Value,
    authorization: Option<&str>,
) -> (reqwest::StatusCode, Value) {
    let mut request = reqwest::Client::new()
        .post(format!("{}{BATCH_PATH}", server.base_url))
        .header("Content-Type", LFS_CONTENT_TYPE)
        .header("Accept", LFS_CONTENT_TYPE);
    if let Some(authorization) = authorization {
        request = request.header("Authorization", authorization);
    }
    let response = request
        .body(serde_json::to_vec(&body).unwrap())
        .send()
        .await
        .unwrap();
    let status = response.status();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.starts_with(LFS_CONTENT_TYPE),
        "batch replied with content-type {content_type}"
    );
    let value = response.json().await.unwrap();
    (status, value)
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_batch_offering_xet_negotiates_the_xet_transfer() {
    let server = TestServer::start().await;

    let (status, body) = batch(
        &server,
        json!({
            "operation": "upload",
            "transfers": ["basic", "xet"],
            "objects": [{"oid": "a".repeat(64), "size": 1234}],
            "hash_algo": "sha256"
        }),
    )
    .await;

    assert_eq!(status, 200);
    assert_eq!(body["transfer"], "xet");

    let object = &body["objects"][0];
    // Nothing authenticated this batch, so the action carries no authorization
    // and must not claim otherwise -- see the dedicated test below.
    assert!(!authenticated(object));
    assert!(object.get("error").is_none(), "unexpected error entry");

    let action = &object["actions"]["upload"];
    assert_eq!(action["href"], format!("{}/xet-token", server.base_url));
    assert_eq!(action["expires_in"], 3600);

    // git-xet looks these up with exact-case string matching and fails the
    // transfer if any is missing (docs/research/git-xet.md section 4).
    let header = &action["header"];
    assert_eq!(header["X-Xet-Cas-Url"], server.base_url);
    assert_eq!(header["X-Xet-Access-Token"], "anonymous");
    let expiration = header["X-Xet-Token-Expiration"]
        .as_str()
        .expect("expiration must be a JSON string");
    assert!(
        expiration.parse::<u64>().is_ok(),
        "expiration {expiration} must parse as u64"
    );
    assert!(header["X-Xet-Session-Id"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));

    // A verify action would make git-lfs call back for confirmation we do not
    // implement, so it is deliberately never emitted.
    assert!(object["actions"].get("verify").is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn upload_batch_without_xet_is_refused_per_object() {
    let server = TestServer::start().await;

    let (status, body) = batch(
        &server,
        json!({
            "operation": "upload",
            "transfers": ["basic"],
            "objects": [{"oid": "b".repeat(64), "size": 10}],
            "hash_algo": "sha256"
        }),
    )
    .await;

    assert_eq!(status, 200, "per-object failures ride inside a 200");
    let object = &body["objects"][0];
    assert_eq!(object["error"]["code"], 422);
    assert!(object["error"]["message"]
        .as_str()
        .unwrap()
        .contains("git xet install"));
    assert!(
        object.get("actions").is_none(),
        "refused object must carry no actions"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn download_batch_uses_basic_transfer_and_serves_the_object() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();

    let data = pseudo_random_bytes(61, 400 * 1024);
    let oid = hex::encode(Sha256::digest(&data));
    upload_all(&server.base_url, cache.path(), &[("obj.bin", data.clone())]).await;

    let (status, body) = batch(
        &server,
        json!({
            "operation": "download",
            "transfers": ["basic", "xet"],
            "objects": [
                {"oid": oid, "size": data.len()},
                {"oid": "c".repeat(64), "size": 5}
            ],
            "hash_algo": "sha256"
        }),
    )
    .await;

    assert_eq!(status, 200);
    // git-xet cannot download, so a download batch is never answered with xet.
    assert_eq!(body["transfer"], "basic");

    let found = &body["objects"][0];
    assert!(!authenticated(found));
    let href = found["actions"]["download"]["href"].as_str().unwrap();
    assert_eq!(href, format!("{}/lfs/objects/{oid}", server.base_url));

    let missing = &body["objects"][1];
    assert_eq!(missing["error"]["code"], 404);

    // The href must serve bytes that hash back to the oid.
    let fetched = reqwest::get(href).await.unwrap();
    assert_eq!(fetched.status(), 200);
    assert_eq!(
        fetched.headers()["content-length"].to_str().unwrap(),
        data.len().to_string()
    );
    let bytes = fetched.bytes().await.unwrap();
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        oid,
        "served bytes do not match the oid"
    );
    assert!(bytes.as_ref() == data.as_slice());
}

/// `authenticated: true` tells git-lfs not to apply its own credentials to the
/// action href. Claiming it on an action that carries no authorization makes
/// every clone 401 behind the authenticating reverse proxy the README
/// recommends, because git-lfs does not copy the batch request's Authorization
/// onto the action request. Both action kinds must therefore claim it only when
/// they really embed it.
#[tokio::test(flavor = "multi_thread")]
async fn actions_claim_authenticated_only_when_they_embed_authorization() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(63, 128 * 1024);
    let oid = hex::encode(Sha256::digest(&data));
    upload_all(&server.base_url, cache.path(), &[("a.bin", data)]).await;

    let upload_request = json!({
        "operation": "upload",
        "transfers": ["basic", "xet"],
        "objects": [{"oid": "e".repeat(64), "size": 7}],
        "hash_algo": "sha256"
    });
    let download_request = json!({
        "operation": "download",
        "transfers": ["basic"],
        "objects": [{"oid": oid, "size": 1}],
        "hash_algo": "sha256"
    });

    // No proxy in front: nothing to embed, so nothing may be claimed and
    // git-lfs is left free to apply its own credential chain.
    let (_, body) = batch_as(&server, upload_request.clone(), None).await;
    let object = &body["objects"][0];
    assert!(!authenticated(object), "{object}");
    assert!(
        object["actions"]["upload"]["header"]
            .get("Authorization")
            .is_none(),
        "no authorization was available to embed: {object}"
    );

    let (_, body) = batch_as(&server, download_request.clone(), None).await;
    let object = &body["objects"][0];
    assert!(!authenticated(object), "{object}");
    assert!(
        object["actions"]["download"]["header"]
            .get("Authorization")
            .is_none(),
        "no authorization was available to embed: {object}"
    );

    // Behind a proxy: the accepted credential rides along in the action, and
    // only then is the claim true.
    let proxy = "Bearer proxy-session-token";
    let (_, body) = batch_as(&server, upload_request, Some(proxy)).await;
    let object = &body["objects"][0];
    assert!(authenticated(object), "{object}");
    assert_eq!(
        object["actions"]["upload"]["header"]["Authorization"],
        proxy
    );
    // The X-Xet-* set git-xet consumes is untouched.
    assert_eq!(
        object["actions"]["upload"]["header"]["X-Xet-Cas-Url"],
        server.base_url
    );

    let (_, body) = batch_as(&server, download_request, Some(proxy)).await;
    let object = &body["objects"][0];
    assert!(authenticated(object), "{object}");
    assert_eq!(
        object["actions"]["download"]["header"]["Authorization"],
        proxy
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_lfs_oid_is_a_bad_request() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();
    // Not 64 lowercase-hex chars: rejected before the index lookup.
    for bad in ["not-a-valid-oid", &"A".repeat(64), &"a".repeat(63)] {
        let resp = http
            .get(format!("{}/lfs/objects/{bad}", server.base_url))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 400, "oid {bad} should be a 400");
    }
    // Well-formed but unknown stays 404.
    let resp = http
        .get(format!(
            "{}/lfs/objects/{}",
            server.base_url,
            "a".repeat(64)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

#[tokio::test(flavor = "multi_thread")]
async fn unknown_lfs_object_is_not_found() {
    let server = TestServer::start().await;
    let response = reqwest::get(format!(
        "{}/lfs/objects/{}",
        server.base_url,
        "d".repeat(64)
    ))
    .await
    .unwrap();
    assert_eq!(response.status(), 404);
}
