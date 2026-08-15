//! Global chunk deduplication probe.

use axum::extract::{Path, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use xetcas_contracts::v1::validate::validate_chunk_dedup_query;
use xetcas_contracts::v1::ChunkDedupQuery;

use crate::dedup_shard::build_keyed_dedup_shard;
use crate::error::{AppError, AppResult};
use crate::state::AppState;

/// Answer a global-dedup probe with a shard describing the owning xorb.
///
/// A miss is a plain 404, which the client treats as "chunk not tracked" and
/// moves on; global dedup is best effort
/// (docs/research/api-surface.md section 1.4).
pub async fn get(
    State(state): State<AppState>,
    Path((prefix, hash)): Path<(String, String)>,
) -> AppResult<Response> {
    validate_chunk_dedup_query(&ChunkDedupQuery {
        prefix,
        hash: hash.clone(),
    })
    .map_err(AppError::bad_request)?;

    let Some((xorb_hash, _index)) = state.index.lookup_chunk(&hash).await? else {
        return Err(AppError::NotFound);
    };
    let record = state
        .index
        .get_xorb(&xorb_hash)
        .await?
        .ok_or(AppError::NotFound)?;
    let disk_bytes = record.frames_length;

    let shard = tokio::task::spawn_blocking(move || build_keyed_dedup_shard(&record, disk_bytes))
        .await
        .map_err(|e| AppError::internal(format!("dedup shard task: {e}")))?
        .map_err(AppError::internal)?;

    tracing::debug!(chunk = %hash, xorb = %xorb_hash, "served dedup shard");
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], shard).into_response())
}
