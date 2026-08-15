//! File reconstruction.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::Json;
use xetcas_contracts::constants::PREFIX_DEFAULT;
use xetcas_contracts::v1::validate::{
    validate_query_reconstruction_request, validate_query_reconstruction_response,
};
use xetcas_contracts::v1::{
    ByteRange, CasReconstructionFetchInfo, CasReconstructionTerm, FetchInfoList, IndexRange,
    QueryReconstructionRequest, QueryReconstructionResponse,
};

use crate::error::{AppError, AppResult};
use crate::http_range::parse_range_header;
use crate::reconstruction::{plan_reconstruction, XorbMeta};
use crate::state::AppState;

/// Reconstruct a file, optionally restricted by a `Range` header.
///
/// A range whose start is at or past EOF answers 416, not 404 and not an empty
/// 200: that is how the client detects end-of-file while walking a file in
/// blocks (docs/research/api-surface.md section 5.3).
pub async fn get(
    State(state): State<AppState>,
    Path(file_id): Path<String>,
    headers: HeaderMap,
) -> AppResult<Json<QueryReconstructionResponse>> {
    validate_query_reconstruction_request(&QueryReconstructionRequest {
        file_id: file_id.clone(),
        range: None,
    })
    .map_err(AppError::bad_request)?;

    let spec = match headers.get(header::RANGE) {
        Some(value) => Some(
            value
                .to_str()
                .ok()
                .and_then(parse_range_header)
                .ok_or_else(|| AppError::bad_request("malformed Range header"))?,
        ),
        None => None,
    };

    let file = state
        .index
        .get_file(&file_id)
        .await?
        .ok_or(AppError::NotFound)?;

    let range = match spec {
        Some(spec) => Some(
            spec.resolve(file.file_length)
                .ok_or(AppError::RangeNotSatisfiable)?,
        ),
        None => None,
    };

    let mut hashes: Vec<String> = file.terms.iter().map(|t| t.xorb_hash.clone()).collect();
    hashes.sort();
    hashes.dedup();
    let records = state.index.get_xorbs(hashes).await?;
    let metas: HashMap<String, XorbMeta> = records
        .iter()
        .map(|(hash, record)| (hash.clone(), XorbMeta::from_record(record)))
        .collect();

    let plan = plan_reconstruction(&file, range, &metas)
        .map_err(AppError::internal)?
        .ok_or(AppError::RangeNotSatisfiable)?;

    let terms = plan
        .terms
        .into_iter()
        .map(|t| CasReconstructionTerm {
            hash: t.xorb,
            range: Some(IndexRange {
                start: t.chunk_start,
                end: t.chunk_end,
            }),
            unpacked_length: t.unpacked_length,
        })
        .collect();

    let base = state.config.public_base();
    let mut fetch_info = HashMap::new();
    for (xorb, ranges) in plan.fetch {
        let entries = ranges
            .into_iter()
            .map(|r| CasReconstructionFetchInfo {
                range: Some(IndexRange {
                    start: r.chunk_start,
                    end: r.chunk_end,
                }),
                url: format!("{base}/v1/xorbs/{PREFIX_DEFAULT}/{xorb}/data"),
                // url_range is end-INCLUSIVE: the client copies it verbatim
                // into a Range header.
                url_range: Some(ByteRange {
                    start: r.byte_start,
                    end: r.byte_end.saturating_sub(1),
                }),
            })
            .collect();
        fetch_info.insert(xorb, FetchInfoList { entries });
    }

    let response = QueryReconstructionResponse {
        offset_into_first_range: plan.offset_into_first_range,
        terms,
        fetch_info,
    };
    // Every nested message is populated above: prost renders an absent one as
    // JSON null, which the client rejects outright.
    validate_query_reconstruction_response(&response).map_err(AppError::internal)?;
    Ok(Json(response))
}
