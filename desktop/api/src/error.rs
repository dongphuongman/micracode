//! HTTP error type. Serialises to FastAPI's `{"detail": "..."}` shape so the
//! existing TS client (`ApiError`) keeps working unchanged.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;

use crate::storage::StorageError;

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    /// 422 — matches FastAPI path/body validation failures.
    #[error("{0}")]
    Unprocessable(String),
    /// 502 — an upstream dependency (e.g. the Anthropic API) failed.
    #[error("{0}")]
    BadGateway(String),
    /// 503 — a feature is unavailable because it isn't configured.
    #[error("{0}")]
    Unavailable(String),
    #[error("{0}")]
    Internal(String),
}

impl ApiError {
    fn status(&self) -> StatusCode {
        match self {
            ApiError::NotFound(_) => StatusCode::NOT_FOUND,
            ApiError::BadRequest(_) => StatusCode::BAD_REQUEST,
            ApiError::Unprocessable(_) => StatusCode::UNPROCESSABLE_ENTITY,
            ApiError::BadGateway(_) => StatusCode::BAD_GATEWAY,
            ApiError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Translate text-generation failures into HTTP errors (PRD FR `ClaudeTextGeneration`).
impl From<crate::text_generation::TextGenError> for ApiError {
    fn from(err: crate::text_generation::TextGenError) -> Self {
        use crate::text_generation::TextGenError;
        match err {
            // Not configured → unavailable, not the client's fault.
            TextGenError::MissingApiKey => ApiError::Unavailable(err.to_string()),
            // Anything else is an upstream/provider failure → 502.
            TextGenError::Http(_)
            | TextGenError::Api { .. }
            | TextGenError::Refused
            | TextGenError::Empty => ApiError::BadGateway(err.to_string()),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        let detail = self.to_string();
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            tracing::error!("internal error: {detail}");
            // Don't leak internals, matching the Python catch-all handler.
            return (status, Json(json!({ "detail": "Internal Server Error" }))).into_response();
        }
        (status, Json(json!({ "detail": detail }))).into_response()
    }
}

/// Translate VCS/checkpoint failures into HTTP errors (PRD FR6).
impl From<core_vcs::VcsError> for ApiError {
    fn from(err: core_vcs::VcsError) -> Self {
        use core_vcs::VcsError;
        match err {
            VcsError::NotFound(id) => ApiError::NotFound(format!("checkpoint not found: {id}")),
            VcsError::InvalidId(id) => {
                ApiError::Unprocessable(format!("invalid checkpoint id: {id}"))
            }
            VcsError::Git(e) => ApiError::Internal(e.to_string()),
        }
    }
}

/// Translate storage failures into HTTP errors. `NotFound` is mapped here, but
/// most handlers check existence explicitly first (as the Python routes do).
impl From<StorageError> for ApiError {
    fn from(err: StorageError) -> Self {
        match err {
            StorageError::NotFound => ApiError::NotFound("project not found".to_string()),
            StorageError::InvalidSlug(s) => {
                ApiError::Unprocessable(format!("invalid project id: {s}"))
            }
            StorageError::InvalidSnapshotId(s) => {
                ApiError::Unprocessable(format!("invalid snapshot id: {s}"))
            }
            StorageError::PathEscape(msg) | StorageError::Validation(msg) => {
                ApiError::BadRequest(msg)
            }
            StorageError::Io(e) => ApiError::Internal(e.to_string()),
        }
    }
}
