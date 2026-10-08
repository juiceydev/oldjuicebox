//! Shared QUIC/HTTP/3, ban-list, and file-validation utilities.

pub mod ban;
pub mod file_validation;
pub mod proxy;

#[cfg(feature = "quic")]
pub mod server;

#[cfg(feature = "quic")]
pub use server::{
    generate_self_signed_cert, get_or_generate_cert, load_cert_for_pinning, start_quic_server,
    start_quic_server_with_limits, QuicServerLimits,
};

/// Compare secret contents without early exit.
pub fn constant_time_eq(a: &str, b: &str) -> bool {
    use subtle::ConstantTimeEq;
    let max_len = a.len().max(b.len());
    let a_padded = a.bytes().chain(std::iter::repeat(0u8)).take(max_len);
    let b_padded = b.bytes().chain(std::iter::repeat(0u8)).take(max_len);
    let mut result = 0u8;
    for (x, y) in a_padded.zip(b_padded) {
        result |= x ^ y;
    }
    result.ct_eq(&0u8).into()
}

/// Extract a single bearer token from the Authorization header.
pub fn extract_bearer_token(headers: &axum::http::HeaderMap) -> Option<&str> {
    let auth = headers.get("authorization")?.to_str().ok()?;
    let mut parts = auth.split_whitespace();
    let scheme = parts.next()?;
    let token = parts.next()?;
    if scheme.eq_ignore_ascii_case("bearer") && parts.next().is_none() {
        Some(token)
    } else {
        None
    }
}

/// Wait for Ctrl+C or SIGTERM.
pub async fn shutdown_signal(service_name: &str) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("Failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            tracing::info!("{} shutting down...", service_name);
        },
        _ = terminate => {
            tracing::info!("{} terminated...", service_name);
        },
    }
}

/// Add security headers while allowing file previews to be embedded.
pub async fn add_security_headers(
    req: axum::http::Request<axum::body::Body>,
    next: axum::middleware::Next,
) -> impl axum::response::IntoResponse {
    use axum::http::{header, HeaderValue};

    let is_file_route = req.uri().path().starts_with("/f/");

    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );

    if is_file_route {
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:",
            ),
        );
    } else {
        headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
        headers.insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'",
            ),
        );
    }

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_time_eq_equal() {
        assert!(constant_time_eq("hello", "hello"));
    }

    #[test]
    fn constant_time_eq_unequal() {
        assert!(!constant_time_eq("hello", "world"));
    }

    #[test]
    fn constant_time_eq_different_lengths() {
        assert!(!constant_time_eq("a", "ab"));
        assert!(!constant_time_eq("ab", "a"));
    }

    #[test]
    fn constant_time_eq_empty() {
        assert!(constant_time_eq("", ""));
    }

    #[test]
    fn extract_bearer_token_requires_one_token() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("authorization", "bearer token".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), Some("token"));
        headers.insert("authorization", "Bearer token extra".parse().unwrap());
        assert_eq!(extract_bearer_token(&headers), None);
    }
}
