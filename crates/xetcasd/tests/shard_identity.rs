//! Shards are client-asserted metadata about content the server already holds,
//! so every identifier in one has to be derived from that content rather than
//! copied: the file hash and the sha256 key the file index, and the cas-info
//! dedup flags key the global-dedup table.

mod common;

use std::io::Cursor;

use common::{pseudo_random_bytes, upload_all, TestServer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use xet_core_structures::merklehash::{file_hash, MerkleHash};
use xet_core_structures::metadata_shard::chunk_verification::range_hash_from_chunks;
use xet_core_structures::metadata_shard::file_structs::{
    FileDataSequenceEntry, FileDataSequenceHeader, FileMetadataExt, FileVerificationEntry,
    MDBFileInfo,
};
use xet_core_structures::metadata_shard::shard_in_memory::MDBInMemoryShard;
use xet_core_structures::metadata_shard::xorb_structs::{
    MDBXorbInfo, XorbChunkSequenceEntry, XorbChunkSequenceHeader,
};
use xet_core_structures::xorb_object::deserialize_chunk;
use xetcas_contracts::v1::XorbRecord;

/// A syntactically valid hash that names nothing this server holds.
const ABSENT: &str = "2222222222222222222222222222222222222222222222222222222222222222";

fn absent() -> MerkleHash {
    MerkleHash::from_hex(ABSENT).unwrap()
}

/// Filler chunk hash for a cas-info block, distinct per slot.
fn filler(index: u32) -> MerkleHash {
    MerkleHash::from_hex(&format!("{:064x}", 0xF00D_0000u64 + u64::from(index))).unwrap()
}

/// One stored xorb plus the geometry of its chunks, as the server sees them.
struct StoredXorb {
    hash: MerkleHash,
    record: XorbRecord,
}

impl StoredXorb {
    /// Hash and uncompressed length of chunk `index`.
    fn chunk(&self, index: usize) -> (MerkleHash, u32) {
        let hash = xetcasd::dedup_shard::chunk_hash_at(&self.record, index).unwrap();
        let end = self.record.unpacked_chunk_offsets[index];
        let start = if index == 0 {
            0
        } else {
            self.record.unpacked_chunk_offsets[index - 1]
        };
        (hash, end - start)
    }
}

/// Upload one real file through the genuine client and return the xorb it made.
async fn stored_xorb(server: &TestServer, cache: &TempDir, seed: u64) -> StoredXorb {
    let data = pseudo_random_bytes(seed, 400 * 1024);
    let infos = upload_all(&server.base_url, cache.path(), &[("s.bin", data)]).await;
    let file = server
        .state
        .index
        .get_file(infos[0].hash())
        .await
        .unwrap()
        .expect("file registered");
    let xorb_hash = file.terms[0].xorb_hash.clone();
    let record = server
        .state
        .index
        .get_xorb(&xorb_hash)
        .await
        .unwrap()
        .expect("xorb stored");
    StoredXorb {
        hash: MerkleHash::from_hex(&xorb_hash).unwrap(),
        record,
    }
}

/// A truncated one-file shard describing chunk 0 of `xorb` as a whole file.
///
/// Both identifiers default to the truthful value, so a test overrides exactly
/// the one it is falsifying.
fn one_chunk_file_shard(
    xorb: &StoredXorb,
    file_hash_override: Option<MerkleHash>,
    sha256: Option<MerkleHash>,
) -> Vec<u8> {
    let (chunk_hash, chunk_len) = xorb.chunk(0);
    let honest = file_hash(&[(chunk_hash, u64::from(chunk_len))]);
    let info = MDBFileInfo {
        metadata: FileDataSequenceHeader::new(
            file_hash_override.unwrap_or(honest),
            1u32,
            true,
            sha256.is_some(),
        ),
        segments: vec![FileDataSequenceEntry {
            xorb_hash: xorb.hash,
            xorb_flags: 0,
            unpacked_segment_bytes: chunk_len,
            chunk_index_start: 0,
            chunk_index_end: 1,
        }],
        verification: vec![FileVerificationEntry {
            range_hash: range_hash_from_chunks(&[chunk_hash]),
            _unused: [0; 2],
        }],
        metadata_ext: sha256.map(FileMetadataExt::new),
    };

    let mut shard = MDBInMemoryShard::default();
    shard.add_file_reconstruction_info(info).unwrap();
    shard.to_bytes().unwrap()
}

/// A truncated shard whose only content is a cas-info block claiming that
/// `chunk_hash` sits at `index` of `xorb_hash`, flagged for global dedup.
fn dedup_flag_shard(xorb_hash: MerkleHash, index: u32, chunk_hash: MerkleHash) -> Vec<u8> {
    let chunks = (0..=index)
        .map(|i| {
            let hash = if i == index { chunk_hash } else { filler(i) };
            XorbChunkSequenceEntry::new(hash, 1024u32, i * 1024).with_global_dedup_flag(i == index)
        })
        .collect();
    let info = MDBXorbInfo {
        metadata: XorbChunkSequenceHeader {
            xorb_hash,
            xorb_flags: 0,
            num_entries: index + 1,
            num_bytes_in_xorb: (index + 1) * 1024,
            num_bytes_on_disk: (index + 1) * 1024,
        },
        chunks,
    };
    let mut shard = MDBInMemoryShard::default();
    shard.add_xorb_block(info).unwrap();
    shard.to_bytes().unwrap()
}

async fn post_shard(server: &TestServer, body: Vec<u8>) -> (reqwest::StatusCode, String) {
    let response = reqwest::Client::new()
        .post(format!("{}/v1/shards", server.base_url))
        .body(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    (status, response.text().await.unwrap())
}

/// What the LFS bridge says about uploading `oid` right now.
async fn upload_batch_object(server: &TestServer, oid: &str) -> Value {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/git/t/r.git/info/lfs/objects/batch",
            server.base_url
        ))
        .header("Content-Type", "application/vnd.git-lfs+json")
        .body(
            serde_json::to_vec(&json!({
                "operation": "upload",
                "transfers": ["basic", "xet"],
                "objects": [{"oid": oid, "size": 1}],
                "hash_algo": "sha256"
            }))
            .unwrap(),
        )
        .send()
        .await
        .unwrap();
    let body: Value = response.json().await.unwrap();
    body["objects"][0].clone()
}

/// Decode the first chunk of a stored xorb through the public data route.
async fn chunk_zero_bytes(server: &TestServer, xorb: &StoredXorb) -> Vec<u8> {
    let bytes = reqwest::get(format!(
        "{}/v1/xorbs/default/{}/data",
        server.base_url,
        xorb.hash.hex()
    ))
    .await
    .unwrap()
    .bytes()
    .await
    .unwrap();
    let (data, _compressed, _uncompressed) =
        deserialize_chunk(&mut Cursor::new(bytes.as_ref())).unwrap();
    data
}

/// `files.file_hash` is a PRIMARY KEY written with INSERT OR IGNORE, so a shard
/// that names an identifier it did not earn squats it forever: the genuine
/// registration is silently dropped and that id reconstructs to these bytes.
#[tokio::test(flavor = "multi_thread")]
async fn a_shard_whose_file_hash_does_not_match_its_terms_is_rejected() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let xorb = stored_xorb(&server, &cache, 91).await;

    let (status, body) =
        post_shard(&server, one_chunk_file_shard(&xorb, Some(absent()), None)).await;
    assert_eq!(status, 400, "body: {body}");
    assert!(body.contains("file hash mismatch"), "body: {body}");
    assert!(
        server.state.index.get_file(ABSENT).await.unwrap().is_none(),
        "the squatted file hash reached the index"
    );

    // The same shard carrying the hash it actually earns is accepted, so the
    // check discriminates rather than rejecting everything hand-built.
    let (status, body) = post_shard(&server, one_chunk_file_shard(&xorb, None, None)).await;
    assert_eq!(status, 200, "body: {body}");
}

/// The sha256 is the git-lfs oid. An unverified one is a data-loss path, not
/// merely a wrong download: the LFS upload batch reports any known oid as
/// "already stored", so a squatted oid makes git-lfs skip the genuine upload of
/// that object and it is never stored at all.
#[tokio::test(flavor = "multi_thread")]
async fn a_shard_whose_sha256_does_not_match_its_content_is_rejected() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let xorb = stored_xorb(&server, &cache, 92).await;

    // The oid of a real object a later push would legitimately upload.
    let victim_oid = hex::encode(Sha256::digest(b"an object nobody has pushed here yet"));
    let (status, body) = post_shard(
        &server,
        one_chunk_file_shard(
            &xorb,
            None,
            Some(MerkleHash::from_hex(&victim_oid).unwrap()),
        ),
    )
    .await;
    assert_eq!(status, 400, "body: {body}");
    assert!(body.contains("sha256 mismatch"), "body: {body}");
    assert!(
        server
            .state
            .index
            .file_by_sha256(&victim_oid)
            .await
            .unwrap()
            .is_none(),
        "the squatted oid reached the index"
    );

    // The consequence that matters: a later push of the real object must still
    // be asked for, not reported as already stored.
    let object = upload_batch_object(&server, &victim_oid).await;
    assert!(
        object["actions"].get("upload").is_some(),
        "a legitimate upload of {victim_oid} was skipped as already stored: {object}"
    );

    // A shard carrying the sha256 of the content it really describes is fine.
    let true_oid = hex::encode(Sha256::digest(chunk_zero_bytes(&server, &xorb).await));
    let (status, body) = post_shard(
        &server,
        one_chunk_file_shard(&xorb, None, Some(MerkleHash::from_hex(&true_oid).unwrap())),
    )
    .await;
    assert_eq!(status, 200, "body: {body}");
    assert!(
        server
            .state
            .index
            .file_by_sha256(&true_oid)
            .await
            .unwrap()
            .is_some(),
        "an honest sha256 must be registered"
    );
}

/// `chunks` has chunk_hash as its PRIMARY KEY and is written with
/// INSERT OR IGNORE, so the first mapping for a hash wins permanently. A
/// mapping the stored xorbs do not corroborate must therefore never be written.
#[tokio::test(flavor = "multi_thread")]
async fn dedup_mappings_the_stored_xorbs_do_not_corroborate_are_not_indexed() {
    let server = TestServer::start().await;
    let cache = TempDir::new().unwrap();
    let xorb = stored_xorb(&server, &cache, 93).await;

    // A chunk of a xorb we hold that is not indexed yet: neither the file's
    // first chunk nor dedup-eligible by hash.
    let mut target = None;
    for index in 0..xorb.record.num_chunks as usize {
        let (hash, _len) = xorb.chunk(index);
        if server
            .state
            .index
            .lookup_chunk(&hash.hex())
            .await
            .unwrap()
            .is_none()
        {
            target = Some((index as u32, hash));
            break;
        }
    }
    let (index, chunk_hash) = target.expect("the xorb must have an unindexed chunk");

    // Poison 1: the right chunk hash, attributed to a xorb this server does not
    // hold. Indexed, it would answer every later dedup probe for that chunk
    // with a shard naming a xorb that cannot be fetched.
    let (status, body) = post_shard(&server, dedup_flag_shard(absent(), index, chunk_hash)).await;
    assert_eq!(
        status, 200,
        "an advisory dedup hint must not fail the shard: {body}"
    );
    assert!(
        server
            .state
            .index
            .lookup_chunk(&chunk_hash.hex())
            .await
            .unwrap()
            .is_none(),
        "a mapping naming an absent xorb was indexed"
    );

    // Poison 2: a xorb we do hold, at an index whose stored chunk hash is
    // something else entirely.
    let (status, body) = post_shard(&server, dedup_flag_shard(xorb.hash, index, absent())).await;
    assert_eq!(status, 200, "body: {body}");
    assert!(
        server
            .state
            .index
            .lookup_chunk(ABSENT)
            .await
            .unwrap()
            .is_none(),
        "a mapping contradicting the stored xorb was indexed"
    );

    // The correct mapping, which either poison would otherwise have blocked.
    let (status, body) = post_shard(&server, dedup_flag_shard(xorb.hash, index, chunk_hash)).await;
    assert_eq!(status, 200, "body: {body}");
    let resolved = server
        .state
        .index
        .lookup_chunk(&chunk_hash.hex())
        .await
        .unwrap()
        .expect("the corroborated mapping must be indexed");
    assert_eq!(
        resolved,
        (xorb.hash.hex(), index),
        "the correct mapping was shadowed"
    );
}
