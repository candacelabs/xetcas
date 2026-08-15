//! The Git LFS bridge.
//!
//! Uploads negotiate the "xet" transfer so git-xet performs the chunked,
//! deduplicated CAS upload. Downloads are always answered with the stock
//! "basic" transfer, because git-xet deliberately implements no download path
//! (docs/research/git-xet.md sections 6.2 and 7); the server reconstructs the
//! file itself and serves the bytes.

use std::collections::HashMap;

use axum::body::{Body, Bytes};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap};
use axum::response::{IntoResponse, Response};
use axum::Json;
use xetcas_contracts::constants::{
    HASH_ALGO_SHA256, HEADER_XET_ACCESS_TOKEN, HEADER_XET_CAS_URL, HEADER_XET_SESSION_ID,
    HEADER_XET_TOKEN_EXPIRATION, LFS_CONTENT_TYPE, TRANSFER_BASIC, TRANSFER_XET,
};
use xetcas_contracts::v1::validate::{
    validate_cas_token_info, validate_lfs_batch_request, validate_lfs_batch_response,
    validate_lfs_object_spec,
};
use xetcas_contracts::v1::{
    CasTokenInfo, LfsAction, LfsBatchObject, LfsBatchRequest, LfsBatchResponse, LfsObjectError,
    LfsObjectSpec,
};

use crate::error::{AppError, AppResult};
use crate::filestream::stream_file;
use crate::state::AppState;

/// Advertised validity of a minted token, in seconds.
const TOKEN_TTL_SECS: u64 = 3600;

/// The token bootstrap and refresh route named by every upload action href.
///
/// git-xet calls this when its JWT expires; the body is xet-core CasJWTInfo,
/// camelCase (docs/research/git-xet.md section 5.2).
pub async fn token(State(state): State<AppState>) -> AppResult<Response> {
    let info = CasTokenInfo {
        cas_url: state.config.public_base().to_string(),
        exp: crate::now_secs() + TOKEN_TTL_SECS,
        access_token: state.config.advertised_token().to_string(),
    };
    validate_cas_token_info(&info).map_err(AppError::internal)?;
    Ok(Json(info).into_response())
}

/// Serve an LFS object by oid, reconstructing it from CAS on the fly.
pub async fn download(
    State(state): State<AppState>,
    Path(oid): Path<String>,
) -> AppResult<Response> {
    // The oid is the only boundary that reached the index without its contract
    // validator; enforce bridge.proto's ^[0-9a-f]{64}$ before the lookup so a
    // malformed oid is a 400, not a 404 on a query that could never match.
    validate_lfs_object_spec(&LfsObjectSpec {
        oid: oid.clone(),
        size: 0,
    })
    .map_err(AppError::bad_request)?;
    let file = state
        .index
        .file_by_sha256(&oid)
        .await?
        .ok_or(AppError::NotFound)?;
    let length = file.file_length;
    let stream = stream_file(&state, &file).await?;

    tracing::info!(oid = %oid, bytes = length, "serving lfs object");
    Response::builder()
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, length)
        .body(Body::from_stream(stream))
        .map_err(|e| AppError::internal(format!("build response: {e}")))
}

/// The `Authorization` this batch request arrived with, to be echoed into the
/// actions it hands back.
///
/// Git LFS reads an object's `authenticated: true` as "the action already
/// embeds authorization, do not apply your own", and git-lfs 3.x does NOT copy
/// the batch request's Authorization onto the action request it then makes
/// (`basicAdapter::doHTTP` calls `makeRequest` instead of
/// `DoAPIRequestWithAuth` when the flag is set). So a deployment that follows
/// the README and fronts xetcasd with an authenticating reverse proxy would see
/// the batch authenticate and the follow-up download GET arrive bare, get a 401
/// from the proxy, and fail the clone. Echoing the accepted credential into the
/// action -- and only then claiming `authenticated` -- is what makes the two
/// consistent. The href is always this server's own public base, so nothing is
/// sent anywhere new.
///
/// With no proxy there is nothing to forward, `authenticated` stays false, and
/// git-lfs applies its normal credential chain (which for an open server means
/// no credentials at all).
fn forwarded_authorization(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

/// Handle a Git LFS batch request for `repo`.
pub async fn batch(
    state: AppState,
    repo: &str,
    headers: &HeaderMap,
    body: Bytes,
) -> AppResult<Response> {
    let request: LfsBatchRequest = serde_json::from_slice(&body)
        .map_err(|e| AppError::bad_request(format!("malformed batch request: {e}")))?;
    validate_lfs_batch_request(&request).map_err(AppError::bad_request)?;

    let auth = forwarded_authorization(headers);
    let response = match request.operation.as_str() {
        "upload" => upload_batch(&state, &request, auth.as_deref()).await?,
        "download" => download_batch(&state, &request, auth.as_deref()).await?,
        other => {
            return Err(AppError::bad_request(format!(
                "unsupported operation {other}"
            )))
        }
    };
    validate_lfs_batch_response(&response).map_err(AppError::internal)?;

    tracing::info!(
        repo = %repo,
        operation = %request.operation,
        objects = response.objects.len(),
        transfer = %response.transfer,
        "lfs batch"
    );
    Ok(([(header::CONTENT_TYPE, LFS_CONTENT_TYPE)], Json(response)).into_response())
}

/// Upload batch: negotiate the xet transfer, or refuse.
async fn upload_batch(
    state: &AppState,
    request: &LfsBatchRequest,
    auth: Option<&str>,
) -> AppResult<LfsBatchResponse> {
    let offers_xet = request.transfers.iter().any(|t| t == TRANSFER_XET);
    let base = state.config.public_base();
    let mut objects = Vec::with_capacity(request.objects.len());

    for spec in &request.objects {
        if !offers_xet {
            // Without git-xet there is no way to get chunked content into CAS,
            // and accepting a basic upload would store bytes this server cannot
            // address. Refuse per object so git-lfs reports it usefully.
            objects.push(LfsBatchObject {
                oid: spec.oid.clone(),
                size: spec.size,
                authenticated: false,
                actions: HashMap::new(),
                error: Some(LfsObjectError {
                    code: 422,
                    message: "xetcasd requires the xet transfer agent for uploads -- run 'git xet install'"
                        .to_string(),
                }),
            });
            continue;
        }

        // Already stored: an entry with no actions tells git-lfs to skip it.
        // There is no action request for `authenticated` to describe, so it
        // stays false rather than making a claim about a request that will
        // never happen.
        if state.index.file_by_sha256(&spec.oid).await?.is_some() {
            objects.push(LfsBatchObject {
                oid: spec.oid.clone(),
                size: spec.size,
                authenticated: false,
                actions: HashMap::new(),
                error: None,
            });
            continue;
        }

        let action = upload_action(state, base, auth);
        objects.push(LfsBatchObject {
            oid: spec.oid.clone(),
            size: spec.size,
            authenticated: embeds_authorization(&action),
            actions: HashMap::from([("upload".to_string(), action)]),
            error: None,
        });
    }

    Ok(LfsBatchResponse {
        transfer: if offers_xet {
            TRANSFER_XET
        } else {
            TRANSFER_BASIC
        }
        .to_string(),
        objects,
        hash_algo: HASH_ALGO_SHA256.to_string(),
    })
}

/// Whether an action's own headers carry the authorization for its href.
///
/// This is exactly the condition `LfsBatchObject.authenticated` is defined by
/// in bridge.proto: "whether href/header already embed authorization".
fn embeds_authorization(action: &LfsAction) -> bool {
    action.header.contains_key(AUTHORIZATION_HEADER)
}

/// Header name used in action header maps. Spelled out rather than taken from
/// `header::AUTHORIZATION` because git-lfs copies these keys verbatim into the
/// request and the canonical capitalization is what other LFS servers emit.
const AUTHORIZATION_HEADER: &str = "Authorization";

/// The action git-xet reads its CAS endpoint and token out of.
///
/// Every key is matched case-sensitively by git-xet and all but the session id
/// are mandatory; the expiration is a decimal string, parsed with `u64::from_str`
/// (docs/research/git-xet.md section 4). The `X-Xet-*` set is what git-xet
/// consumes, and it is NOT authorization for the href: git-xet authenticates
/// the token route from its own credential chain (git-xet.md section 5.4). So
/// the accepted `Authorization` is forwarded here too, and `authenticated` is
/// derived from whether it ended up in the map.
fn upload_action(state: &AppState, base: &str, auth: Option<&str>) -> LfsAction {
    let mut header = HashMap::from([
        (HEADER_XET_CAS_URL.to_string(), base.to_string()),
        (
            HEADER_XET_ACCESS_TOKEN.to_string(),
            state.config.advertised_token().to_string(),
        ),
        (
            HEADER_XET_TOKEN_EXPIRATION.to_string(),
            (crate::now_secs() + TOKEN_TTL_SECS).to_string(),
        ),
        (
            HEADER_XET_SESSION_ID.to_string(),
            uuid::Uuid::new_v4().to_string(),
        ),
    ]);
    if let Some(auth) = auth {
        header.insert(AUTHORIZATION_HEADER.to_string(), auth.to_string());
    }
    LfsAction {
        href: format!("{base}/xet-token"),
        header,
        expires_in: TOKEN_TTL_SECS,
    }
}

/// The basic-transfer download action, which git-lfs fetches itself.
fn download_action(base: &str, oid: &str, auth: Option<&str>) -> LfsAction {
    let mut header = HashMap::new();
    if let Some(auth) = auth {
        header.insert(AUTHORIZATION_HEADER.to_string(), auth.to_string());
    }
    LfsAction {
        href: format!("{base}/lfs/objects/{oid}"),
        header,
        expires_in: TOKEN_TTL_SECS,
    }
}

/// Download batch: always the basic transfer.
async fn download_batch(
    state: &AppState,
    request: &LfsBatchRequest,
    auth: Option<&str>,
) -> AppResult<LfsBatchResponse> {
    let base = state.config.public_base();
    let mut objects = Vec::with_capacity(request.objects.len());

    for spec in &request.objects {
        let known = state.index.file_by_sha256(&spec.oid).await?;
        let (actions, error, authenticated) = match known {
            Some(_) => {
                // git-lfs performs this GET itself, and it is the request the
                // `authenticated` flag actually governs.
                let action = download_action(base, &spec.oid, auth);
                let authenticated = embeds_authorization(&action);
                (
                    HashMap::from([("download".to_string(), action)]),
                    None,
                    authenticated,
                )
            }
            None => (
                HashMap::new(),
                Some(LfsObjectError {
                    code: 404,
                    message: format!("object {} not found", spec.oid),
                }),
                false,
            ),
        };
        objects.push(LfsBatchObject {
            oid: spec.oid.clone(),
            size: spec.size,
            authenticated,
            actions,
            error,
        });
    }

    Ok(LfsBatchResponse {
        transfer: TRANSFER_BASIC.to_string(),
        objects,
        hash_algo: HASH_ALGO_SHA256.to_string(),
    })
}
