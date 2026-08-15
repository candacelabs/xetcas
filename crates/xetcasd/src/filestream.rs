//! Server-side file reconstruction.
//!
//! git-xet cannot download -- it is upload-only, and every LFS download batch
//! is answered with the basic transfer (docs/research/git-xet.md section 7).
//! So the server, not the client, reassembles the file: walk the stored terms,
//! read each chunk frame out of its xorb, decompress it, and emit the bytes.
//!
//! The walk is chunk-at-a-time so peak memory stays at one decompressed chunk
//! (at most 128 KiB) regardless of file size.

use std::io::{Seek, SeekFrom};
use std::path::PathBuf;

use bytes::Bytes;
use xet_core_structures::xorb_object::deserialize_chunk;

use crate::error::{AppError, AppResult};
use crate::reconstruction::XorbMeta;
use crate::state::AppState;
use xetcas_contracts::v1::FileRecord;

/// One contiguous read: a xorb object, where the term starts, and how many
/// chunk frames to decode from there.
struct Segment {
    path: PathBuf,
    byte_start: u64,
    num_chunks: u32,
}

/// Resolve a file terms into the physical reads that reproduce its bytes.
async fn plan_segments(state: &AppState, file: &FileRecord) -> AppResult<Vec<Segment>> {
    let mut hashes: Vec<String> = file.terms.iter().map(|t| t.xorb_hash.clone()).collect();
    hashes.sort();
    hashes.dedup();
    let records = state.index.get_xorbs(hashes).await?;

    let mut segments = Vec::with_capacity(file.terms.len());
    for term in &file.terms {
        let record = records
            .get(&term.xorb_hash)
            .ok_or_else(|| AppError::internal(format!("missing xorb {}", term.xorb_hash)))?;
        let meta = XorbMeta::from_record(record);
        let (byte_start, _) = meta
            .byte_offset(term.chunk_index_start, term.chunk_index_end)
            .map_err(AppError::internal)?;
        segments.push(Segment {
            path: state.xorbs.path_for(&term.xorb_hash),
            byte_start,
            num_chunks: term.chunk_index_end - term.chunk_index_start,
        });
    }
    Ok(segments)
}

/// Reassemble `file` and return a byte stream of its contents.
///
/// Decoding runs on a blocking thread and hands finished chunks over a bounded
/// channel, so a slow client throttles the reader instead of letting the whole
/// file accumulate in memory.
pub async fn stream_file(
    state: &AppState,
    file: &FileRecord,
) -> AppResult<tokio_stream::wrappers::ReceiverStream<Result<Bytes, std::io::Error>>> {
    let segments = plan_segments(state, file).await?;

    // Cap the number of decoders that can park a blocking-pool thread on a slow
    // or stalled client, so a burst of dead downloads cannot starve the sqlite
    // ops that share that pool (state::DEFAULT_DOWNLOAD_CONCURRENCY). The permit
    // is moved into the decode task and released when it ends -- including when
    // the client hangs up and blocking_send fails.
    let permit = state
        .download_permits
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| AppError::internal("download semaphore closed"))?;

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(8);

    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        for segment in segments {
            if let Err(e) = emit_segment(&segment, &tx) {
                // The receiver going away simply means the client hung up.
                let _ = tx.blocking_send(Err(e));
                return;
            }
        }
    });

    Ok(tokio_stream::wrappers::ReceiverStream::new(rx))
}

/// SHA-256 of a file's reconstructed content, hashed straight out of the stored
/// xorbs.
///
/// Used to check the sha256 a shard claims for a file before it is indexed: it
/// is the git-lfs oid and cannot be derived from chunk hashes, so hashing the
/// content is the only way to know it is true. Peak memory is one decompressed
/// chunk, exactly as for [`stream_file`].
pub async fn content_sha256(state: &AppState, file: &FileRecord) -> AppResult<String> {
    use sha2::{Digest, Sha256};

    let segments = plan_segments(state, file).await?;
    tokio::task::spawn_blocking(move || -> Result<String, std::io::Error> {
        let mut hasher = Sha256::new();
        for segment in &segments {
            decode_segment(segment, |chunk| {
                hasher.update(&chunk);
                true
            })?;
        }
        Ok(hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| AppError::internal(format!("sha256 task: {e}")))?
    .map_err(|e| AppError::internal(format!("hashing file content: {e}")))
}

/// Decode one term chunk frames and send each decompressed chunk downstream.
fn emit_segment(
    segment: &Segment,
    tx: &tokio::sync::mpsc::Sender<Result<Bytes, std::io::Error>>,
) -> Result<(), std::io::Error> {
    decode_segment(segment, |chunk| {
        tx.blocking_send(Ok(Bytes::from(chunk))).is_ok()
    })
}

/// Decode a term chunk frames in order, handing each decompressed chunk to
/// `sink`. Returning false from `sink` stops early without an error, which is
/// how a hung-up client ends a download.
fn decode_segment(
    segment: &Segment,
    mut sink: impl FnMut(Vec<u8>) -> bool,
) -> Result<(), std::io::Error> {
    let mut handle = std::fs::File::open(&segment.path)?;
    handle.seek(SeekFrom::Start(segment.byte_start))?;
    let mut reader = std::io::BufReader::new(handle);

    for _ in 0..segment.num_chunks {
        let (data, _compressed, _uncompressed) = deserialize_chunk(&mut reader)
            .map_err(|e| std::io::Error::other(format!("chunk decode: {e}")))?;
        if !sink(data) {
            return Ok(());
        }
    }
    Ok(())
}
