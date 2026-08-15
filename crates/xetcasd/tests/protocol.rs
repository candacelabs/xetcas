//! Wire-level tests: status codes, rejections, and the dedup shard contract.

mod common;

use std::io::Cursor;

use common::{pseudo_random_bytes, upload_all, TestServer};
use tempfile::TempDir;
use xet_core_structures::merklehash::MerkleHash;
use xet_core_structures::metadata_shard::file_structs::{
    FileDataSequenceEntry, FileDataSequenceHeader, FileVerificationEntry, MDBFileInfo,
};
use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
use xet_core_structures::metadata_shard::MDBShardInfo;
use xet_core_structures::xorb_object::{
    deserialize_chunk, reconstruct_xorb_with_footer, serialize_chunk, CompressionScheme,
};

/// A syntactically valid but arbitrary 64-hex hash.
const WRONG_HASH: &str = "1111111111111111111111111111111111111111111111111111111111111111";

#[tokio::test(flavor = "multi_thread")]
async fn same_hash_reupload_with_other_compression_keeps_the_first_layout() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();
    // Highly compressible content so lz4's layout differs from none's; both
    // address the same compression-independent xorb hash.
    let content = vec![0x41u8; 100_000];
    let mut none_body = Vec::new();
    serialize_chunk(&content, &mut none_body, CompressionScheme::None).unwrap();
    let mut lz4_body = Vec::new();
    serialize_chunk(&content, &mut lz4_body, CompressionScheme::LZ4).unwrap();
    assert_ne!(none_body.len(), lz4_body.len(), "layouts must differ");
    let (_o, hash) = reconstruct_xorb_with_footer(&mut Vec::new(), &none_body).unwrap();
    let hash = hash.hex();
    let url = format!("{}/v1/xorbs/default/{hash}", server.base_url);
    let first: serde_json::Value = http
        .post(&url)
        .body(none_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first["was_inserted"], true);
    let stored_path = server.state.xorbs.path_for(&hash);
    let after_first = std::fs::read(&stored_path).unwrap();
    let second: serde_json::Value = http
        .post(&url)
        .body(lz4_body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        second["was_inserted"], false,
        "second upload must not insert"
    );
    let after_second = std::fs::read(&stored_path).unwrap();
    assert_eq!(after_first, after_second, "the stored blob was rewritten");
    // The data route still decodes to the original content.
    let data = http
        .get(format!("{url}/data"))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let (decoded, _, _) = deserialize_chunk(&mut Cursor::new(data.as_ref())).unwrap();
    assert!(decoded == content, "reconstructed bytes differ");
}

#[tokio::test(flavor = "multi_thread")]
async fn xorb_body_that_does_not_hash_to_the_path_hash_is_rejected() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();

    // A well-formed single-frame xorb body, addressed to the wrong hash.
    let mut body = Vec::new();
    serialize_chunk(b"some chunk contents", &mut body, CompressionScheme::None).unwrap();

    let response = http
        .post(format!("{}/v1/xorbs/default/{WRONG_HASH}", server.base_url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400, "hash mismatch must be a hard 400");
    assert!(response.text().await.unwrap().contains("hash"));
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_xorb_frames_are_rejected() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();

    let response = http
        .post(format!("{}/v1/xorbs/default/{WRONG_HASH}", server.base_url))
        .body(vec![0xFFu8; 64])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

/// A body of many minimal empty-chunk headers must be rejected fast by the
/// pre-parse guard, before reconstruct_xorb_with_footer expands ~1 GB of chunk
/// vectors (memory-amplification protection).
#[tokio::test(flavor = "multi_thread")]
async fn oversized_chunk_count_is_rejected_before_the_full_parse() {
    let server = TestServer::start().await;
    let mut body = Vec::new();
    for _ in 0..9000 {
        serialize_chunk(b"", &mut body, CompressionScheme::None).unwrap();
    }
    let response = reqwest::Client::new()
        .post(format!("{}/v1/xorbs/default/{WRONG_HASH}", server.base_url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    // The distinctive pre-check message, not the post-parse count error.
    assert!(response.text().await.unwrap().contains("chunk frames"));
}

#[tokio::test(flavor = "multi_thread")]
async fn truncated_shard_is_rejected() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();

    let response = http
        .post(format!("{}/v1/shards", server.base_url))
        .body(vec![0x01u8; 24])
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

/// Build a one-file shard body, optionally corrupting the verification hash.
fn shard_with_file(
    file_hash: MerkleHash,
    xorb_hash: MerkleHash,
    chunk_start: u32,
    chunk_end: u32,
    unpacked: u32,
    range_hash: Option<MerkleHash>,
) -> Vec<u8> {
    let verification = range_hash
        .map(|range_hash| {
            vec![FileVerificationEntry {
                range_hash,
                _unused: [0; 2],
            }]
        })
        .unwrap_or_default();

    let info = MDBFileInfo {
        metadata: FileDataSequenceHeader::new(file_hash, 1u32, !verification.is_empty(), false),
        segments: vec![FileDataSequenceEntry {
            xorb_hash,
            xorb_flags: 0,
            unpacked_segment_bytes: unpacked,
            chunk_index_start: chunk_start,
            chunk_index_end: chunk_end,
        }],
        verification,
        metadata_ext: None,
    };

    let mut shard = MDBInMemoryShard::default();
    shard.add_file_reconstruction_info(info).unwrap();
    shard.to_bytes().unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_referencing_an_unknown_xorb_is_rejected() {
    let server = TestServer::start().await;
    let http = reqwest::Client::new();

    let body = shard_with_file(
        MerkleHash::from_hex(WRONG_HASH).unwrap(),
        MerkleHash::from_hex(WRONG_HASH).unwrap(),
        0,
        1,
        100,
        None,
    );

    let response = http
        .post(format!("{}/v1/shards", server.base_url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response.text().await.unwrap().contains("unknown xorb"));
}

#[tokio::test(flavor = "multi_thread")]
async fn shard_with_a_bad_verification_hash_is_rejected() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(31, 300 * 1024);
    let infos = upload_all(&server.base_url, cache.path(), &[("v.bin", data)]).await;

    // Copy a real, accepted file entry and flip one byte of its range hash.
    let stored = server
        .state
        .index
        .get_file(infos[0].hash())
        .await
        .unwrap()
        .expect("file registered");
    let term = &stored.terms[0];

    let mut corrupted = [0u8; 32];
    corrupted.copy_from_slice(&stored.verification_range_hashes[0..32]);
    corrupted[0] ^= 0xFF;

    let body = shard_with_file(
        MerkleHash::from_hex(&stored.file_hash).unwrap(),
        MerkleHash::from_hex(&term.xorb_hash).unwrap(),
        term.chunk_index_start,
        term.chunk_index_end,
        term.unpacked_segment_bytes,
        Some(MerkleHash::from_slice(&corrupted).unwrap()),
    );

    let response = reqwest::Client::new()
        .post(format!("{}/v1/shards", server.base_url))
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    assert!(response
        .text()
        .await
        .unwrap()
        .contains("verification hash mismatch"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reconstruction_range_and_lookup_status_codes() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(41, 300 * 1024);
    let len = data.len() as u64;
    let infos = upload_all(&server.base_url, cache.path(), &[("s.bin", data)]).await;
    let file_id = infos[0].hash();

    let http = reqwest::Client::new();
    let url = format!("{}/v1/reconstructions/{file_id}", server.base_url);

    let full = http.get(&url).send().await.unwrap();
    assert_eq!(full.status(), 200);

    // Past EOF is the client's end-of-file signal and must be 416, not 404.
    let past = http
        .get(&url)
        .header("Range", format!("bytes={len}-{}", len + 100))
        .send()
        .await
        .unwrap();
    assert_eq!(past.status(), 416);

    // Open-ended and suffix forms are accepted even though this client never
    // emits them (docs/research/dataplane.md section 8.6).
    let open = http
        .get(&url)
        .header("Range", "bytes=1024-")
        .send()
        .await
        .unwrap();
    assert_eq!(open.status(), 200);
    let suffix = http
        .get(&url)
        .header("Range", "bytes=-4096")
        .send()
        .await
        .unwrap();
    assert_eq!(suffix.status(), 200);

    let missing = http
        .get(format!(
            "{}/v1/reconstructions/{WRONG_HASH}",
            server.base_url
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
}

/// The dedup route must return a shard the real client can import: keyed chunk
/// hashes, a live expiry, and a lookup that resolves back to the owning xorb.
#[tokio::test(flavor = "multi_thread")]
async fn dedup_route_returns_an_importable_keyed_shard() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let data = pseudo_random_bytes(51, 300 * 1024);
    let infos = upload_all(&server.base_url, cache.path(), &[("d.bin", data)]).await;

    let stored = server
        .state
        .index
        .get_file(infos[0].hash())
        .await
        .unwrap()
        .expect("file registered");
    let term = &stored.terms[0];
    let xorb = server
        .state
        .index
        .get_xorb(&term.xorb_hash)
        .await
        .unwrap()
        .expect("xorb stored");

    // The first chunk of a file is always indexed, whatever its hash.
    let chunk_hash =
        xetcasd::dedup_shard::chunk_hash_at(&xorb, term.chunk_index_start as usize).unwrap();

    let response = reqwest::Client::new()
        .get(format!(
            "{}/v1/chunks/default/{}",
            server.base_url,
            chunk_hash.hex()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body = response.bytes().await.unwrap();

    let mut reader = Cursor::new(body.as_ref());
    let shard = MDBShardInfo::load_from_reader(&mut reader).expect("parse dedup shard");

    let key = shard.chunk_hmac_key().expect("dedup shard must be keyed");
    assert_ne!(key, MerkleHash::default(), "hmac key must be non-zero");

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let expiry = shard.metadata.shard_key_expiry;
    assert!(expiry > now, "expiry {expiry} already lapsed at {now}");
    assert!(
        expiry <= now + 24 * 60 * 60 + 60,
        "expiry {expiry} unreasonably far out"
    );

    // A lookup by the UNKEYED hash must resolve: the shard applies its own key.
    let hit = shard
        .chunk_hash_dedup_query(&mut reader, &[chunk_hash])
        .expect("dedup query")
        .expect("queried chunk must be found");
    assert_eq!(
        hit.1.xorb_hash.hex(),
        term.xorb_hash,
        "resolved to the wrong xorb"
    );
}

/// With a token configured, CAS writes require it while reads and the
/// unauthenticated fetch path stay open -- the client fetches reconstruction
/// URLs with no Authorization header at all.
#[tokio::test(flavor = "multi_thread")]
async fn configured_token_gates_writes_only() {
    let server = TestServer::start_with_token(Some("s3cret".to_string())).await;
    let http = reqwest::Client::new();

    let mut body = Vec::new();
    serialize_chunk(b"contents", &mut body, CompressionScheme::None).unwrap();
    let url = format!("{}/v1/xorbs/default/{WRONG_HASH}", server.base_url);

    let anonymous = http.post(&url).body(body.clone()).send().await.unwrap();
    assert_eq!(anonymous.status(), 401, "writes must require the token");

    let wrong = http
        .post(&url)
        .header("Authorization", "Bearer nope")
        .body(body.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 401);

    // With the right token the request gets past auth and fails on its merits
    // (this body does not hash to the addressed hash).
    let authorized = http
        .post(&url)
        .header("Authorization", "Bearer s3cret")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(authorized.status(), 400);

    // Reads stay open.
    let health = http
        .get(format!("{}/health", server.base_url))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
}
