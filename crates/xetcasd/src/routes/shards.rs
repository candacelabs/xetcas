//! Shard upload: file reconstruction metadata plus xorb listings.

use std::collections::HashMap;
use std::io::Cursor;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use xet_core_structures::merklehash::{file_hash, MerkleHash};
use xet_core_structures::metadata_shard::chunk_verification::range_hash_from_chunks;
use xet_core_structures::metadata_shard::streaming_shard::MDBMinimalShard;
use xet_core_structures::metadata_shard::xorb_structs::MDB_CHUNK_WITH_GLOBAL_DEDUP_FLAG;
use xetcas_contracts::constants::HASH_BYTES;
use xetcas_contracts::v1::validate::{validate_file_record, validate_upload_shard_response};
use xetcas_contracts::v1::{FileRecord, FileTermRecord, UploadShardResponse, XorbRecord};

use crate::dedup_shard::chunk_hash_at;
use crate::error::{AppError, AppResult};
use crate::reconstruction::XorbMeta;
use crate::state::AppState;

/// (chunk hash, owning xorb hash, index within that xorb).
type ChunkIndexEntry = (String, String, u32);

/// `result` values, following xet-core's numeric convention.
const RESULT_EXISTS: u32 = 0;
const RESULT_SYNC_PERFORMED: u32 = 1;

/// A file as it appears in an uploaded shard, before checking.
struct ShardFile {
    file_hash: String,
    sha256: String,
    terms: Vec<FileTermRecord>,
    /// Raw 32-byte range hashes, one per term, when the shard carried them.
    verification: Vec<u8>,
}

/// Everything decoded from a shard body.
struct ShardData {
    files: Vec<ShardFile>,
    /// Chunks the cas-info section flagged as global-dedup candidates.
    flagged_chunks: Vec<ChunkIndexEntry>,
}

/// Decode a truncated shard body into plain data.
fn decode_shard(body: &[u8]) -> AppResult<ShardData> {
    let shard = MDBMinimalShard::from_reader(&mut Cursor::new(body), true, true)
        .map_err(|e| AppError::bad_request(format!("malformed shard: {e}")))?;

    let mut files = Vec::with_capacity(shard.num_files());
    for i in 0..shard.num_files() {
        let view = shard
            .file(i)
            .ok_or_else(|| AppError::bad_request("truncated file section"))?;
        let n = view.num_entries();

        let mut terms = Vec::with_capacity(n);
        for e in 0..n {
            let entry = view.entry(e);
            terms.push(FileTermRecord {
                xorb_hash: entry.xorb_hash.hex(),
                chunk_index_start: entry.chunk_index_start,
                chunk_index_end: entry.chunk_index_end,
                unpacked_segment_bytes: entry.unpacked_segment_bytes,
            });
        }

        let mut verification = Vec::new();
        if view.contains_verification() {
            for e in 0..n {
                verification.extend_from_slice(view.verification(e).range_hash.as_bytes());
            }
        }

        files.push(ShardFile {
            file_hash: view.file_hash().hex(),
            sha256: view
                .metadata_ext()
                .map(|m| m.sha256.hex())
                .unwrap_or_default(),
            terms,
            verification,
        });
    }

    let mut flagged_chunks = Vec::new();
    for i in 0..shard.num_xorb() {
        let view = shard
            .xorb(i)
            .ok_or_else(|| AppError::bad_request("truncated xorb section"))?;
        let xorb_hash = view.xorb_hash().hex();
        for c in 0..view.num_entries() {
            let chunk = view.chunk(c);
            if chunk.flags & MDB_CHUNK_WITH_GLOBAL_DEDUP_FLAG != 0 {
                flagged_chunks.push((chunk.chunk_hash.hex(), xorb_hash.clone(), c as u32));
            }
        }
    }

    Ok(ShardData {
        files,
        flagged_chunks,
    })
}

/// Check every file in a decoded shard against stored xorb metadata and turn
/// it into index records.
///
/// The checks mirror what a "validating" server owes the client
/// (docs/research/dataplane.md section 8.3): each term must name a xorb we
/// hold, stay inside its chunk count, and declare the byte length its chunk
/// range actually decodes to. When the shard carries verification entries --
/// the client always sends them -- the range hash is recomputed from our own
/// stored chunk hashes and must match. On top of that the file hash itself is
/// recomputed from those chunks: it is client-asserted exactly like the xorb
/// upload's path hash, and it is the primary key of an insert-only table.
fn build_records(
    data: ShardData,
    xorbs: &HashMap<String, XorbRecord>,
) -> AppResult<(Vec<FileRecord>, Vec<ChunkIndexEntry>)> {
    let mut files = Vec::with_capacity(data.files.len());
    let mut chunks = validated_chunk_mappings(data.flagged_chunks, xorbs);

    for file in data.files {
        // A zero-byte file (`.gitkeep`, an empty `__init__.py`) legitimately has
        // no terms: the genuine xet-data client emits a zero-entry
        // `FileDataSequenceHeader` for it, and the upstream reference server
        // accepts it (xet_data test_with_empty_file / test_with_all_empty_files).
        // Rejecting it would 400 -- fatal, non-retried -- and orphan every other
        // file's xorbs in the same `FileUploadSession::finalize`. The contract
        // validator accepts `file_length == 0 == sum([])`, and reconstruction
        // already plans an empty file (docs/research/dataplane.md section 8.6).
        let has_verification = !file.verification.is_empty();
        if has_verification && file.verification.len() != file.terms.len() * HASH_BYTES {
            return Err(AppError::bad_request(
                "verification section length mismatch",
            ));
        }

        let mut file_length = 0u64;
        // The file's ordered (chunk hash, uncompressed size) pairs, read out of
        // OUR stored xorbs rather than off the wire. The file hash is defined as
        // an aggregation over exactly this list and each term's verification
        // hash over its slice of it (docs/research/binary-formats.md 1.5, 1.6).
        let mut file_chunks: Vec<(MerkleHash, u64)> = Vec::new();

        for (i, term) in file.terms.iter().enumerate() {
            let record = xorbs.get(&term.xorb_hash).ok_or_else(|| {
                AppError::bad_request(format!("shard references unknown xorb {}", term.xorb_hash))
            })?;
            let meta = XorbMeta::from_record(record);

            if term.chunk_index_start >= term.chunk_index_end
                || term.chunk_index_end > meta.num_chunks()
            {
                return Err(AppError::bad_request(format!(
                    "term chunk range {} to {} outside xorb {} with {} chunks",
                    term.chunk_index_start,
                    term.chunk_index_end,
                    term.xorb_hash,
                    meta.num_chunks()
                )));
            }

            let expected = meta
                .unpacked_len(term.chunk_index_start, term.chunk_index_end)
                .map_err(AppError::bad_request)?;
            if expected != u64::from(term.unpacked_segment_bytes) {
                return Err(AppError::bad_request(format!(
                    "term declares {} bytes, xorb {} chunk range holds {expected}",
                    term.unpacked_segment_bytes, term.xorb_hash
                )));
            }

            let term_start = file_chunks.len();
            for index in term.chunk_index_start..term.chunk_index_end {
                let chunk_hash =
                    chunk_hash_at(record, index as usize).map_err(AppError::bad_request)?;
                let chunk_len = meta.chunk_len(index).map_err(AppError::bad_request)?;
                file_chunks.push((chunk_hash, u64::from(chunk_len)));
            }

            if has_verification {
                check_range_hash(
                    &file_chunks[term_start..],
                    &file.verification,
                    i,
                    &term.xorb_hash,
                )?;
            }

            file_length += u64::from(term.unpacked_segment_bytes);
        }

        // Derive the identifier instead of copying it. `files` is keyed by
        // file_hash and insert-only, so a shard that names someone else's hash
        // squats the key permanently: the later, genuine registration is
        // silently ignored and every reconstruction of that id returns this
        // shard's bytes. An empty file legitimately hashes to all zeros --
        // file_hash([]) short-circuits before the HMAC step, which is what the
        // client emits too.
        let computed = file_hash(&file_chunks);
        if computed.hex() != file.file_hash {
            return Err(AppError::bad_request(format!(
                "file hash mismatch: shard claims {}, its terms hash to {}",
                file.file_hash,
                computed.hex()
            )));
        }

        // The client always probes a file's very first chunk for global dedup,
        // regardless of the eligibility modulus, so index it explicitly
        // (docs/research/dataplane.md section 4.2). An empty file has no first
        // chunk, so there is nothing to index.
        if let Some(first) = file.terms.first() {
            if let Some(record) = xorbs.get(&first.xorb_hash) {
                let index = first.chunk_index_start as usize;
                if let Ok(chunk_hash) = chunk_hash_at(record, index) {
                    chunks.push((chunk_hash.hex(), first.xorb_hash.clone(), index as u32));
                }
            }
        }

        let record = FileRecord {
            file_hash: file.file_hash,
            file_length,
            sha256: file.sha256,
            terms: file.terms,
            verification_range_hashes: file.verification,
            created_at: crate::now_secs(),
        };
        validate_file_record(&record).map_err(AppError::bad_request)?;
        files.push(record);
    }

    Ok((files, chunks))
}

/// Recompute a term's verification range hash from our own stored chunk hashes.
fn check_range_hash(
    term_chunks: &[(MerkleHash, u64)],
    verification: &[u8],
    index: usize,
    xorb_hash: &str,
) -> AppResult<()> {
    let hashes: Vec<MerkleHash> = term_chunks.iter().map(|(hash, _)| *hash).collect();
    let computed = range_hash_from_chunks(&hashes);
    let start = index * HASH_BYTES;
    let claimed = &verification[start..start + HASH_BYTES];
    if computed.as_bytes() != claimed {
        return Err(AppError::bad_request(format!(
            "verification hash mismatch on term {index} of xorb {xorb_hash}"
        )));
    }
    Ok(())
}

/// Keep only the cas-info dedup flags that our own stored xorbs corroborate.
///
/// The flagged tuples are a HINT for the global-dedup index, not part of any
/// file's reconstruction contract, so an entry we cannot corroborate is dropped
/// rather than failing the whole shard -- a fatal, non-retried 400 on an
/// advisory hint would orphan every other file in the same session finalize.
/// Dropping is what matters: `chunks` has chunk_hash as its PRIMARY KEY and is
/// written with INSERT OR IGNORE, so one bogus mapping would permanently shadow
/// the real one and turn every later global-dedup query for that chunk into a
/// persistent miss or an irrelevant shard.
fn validated_chunk_mappings(
    flagged: Vec<ChunkIndexEntry>,
    xorbs: &HashMap<String, XorbRecord>,
) -> Vec<ChunkIndexEntry> {
    let mut kept = Vec::with_capacity(flagged.len());
    for (chunk_hash, xorb_hash, index) in flagged {
        let Some(record) = xorbs.get(&xorb_hash) else {
            tracing::warn!(
                xorb = %xorb_hash,
                "dropping a dedup mapping for a xorb this server does not hold"
            );
            continue;
        };
        match chunk_hash_at(record, index as usize) {
            Ok(actual) if actual.hex() == chunk_hash => kept.push((chunk_hash, xorb_hash, index)),
            _ => tracing::warn!(
                xorb = %xorb_hash,
                index,
                "dropping a dedup mapping that does not match the stored xorb"
            ),
        }
    }
    kept
}

/// Check each record's claimed SHA-256 against the content it describes.
///
/// The shard's `FileMetadataExt.sha256` is the git-lfs oid, and it is the only
/// key the LFS bridge can answer a download by (docs/research/git-xet.md
/// section 6.4). It is not derivable from chunk hashes, so unlike the file hash
/// it can only be checked by hashing the reconstructed content -- which the
/// server can do, because every referenced xorb has already been verified to be
/// present above. This is the expensive check, so it is skipped for a record
/// whose file hash AND sha256 are already registered: that is the ordinary
/// idempotent re-upload, and the pair was verified when it was first accepted.
///
/// Accepting an unverified oid is the data-loss path, not merely a wrong
/// download: `upload_batch` reports an object with a known sha256 as "already
/// stored", so a squatted oid makes git-lfs skip the genuine upload of that
/// object entirely and it is never stored at all. Peak memory here is one
/// decompressed chunk, and V1 shard upload has no client-side read timeout.
async fn verify_sha256(state: &AppState, files: &[FileRecord]) -> AppResult<()> {
    for file in files {
        if file.sha256.is_empty() {
            continue;
        }
        let known = state.index.get_file(&file.file_hash).await?;
        if known.is_some_and(|f| f.sha256 == file.sha256) {
            continue;
        }
        let computed = crate::filestream::content_sha256(state, file).await?;
        if computed != file.sha256 {
            return Err(AppError::bad_request(format!(
                "sha256 mismatch: shard claims {} for file {}, its content hashes to {computed}",
                file.sha256, file.file_hash
            )));
        }
    }
    Ok(())
}

/// `POST /v1/shards`.
///
/// The body is a TRUNCATED shard: header, file-info section, xorb-info section,
/// both bookend-terminated, with no lookup tables and no footer
/// (docs/research/dataplane.md section 8.2). Re-uploading the same shard must
/// succeed, so registration is idempotent.
pub async fn upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> AppResult<Json<UploadShardResponse>> {
    state.authorize(&headers)?;
    if body.is_empty() {
        return Err(AppError::bad_request("empty shard body"));
    }

    let data = tokio::task::spawn_blocking(move || decode_shard(&body))
        .await
        .map_err(|e| AppError::internal(format!("shard decode task: {e}")))??;

    // Both the file terms AND the cas-info section's flagged chunks are checked
    // against stored xorbs, so both sets have to be fetched.
    let mut referenced: Vec<String> = data
        .files
        .iter()
        .flat_map(|f| f.terms.iter().map(|t| t.xorb_hash.clone()))
        .chain(data.flagged_chunks.iter().map(|(_, xorb, _)| xorb.clone()))
        .collect();
    referenced.sort();
    referenced.dedup();
    let xorbs = state.index.get_xorbs(referenced).await?;

    let (files, chunks) = build_records(data, &xorbs)?;
    verify_sha256(&state, &files).await?;
    let total = files.len();
    let new_files = state.index.put_files(files, chunks).await?;

    let result = if new_files == 0 {
        RESULT_EXISTS
    } else {
        RESULT_SYNC_PERFORMED
    };
    tracing::info!(files = total, new = new_files, "registered shard");

    let response = UploadShardResponse { result };
    validate_upload_shard_response(&response).map_err(AppError::internal)?;
    Ok(Json(response))
}
