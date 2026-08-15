//! Xorb upload and the data route reconstructions point at.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use xet_core_structures::merklehash::MerkleHash;
use xet_core_structures::metadata_shard::hash_is_global_dedup_eligible;
use xet_core_structures::xorb_object::{reconstruct_xorb_with_footer, XORB_CHUNK_HEADER_LENGTH};
use xetcas_contracts::constants::{HASH_BYTES, MAX_CHUNKS_PER_XORB, MAX_XORB_UNPACKED_BYTES};
use xetcas_contracts::v1::validate::{validate_upload_xorb_key, validate_xorb_record};
use xetcas_contracts::v1::{UploadXorbKey, UploadXorbResponse, XorbRecord};

use crate::error::{AppError, AppResult};
use crate::http_range::parse_range_header;
use crate::state::AppState;

/// Largest body accepted on the xorb and shard routes. A xorb is capped at
/// 64 MiB of content; the margin covers frame headers and the footer.
pub const MAX_UPLOAD_BYTES: usize = 68 * 1024 * 1024;

/// `POST /v1/xorbs/{prefix}/{hash}`.
///
/// The body is a bare concatenation of chunk frames with NO footer. The path
/// hash is client-asserted, so it is recomputed from the chunk data and the
/// upload rejected on mismatch (docs/research/dataplane.md section 8.1). The
/// object is stored with a canonical footer appended, leaving the frames at
/// offset 0 so fetch ranges index the stored file directly.
///
/// The body is taken as a stream, not as `Bytes`, so the concurrency permit can
/// be acquired BEFORE anything is buffered -- see
/// [`crate::state::DEFAULT_XORB_UPLOAD_CONCURRENCY`].
pub async fn upload(
    State(state): State<AppState>,
    Path((prefix, hash)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> AppResult<Json<UploadXorbResponse>> {
    state.authorize(&headers)?;

    let key = UploadXorbKey {
        prefix: prefix.clone(),
        hash: hash.clone(),
    };
    validate_upload_xorb_key(&key).map_err(AppError::bad_request)?;

    let expected =
        MerkleHash::from_hex(&hash).map_err(|e| AppError::bad_request(format!("bad hash: {e}")))?;

    // Bound how many near-limit bodies can be resident at once. The permit is
    // taken before `to_bytes`, so it caps buffered BYTES and not merely
    // verification work; releasing it at the end of the handler also covers the
    // second, footered copy `verify_xorb` builds.
    let _permit = state
        .upload_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::internal("upload semaphore closed"))?;

    let body = axum::body::to_bytes(body, MAX_UPLOAD_BYTES)
        .await
        .map_err(|e| AppError::bad_request(format!("reading xorb body: {e}")))?;

    if body.is_empty() {
        return Err(AppError::bad_request("empty xorb body"));
    }

    let (stored, record) = tokio::task::spawn_blocking(move || verify_xorb(&body, &expected))
        .await
        .map_err(|e| AppError::internal(format!("xorb verify task: {e}")))??;

    validate_xorb_record(&record).map_err(AppError::bad_request)?;

    let dedup_chunks = eligible_chunks(&record)?;
    let num_chunks = record.num_chunks;
    let disk_bytes = stored.len() as u64;

    // Serialize the check-write-insert against other uploads of the SAME hash.
    // The hash is compression-independent but the stored frame layout is not, so
    // a concurrent or later re-upload under a different compression policy must
    // not overwrite the first writer's blob while its record still describes the
    // original layout -- that would silently corrupt every reconstruction that
    // indexes it (docs/research/dataplane.md section 8.1). First writer wins.
    let _guard = state.xorb_locks.lock(&hash).await;
    if state.index.get_xorb(&hash).await?.is_some() {
        // An earlier writer already stored this xorb; its blob and record agree,
        // so leave the blob untouched.
        return Ok(Json(UploadXorbResponse {
            was_inserted: false,
        }));
    }

    // Write the object before indexing it: a record whose object is missing
    // would break every later reconstruction, while an orphan object is inert.
    state.xorbs.write_atomic(&hash, stored).await?;
    let was_inserted = state
        .index
        .put_xorb(record, dedup_chunks, disk_bytes)
        .await?;

    if was_inserted {
        tracing::info!(xorb = %hash, chunks = num_chunks, "stored xorb");
    }
    Ok(Json(UploadXorbResponse { was_inserted }))
}

/// Cheap structural guard against a memory-amplification xorb upload.
///
/// `reconstruct_xorb_with_footer` expands the ENTIRE body before the num_chunks
/// check runs, so a ~68 MiB body of empty 8-byte chunk headers parses into
/// ~8.9M chunks and ~1 GB of vectors before rejection; a burst of those (the
/// client documents 64 concurrent uploads) can OOM the server. Walk the frame
/// headers first -- read each header's compressed length and skip its payload,
/// WITHOUT hashing or allocating -- and bail the instant the count exceeds the
/// limit. Work is bounded to MAX_CHUNKS_PER_XORB + 1 header reads and O(1)
/// memory. Note a plain `body.len() / XORB_CHUNK_HEADER_LENGTH` bound cannot be
/// used: a legit 64 MiB xorb has ~64 KiB frames, so any real body over ~64 KiB
/// exceeds it and all normal traffic would be rejected.
/// (docs/research/api-surface.md section 1.5.)
fn chunk_frame_count_exceeds_limit(body: &[u8]) -> bool {
    let mut offset = 0usize;
    let mut count: u32 = 0;
    while offset + XORB_CHUNK_HEADER_LENGTH <= body.len() {
        // Compressed length: 3 little-endian bytes at header offset 1.
        let compressed_len =
            u32::from_le_bytes([body[offset + 1], body[offset + 2], body[offset + 3], 0]) as usize;
        offset += XORB_CHUNK_HEADER_LENGTH + compressed_len;
        count += 1;
        if count > MAX_CHUNKS_PER_XORB {
            return true;
        }
    }
    false
}

/// Parse, decompress and hash-check an uploaded xorb body, returning the bytes
/// to store and the index record describing them.
fn verify_xorb(body: &[u8], expected: &MerkleHash) -> AppResult<(Vec<u8>, XorbRecord)> {
    // Reject an implausible chunk count BEFORE reconstruct_xorb_with_footer
    // expands the whole body (see chunk_frame_count_exceeds_limit).
    if chunk_frame_count_exceeds_limit(body) {
        return Err(AppError::bad_request(format!(
            "xorb declares more than {MAX_CHUNKS_PER_XORB} chunk frames"
        )));
    }
    let mut stored = Vec::with_capacity(body.len() + 4096);
    let (object, computed) = reconstruct_xorb_with_footer(&mut stored, body)
        .map_err(|e| AppError::bad_request(format!("malformed xorb: {e}")))?;

    if computed != *expected {
        return Err(AppError::bad_request(
            "xorb body does not hash to the addressed hash",
        ));
    }

    let info = object.info;
    if info.num_chunks == 0 {
        return Err(AppError::bad_request("xorb contains no chunks"));
    }
    if info.num_chunks > MAX_CHUNKS_PER_XORB {
        return Err(AppError::bad_request(format!(
            "xorb has {} chunks, limit is {MAX_CHUNKS_PER_XORB}",
            info.num_chunks
        )));
    }

    let frames_length = u64::from(*info.chunk_boundary_offsets.last().unwrap_or(&0));
    let unpacked_length = u64::from(*info.unpacked_chunk_offsets.last().unwrap_or(&0));
    if unpacked_length > MAX_XORB_UNPACKED_BYTES {
        return Err(AppError::bad_request(format!(
            "xorb holds {unpacked_length} bytes, limit is {MAX_XORB_UNPACKED_BYTES}"
        )));
    }

    let mut chunk_hashes = Vec::with_capacity(info.chunk_hashes.len() * HASH_BYTES);
    for chunk_hash in &info.chunk_hashes {
        chunk_hashes.extend_from_slice(chunk_hash.as_bytes());
    }

    let record = XorbRecord {
        xorb_hash: computed.hex(),
        num_chunks: info.num_chunks,
        frames_length,
        unpacked_length,
        chunk_boundary_offsets: info.chunk_boundary_offsets,
        unpacked_chunk_offsets: info.unpacked_chunk_offsets,
        chunk_hashes,
        created_at: crate::now_secs(),
    };
    Ok((stored, record))
}

/// Chunks of this xorb that may answer a global-dedup probe.
///
/// The client only ever probes a chunk whose hash satisfies the upstream
/// eligibility rule, so indexing anything else is dead weight
/// (docs/research/api-surface.md section 1.4).
fn eligible_chunks(record: &XorbRecord) -> AppResult<Vec<(String, u32)>> {
    let mut out = Vec::new();
    for index in 0..record.num_chunks as usize {
        let chunk_hash =
            crate::dedup_shard::chunk_hash_at(record, index).map_err(AppError::bad_request)?;
        if hash_is_global_dedup_eligible(&chunk_hash) {
            out.push((chunk_hash.hex(), index as u32));
        }
    }
    Ok(out)
}

/// The fetch target named by every reconstruction fetch_info entry.
///
/// The client fetches this with an UNAUTHENTICATED HTTP client, so no token is
/// required here regardless of configuration
/// (docs/research/api-surface.md section 1.10). Only the chunk-frame region is
/// served; the at-rest footer past `frames_length` is ours, not the client's.
pub async fn data(
    State(state): State<AppState>,
    Path((_prefix, hash)): Path<(String, String)>,
    headers: HeaderMap,
) -> AppResult<Response> {
    let record = state
        .index
        .get_xorb(&hash)
        .await?
        .ok_or(AppError::NotFound)?;
    let frames_length = record.frames_length;

    let (start, end, status) = match headers.get(header::RANGE) {
        Some(value) => {
            let spec = value
                .to_str()
                .ok()
                .and_then(parse_range_header)
                .ok_or_else(|| AppError::bad_request("malformed Range header"))?;
            let (start, end) = spec
                .resolve(frames_length)
                .ok_or(AppError::RangeNotSatisfiable)?;
            (start, end, StatusCode::PARTIAL_CONTENT)
        }
        None => (0, frames_length, StatusCode::OK),
    };

    let length = end - start;
    let path = state.xorbs.path_for(&hash);
    let file = tokio::fs::File::open(&path)
        .await
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => AppError::NotFound,
            _ => AppError::internal(format!("open xorb: {e}")),
        })?;

    let mut reader = file;
    tokio::io::AsyncSeekExt::seek(&mut reader, std::io::SeekFrom::Start(start)).await?;
    let stream = tokio_util::io::ReaderStream::new(tokio::io::AsyncReadExt::take(reader, length));

    let mut response = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .header(header::ACCEPT_RANGES, "bytes");
    if status == StatusCode::PARTIAL_CONTENT {
        response = response.header(
            header::CONTENT_RANGE,
            format!(
                "bytes {}-{}/{}",
                start,
                end.saturating_sub(1),
                frames_length
            ),
        );
    }
    response
        .body(axum::body::Body::from_stream(stream))
        .map_err(|e| AppError::internal(format!("build response: {e}")))
        .map(IntoResponse::into_response)
}
