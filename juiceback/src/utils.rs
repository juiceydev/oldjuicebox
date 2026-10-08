use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use aes_gcm::aead::Aead;
use aes_gcm::aead::KeyInit;
use aes_gcm::{Aes256Gcm, Nonce};

use crate::error::AppError;
use crate::state::AppState;

/// Constant-time string comparison to prevent timing attacks on secrets
/// like delete tokens. Pads the shorter input to avoid leaking length.
pub(crate) use juiceutils::constant_time_eq;

// == IP encryption / hashing ==

/// Compute HMAC-SHA256(pepper, ip) for deterministic ban lookups.
pub use juiceutils::ban::hash_ip_for_ban;

/// Truncate a hex string to 12 characters for display in logs/notifications.
pub use juiceutils::ban::truncate_hash;

/// Encrypt an IP address with AES-256-GCM using a random 12-byte nonce.
/// Returns `"nonce_hex:ciphertext_hex"` where ciphertext includes the 16-byte auth tag.
pub fn encrypt_ip(ip: &str, key_hex: &str) -> Option<String> {
    let key_bytes = hex::decode(key_hex).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).ok()?;
    let mut nonce_bytes = [0u8; 12];
    // Fill nonce with cryptographically secure random bytes.
    // Uses /dev/urandom on Linux (the production platform).
    {
        use std::io::Read;
        let mut f = std::fs::File::open("/dev/urandom").ok()?;
        f.read_exact(&mut nonce_bytes).ok()?;
    }
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, ip.as_bytes()).ok()?;
    Some(format!(
        "{}:{}",
        hex::encode(nonce_bytes),
        hex::encode(ciphertext)
    ))
}

/// Decrypt an IP address that was encrypted with [`encrypt_ip`].
pub fn decrypt_ip(encrypted: &str, key_hex: &str) -> Option<String> {
    let (nonce_hex, ct_hex) = encrypted.split_once(':')?;
    let key_bytes = hex::decode(key_hex).ok()?;
    let nonce_bytes = hex::decode(nonce_hex).ok()?;
    let ct_bytes = hex::decode(ct_hex).ok()?;
    let cipher = Aes256Gcm::new_from_slice(&key_bytes).ok()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let plaintext = cipher.decrypt(nonce, ct_bytes.as_ref()).ok()?;
    String::from_utf8(plaintext).ok()
}

/// Validate a file ID: must be non-empty, within length bounds,
/// and contain only alphanumeric, `-`, or `_`.
pub fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() >= crate::constants::MIN_CUSTOM_ID_LEN
        && id.len() <= crate::constants::MAX_CUSTOM_ID_LEN
        && id
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

/// Normalize a custom ID: trim whitespace and convert to lowercase.
pub fn normalize_custom_id(id: &str) -> String {
    id.trim().to_lowercase()
}

/// Build the public URL for a file, including an extension for client compatibility.
pub fn public_url(
    base_url: &str,
    storage_host: &Option<String>,
    id: &str,
    filename: &str,
) -> String {
    let host = match storage_host {
        Some(host) if !host.is_empty() => host.as_str(),
        _ => base_url,
    };
    let host = if host.contains("://") {
        host.to_string()
    } else {
        format!("https://{}", host)
    };
    match filename.rsplit('.').next() {
        Some(ext) if !ext.is_empty() && ext != filename => format!("{}/f/{}.{}", host, id, ext),
        _ => format!("{}/f/{}", host, id),
    }
}

pub fn log_quic_throughput(id: &str, size: u64, total: Duration, parse: Duration) {
    let throughput = if total.as_secs_f64() > 0.0 {
        size as f64 / total.as_secs_f64() / crate::constants::BYTES_PER_MIB
    } else {
        0.0
    };
    tracing::info!(
        "quic_upload: id={} size={} total={:?} throughput={:.2} MB/s multipart_parse={:?}",
        id,
        size,
        total,
        throughput,
        parse,
    );
}

#[derive(Clone, Copy, Debug)]
pub struct ClientIp(pub IpAddr);

pub fn client_ip(headers: &HeaderMap, peer_ip: IpAddr, state: &AppState) -> IpAddr {
    juiceutils::proxy::client_ip(headers, peer_ip, &state.config.trusted_proxy_cidrs)
}

pub async fn client_ip_middleware(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    mut req: Request<Body>,
    next: Next,
) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|value| value.0.ip())
        .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
    let ip = client_ip(req.headers(), peer, &state);
    req.extensions_mut().insert(ClientIp(ip));
    tracing::Span::current().record("client_ip", tracing::field::display(ip));
    next.run(req).await
}

#[derive(Clone, Debug)]
pub struct TrustedClientIpKeyExtractor {
    trusted_proxy_cidrs: Arc<Vec<juiceutils::proxy::IpCidr>>,
}

impl TrustedClientIpKeyExtractor {
    pub fn new(cidrs: Vec<juiceutils::proxy::IpCidr>) -> Self {
        Self {
            trusted_proxy_cidrs: Arc::new(cidrs),
        }
    }
}

impl tower_governor::key_extractor::KeyExtractor for TrustedClientIpKeyExtractor {
    type Key = IpAddr;

    fn extract<T>(
        &self,
        req: &axum::http::Request<T>,
    ) -> Result<Self::Key, tower_governor::GovernorError> {
        if let Some(client_ip) = req.extensions().get::<ClientIp>() {
            return Ok(client_ip.0);
        }
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|value| value.0.ip())
            .ok_or(tower_governor::GovernorError::UnableToExtractKey)?;
        Ok(juiceutils::proxy::client_ip(
            req.headers(),
            peer,
            &self.trusted_proxy_cidrs,
        ))
    }
}

/// Axum middleware that checks if the client IP is banned.
// Proxy-aware client address extraction.
pub async fn ban_check_middleware(
    axum::extract::State(state): axum::extract::State<Arc<AppState>>,
    req: Request<Body>,
    next: Next,
) -> Response {
    // The ban-status endpoint must stay reachable for banned IPs: it is what
    // the frontend uses to render the /banned page (with the reason). Blocking
    // it would make the ban page itself unreachable.
    if req.uri().path() == "/api/ban-status" {
        return next.run(req).await;
    }

    let raw_ip = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|value| client_ip(req.headers(), value.0.ip(), &state))
        .or_else(|| {
            req.extensions()
                .get::<ClientIp>()
                .map(|value| value.0)
                .filter(|ip| !ip.is_unspecified())
        })
        .unwrap_or_else(|| {
            let peer = req
                .extensions()
                .get::<ConnectInfo<SocketAddr>>()
                .map(|value| value.0.ip())
                .unwrap_or(IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
            client_ip(req.headers(), peer, &state)
        })
        .to_string();
    let hashed = hash_ip_for_ban(&raw_ip, &state.config.ip_pepper);
    if state.is_banned(&hashed) {
        return AppError::Forbidden("you are banned".into()).into_response();
    }

    next.run(req).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_with_storage_host() {
        let url = public_url(
            "http://localhost:6402",
            &Some("https://files.example.com".into()),
            "abc123",
            "cute.gif",
        );
        assert_eq!(url, "https://files.example.com/f/abc123.gif");
    }

    #[test]
    fn public_url_without_storage_host() {
        let url = public_url("http://localhost:6402", &None, "abc123", "cute.gif");
        assert_eq!(url, "http://localhost:6402/f/abc123.gif");
    }

    #[test]
    fn public_url_empty_storage_host() {
        let url = public_url(
            "http://localhost:6402",
            &Some("".into()),
            "abc123",
            "cute.gif",
        );
        assert_eq!(url, "http://localhost:6402/f/abc123.gif");
    }

    #[test]
    fn public_url_bare_storage_host() {
        let url = public_url(
            "https://f.juicey.dev",
            &Some("fx.juicey.dev".into()),
            "abc123",
            "cute.gif",
        );
        assert_eq!(url, "https://fx.juicey.dev/f/abc123.gif");
    }
}
