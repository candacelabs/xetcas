//! HTTP surface.

pub mod chunks;
pub mod git;
pub mod lfs;
pub mod reconstructions;
pub mod shards;
pub mod xorbs;

use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::routing::{any, get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::error::AppResult;
use crate::state::AppState;

/// Health and basic store statistics.
///
/// Constant time by construction. The container health-checks this every five
/// seconds with a three-second timeout, so nothing here may scale with the
/// store: the byte total is maintained transactionally by the index as xorbs
/// are registered, and the object tree is only PROBED (see
/// [`crate::xorbstore::XorbStore::probe`]) rather than walked. The probe is
/// what keeps the endpoint honest -- a failed mount or unreadable object root
/// fails the request instead of reporting `"status":"ok"` while every
/// download 500s.
async fn health(State(state): State<AppState>) -> AppResult<Json<serde_json::Value>> {
    state.xorbs.probe().await?;
    let stats = state.index.stats().await?;
    Ok(Json(json!({
        "status": "ok",
        "xorbs": stats.xorbs,
        "files": stats.files,
        "stored_bytes": stats.stored_bytes,
    })))
}

/// Transfer telemetry. Fire-and-forget on the client side; the body is dropped.
async fn telemetry() -> StatusCode {
    StatusCode::OK
}

/// Every v2 route answers 404.
///
/// That is the documented signal for "this server is v1 only", and the client
/// caches the fallback per connection. It must NOT be 501: the client treats
/// 501 as permanently fatal everywhere else
/// (docs/research/api-surface.md section 5.4).
async fn v2_unsupported() -> StatusCode {
    StatusCode::NOT_FOUND
}

/// Build the full router.
pub fn router(state: AppState) -> Router {
    let uploads = DefaultBodyLimit::max(xorbs::MAX_UPLOAD_BYTES);

    Router::new()
        .route("/health", get(health))
        .route(
            "/v1/xorbs/{prefix}/{hash}",
            post(xorbs::upload).layer(uploads),
        )
        .route("/v1/xorbs/{prefix}/{hash}/data", get(xorbs::data))
        .route("/v1/reconstructions/{file_id}", get(reconstructions::get))
        .route("/v1/chunks/{prefix}/{hash}", get(chunks::get))
        .route("/v1/shards", post(shards::upload).layer(uploads))
        .route("/v1/telemetry", post(telemetry))
        .route("/v2/shards", any(v2_unsupported))
        .route("/v2/reconstructions/{*rest}", any(v2_unsupported))
        .route("/v2/file-chunk-hashes/{*rest}", any(v2_unsupported))
        .route("/xet-token", get(lfs::token))
        .route("/lfs/objects/{oid}", get(lfs::download))
        .route(
            "/git/{*path}",
            any(git::dispatch).layer(DefaultBodyLimit::disable()),
        )
        .with_state(state)
}
