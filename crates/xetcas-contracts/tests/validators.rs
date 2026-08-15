//! Validator tables: one accept/reject set per message that carries rules.
//!
//! Every Liquid Proto `expr` in `proto/xetcas/v1/*.proto` gets at least one
//! rejecting case here, and every cross-field invariant gets one too. The Go
//! twin of the refinement half lives in `go/xetcasv1/validate_test.go`; the
//! two must agree.

mod common;

use std::collections::HashMap;

use xetcas_contracts::constants::{HASH_BYTES, MAX_CHUNKS_PER_XORB, MAX_XORB_UNPACKED_BYTES};
use xetcas_contracts::v1::validate::*;
use xetcas_contracts::v1::*;
use xetcas_contracts::ValidationError;

use common::{EXAMPLE_HASH, OTHER_HASH};

const SHORT_HASH: &str = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef12345";
const LONG_HASH: &str = "a1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef1234567";
const UPPER_HASH: &str = "A1B2C3D4E5F6789012345678901234567890ABCDEF1234567890ABCDEF123456";
const NON_HEX_HASH: &str = "g1b2c3d4e5f6789012345678901234567890abcdef1234567890abcdef123456";

const HASH_HEX_PREDICATE: &str = "len(this) == 64 && matches(this, \"^[0-9a-f]{64}$\")";
const NON_EMPTY_PREDICATE: &str = "len(this) >= 1";

/// Every spelling a 64-char lowercase-hex refinement must reject.
const BAD_HASHES: &[&str] = &[
    "",
    SHORT_HASH,
    LONG_HASH,
    UPPER_HASH,
    NON_HEX_HASH,
    // 64 characters, but not hex at all.
    "zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz",
];

#[track_caller]
fn assert_violation(result: Result<(), ValidationError>, field: &str, predicate: &str) {
    let error = result.expect_err("expected a validation error");
    assert_eq!(error.field, field, "wrong field in {error}");
    assert_eq!(error.predicate, predicate, "wrong predicate in {error}");
}

// ---------------------------------------------------------------------------
// transfer.proto
// ---------------------------------------------------------------------------

#[test]
fn index_range_rejects_inversion_but_allows_an_empty_range() {
    validate_index_range(&IndexRange { start: 0, end: 4 }).expect("ordered");
    validate_index_range(&IndexRange { start: 4, end: 4 }).expect("empty ranges are legal");
    assert_violation(
        validate_index_range(&IndexRange { start: 5, end: 4 }),
        "start",
        "this <= end",
    );
}

#[test]
fn byte_range_rejects_inversion() {
    validate_byte_range(&ByteRange {
        start: 0,
        end: 131_071,
    })
    .expect("ordered");
    // An end-inclusive range of one byte.
    validate_byte_range(&ByteRange { start: 9, end: 9 }).expect("single byte");
    assert_violation(
        validate_byte_range(&ByteRange { start: 10, end: 9 }),
        "start",
        "this <= end",
    );
}

#[test]
fn query_reconstruction_request_checks_the_file_id_and_range() {
    let valid = QueryReconstructionRequest {
        file_id: EXAMPLE_HASH.to_owned(),
        range: Some(ByteRange { start: 0, end: 99 }),
    };
    validate_query_reconstruction_request(&valid).expect("valid");

    // Absent Range means the whole file; still valid.
    validate_query_reconstruction_request(&QueryReconstructionRequest {
        file_id: EXAMPLE_HASH.to_owned(),
        range: None,
    })
    .expect("no Range header");

    for bad in BAD_HASHES {
        assert_violation(
            validate_query_reconstruction_request(&QueryReconstructionRequest {
                file_id: (*bad).to_owned(),
                range: None,
            }),
            "file_id",
            HASH_HEX_PREDICATE,
        );
    }

    assert_violation(
        validate_query_reconstruction_request(&QueryReconstructionRequest {
            file_id: EXAMPLE_HASH.to_owned(),
            range: Some(ByteRange { start: 100, end: 9 }),
        }),
        "start",
        "this <= end",
    );
}

#[test]
fn reconstruction_term_requires_a_hex_hash_and_a_present_range() {
    let valid = CasReconstructionTerm {
        hash: EXAMPLE_HASH.to_owned(),
        range: Some(IndexRange { start: 0, end: 4 }),
        unpacked_length: 263_873,
    };
    validate_cas_reconstruction_term(&valid).expect("valid");

    assert_violation(
        validate_cas_reconstruction_term(&CasReconstructionTerm {
            hash: UPPER_HASH.to_owned(),
            ..valid.clone()
        }),
        "hash",
        HASH_HEX_PREDICATE,
    );
    assert_violation(
        validate_cas_reconstruction_term(&CasReconstructionTerm {
            range: None,
            ..valid.clone()
        }),
        "range",
        "message field is present",
    );
    assert_violation(
        validate_cas_reconstruction_term(&CasReconstructionTerm {
            range: Some(IndexRange { start: 4, end: 0 }),
            ..valid
        }),
        "start",
        "this <= end",
    );
}

#[test]
fn fetch_info_requires_a_url_and_both_ranges() {
    let valid = CasReconstructionFetchInfo {
        range: Some(IndexRange { start: 0, end: 4 }),
        url: "https://transfer.example/xorb".to_owned(),
        url_range: Some(ByteRange {
            start: 0,
            end: 131_071,
        }),
    };
    validate_cas_reconstruction_fetch_info(&valid).expect("valid");

    assert_violation(
        validate_cas_reconstruction_fetch_info(&CasReconstructionFetchInfo {
            url: String::new(),
            ..valid.clone()
        }),
        "url",
        NON_EMPTY_PREDICATE,
    );
    assert_violation(
        validate_cas_reconstruction_fetch_info(&CasReconstructionFetchInfo {
            range: None,
            ..valid.clone()
        }),
        "range",
        "message field is present",
    );
    assert_violation(
        validate_cas_reconstruction_fetch_info(&CasReconstructionFetchInfo {
            url_range: None,
            ..valid
        }),
        "url_range",
        "message field is present",
    );
}

#[test]
fn reconstruction_response_rejects_a_non_hash_fetch_info_key() {
    let mut response = common::openapi_v1_response();
    validate_query_reconstruction_response(&response).expect("the OpenAPI example is valid");

    let list = response
        .fetch_info
        .remove(EXAMPLE_HASH)
        .expect("example key");
    response.fetch_info.insert("not-a-hash".to_owned(), list);
    assert_violation(
        validate_query_reconstruction_response(&response),
        "fetch_info",
        HASH_HEX_PREDICATE,
    );
}

#[test]
fn reconstruction_response_walks_into_nested_terms_and_entries() {
    let mut response = common::openapi_v1_response();
    response.terms[0].hash = NON_HEX_HASH.to_owned();
    let error = validate_query_reconstruction_response(&response).expect_err("nested failure");
    assert_eq!(error.message, "candace.xetcas.v1.CasReconstructionTerm");
    assert_eq!(error.field, "hash");

    let mut response = common::openapi_v1_response();
    response
        .fetch_info
        .get_mut(EXAMPLE_HASH)
        .expect("example key")
        .entries[0]
        .url
        .clear();
    let error = validate_query_reconstruction_response(&response).expect_err("nested failure");
    assert_eq!(
        error.message,
        "candace.xetcas.v1.CasReconstructionFetchInfo"
    );
    assert_eq!(error.field, "url");
}

#[test]
fn v2_response_validates_multi_range_entries() {
    let response = common::openapi_v2_response();
    validate_query_reconstruction_response_v2(&response).expect("the OpenAPI example is valid");

    let mut broken = common::openapi_v2_response();
    broken
        .xorbs
        .get_mut(EXAMPLE_HASH)
        .expect("example key")
        .entries[0]
        .url
        .clear();
    assert_violation(
        validate_query_reconstruction_response_v2(&broken),
        "url",
        NON_EMPTY_PREDICATE,
    );

    let mut broken = common::openapi_v2_response();
    broken
        .xorbs
        .get_mut(EXAMPLE_HASH)
        .expect("example key")
        .entries[0]
        .ranges[0]
        .bytes = None;
    assert_violation(
        validate_query_reconstruction_response_v2(&broken),
        "bytes",
        "message field is present",
    );
}

#[test]
fn path_parameter_messages_require_a_prefix_and_a_hex_hash() {
    validate_chunk_dedup_query(&ChunkDedupQuery {
        prefix: "default".to_owned(),
        hash: EXAMPLE_HASH.to_owned(),
    })
    .expect("the prefix the real client sends");
    // The OpenAPI spec's documented dedup prefix; a permissive server accepts
    // any non-empty prefix.
    validate_chunk_dedup_query(&ChunkDedupQuery {
        prefix: "default-merkledb".to_owned(),
        hash: EXAMPLE_HASH.to_owned(),
    })
    .expect("any non-empty prefix");

    assert_violation(
        validate_chunk_dedup_query(&ChunkDedupQuery {
            prefix: String::new(),
            hash: EXAMPLE_HASH.to_owned(),
        }),
        "prefix",
        NON_EMPTY_PREDICATE,
    );
    assert_violation(
        validate_chunk_dedup_query(&ChunkDedupQuery {
            prefix: "default".to_owned(),
            hash: SHORT_HASH.to_owned(),
        }),
        "hash",
        HASH_HEX_PREDICATE,
    );

    validate_upload_xorb_key(&UploadXorbKey {
        prefix: "default".to_owned(),
        hash: EXAMPLE_HASH.to_owned(),
    })
    .expect("valid");
    assert_violation(
        validate_upload_xorb_key(&UploadXorbKey {
            prefix: String::new(),
            hash: EXAMPLE_HASH.to_owned(),
        }),
        "prefix",
        NON_EMPTY_PREDICATE,
    );
    assert_violation(
        validate_upload_xorb_key(&UploadXorbKey {
            prefix: "default".to_owned(),
            hash: NON_HEX_HASH.to_owned(),
        }),
        "hash",
        HASH_HEX_PREDICATE,
    );
}

#[test]
fn upload_shard_response_result_is_zero_or_one() {
    for result in [0, 1] {
        validate_upload_shard_response(&UploadShardResponse { result }).expect("in contract");
    }
    for result in [2, 3, u32::MAX] {
        assert_violation(
            validate_upload_shard_response(&UploadShardResponse { result }),
            "result",
            "this <= 1",
        );
    }
}

// ---------------------------------------------------------------------------
// storage.proto
// ---------------------------------------------------------------------------

fn valid_xorb_record() -> XorbRecord {
    XorbRecord {
        xorb_hash: EXAMPLE_HASH.to_owned(),
        num_chunks: 3,
        frames_length: 300,
        unpacked_length: 384,
        chunk_boundary_offsets: vec![100, 200, 300],
        unpacked_chunk_offsets: vec![128, 256, 384],
        chunk_hashes: vec![0u8; HASH_BYTES * 3],
        created_at: 1_756_489_133,
    }
}

#[test]
fn xorb_record_accepts_a_consistent_record() {
    validate_xorb_record(&valid_xorb_record()).expect("valid");
}

#[test]
fn xorb_record_enforces_its_field_refinements() {
    for bad in BAD_HASHES {
        assert_violation(
            validate_xorb_record(&XorbRecord {
                xorb_hash: (*bad).to_owned(),
                ..valid_xorb_record()
            }),
            "xorb_hash",
            HASH_HEX_PREDICATE,
        );
    }

    assert_violation(
        validate_xorb_record(&XorbRecord {
            num_chunks: 0,
            chunk_boundary_offsets: Vec::new(),
            unpacked_chunk_offsets: Vec::new(),
            chunk_hashes: Vec::new(),
            ..valid_xorb_record()
        }),
        "num_chunks",
        "this >= 1 && this <= 8192",
    );
    assert_violation(
        validate_xorb_record(&XorbRecord {
            num_chunks: MAX_CHUNKS_PER_XORB + 1,
            ..valid_xorb_record()
        }),
        "num_chunks",
        "this >= 1 && this <= 8192",
    );
    assert_violation(
        validate_xorb_record(&XorbRecord {
            unpacked_length: MAX_XORB_UNPACKED_BYTES + 1,
            ..valid_xorb_record()
        }),
        "unpacked_length",
        "this <= 67108864",
    );
}

#[test]
fn xorb_record_enforces_its_parallel_array_invariants() {
    assert_violation(
        validate_xorb_record(&XorbRecord {
            chunk_boundary_offsets: vec![100, 300],
            ..valid_xorb_record()
        }),
        "chunk_boundary_offsets",
        "len(this) == num_chunks",
    );
    assert_violation(
        validate_xorb_record(&XorbRecord {
            unpacked_chunk_offsets: vec![128, 256, 384, 512],
            ..valid_xorb_record()
        }),
        "unpacked_chunk_offsets",
        "len(this) == num_chunks",
    );
    assert_violation(
        validate_xorb_record(&XorbRecord {
            chunk_hashes: vec![0u8; HASH_BYTES * 2],
            ..valid_xorb_record()
        }),
        "chunk_hashes",
        "len(this) == 32 * num_chunks",
    );
    // A trailing boundary that disagrees with frames_length would hand out
    // url_ranges past the end of the stored frames.
    assert_violation(
        validate_xorb_record(&XorbRecord {
            frames_length: 301,
            ..valid_xorb_record()
        }),
        "chunk_boundary_offsets",
        "last(this) == frames_length",
    );
    assert_violation(
        validate_xorb_record(&XorbRecord {
            unpacked_length: 385,
            ..valid_xorb_record()
        }),
        "unpacked_chunk_offsets",
        "last(this) == unpacked_length",
    );
}

#[test]
fn xorb_record_accepts_the_documented_limits_exactly() {
    let mut record = valid_xorb_record();
    record.num_chunks = 1;
    record.frames_length = 64;
    record.unpacked_length = MAX_XORB_UNPACKED_BYTES;
    record.chunk_boundary_offsets = vec![64];
    record.unpacked_chunk_offsets = vec![u32::try_from(MAX_XORB_UNPACKED_BYTES).expect("64 MiB")];
    record.chunk_hashes = vec![0u8; HASH_BYTES];
    validate_xorb_record(&record).expect("a xorb at exactly 64 MiB is legal");
}

fn valid_file_record() -> FileRecord {
    FileRecord {
        file_hash: EXAMPLE_HASH.to_owned(),
        file_length: 1_536,
        sha256: OTHER_HASH.to_owned(),
        terms: vec![
            FileTermRecord {
                xorb_hash: EXAMPLE_HASH.to_owned(),
                chunk_index_start: 0,
                chunk_index_end: 2,
                unpacked_segment_bytes: 1_024,
            },
            FileTermRecord {
                xorb_hash: OTHER_HASH.to_owned(),
                chunk_index_start: 4,
                chunk_index_end: 5,
                unpacked_segment_bytes: 512,
            },
        ],
        verification_range_hashes: vec![0u8; HASH_BYTES * 2],
        created_at: 1_756_489_133,
    }
}

#[test]
fn file_record_accepts_a_consistent_record() {
    validate_file_record(&valid_file_record()).expect("valid");

    // A shard without FileMetadataExt carries no sha256, and one without
    // verification entries carries no range hashes.
    let mut bare = valid_file_record();
    bare.sha256 = String::new();
    bare.verification_range_hashes = Vec::new();
    validate_file_record(&bare).expect("absent sha256 and verification hashes are legal");
}

#[test]
fn file_record_enforces_its_field_refinements() {
    for bad in BAD_HASHES {
        assert_violation(
            validate_file_record(&FileRecord {
                file_hash: (*bad).to_owned(),
                ..valid_file_record()
            }),
            "file_hash",
            HASH_HEX_PREDICATE,
        );
    }
    // The sha256 refinement differs: empty is legal, everything else must be
    // a full 64-char lowercase hex string.
    for bad in BAD_HASHES.iter().filter(|value| !value.is_empty()) {
        assert_violation(
            validate_file_record(&FileRecord {
                sha256: (*bad).to_owned(),
                ..valid_file_record()
            }),
            "sha256",
            "len(this) == 0 || (len(this) == 64 && matches(this, \"^[0-9a-f]{64}$\"))",
        );
    }
}

#[test]
fn file_record_enforces_its_cross_field_invariants() {
    assert_violation(
        validate_file_record(&FileRecord {
            file_length: 1_537,
            ..valid_file_record()
        }),
        "file_length",
        "this == sum(terms.unpacked_segment_bytes)",
    );
    assert_violation(
        validate_file_record(&FileRecord {
            verification_range_hashes: vec![0u8; HASH_BYTES],
            ..valid_file_record()
        }),
        "verification_range_hashes",
        "len(this) == 0 || len(this) == 32 * len(terms)",
    );
    // Nested terms are validated too.
    let mut broken = valid_file_record();
    broken.terms[1].xorb_hash = SHORT_HASH.to_owned();
    let error = validate_file_record(&broken).expect_err("nested failure");
    assert_eq!(error.message, "candace.xetcas.v1.FileTermRecord");
    assert_eq!(error.field, "xorb_hash");
}

#[test]
fn file_term_record_rejects_an_inverted_chunk_range() {
    validate_file_term_record(&FileTermRecord {
        xorb_hash: EXAMPLE_HASH.to_owned(),
        chunk_index_start: 2,
        chunk_index_end: 2,
        unpacked_segment_bytes: 0,
    })
    .expect("an empty term is structurally legal");

    assert_violation(
        validate_file_term_record(&FileTermRecord {
            xorb_hash: EXAMPLE_HASH.to_owned(),
            chunk_index_start: 3,
            chunk_index_end: 2,
            unpacked_segment_bytes: 10,
        }),
        "chunk_index_start",
        "this <= chunk_index_end",
    );
}

// ---------------------------------------------------------------------------
// bridge.proto
// ---------------------------------------------------------------------------

#[test]
fn lfs_object_spec_requires_a_hex_oid() {
    validate_lfs_object_spec(&LfsObjectSpec {
        oid: EXAMPLE_HASH.to_owned(),
        size: 1,
    })
    .expect("valid");
    for bad in BAD_HASHES {
        assert_violation(
            validate_lfs_object_spec(&LfsObjectSpec {
                oid: (*bad).to_owned(),
                size: 1,
            }),
            "oid",
            HASH_HEX_PREDICATE,
        );
    }
}

fn valid_batch_request() -> LfsBatchRequest {
    LfsBatchRequest {
        operation: "upload".to_owned(),
        transfers: vec!["xet".to_owned(), "basic".to_owned()],
        r#ref: Some(LfsRef {
            name: "refs/heads/main".to_owned(),
        }),
        objects: vec![LfsObjectSpec {
            oid: EXAMPLE_HASH.to_owned(),
            size: 1_048_576,
        }],
        hash_algo: "sha256".to_owned(),
    }
}

#[test]
fn lfs_batch_request_pins_operation_and_hash_algo() {
    validate_lfs_batch_request(&valid_batch_request()).expect("valid");
    validate_lfs_batch_request(&LfsBatchRequest {
        operation: "download".to_owned(),
        ..valid_batch_request()
    })
    .expect("download is the other legal operation");
    validate_lfs_batch_request(&LfsBatchRequest {
        hash_algo: String::new(),
        ..valid_batch_request()
    })
    .expect("git-lfs may omit hash_algo");

    for operation in ["", "verify", "Upload", "upload "] {
        assert_violation(
            validate_lfs_batch_request(&LfsBatchRequest {
                operation: operation.to_owned(),
                ..valid_batch_request()
            }),
            "operation",
            "this == \"upload\" || this == \"download\"",
        );
    }
    for algo in ["sha1", "SHA256", "sha256 "] {
        assert_violation(
            validate_lfs_batch_request(&LfsBatchRequest {
                hash_algo: algo.to_owned(),
                ..valid_batch_request()
            }),
            "hash_algo",
            "len(this) == 0 || this == \"sha256\"",
        );
    }

    // Nested objects are validated.
    let error = validate_lfs_batch_request(&LfsBatchRequest {
        objects: vec![LfsObjectSpec {
            oid: UPPER_HASH.to_owned(),
            size: 1,
        }],
        ..valid_batch_request()
    })
    .expect_err("nested failure");
    assert_eq!(error.message, "candace.xetcas.v1.LfsObjectSpec");
}

#[test]
fn lfs_action_requires_an_href() {
    validate_lfs_action(&LfsAction {
        href: "https://lfs.example/token".to_owned(),
        header: HashMap::new(),
        expires_in: 0,
    })
    .expect("valid");
    assert_violation(
        validate_lfs_action(&LfsAction {
            href: String::new(),
            header: HashMap::new(),
            expires_in: 3600,
        }),
        "href",
        NON_EMPTY_PREDICATE,
    );
}

#[test]
fn lfs_batch_object_rejects_actions_and_error_together() {
    let skip_entry = LfsBatchObject {
        oid: EXAMPLE_HASH.to_owned(),
        size: 1,
        authenticated: false,
        actions: HashMap::new(),
        error: None,
    };
    validate_lfs_batch_object(&skip_entry)
        .expect("no actions and no error means 'already have it'");

    let action = LfsAction {
        href: "https://lfs.example/token".to_owned(),
        header: HashMap::new(),
        expires_in: 0,
    };
    validate_lfs_batch_object(&LfsBatchObject {
        actions: HashMap::from([("upload".to_owned(), action.clone())]),
        ..skip_entry.clone()
    })
    .expect("an action alone is legal");
    validate_lfs_batch_object(&LfsBatchObject {
        error: Some(LfsObjectError {
            code: 404,
            message: "missing".to_owned(),
        }),
        ..skip_entry.clone()
    })
    .expect("an error alone is legal");

    assert_violation(
        validate_lfs_batch_object(&LfsBatchObject {
            actions: HashMap::from([("upload".to_owned(), action.clone())]),
            error: Some(LfsObjectError {
                code: 422,
                message: "validation error".to_owned(),
            }),
            ..skip_entry.clone()
        }),
        "actions",
        "at most one of actions/error is populated",
    );

    // Nested actions are validated.
    let error = validate_lfs_batch_object(&LfsBatchObject {
        actions: HashMap::from([(
            "upload".to_owned(),
            LfsAction {
                href: String::new(),
                ..action
            },
        )]),
        ..skip_entry
    })
    .expect_err("nested failure");
    assert_eq!(error.message, "candace.xetcas.v1.LfsAction");
}

#[test]
fn lfs_batch_response_pins_the_transfer_name() {
    for transfer in ["xet", "basic"] {
        validate_lfs_batch_response(&LfsBatchResponse {
            transfer: transfer.to_owned(),
            objects: Vec::new(),
            hash_algo: "sha256".to_owned(),
        })
        .expect("negotiated transfer");
    }
    for transfer in ["", "tus", "Xet", "lfs-standalone-file"] {
        assert_violation(
            validate_lfs_batch_response(&LfsBatchResponse {
                transfer: transfer.to_owned(),
                objects: Vec::new(),
                hash_algo: String::new(),
            }),
            "transfer",
            "this == \"xet\" || this == \"basic\"",
        );
    }
}

#[test]
fn cas_token_info_requires_both_url_and_token() {
    validate_cas_token_info(&CasTokenInfo {
        cas_url: "https://cas.example".to_owned(),
        exp: 1_756_489_133,
        access_token: "ey...jQ".to_owned(),
    })
    .expect("valid");
    assert_violation(
        validate_cas_token_info(&CasTokenInfo {
            cas_url: String::new(),
            exp: 1,
            access_token: "ey...jQ".to_owned(),
        }),
        "cas_url",
        NON_EMPTY_PREDICATE,
    );
    assert_violation(
        validate_cas_token_info(&CasTokenInfo {
            cas_url: "https://cas.example".to_owned(),
            exp: 1,
            access_token: String::new(),
        }),
        "access_token",
        NON_EMPTY_PREDICATE,
    );
}

#[test]
fn validation_errors_name_the_message_field_and_predicate() {
    let error = validate_upload_shard_response(&UploadShardResponse { result: 9 })
        .expect_err("out of contract");
    assert_eq!(error.message, "candace.xetcas.v1.UploadShardResponse");
    assert_eq!(error.field, "result");
    assert_eq!(error.predicate, "this <= 1");

    let rendered = error.to_string();
    for fragment in [
        "candace.xetcas.v1.UploadShardResponse",
        "result",
        "this <= 1",
    ] {
        assert!(rendered.contains(fragment), "{rendered} lacks {fragment}");
    }
    // The rejected value is never echoed: these errors go into logs.
    assert!(!rendered.contains('9'));
}
