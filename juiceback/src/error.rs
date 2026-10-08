//! Error types mapped to HTTP status codes and JSON response bodies.

use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

/// Application errors exposed through the HTTP API.
#[derive(Debug)]
pub enum AppError {
    // -- 404 / 410 --
    NotFound,
    Gone,
    Forbidden(String),
    /// Resource already exists (e.g. custom file ID taken)
    Conflict(String),

    // -- Upload validation (400) --
    /// Upload body claims gzip but decompression failed
    GzipDecodeFailed,

    // -- TUS protocol errors (400 / 409 / 404) --
    /// Upload-Length header missing or unparseable
    TusMissingLength,
    /// Upload-Offset header missing or unparseable
    TusMissingOffset,
    /// Client-supplied offset does not match server state
    TusOffsetMismatch,
    /// TUS upload session not found
    TusSessionNotFound,

    // -- Size / rate (413 / 429) --
    PayloadTooLarge,
    RateLimited,
    /// Account temporarily locked due to too many failed login attempts
    TooManyRequests(String),
    /// File type is blocked (dangerous executable, script, etc.)
    BlockedFileType(String),

    // -- Storage backend --
    /// Cannot reach juicehost at all (connection refused / dns)
    JuicehostUnreachable(String),
    /// juicehost returned a non-2xx status
    JuicehostRejected(String),
    /// juicehost returned 507, disk is full
    InsufficientStorage(String),
    // -- Generic server errors (500) --
    FilesystemError(std::io::Error),
    DatabaseError(rusqlite::Error),
    /// Could not acquire a connection from the database pool
    DbPoolError(String),
    /// A background task (spawn_blocking) panicked
    TaskPanicked(String),
    BadRequest(String),
    Unauthorized(String),
    InvalidMultipart(#[allow(dead_code)] String),
    Internal(String),
    ServiceUnavailable(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, error_code, message) = match self {
            // -- 404 / 403 / 410 / 409 --
            AppError::NotFound => (
                StatusCode::NOT_FOUND,
                "FILE_NOT_FOUND",
                "File not found",
            ),
            AppError::Gone => (
                StatusCode::GONE,
                "FILE_EXPIRED",
                "File has expired",
            ),
            AppError::Forbidden(ref msg) => (
                StatusCode::FORBIDDEN,
                "FORBIDDEN",
                msg.as_str(),
            ),
            AppError::Conflict(ref msg) => (
                StatusCode::CONFLICT,
                "CONFLICT",
                msg.as_str(),
            ),

            // -- Upload validation --
            AppError::GzipDecodeFailed => (
                StatusCode::BAD_REQUEST,
                "GZIP_DECODE_FAILED",
                "Gzip decompression failed, the data may be corrupt or not actually gzip-compressed",
            ),

            // -- TUS protocol --
            AppError::TusMissingLength => (
                StatusCode::BAD_REQUEST,
                "TUS_MISSING_LENGTH",
                "Missing or invalid Upload-Length header",
            ),
            AppError::TusMissingOffset => (
                StatusCode::BAD_REQUEST,
                "TUS_MISSING_OFFSET",
                "Missing or invalid Upload-Offset header",
            ),
            AppError::TusOffsetMismatch => (
                StatusCode::CONFLICT,
                "TUS_OFFSET_MISMATCH",
                "Upload offset does not match server state",
            ),
            AppError::TusSessionNotFound => (
                StatusCode::NOT_FOUND,
                "TUS_SESSION_NOT_FOUND",
                "TUS upload session not found... it may have expired or been cancelled",
            ),
            // -- Size / rate --
            AppError::PayloadTooLarge => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "FILE_TOO_LARGE",
                "File exceeds the maximum allowed size",
            ),
            AppError::RateLimited => (
                StatusCode::TOO_MANY_REQUESTS,
                "RATE_LIMITED",
                "Too many uploads!!! please wait before trying again",
            ),
            AppError::TooManyRequests(ref msg) => (
                StatusCode::TOO_MANY_REQUESTS,
                "ACCOUNT_LOCKED",
                msg.as_str(),
            ),
            AppError::BlockedFileType(ref reason) => (
                StatusCode::BAD_REQUEST,
                "BLOCKED_FILE_TYPE",
                reason.as_str(),
            ),

            // -- Storage backend --
            AppError::JuicehostUnreachable(ref e) => {
                let clean = extract_error_message(e);
                let clean = if clean.trim().is_empty() {
                    "File storage backend is unreachable"
                } else {
                    clean
                };
                (
                    StatusCode::BAD_GATEWAY,
                    "STORAGE_UNREACHABLE",
                    clean,
                )
            }
            AppError::JuicehostRejected(ref e) => {
                let clean = extract_error_message(e);
                let clean = if clean.trim().is_empty() {
                    "File storage backend rejected the upload"
                } else {
                    clean
                };
                (
                    StatusCode::BAD_GATEWAY,
                    "STORAGE_REJECTED",
                    clean,
                )
            }
            AppError::InsufficientStorage(ref e) => {
                let clean = extract_error_message(e);
                let clean = if clean.trim().is_empty() {
                    "This instance is out of storage! Try again later."
                } else {
                    clean
                };
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "INSUFFICIENT_STORAGE",
                    clean,
                )
            }
            // -- Generic server errors --
            AppError::FilesystemError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DISK_ERROR",
                "A server disk error occurred",
            ),
            AppError::DatabaseError(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "DATABASE_ERROR",
                "A database error occurred",
            ),
            AppError::DbPoolError(_) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "DB_POOL_ERROR",
                "The server is temporarily unable to process uploads - please try again",
            ),
            AppError::TaskPanicked(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "TASK_PANICKED",
                "An internal processing task failed unexpectedly",
            ),
            AppError::BadRequest(ref msg) => (StatusCode::BAD_REQUEST, "BAD_REQUEST", msg.as_str()),
            AppError::Unauthorized(ref msg) => (StatusCode::UNAUTHORIZED, "UNAUTHORIZED", msg.as_str()),
            AppError::InvalidMultipart(_) => {
                (StatusCode::BAD_REQUEST, "INVALID_MULTIPART", "Invalid multipart data")
            }
            AppError::Internal(_) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "An internal error occurred",
            ),
            AppError::ServiceUnavailable(ref msg) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "SERVICE_UNAVAILABLE",
                msg.as_str(),
            ),
        };

        let body = Json(json!({
            "error": error_code,
            "message": message,
        }));

        (status, body).into_response()
    }
}

/// Extract a clean message from errors formatted as `[CODE] message (status=N)`.
fn extract_error_message(e: &str) -> &str {
    e.strip_prefix('[')
        .and_then(|s| s.find("] "))
        .map(|i| &e[i + 2..])
        .and_then(|s| s.rsplit_once(" (status="))
        .map(|(msg, _)| msg)
        .unwrap_or(e)
}

impl From<std::io::Error> for AppError {
    fn from(err: std::io::Error) -> Self {
        AppError::FilesystemError(err)
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(err: rusqlite::Error) -> Self {
        AppError::DatabaseError(err)
    }
}

impl AppError {
    /// Log the error at the appropriate level when creating the error,
    /// not when converting it to an HTTP response.
    pub fn log_error(&self) {
        match self {
            AppError::JuicehostUnreachable(e) => tracing::error!("juicehost unreachable: {}", e),
            AppError::JuicehostRejected(e) => tracing::error!("juicehost rejected upload: {}", e),
            AppError::InsufficientStorage(e) => tracing::warn!("juicehost out of storage: {}", e),
            AppError::FilesystemError(e) => tracing::error!("filesystem error: {}", e),
            AppError::DatabaseError(e) => tracing::error!("database error: {}", e),
            AppError::DbPoolError(e) => tracing::error!("database pool error: {}", e),
            AppError::TaskPanicked(e) => tracing::error!("background task panicked: {}", e),
            AppError::Internal(e) => tracing::error!("internal error: {}", e),
            _ => {}
        }
    }

    pub fn from_juicehost_error(e: String) -> Self {
        let err = if e.contains("error trying to connect")
            || e.contains("dns error")
            || e.contains("connection refused")
            || e.contains("error sending request")
            || e.contains("QUIC handshake failed")
            || e.contains("QUIC connect error")
        {
            AppError::JuicehostUnreachable(e)
        } else if e.contains("[INSUFFICIENT_STORAGE]") || e.contains("(status=507)") {
            AppError::InsufficientStorage(e)
        } else {
            AppError::JuicehostRejected(e)
        };
        err.log_error();
        err
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Response, StatusCode};
    use axum::response::IntoResponse;

    fn status_for(error: AppError) -> StatusCode {
        let resp: Response<Body> = error.into_response();
        resp.status()
    }

    async fn body_for(error: AppError) -> serde_json::Value {
        let resp: Response<Body> = error.into_response();
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    #[tokio::test]
    async fn not_found_returns_404() {
        assert_eq!(status_for(AppError::NotFound), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn gone_returns_410() {
        assert_eq!(status_for(AppError::Gone), StatusCode::GONE);
    }

    #[tokio::test]
    async fn forbidden_returns_403() {
        assert_eq!(
            status_for(AppError::Forbidden("test".into())),
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn forbidden_body() {
        let body = body_for(AppError::Forbidden("nope".into())).await;
        assert_eq!(body["error"], "FORBIDDEN");
        assert_eq!(body["message"], "nope");
    }

    #[tokio::test]
    async fn payload_too_large_returns_413() {
        assert_eq!(
            status_for(AppError::PayloadTooLarge),
            StatusCode::PAYLOAD_TOO_LARGE
        );
    }

    #[tokio::test]
    async fn rate_limited_returns_429() {
        assert_eq!(
            status_for(AppError::RateLimited),
            StatusCode::TOO_MANY_REQUESTS
        );
    }

    #[tokio::test]
    async fn tus_missing_length_returns_400() {
        assert_eq!(
            status_for(AppError::TusMissingLength),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn tus_offset_mismatch_returns_409() {
        assert_eq!(
            status_for(AppError::TusOffsetMismatch),
            StatusCode::CONFLICT
        );
    }

    #[tokio::test]
    async fn tus_session_not_found_returns_404() {
        assert_eq!(
            status_for(AppError::TusSessionNotFound),
            StatusCode::NOT_FOUND
        );
    }

    #[tokio::test]
    async fn juicehost_unreachable_returns_502() {
        assert_eq!(
            status_for(AppError::JuicehostUnreachable("conn refused".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn juicehost_rejected_returns_502() {
        assert_eq!(
            status_for(AppError::JuicehostRejected("500".into())),
            StatusCode::BAD_GATEWAY
        );
    }

    #[tokio::test]
    async fn insufficient_storage_returns_503() {
        assert_eq!(
            status_for(AppError::InsufficientStorage("full".into())),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[tokio::test]
    async fn filesystem_error_returns_500() {
        let err = std::io::Error::other("disk");
        assert_eq!(
            status_for(AppError::FilesystemError(err)),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn database_error_returns_500() {
        let err = rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("test".into()),
        );
        assert_eq!(
            status_for(AppError::DatabaseError(err)),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn bad_request_returns_400() {
        assert_eq!(
            status_for(AppError::BadRequest("bad".into())),
            StatusCode::BAD_REQUEST
        );
    }

    #[tokio::test]
    async fn unauthorized_returns_401() {
        assert_eq!(
            status_for(AppError::Unauthorized("no".into())),
            StatusCode::UNAUTHORIZED
        );
    }

    #[tokio::test]
    async fn internal_returns_500() {
        assert_eq!(
            status_for(AppError::Internal("oops".into())),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }

    #[tokio::test]
    async fn internal_body_is_generic() {
        let body = body_for(AppError::Internal("secret details".into())).await;
        assert_eq!(body["error"], "INTERNAL_ERROR");
        assert_eq!(body["message"], "An internal error occurred");
    }

    #[tokio::test]
    async fn invalid_multipart_body_is_generic() {
        let body = body_for(AppError::InvalidMultipart("parse failed".into())).await;
        assert_eq!(body["error"], "INVALID_MULTIPART");
        assert_eq!(body["message"], "Invalid multipart data");
    }

    #[test]
    fn extract_error_message_with_prefix() {
        let msg = extract_error_message("[STORAGE_FULL] disk is full (status=507)");
        assert_eq!(msg, " disk is full");
    }

    #[test]
    fn extract_error_message_without_prefix() {
        let msg = extract_error_message("plain error");
        assert_eq!(msg, "plain error");
    }

    #[test]
    fn extract_error_message_empty() {
        let msg = extract_error_message("");
        assert_eq!(msg, "");
    }

    #[test]
    fn from_juicehost_error_connection_refused() {
        let err = AppError::from_juicehost_error("connection refused".into());
        assert!(matches!(err, AppError::JuicehostUnreachable(_)));
    }

    #[test]
    fn from_juicehost_error_dns() {
        let err = AppError::from_juicehost_error("dns error: failed".into());
        assert!(matches!(err, AppError::JuicehostUnreachable(_)));
    }

    #[test]
    fn from_juicehost_error_insufficient_storage() {
        let err = AppError::from_juicehost_error("[INSUFFICIENT_STORAGE] full".into());
        assert!(matches!(err, AppError::InsufficientStorage(_)));
    }

    #[test]
    fn from_juicehost_error_status_507() {
        let err = AppError::from_juicehost_error("something (status=507)".into());
        assert!(matches!(err, AppError::InsufficientStorage(_)));
    }

    #[test]
    fn from_juicehost_error_generic() {
        let err = AppError::from_juicehost_error("random error".into());
        assert!(matches!(err, AppError::JuicehostRejected(_)));
    }

    #[test]
    fn from_juicehost_error_quic_handshake() {
        let err = AppError::from_juicehost_error("QUIC handshake failed".into());
        assert!(matches!(err, AppError::JuicehostUnreachable(_)));
    }

    #[test]
    fn from_juicehost_error_io_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let app_err: AppError = io_err.into();
        assert!(matches!(app_err, AppError::FilesystemError(_)));
    }
}
