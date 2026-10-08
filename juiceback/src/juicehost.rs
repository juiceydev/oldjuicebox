//! Communication with the juicehost storage backend.
//! Provides streaming and buffered push operations, file rename, and deletion.

use std::sync::Arc;

use crate::state::AppState;
use crate::upload_mode::UploadMode;

/// Configuration fetched from juicehost at startup.
/// This is the single source of truth for upload limits, TTL, danger level, etc.
#[derive(Debug, Clone)]
pub struct JuicehostConfig {
    pub max_file_size_bytes: u64,
    pub default_ttl_hours: f64,
    pub allowed_ttl_hours: Vec<f64>,
    pub danger_level: crate::file_validation::ProtectionLevel,
    pub quick_link: bool,
    pub custom_id_enabled: bool,
    pub ultrafast: bool,
}

/// Fetch juicehost config at startup.
pub async fn fetch_juicehost_config(
    client: &reqwest::Client,
    juicehost_url: &str,
    headers: &reqwest::header::HeaderMap,
) -> Result<JuicehostConfig, String> {
    if juicehost_url.is_empty() {
        return Err("JUICEHOST_URL not set".into());
    }

    let url = format!("{}/api/config", juicehost_url);
    let resp = client
        .get(&url)
        .headers(headers.clone())
        .send()
        .await
        .map_err(|e| format!("juicehost config fetch error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "juicehost config fetch failed: status={} body={}",
            status, body
        ));
    }

    let cfg: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("juicehost config parse error: {}", e))?;

    let max_file_size_bytes = cfg
        .get("max_file_size_bytes")
        .and_then(|v| v.as_u64())
        .unwrap_or(524288000);

    let default_ttl_hours = cfg
        .get("default_ttl_hours")
        .and_then(|v| v.as_f64())
        .unwrap_or(24.0);

    let allowed_ttl_hours = cfg
        .get("allowed_ttl_hours")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_f64()).collect::<Vec<f64>>())
        .unwrap_or_else(|| vec![0.5, 1.0, 6.0, 12.0, 24.0, 72.0, 168.0]);

    let danger_level_str = cfg
        .get("danger_level")
        .and_then(|v| v.as_str())
        .unwrap_or("high");
    let danger_level = crate::file_validation::ProtectionLevel::parse(danger_level_str);

    let quick_link = cfg
        .get("quick_link")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let custom_id_enabled = cfg
        .get("custom_id")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let ultrafast = cfg
        .get("ultrafast")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    tracing::info!(
        "juicehost config: max_file_size={}MB default_ttl={}h danger={} quick_link={} custom_id={} ultrafast={}",
        max_file_size_bytes / (1024 * 1024),
        default_ttl_hours,
        danger_level_str,
        quick_link,
        custom_id_enabled,
        ultrafast,
    );

    Ok(JuicehostConfig {
        max_file_size_bytes,
        default_ttl_hours,
        allowed_ttl_hours,
        danger_level,
        quick_link,
        custom_id_enabled,
        ultrafast,
    })
}

fn require_juicehost_url(url: &str) -> Result<(), String> {
    if url.is_empty() {
        Err("JUICEHOST_URL not set".into())
    } else {
        Ok(())
    }
}

/// A request target with credentials appropriate for that target.
pub(crate) struct JuicehostTarget {
    pub base_url: String,
    pub client: reqwest::Client,
    pub headers: reqwest::header::HeaderMap,
}

/// Normalize and resolve a user-selected host, rejecting targets unsafe for SSRF.
pub(crate) async fn custom_juicehost_target(host: &str) -> Result<JuicehostTarget, String> {
    let trimmed = host.trim();
    if trimmed.is_empty() {
        return Err("empty host".into());
    }
    let with_scheme = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("https://{}", trimmed)
    };
    let parsed = url::Url::parse(&with_scheme).map_err(|e| format!("invalid host URL: {}", e))?;
    if parsed.scheme() != "http" && parsed.scheme() != "https" {
        return Err(format!(
            "host scheme must be http or https, got {}",
            parsed.scheme()
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("host URL must not contain credentials".into());
    }
    if parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() {
        return Err("host URL must not contain a path, query, or fragment".into());
    }
    let hostname = parsed
        .host_str()
        .ok_or_else(|| "host must include a hostname".to_string())?;
    if hostname.ends_with('.') {
        return Err("host URL must not contain a trailing-dot hostname".into());
    }

    let port = parsed
        .port_or_known_default()
        .ok_or_else(|| "host URL must include a valid port".to_string())?;
    let name = hostname.to_ascii_lowercase();
    if is_forbidden_hostname(&name) {
        return Err(format!(
            "refusing to reach local or metadata host: {}",
            hostname
        ));
    }

    let mut resolved = if let Ok(ip) = name.parse::<std::net::IpAddr>() {
        vec![std::net::SocketAddr::new(ip, port)]
    } else {
        tokio::net::lookup_host((name.as_str(), port))
            .await
            .map_err(|e| format!("dns lookup failed for {}: {}", name, e))?
            .collect::<Vec<_>>()
    };
    resolved.sort_unstable();
    resolved.dedup();
    if resolved.is_empty() {
        return Err(format!("host did not resolve: {}", hostname));
    }
    if let Some(addr) = resolved.iter().find(|addr| !is_public_ip(addr.ip())) {
        return Err(format!(
            "refusing to reach non-public address: {}",
            addr.ip()
        ));
    }

    let mut builder = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none());
    if name.parse::<std::net::IpAddr>().is_err() {
        // Pin the validated DNS result so connection setup cannot resolve a new address.
        builder = builder.resolve_to_addrs(&name, &resolved);
    }
    let client = builder
        .build()
        .map_err(|e| format!("failed to build custom host client: {}", e))?;

    Ok(JuicehostTarget {
        base_url: parsed.origin().ascii_serialization(),
        client,
        headers: reqwest::header::HeaderMap::new(),
    })
}

/// Only publicly routable addresses are valid custom-host destinations.
pub(crate) fn is_public_ip(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V4(v4) => is_public_ipv4(v4.octets()),
        std::net::IpAddr::V6(v6) => {
            let octets = v6.octets();
            if let Some(v4) = v6.to_ipv4() {
                return is_public_ipv4(v4.octets());
            }
            !(v6.is_unspecified()
                || v6.is_loopback()
                || v6.is_multicast()
                || (octets[0] & 0xfe) == 0xfc
                || (octets[0] == 0xfe && octets[1] & 0xc0 == 0x80)
                || octets[..4] == [0x20, 0x01, 0x0d, 0xb8])
        }
    }
}

fn is_public_ipv4([a, b, c, _]: [u8; 4]) -> bool {
    !matches!(
        (a, b, c),
        (0, _, _)
            | (10, _, _)
            | (100, 64..=127, _)
            | (127, _, _)
            | (169, 254, _)
            | (172, 16..=31, _)
            | (192, 0, 0 | 2)
            | (192, 168, _)
            | (198, 18 | 19 | 51, _)
            | (203, 0, 113)
            | (224..=255, _, _)
    )
}

pub(crate) fn is_forbidden_hostname(name: &str) -> bool {
    name == "localhost"
        || name.ends_with(".localhost")
        || name.ends_with(".local")
        || name.ends_with(".internal")
        || name == "metadata"
        || name.starts_with("metadata.")
        || name == "instance-data"
        || name.starts_with("instance-data.")
}

/// Resolve the juicehost base URL to push/delete against.
///
/// A non-empty custom `host` routes to that host's public URL; otherwise the
/// configured internal `juicehost_url` is used.
async fn resolve_juicehost_target(
    state: &Arc<AppState>,
    host: Option<&str>,
) -> Result<JuicehostTarget, String> {
    match host.map(str::trim).filter(|h| !h.is_empty()) {
        Some(h) => custom_juicehost_target(h).await,
        None => {
            require_juicehost_url(&state.config.juicehost_url)?;
            Ok(JuicehostTarget {
                base_url: state.config.juicehost_url.trim_end_matches('/').to_string(),
                client: state.http.clone(),
                headers: state.juicehost_headers.clone(),
            })
        }
    }
}

/// Return the pre-built authentication and origin headers sent to juicehost.
pub(crate) fn juicehost_headers(state: &Arc<AppState>) -> reqwest::header::HeaderMap {
    state.juicehost_headers.clone()
}

/// Query juicehost to confirm a file physically exists on disk and report its
/// stored size. Returns `Ok(Some(size_bytes))` when present, `Ok(None)` when
/// the file is not on juicehost, and `Err(String)` when juicehost is
/// unreachable or returned an unexpected status. Used to verify ultrafast
/// uploads actually landed before marking them ready.
pub async fn stat_file_on_juicehost(
    state: &Arc<AppState>,
    id: &str,
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<Option<u64>, String> {
    let custom_host = host.map(str::trim).filter(|h| !h.is_empty());
    if custom_host.is_none() {
        require_juicehost_url(&state.config.juicehost_url)?;
    }
    let target = resolve_juicehost_target(state, custom_host).await?;

    let mut headers = target.headers.clone();
    if let Some(cap) = capability {
        headers.insert("x-juicehost-file-capability", cap.parse().unwrap());
    }

    let url = format!("{}/internal/file/{}/stat", target.base_url, id);
    let resp = target
        .client
        .get(&url)
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("juicehost stat request error: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(format!(
            "juicehost stat failed: status={} body={}",
            status, body
        ));
    }

    let json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("juicehost stat parse error: {}", e))?;
    match json.get("exists").and_then(|v| v.as_bool()) {
        Some(true) => Ok(json.get("size_bytes").and_then(|v| v.as_u64())),
        _ => Ok(None),
    }
}

/// Parse juicehost's error json into something readable. used by http + quic.
pub(crate) fn format_error_response(status: u16, body_text: String) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(&body_text).ok();
    let error_code = parsed
        .as_ref()
        .and_then(|v| v.get("error").and_then(|e| e.as_str()))
        .unwrap_or("UNKNOWN_ERROR");
    let detail = parsed
        .as_ref()
        .and_then(|v| v.get("message").and_then(|m| m.as_str()));
    match detail {
        Some(d) if !d.is_empty() => format!("[{}] {} (status={})", error_code, d, status),
        _ if body_text.is_empty() => format!("{} (status={}, body=<empty>)", error_code, status),
        _ => format!("[{}] {} (status={})", error_code, body_text, status),
    }
}

/// Stream a file to juicehost over HTTP or QUIC. channel-based, no full buffering.
/// Close the sender = EOF. QUIC falls back to TCP.
#[allow(clippy::too_many_arguments)]
#[tracing::instrument(skip_all)]
pub async fn push_file_streaming(
    state: &Arc<AppState>,
    id: &str,
    filename: &str,
    mime_type: &str,
    chunk_rx: tokio::sync::mpsc::Receiver<Result<bytes::Bytes, String>>,
    host: Option<String>,
    mode: &UploadMode,
    capability: Option<&str>,
) -> Result<(), String> {
    let custom_host = host.as_deref().map(str::trim).filter(|h| !h.is_empty());
    let target = resolve_juicehost_target(state, custom_host).await?;

    if *mode == UploadMode::Quic
        && custom_host.is_none()
        && !state.config.juicehost_api_key.is_empty()
    {
        // QUIC is internal-only!!
        // always use the direct juicehost URL and never the public-facing host override (which may point to Cloudflare/Nginx)
        require_juicehost_url(&state.config.juicehost_url)?;

        let mut chunks = Vec::new();
        let mut rx = chunk_rx;

        match crate::quic::push_file_streaming_quic_streamed(
            state,
            id,
            filename,
            mime_type,
            &mut rx,
            &mut chunks,
        )
        .await
        {
            Ok(()) => {
                drop(chunks);
                return Ok(());
            }
            Err(e) => {
                // If juicehost itself returned a non-2xx (e.g. 507 disk full),
                // return that error directly as we kinda have no point retrying over TCP.
                if e.contains("(status=") {
                    return Err(e);
                }
                tracing::warn!("QUIC push failed for {}, falling back to HTTP: {}", id, e);
            }
        }

        // QUIC failed partway.. nuke any leftover chunks.
        while let Some(chunk) = rx.recv().await {
            if let Ok(data) = chunk {
                chunks.push(data);
            }
        }

        if chunks.is_empty() {
            return Err("no data received for QUIC push".into());
        }

        return push_buffered_to_http(state, id, filename, mime_type, chunks, None, capability)
            .await;
    }

    let encoded_fn =
        percent_encoding::utf8_percent_encode(filename, percent_encoding::NON_ALPHANUMERIC)
            .to_string();
    let url = format!(
        "{}/internal/file/stream/{}/{}",
        target.base_url, id, encoded_fn
    );
    let stream = tokio_stream::wrappers::ReceiverStream::new(chunk_rx);
    let body = reqwest::Body::wrap_stream(stream);

    let req_start = std::time::Instant::now();
    let mut request = target
        .client
        .post(&url)
        .header("x-mime-type", mime_type)
        .headers(target.headers);
    if let Some(capability) = capability {
        request = request.header("x-juicehost-file-capability", capability);
    }
    let resp = request
        .body(body)
        .send()
        .await
        .map_err(|e| format!("juicehost streaming push error: {}", e))?;
    let send_time = req_start.elapsed();

    if resp.status().is_success() {
        tracing::debug!("push_stream: id={} url={} send={:?}", id, url, send_time,);
        tracing::info!("juicehost streaming push: id={} ok", id);
        Ok(())
    } else {
        let status = resp.status();
        let resp_start = std::time::Instant::now();
        let body_text = resp.text().await.unwrap_or_default();
        let resp_time = resp_start.elapsed();
        tracing::warn!(
            "push_stream error: id={} url={} send={:?} resp_read={:?} status={}",
            id,
            url,
            send_time,
            resp_time,
            status.as_u16(),
        );
        Err(format!(
            "{} (url={})",
            format_error_response(status.as_u16(), body_text),
            url
        ))
    }
}

/// Push pre-buffered chunks to juicehost over HTTP.
async fn push_buffered_to_http(
    state: &Arc<AppState>,
    id: &str,
    filename: &str,
    mime_type: &str,
    chunks: Vec<bytes::Bytes>,
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<(), String> {
    let data = tokio::task::spawn_blocking(move || {
        let total: usize = chunks.iter().map(|c| c.len()).sum();
        let mut data = Vec::with_capacity(total);
        for c in chunks {
            data.extend_from_slice(&c);
        }
        data
    })
    .await
    .map_err(|_| "data concatenation panicked".to_string())?;
    push_file_to_juicehost(state, id, filename, mime_type, data, host, capability).await
}

/// Push raw bytes to juicehost and it will stay on juicehost's disk
#[tracing::instrument(skip_all)]
pub async fn push_file_to_juicehost(
    state: &Arc<AppState>,
    id: &str,
    filename: &str,
    mime_type: &str,
    data: Vec<u8>,
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<(), String> {
    let target = resolve_juicehost_target(state, host).await?;

    let build_start = std::time::Instant::now();
    let file_part = reqwest::multipart::Part::bytes(data)
        .file_name(filename.to_string())
        .mime_str(mime_type)
        .map_err(|e| format!("mime error: {}", e))?;

    let form = reqwest::multipart::Form::new()
        .text("id", id.to_string())
        .text("filename", filename.to_string())
        .part("file", file_part);
    let build_time = build_start.elapsed();

    let send_start = std::time::Instant::now();
    let url = format!("{}/internal/file", target.base_url);
    let mut request = target.client.post(&url).headers(target.headers);
    if let Some(capability) = capability {
        request = request.header("x-juicehost-file-capability", capability);
    }
    let resp = request
        .multipart(form)
        .send()
        .await
        .map_err(|e| format!("juicehost push error: {}", e))?;
    let send_time = send_start.elapsed();

    tracing::debug!(
        "juicehost push: id={} build={:?} send={:?} status={}",
        id,
        build_time,
        send_time,
        resp.status()
    );

    if resp.status().is_success() {
        tracing::info!("juicehost push: id={} ok", id);
        Ok(())
    } else {
        let body_start = std::time::Instant::now();
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let body_time = body_start.elapsed();
        tracing::warn!(
            "juicehost push error: id={} url={} body_read={:?} status={}",
            id,
            url,
            body_time,
            status.as_u16(),
        );
        Err(format!(
            "{} (url={})",
            format_error_response(status.as_u16(), body),
            url
        ))
    }
}

pub async fn rename_file_on_juicehost(
    state: &Arc<AppState>,
    old_id: &str,
    new_id: &str,
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<(), String> {
    let custom_host = host.map(str::trim).filter(|h| !h.is_empty());
    if custom_host.is_none() && state.config.juicehost_url.is_empty() {
        return Ok(());
    }
    let target = resolve_juicehost_target(state, custom_host).await?;

    let body = serde_json::json!({ "new_id": new_id });

    let mut headers = target.headers.clone();
    if let Some(cap) = capability {
        headers.insert("x-juicehost-file-capability", cap.parse().unwrap());
    }

    let resp = target
        .client
        .post(format!(
            "{}/internal/file/{}/rename",
            target.base_url, old_id
        ))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("juicehost rename request failed: {}", e))?;

    if resp.status().is_success() {
        tracing::info!("juicehost rename: {} -> {} ok", old_id, new_id);
        Ok(())
    } else {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::NOT_FOUND {
            tracing::error!("juicehost rename: {} not found on disk", old_id);
            return Err(format!(
                "juicehost rename failed: source file {} not found on disk (status=404)",
                old_id
            ));
        }
        Err(format!(
            "juicehost rename failed: status={} body={}",
            status, body_text
        ))
    }
}

pub async fn delete_file_on_juicehost(
    state: &Arc<AppState>,
    id: &str,
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<(), String> {
    let custom_host = host.map(str::trim).filter(|h| !h.is_empty());
    if custom_host.is_none() && state.config.juicehost_url.is_empty() {
        return Ok(());
    }
    let target = resolve_juicehost_target(state, custom_host).await?;

    let mut headers = target.headers.clone();
    if let Some(cap) = capability {
        headers.insert("x-juicehost-file-capability", cap.parse().unwrap());
    }

    let resp = target
        .client
        .delete(format!("{}/internal/file/{}", target.base_url, id))
        .headers(headers)
        .send()
        .await
        .map_err(|e| format!("juicehost delete request failed: {}", e))?;

    if resp.status().is_success() {
        tracing::info!("juicehost delete: id={} ok", id);
        Ok(())
    } else if resp.status() == reqwest::StatusCode::NOT_FOUND {
        tracing::warn!("juicehost delete: id={} not found (already gone)", id);
        Ok(())
    } else {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        Err(format!(
            "juicehost delete failed: status={} body={}",
            status, body
        ))
    }
}

/// Concatenate multiple part files on juicehost into a single target file.
/// Used by parallel TUS uploads to assemble split files.
pub async fn concat_files(
    state: &Arc<AppState>,
    target_id: &str,
    filename: &str,
    part_ids: &[String],
    host: Option<&str>,
    capability: Option<&str>,
) -> Result<(), String> {
    let target = resolve_juicehost_target(state, host).await?;

    let body = serde_json::json!({
        "target_id": target_id,
        "filename": filename,
        "parts": part_ids,
    });

    let mut headers = target.headers.clone();
    if let Some(cap) = capability {
        headers.insert("x-juicehost-file-capability", cap.parse().unwrap());
    }

    let resp = target
        .client
        .post(format!("{}/internal/file/concat", target.base_url))
        .headers(headers)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("juicehost concat request failed: {}", e))?;

    if resp.status().is_success() {
        tracing::info!("juicehost concat: {} <- {:?} ok", target_id, part_ids);
        Ok(())
    } else {
        let status = resp.status();
        let body_text = resp.text().await.unwrap_or_default();
        Err(format!(
            "juicehost concat failed: status={} body={}",
            status, body_text
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_error_response_with_json() {
        let body = r#"{"error": "STORAGE_FULL", "message": "disk is full"}"#.to_string();
        let result = format_error_response(507, body);
        assert!(result.contains("[STORAGE_FULL]"));
        assert!(result.contains("disk is full"));
        assert!(result.contains("status=507"));
    }

    #[test]
    fn format_error_response_empty_body() {
        let result = format_error_response(500, "".to_string());
        assert!(result.contains("UNKNOWN_ERROR"));
        assert!(result.contains("body=<empty>"));
        assert!(result.contains("status=500"));
    }

    #[test]
    fn format_error_response_invalid_json() {
        let result = format_error_response(403, "forbidden".to_string());
        assert!(result.contains("forbidden"));
        assert!(result.contains("status=403"));
    }

    fn test_state() -> Arc<AppState> {
        let config = crate::config::Config {
            host: "127.0.0.1".into(),
            port: 6401,
            quic_port: 6402,
            database_path: "./test.db".into(),
            rate_limit_per_minute: 10,
            db_pool_size: 8,
            public_base_url: "http://localhost:6402".into(),
            log_level: "info".into(),
            cleanup_interval_minutes: 30,
            juicehost_api_key: "my_api_key".into(),
            juicehost_url: "http://127.0.0.1:6402".into(),
            public_juicehost_url: "http://localhost:6402".into(),
            juiceback_origin: "http://127.0.0.1:6401".into(),
            jwt_secret: "secret".into(),
            cors_origins: vec![],
            report_webhook_url: None,
            smtp_host: None,
            smtp_port: None,
            smtp_username: None,
            smtp_password: None,
            report_email_recipient: None,
            report_email_sender: None,
            quic_cert_path: None,
            ip_encryption_key: "0000000000000000000000000000000000000000000000000000000000000001"
                .into(),
            ip_pepper: "test_pepper".into(),
            trusted_proxy_cidrs: vec![],
            report_retention_days: 90,
            feedback_retention_days: 90,
            cf_api_token: None,
            cf_zone_id: None,
            ticket_jwt_secret: "secret".into(),
            secure_cookies: false,
            direct_upload_enabled: false,
            cobalt_enabled: false,
            cobalt_api_url: "http://localhost:7272".into(),
            cobalt_api_key: "cobalt_key".into(),
            cobalt_session_api_url: None,
            cobalt_session_api_key: None,
            bun_fetch_url: None,
            bun_fetch_key: None,
            fetch_empty_retry_delay_secs: 0,
            dte_enabled: false,
            dte_assumed_bps: crate::constants::DTE_ASSUMED_BPS,
            dte_safety_mult: crate::constants::DTE_SAFETY_MULT,
            dte_base_overhead_secs: crate::constants::DTE_BASE_OVERHEAD_SECS,
            dte_min_ttl_secs: crate::constants::DTE_MIN_TTL_SECS,
            dte_max_ttl_secs: crate::constants::DTE_MAX_TTL_SECS,
            dte_mint_limit: crate::constants::DTE_MINT_LIMIT,
            dte_mint_window_secs: crate::constants::DTE_MINT_WINDOW_SECS,
            dte_mint_burst: crate::constants::DTE_MINT_BURST,
            region_public_juicehosts: std::collections::HashMap::new(),
        };
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert("x-juicehost-api-key", "my_api_key".parse().unwrap());
        headers.insert(
            "x-juiceback-origin",
            "http://127.0.0.1:6401".parse().unwrap(),
        );
        crate::state::AppState::new(
            r2d2::Pool::builder()
                .max_size(1)
                .build(r2d2_sqlite::SqliteConnectionManager::memory())
                .unwrap(),
            config,
            reqwest::Client::new(),
            headers,
            None,
        )
    }

    #[tokio::test]
    async fn custom_target_does_not_forward_internal_headers() {
        let target = resolve_juicehost_target(&test_state(), Some("https://1.1.1.1:8443"))
            .await
            .unwrap();
        assert!(target.headers.is_empty());
    }

    #[tokio::test]
    async fn configured_target_preserves_internal_headers_and_url() {
        let target = resolve_juicehost_target(&test_state(), None).await.unwrap();
        assert_eq!(target.base_url, "http://127.0.0.1:6402");
        assert_eq!(target.headers["x-juicehost-api-key"], "my_api_key");
        assert_eq!(
            target.headers["x-juiceback-origin"],
            "http://127.0.0.1:6401"
        );
    }

    #[test]
    fn require_juicehost_url_empty() {
        assert!(require_juicehost_url("").is_err());
    }

    #[test]
    fn require_juicehost_url_valid() {
        assert!(require_juicehost_url("http://127.0.0.1:6402").is_ok());
    }

    #[tokio::test]
    async fn custom_target_normalizes_bare_public_ip() {
        let target = custom_juicehost_target("1.1.1.1").await.unwrap();
        assert_eq!(target.base_url, "https://1.1.1.1");
    }

    #[tokio::test]
    async fn custom_target_accepts_root_slash_and_keeps_port() {
        let target = custom_juicehost_target(" https://1.1.1.1:8443/ ")
            .await
            .unwrap();
        assert_eq!(target.base_url, "https://1.1.1.1:8443");
    }

    #[tokio::test]
    async fn custom_target_rejects_credentials_and_extra_url_components() {
        assert!(custom_juicehost_target("https://user:pass@1.1.1.1")
            .await
            .is_err());
        assert!(custom_juicehost_target("https://1.1.1.1/api")
            .await
            .is_err());
        assert!(custom_juicehost_target("https://1.1.1.1/?next=x")
            .await
            .is_err());
        assert!(custom_juicehost_target("https://1.1.1.1/#fragment")
            .await
            .is_err());
        assert!(custom_juicehost_target("https://localhost./")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn custom_target_rejects_local_and_metadata_names() {
        assert!(custom_juicehost_target("http://localhost:6402")
            .await
            .is_err());
        assert!(custom_juicehost_target("http://service.internal")
            .await
            .is_err());
        assert!(custom_juicehost_target("http://metadata.google.internal")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn custom_target_rejects_bad_scheme() {
        assert!(custom_juicehost_target("ftp://1.1.1.1").await.is_err());
        assert!(custom_juicehost_target("file:///etc/passwd").await.is_err());
    }

    #[tokio::test]
    async fn custom_target_rejects_non_public_addresses() {
        for host in [
            "http://10.0.0.1",
            "http://192.168.1.1",
            "http://172.16.5.5",
            "http://169.254.169.254",
            "http://127.0.0.1",
            "http://224.0.0.1",
            "http://[::1]",
            "http://[fe80::1]",
            "http://[fc00::1]",
            "http://[ff02::1]",
            "http://[::ffff:127.0.0.1]",
        ] {
            assert!(
                custom_juicehost_target(host).await.is_err(),
                "accepted {host}"
            );
        }
    }
}
