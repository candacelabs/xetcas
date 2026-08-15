//! Error type and its HTTP mapping.
//!
//! Status codes are load-bearing protocol signals, not cosmetics: the client
//! reads 416 as "past EOF", 404 on a `/v2/` route as "fall back to v1", and
//! treats 501 as permanently fatal. See docs/research/api-surface.md section 5.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

/// Anything a handler can fail with.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// The addressed object does not exist.
    #[error("not found")]
    NotFound,

    /// A `Range` request started at or past the end of the object. The client
    /// uses this as its end-of-file signal on reconstructions.
    #[error("range not satisfiable")]
    RangeNotSatisfiable,

    /// The request was malformed or failed verification. Always fatal for the
    /// client: 4xx other than 408/429 is never retried.
    #[error("{0}")]
    BadRequest(String),

    /// A token is configured and the request did not present it.
    #[error("unauthorized")]
    Unauthorized,

    /// Something broke on our side.
    #[error("{0}")]
    Internal(String),
}

impl AppError {
    /// Build a `BadRequest` from anything printable.
    pub fn bad_request(msg: impl std::fmt::Display) -> Self {
        Self::BadRequest(msg.to_string())
    }

    /// Build an `Internal` from anything printable.
    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self::Internal(msg.to_string())
    }
}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::Internal(format!("io: {e}"))
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(format!("index: {e}"))
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::RangeNotSatisfiable => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!(error = %self, "request failed");
        }
        // The client never parses error bodies, only status codes
        // (docs/research/api-surface.md section 0, "Error bodies").
        (status, self.to_string()).into_response()
    }
}

/// Handler result alias.
pub type AppResult<T> = Result<T, AppError>;
