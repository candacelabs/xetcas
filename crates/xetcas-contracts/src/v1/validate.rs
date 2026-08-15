//! Boundary validators for the `candace.xetcas.v1` messages.
//!
//! Two classes of rule live here:
//!
//! 1. **Liquid Proto refinements.** Every `(candace.liquid.v1.field).expr` in
//!    `proto/xetcas/v1/*.proto` is mirrored by exactly one check below, and
//!    each check is commented with the predicate it mirrors verbatim. The Go
//!    side gets the same rules mechanically from `protoc-gen-liquidproto`;
//!    this module is the hand-written Rust twin, so the two must be read
//!    together when a predicate changes.
//! 2. **Cross-field invariants.** Predicates range over a single scalar
//!    (`this`), so relationships between fields — range ordering, parallel
//!    array lengths, derived totals — cannot be expressed in the schema. They
//!    are documented in the proto comments and enforced here.
//!
//! Validators are non-recursive only where a nested message has no rules of
//! its own; otherwise a parent delegates to the child's validator so an error
//! always names the message that actually broke its contract.

use std::collections::HashMap;

use super::{
    ByteRange, CasReconstructionFetchInfo, CasReconstructionTerm, CasTokenInfo, ChunkDedupQuery,
    FetchInfoList, FileRecord, FileTermRecord, IndexRange, LfsAction, LfsBatchObject,
    LfsBatchRequest, LfsBatchResponse, LfsObjectSpec, QueryReconstructionRequest,
    QueryReconstructionResponse, QueryReconstructionResponseV2, UploadShardResponse, UploadXorbKey,
    XorbFetchList, XorbMultiRangeFetch, XorbRangeDescriptor, XorbRecord,
};
use crate::constants::{HASH_BYTES, HASH_HEX_LEN};

/// A single violated contract rule.
///
/// The three parts identify the rule precisely enough to fix it without
/// reading the validator: the fully-qualified protobuf message, the protobuf
/// field name (empty for whole-message invariants), and the predicate that
/// failed — quoted verbatim from the `.proto` where the rule is a Liquid
/// Proto refinement.
///
/// The rejected value is deliberately **not** carried: these messages come
/// from untrusted network input and the error is meant to be safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("{message}: field {field:?} violates predicate {predicate:?}")]
pub struct ValidationError {
    /// Fully-qualified protobuf message name, e.g. `candace.xetcas.v1.FileRecord`.
    pub message: &'static str,
    /// Protobuf field name, or `""` when the rule spans the whole message.
    pub field: &'static str,
    /// The violated predicate.
    pub predicate: &'static str,
}

impl ValidationError {
    /// Builds an error naming the message, field, and violated predicate.
    #[must_use]
    pub const fn new(message: &'static str, field: &'static str, predicate: &'static str) -> Self {
        Self {
            message,
            field,
            predicate,
        }
    }
}

/// Mirrors `len(this) == 64 && matches(this, "^[0-9a-f]{64}$")`.
///
/// Both halves of the predicate collapse to one ASCII scan: the refinement
/// pins the byte length at 64 and the character class is ASCII-only, so 64
/// lowercase-hex bytes is exactly the accepted set.
fn is_hash_hex(value: &str) -> bool {
    value.len() == HASH_HEX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

const HASH_HEX_PREDICATE: &str = "len(this) == 64 && matches(this, \"^[0-9a-f]{64}$\")";
const NON_EMPTY_PREDICATE: &str = "len(this) >= 1";
const PRESENT_PREDICATE: &str = "message field is present";

fn require_hash_hex(
    message: &'static str,
    field: &'static str,
    value: &str,
) -> Result<(), ValidationError> {
    if is_hash_hex(value) {
        Ok(())
    } else {
        Err(ValidationError::new(message, field, HASH_HEX_PREDICATE))
    }
}

fn require_non_empty(
    message: &'static str,
    field: &'static str,
    value: &str,
) -> Result<(), ValidationError> {
    if value.is_empty() {
        Err(ValidationError::new(message, field, NON_EMPTY_PREDICATE))
    } else {
        Ok(())
    }
}

fn require_present<'a, T>(
    message: &'static str,
    field: &'static str,
    value: &'a Option<T>,
) -> Result<&'a T, ValidationError> {
    value
        .as_ref()
        .ok_or(ValidationError::new(message, field, PRESENT_PREDICATE))
}

/// Validates the `fetch_info` / `xorbs` map keys, which the schema documents
/// as xorb hashes but cannot refine (map keys carry no field options).
fn require_hash_hex_keys<V>(
    message: &'static str,
    field: &'static str,
    map: &HashMap<String, V>,
) -> Result<(), ValidationError> {
    for key in map.keys() {
        if !is_hash_hex(key) {
            return Err(ValidationError::new(message, field, HASH_HEX_PREDICATE));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// transfer.proto
// ---------------------------------------------------------------------------

/// Validates [`IndexRange`], the end-EXCLUSIVE chunk-index range `[start, end)`.
///
/// The message carries no refinements; the rule is the cross-field ordering
/// documented on it ("start < end for non-empty ranges … enforced by the
/// boundary validators"). Empty ranges (`start == end`) are accepted; an
/// inverted range is not.
pub fn validate_index_range(value: &IndexRange) -> Result<(), ValidationError> {
    if value.start > value.end {
        return Err(ValidationError::new(
            "candace.xetcas.v1.IndexRange",
            "start",
            "this <= end",
        ));
    }
    Ok(())
}

/// Validates [`ByteRange`], the end-INCLUSIVE byte range `[start, end]`.
///
/// Cross-field invariant only: an inclusive range with `start > end` names no
/// bytes and cannot be rendered as a `bytes={start}-{end}` header.
pub fn validate_byte_range(value: &ByteRange) -> Result<(), ValidationError> {
    if value.start > value.end {
        return Err(ValidationError::new(
            "candace.xetcas.v1.ByteRange",
            "start",
            "this <= end",
        ));
    }
    Ok(())
}

/// Validates [`QueryReconstructionRequest`].
pub fn validate_query_reconstruction_request(
    value: &QueryReconstructionRequest,
) -> Result<(), ValidationError> {
    // file_id: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex(
        "candace.xetcas.v1.QueryReconstructionRequest",
        "file_id",
        &value.file_id,
    )?;
    // Cross-field: the optional Range header, when present, must be orderable.
    if let Some(range) = &value.range {
        validate_byte_range(range)?;
    }
    Ok(())
}

/// Validates [`CasReconstructionTerm`].
pub fn validate_cas_reconstruction_term(
    value: &CasReconstructionTerm,
) -> Result<(), ValidationError> {
    // hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex(
        "candace.xetcas.v1.CasReconstructionTerm",
        "hash",
        &value.hash,
    )?;
    // Cross-field: `range` is required on the wire (the OpenAPI schema marks
    // it required with additionalProperties: false), and must be ordered.
    let range = require_present(
        "candace.xetcas.v1.CasReconstructionTerm",
        "range",
        &value.range,
    )?;
    validate_index_range(range)
}

/// Validates [`CasReconstructionFetchInfo`].
pub fn validate_cas_reconstruction_fetch_info(
    value: &CasReconstructionFetchInfo,
) -> Result<(), ValidationError> {
    // url: len(this) >= 1
    require_non_empty(
        "candace.xetcas.v1.CasReconstructionFetchInfo",
        "url",
        &value.url,
    )?;
    // Cross-field: both ranges are required on the wire and must be ordered.
    let range = require_present(
        "candace.xetcas.v1.CasReconstructionFetchInfo",
        "range",
        &value.range,
    )?;
    validate_index_range(range)?;
    let url_range = require_present(
        "candace.xetcas.v1.CasReconstructionFetchInfo",
        "url_range",
        &value.url_range,
    )?;
    validate_byte_range(url_range)
}

/// Validates every entry of a [`FetchInfoList`].
pub fn validate_fetch_info_list(value: &FetchInfoList) -> Result<(), ValidationError> {
    for entry in &value.entries {
        validate_cas_reconstruction_fetch_info(entry)?;
    }
    Ok(())
}

/// Validates [`QueryReconstructionResponse`] (the v1 reconstruction body).
pub fn validate_query_reconstruction_response(
    value: &QueryReconstructionResponse,
) -> Result<(), ValidationError> {
    for term in &value.terms {
        validate_cas_reconstruction_term(term)?;
    }
    // Cross-field: fetch_info is keyed by xorb hash (documented on the field;
    // map keys cannot carry a refinement).
    require_hash_hex_keys(
        "candace.xetcas.v1.QueryReconstructionResponse",
        "fetch_info",
        &value.fetch_info,
    )?;
    for list in value.fetch_info.values() {
        validate_fetch_info_list(list)?;
    }
    Ok(())
}

/// Validates [`XorbRangeDescriptor`].
pub fn validate_xorb_range_descriptor(value: &XorbRangeDescriptor) -> Result<(), ValidationError> {
    // Cross-field: both halves are required on the wire and must be ordered.
    let chunks = require_present(
        "candace.xetcas.v1.XorbRangeDescriptor",
        "chunks",
        &value.chunks,
    )?;
    validate_index_range(chunks)?;
    let bytes = require_present(
        "candace.xetcas.v1.XorbRangeDescriptor",
        "bytes",
        &value.bytes,
    )?;
    validate_byte_range(bytes)
}

/// Validates [`XorbMultiRangeFetch`].
pub fn validate_xorb_multi_range_fetch(value: &XorbMultiRangeFetch) -> Result<(), ValidationError> {
    // url: len(this) >= 1
    require_non_empty("candace.xetcas.v1.XorbMultiRangeFetch", "url", &value.url)?;
    for range in &value.ranges {
        validate_xorb_range_descriptor(range)?;
    }
    Ok(())
}

/// Validates every entry of a [`XorbFetchList`].
pub fn validate_xorb_fetch_list(value: &XorbFetchList) -> Result<(), ValidationError> {
    for entry in &value.entries {
        validate_xorb_multi_range_fetch(entry)?;
    }
    Ok(())
}

/// Validates [`QueryReconstructionResponseV2`] (the multi-range body).
pub fn validate_query_reconstruction_response_v2(
    value: &QueryReconstructionResponseV2,
) -> Result<(), ValidationError> {
    for term in &value.terms {
        validate_cas_reconstruction_term(term)?;
    }
    // Cross-field: the xorbs map is keyed by xorb hash.
    require_hash_hex_keys(
        "candace.xetcas.v1.QueryReconstructionResponseV2",
        "xorbs",
        &value.xorbs,
    )?;
    for list in value.xorbs.values() {
        validate_xorb_fetch_list(list)?;
    }
    Ok(())
}

/// Validates [`ChunkDedupQuery`] (`GET /v1/chunks/{prefix}/{hash}`).
pub fn validate_chunk_dedup_query(value: &ChunkDedupQuery) -> Result<(), ValidationError> {
    // prefix: len(this) >= 1
    require_non_empty("candace.xetcas.v1.ChunkDedupQuery", "prefix", &value.prefix)?;
    // hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex("candace.xetcas.v1.ChunkDedupQuery", "hash", &value.hash)
}

/// Validates [`UploadXorbKey`] (`POST /v1/xorbs/{prefix}/{hash}` path params).
pub fn validate_upload_xorb_key(value: &UploadXorbKey) -> Result<(), ValidationError> {
    // prefix: len(this) >= 1
    require_non_empty("candace.xetcas.v1.UploadXorbKey", "prefix", &value.prefix)?;
    // hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex("candace.xetcas.v1.UploadXorbKey", "hash", &value.hash)
}

/// Validates [`UploadShardResponse`].
pub fn validate_upload_shard_response(value: &UploadShardResponse) -> Result<(), ValidationError> {
    // result: this <= 1   (0 = shard already existed, 1 = sync performed)
    if value.result > 1 {
        return Err(ValidationError::new(
            "candace.xetcas.v1.UploadShardResponse",
            "result",
            "this <= 1",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// storage.proto
// ---------------------------------------------------------------------------

/// Validates [`XorbRecord`], the per-xorb index entry.
///
/// Beyond the three field refinements this enforces the parallel-array
/// invariants documented on the message: one cumulative offset per chunk in
/// each offset array, 32 raw hash bytes per chunk, and the two arrays'
/// terminating entries agreeing with `frames_length` / `unpacked_length`.
pub fn validate_xorb_record(value: &XorbRecord) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.XorbRecord";

    // xorb_hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex(MESSAGE, "xorb_hash", &value.xorb_hash)?;
    // num_chunks: this >= 1 && this <= 8192
    if !(value.num_chunks >= 1 && value.num_chunks <= 8192) {
        return Err(ValidationError::new(
            MESSAGE,
            "num_chunks",
            "this >= 1 && this <= 8192",
        ));
    }
    // unpacked_length: this <= 67108864
    if value.unpacked_length > 67_108_864 {
        return Err(ValidationError::new(
            MESSAGE,
            "unpacked_length",
            "this <= 67108864",
        ));
    }

    let num_chunks = value.num_chunks as usize;
    // Cross-field: "one entry per chunk" for the physical offset array.
    if value.chunk_boundary_offsets.len() != num_chunks {
        return Err(ValidationError::new(
            MESSAGE,
            "chunk_boundary_offsets",
            "len(this) == num_chunks",
        ));
    }
    // Cross-field: "one entry per chunk" for the uncompressed offset array.
    if value.unpacked_chunk_offsets.len() != num_chunks {
        return Err(ValidationError::new(
            MESSAGE,
            "unpacked_chunk_offsets",
            "len(this) == num_chunks",
        ));
    }
    // Cross-field: "32 * num_chunks bytes" of concatenated raw chunk hashes.
    if value.chunk_hashes.len() != HASH_BYTES * num_chunks {
        return Err(ValidationError::new(
            MESSAGE,
            "chunk_hashes",
            "len(this) == 32 * num_chunks",
        ));
    }
    // Cross-field: "the last equals frames_length".
    let last_boundary = value
        .chunk_boundary_offsets
        .last()
        .copied()
        .expect("num_chunks >= 1 and the length was just checked");
    if u64::from(last_boundary) != value.frames_length {
        return Err(ValidationError::new(
            MESSAGE,
            "chunk_boundary_offsets",
            "last(this) == frames_length",
        ));
    }
    // Cross-field: "the last equals unpacked_length".
    let last_unpacked = value
        .unpacked_chunk_offsets
        .last()
        .copied()
        .expect("num_chunks >= 1 and the length was just checked");
    if u64::from(last_unpacked) != value.unpacked_length {
        return Err(ValidationError::new(
            MESSAGE,
            "unpacked_chunk_offsets",
            "last(this) == unpacked_length",
        ));
    }
    Ok(())
}

/// Validates [`FileTermRecord`], one ordered reconstruction term of a file.
pub fn validate_file_term_record(value: &FileTermRecord) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.FileTermRecord";

    // xorb_hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex(MESSAGE, "xorb_hash", &value.xorb_hash)?;
    // Cross-field: the chunk-index range is end-exclusive, so an inverted
    // range names no chunks.
    if value.chunk_index_start > value.chunk_index_end {
        return Err(ValidationError::new(
            MESSAGE,
            "chunk_index_start",
            "this <= chunk_index_end",
        ));
    }
    Ok(())
}

/// Validates [`FileRecord`], the per-file index entry.
///
/// Cross-field invariants: `file_length` is the sum of the terms' unpacked
/// segment bytes, and `verification_range_hashes` is either absent or exactly
/// 32 bytes per term.
pub fn validate_file_record(value: &FileRecord) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.FileRecord";

    // file_hash: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex(MESSAGE, "file_hash", &value.file_hash)?;
    // sha256: len(this) == 0 || (len(this) == 64 && matches(this, "^[0-9a-f]{64}$"))
    if !(value.sha256.is_empty() || is_hash_hex(&value.sha256)) {
        return Err(ValidationError::new(
            MESSAGE,
            "sha256",
            "len(this) == 0 || (len(this) == 64 && matches(this, \"^[0-9a-f]{64}$\"))",
        ));
    }
    for term in &value.terms {
        validate_file_term_record(term)?;
    }
    // Cross-field: "32 * len(terms) bytes when the shard carried verification
    // entries, else empty".
    if !value.verification_range_hashes.is_empty()
        && value.verification_range_hashes.len() != HASH_BYTES * value.terms.len()
    {
        return Err(ValidationError::new(
            MESSAGE,
            "verification_range_hashes",
            "len(this) == 0 || len(this) == 32 * len(terms)",
        ));
    }
    // Cross-field: "Total file length = sum of term unpacked_segment_bytes".
    let total: u64 = value
        .terms
        .iter()
        .map(|term| u64::from(term.unpacked_segment_bytes))
        .sum();
    if total != value.file_length {
        return Err(ValidationError::new(
            MESSAGE,
            "file_length",
            "this == sum(terms.unpacked_segment_bytes)",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// bridge.proto
// ---------------------------------------------------------------------------

/// Validates [`LfsObjectSpec`], one object named in a batch request.
pub fn validate_lfs_object_spec(value: &LfsObjectSpec) -> Result<(), ValidationError> {
    // oid: len(this) == 64 && matches(this, "^[0-9a-f]{64}$")
    require_hash_hex("candace.xetcas.v1.LfsObjectSpec", "oid", &value.oid)
}

/// Validates [`LfsBatchRequest`].
pub fn validate_lfs_batch_request(value: &LfsBatchRequest) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.LfsBatchRequest";

    // operation: this == "upload" || this == "download"
    if !(value.operation == "upload" || value.operation == "download") {
        return Err(ValidationError::new(
            MESSAGE,
            "operation",
            "this == \"upload\" || this == \"download\"",
        ));
    }
    // hash_algo: len(this) == 0 || this == "sha256"
    if !(value.hash_algo.is_empty() || value.hash_algo == "sha256") {
        return Err(ValidationError::new(
            MESSAGE,
            "hash_algo",
            "len(this) == 0 || this == \"sha256\"",
        ));
    }
    for object in &value.objects {
        validate_lfs_object_spec(object)?;
    }
    Ok(())
}

/// Validates [`LfsAction`].
pub fn validate_lfs_action(value: &LfsAction) -> Result<(), ValidationError> {
    // href: len(this) >= 1
    require_non_empty("candace.xetcas.v1.LfsAction", "href", &value.href)
}

/// Validates [`LfsBatchObject`], one object's entry in a batch response.
///
/// The message carries no field refinements; its rules are the documented
/// invariant "exactly one of actions/error is populated when work or failure
/// is being signaled" (neither populated is the legal "server already has
/// this object, skip it" answer) plus its nested actions.
pub fn validate_lfs_batch_object(value: &LfsBatchObject) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.LfsBatchObject";

    if !value.actions.is_empty() && value.error.is_some() {
        return Err(ValidationError::new(
            MESSAGE,
            "actions",
            "at most one of actions/error is populated",
        ));
    }
    for action in value.actions.values() {
        validate_lfs_action(action)?;
    }
    Ok(())
}

/// Validates [`LfsBatchResponse`].
pub fn validate_lfs_batch_response(value: &LfsBatchResponse) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.LfsBatchResponse";

    // transfer: this == "xet" || this == "basic"
    if !(value.transfer == "xet" || value.transfer == "basic") {
        return Err(ValidationError::new(
            MESSAGE,
            "transfer",
            "this == \"xet\" || this == \"basic\"",
        ));
    }
    for object in &value.objects {
        validate_lfs_batch_object(object)?;
    }
    Ok(())
}

/// Validates [`CasTokenInfo`], the token bootstrap/refresh body.
pub fn validate_cas_token_info(value: &CasTokenInfo) -> Result<(), ValidationError> {
    const MESSAGE: &str = "candace.xetcas.v1.CasTokenInfo";

    // cas_url: len(this) >= 1
    require_non_empty(MESSAGE, "cas_url", &value.cas_url)?;
    // access_token: len(this) >= 1
    require_non_empty(MESSAGE, "access_token", &value.access_token)
}
