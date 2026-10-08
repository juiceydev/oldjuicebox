//! handles multipart file uploads. streams non-gzip data straight to juicehost.
//! gzip uploads get buffered, decompressed, then sent. very based

use axum::{
    extract::{ConnectInfo, Multipart, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use bytes::Bytes;
use serde::Serialize;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use utoipa::ToSchema;

use crate::db::{self, FileRecord};
use crate::error::AppError;
use crate::routes::noscript;
use crate::routes::UserId;
use crate::state::AppState;
use crate::upload_mode::UploadMode;

use jsonwebtoken::{encode, EncodingKey, Header};

/// JSON response returned by a successful upload.
#[derive(Serialize, ToSchema)]
pub struct UploadResponse {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub size_bytes: i64,
    pub mime_type: String,
    pub expires_at: i64,
    pub delete_token: String,
    pub status: String,
}

/// JSON response returned when reserving an upload slot.
#[derive(Serialize, ToSchema)]
pub struct ReserveResponse {
    pub id: String,
    pub url: String,
    pub delete_token: String,
    pub status: String,
}

/// Reservation response for a browser-direct upload. The delete capability is
/// intentionally kept server-side; the browser receives only a short-lived
/// upload ticket.
#[derive(Serialize, ToSchema)]
pub struct DirectUploadReserveResponse {
    pub ticket: String,
    pub file_id: String,
    pub upload_url: String,
    pub url: String,
    pub expires_at: i64,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct DirectUploadReserveRequest {
    pub filename: String,
    pub mime_type: String,
    pub file_size: u64,
    pub ttl_hours: Option<f64>,
    pub host: Option<String>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct DirectUploadCompleteRequest {
    pub file_id: String,
    pub ticket: String,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct DirectParallelReserveRequest {
    pub filename: String,
    pub mime_type: String,
    pub file_size: u64,
    pub ttl_hours: Option<f64>,
    pub num_parts: Option<usize>,
}

/// One part of a parallel direct upload. Sizes are authoritative: the
/// browser must slice part `i` as bytes
/// `[i*step, min((i+1)*step, file_size))` with `step = ceil(file_size /
/// num_parts)`, matching `direct_part_sizes` below.
#[derive(Serialize, ToSchema)]
pub struct DirectParallelPart {
    pub part_index: usize,
    pub part_id: String,
    pub part_size: u64,
    pub ticket: String,
    pub upload_url: String,
}

/// Reservation response for a parallel browser-direct upload. The browser
/// PUTs each part straight to juicehost (no bytes via juiceback), then
/// calls `/upload/direct/parallel/complete` to concat + finalize.
#[derive(Serialize, ToSchema)]
pub struct DirectParallelReserveResponse {
    pub ticket: String,
    pub file_id: String,
    pub url: String,
    pub expires_at: i64,
    pub parts: Vec<DirectParallelPart>,
}

#[derive(serde::Deserialize, ToSchema)]
pub struct DirectParallelCompleteRequest {
    pub file_id: String,
    pub ticket: String,
    pub part_ids: Vec<String>,
}

/// Split `total` bytes into `n` parts with ceil division. The browser
/// client implements the identical formula; the reserve response carries
/// the authoritative per-part sizes.
pub(crate) fn direct_part_sizes(total: u64, n: usize) -> Vec<u64> {
    let step = total.div_ceil(n.max(1) as u64);
    (0..n)
        .map(|i| total.saturating_sub(i as u64 * step).min(step))
        .collect()
}

/// Largest single part PUT juiceback will ticket. Keeps every part request
/// comfortably under Cloudflare's proxied upload caps.
pub(crate) const DIRECT_MAX_PART_BYTES: u64 = 128 * 1024 * 1024;
/// Hard cap on parts per parallel upload (matches TUS parallel parts).
pub(crate) const DIRECT_MAX_PARTS: usize = 64;

/// Parsed multipart upload parameters, extracted from the multipart stream.
struct UploadParams {
    file_id: String,
    filename: String,
    mime_type: String,
    file_data: Option<Vec<u8>>,
    total_bytes: i64,
    selected_host: Option<String>,
    selected_upload_mode: UploadMode,
    ttl_seconds: i64,
    reservation: Option<FileRecord>,
    file_capability: String,
}

pub(crate) fn compute_ticket_ttl_secs(
    file_size_bytes: u64,
    assumed_bps: f64,
    safety_mult: f64,
    base_overhead_secs: i64,
    min_ttl: i64,
    max_ttl: i64,
) -> i64 {
    debug_assert!(assumed_bps > 0.0, "assumed_bps must be positive");
    if file_size_bytes == 0 {
        return min_ttl;
    }
    let estimated = (file_size_bytes as f64 / assumed_bps).ceil() as i64;
    let ttl = estimated * safety_mult as i64 + base_overhead_secs;
    ttl.clamp(min_ttl, max_ttl)
}

pub(crate) fn dte_ticket_ttl_secs(state: &Arc<AppState>, file_size_bytes: u64) -> i64 {
    compute_ticket_ttl_secs(
        file_size_bytes,
        state.config.dte_assumed_bps,
        state.config.dte_safety_mult,
        state.config.dte_base_overhead_secs,
        state.config.dte_min_ttl_secs,
        state.config.dte_max_ttl_secs,
    )
}

/// Normalize a client-selected storage host. Values addressing this very
/// instance (own public base / JuiceHost / backend URLs) collapse to `None`
/// (direct local routing) instead of hairpinning every byte out through the
/// public edge and back — a setting that silently turns a fast LAN/localhost
/// push into a double Cloudflare traversal and stalls big uploads.
/// Genuine third-party hosts pass through untouched.
pub(crate) fn normalize_storage_host(
    selected: Option<String>,
    self_hosts: &[&str],
) -> Option<String> {
    fn norm(s: &str) -> String {
        let s = s.trim().trim_end_matches('/');
        let s = s
            .strip_prefix("https://")
            .or_else(|| s.strip_prefix("http://"))
            .unwrap_or(s);
        s.to_ascii_lowercase()
    }
    let raw = selected.map(|h| h.trim().to_string()).filter(|h| !h.is_empty())?;
    let want = norm(&raw);
    if self_hosts
        .iter()
        .any(|c| !c.trim().is_empty() && norm(c) == want)
    {
        tracing::info!("storage host {raw} addresses this instance; routing direct");
        return None;
    }
    Some(raw)
}

/// This instance's own addresses for `normalize_storage_host`.
pub(crate) fn local_storage_hosts(state: &Arc<AppState>) -> [&str; 3] {
    [
        state.config.public_base_url.as_str(),
        state.config.public_juicehost_url.as_str(),
        state.config.juicehost_url.as_str(),
    ]
}

fn enforce_mint_limit(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    addr: SocketAddr,
) -> Result<(), AppError> {
    if !state.config.dte_enabled {
        return Ok(());
    }
    let ip = crate::utils::client_ip(headers, addr.ip(), state).to_string();
    if state.mint_limiter.allow(&ip) {
        Ok(())
    } else {
        Err(AppError::TooManyRequests(
            "Too many upload tickets minted. Please wait and try again.".into(),
        ))
    }
}

pub(crate) fn region_host(
    state: &Arc<AppState>,
    headers: &HeaderMap,
    addr: SocketAddr,
) -> Option<String> {
    if !juiceutils::proxy::is_trusted(addr.ip(), &state.config.trusted_proxy_cidrs) {
        return None;
    }
    let region = headers
        .get("x-juicebox-region")?
        .to_str()
        .ok()?
        .trim()
        .to_ascii_lowercase();
    if region.is_empty() {
        return None;
    }
    state.config.region_public_juicehosts.get(&region).cloned()
}

pub(crate) fn sanitize_filename(name: &str) -> String {
    let name = name.trim();
    if name.is_empty() {
        return "upload".to_string();
    }
    let mut result = String::with_capacity(name.len().min(crate::constants::MAX_FILENAME_LEN));
    for c in name.chars() {
        if c.is_control() || c == '/' || c == '\\' || c == '\0' {
            continue;
        }
        result.push(c);
        if result.len() > crate::constants::MAX_FILENAME_LEN {
            result.pop();
            break;
        }
    }
    let result = result.replace("..", "");
    if result.is_empty() {
        return "upload".to_string();
    }
    result
}

/// Read a gzip-encoded multipart field, decompress it, and return the raw bytes.
/// Enforces a max decompressed size to guard against gzip bombs.
async fn decompress_gzip_field(
    mut field: axum::extract::multipart::Field<'_>,
    max_size: u64,
) -> Result<(Vec<u8>, i64, std::time::Duration, std::time::Duration), AppError> {
    let read_start = std::time::Instant::now();
    let mut raw = Vec::new();
    while let Some(chunk) = field.chunk().await.map_err(|e| {
        tracing::warn!("gzip field chunk read error: {:?}", e);
        AppError::InvalidMultipart(e.body_text())
    })? {
        raw.extend_from_slice(&chunk);
        if raw.len() as i64 > max_size as i64 {
            return Err(AppError::PayloadTooLarge);
        }
    }
    let read_time = read_start.elapsed();

    let decomp_start = std::time::Instant::now();
    let max_decompressed = max_size;
    let decompressed = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, AppError> {
        use std::io::Read;
        let mut decoder = flate2::read::GzDecoder::new(raw.as_slice());
        let mut decompressed = Vec::new();
        let mut buf = [0u8; crate::constants::GZIP_READ_BUFFER_SIZE];
        loop {
            let n = decoder
                .read(&mut buf)
                .map_err(|_| AppError::GzipDecodeFailed)?;
            if n == 0 {
                break;
            }
            decompressed.extend_from_slice(&buf[..n]);
            if decompressed.len() as u64 > max_decompressed {
                return Err(AppError::PayloadTooLarge);
            }
        }
        Ok(decompressed)
    })
    .await
    .map_err(|_| AppError::TaskPanicked("decompression panicked".into()))??;
    let decomp_time = decomp_start.elapsed();

    let total_bytes = decompressed.len() as i64;

    Ok((decompressed, total_bytes, read_time, decomp_time))
}

/// Stream a multipart file field directly to juicehost via a channel.
/// Returns (total_bytes, chunk_count, backpressure_time, push_wait).
#[allow(clippy::too_many_arguments)]
async fn stream_upload_to_juicehost(
    mut field: axum::extract::multipart::Field<'_>,
    state: &Arc<AppState>,
    file_id: &str,
    filename: &str,
    mime_type: &str,
    host: &Option<String>,
    upload_mode: UploadMode,
    max_size: i64,
    capability: &str,
) -> Result<(i64, u64, std::time::Duration, std::time::Duration), AppError> {
    let (tx, rx) =
        mpsc::channel::<Result<Bytes, String>>(crate::constants::STREAM_CHANNEL_CAPACITY);

    let state_push = Arc::clone(state);
    let push_id = file_id.to_string();
    let push_filename = filename.to_string();
    let push_mime = mime_type.to_string();
    let push_host = host.clone();
    let push_mode = upload_mode;
    let push_capability = capability.to_string();

    let push_handle = tokio::spawn(async move {
        crate::juicehost::push_file_streaming(
            &state_push,
            &push_id,
            &push_filename,
            &push_mime,
            rx,
            push_host,
            &push_mode,
            Some(&push_capability),
        )
        .await
    });

    let read_start = std::time::Instant::now();
    let mut total_bytes: i64 = 0;
    let mut chunk_count: u64 = 0;
    let mut push_backpressure: std::time::Duration = Default::default();

    let first_chunk = field
        .chunk()
        .await
        .map_err(|e| {
            tracing::warn!("stream upload chunk read error: {:?}", e);
            AppError::InvalidMultipart(e.body_text())
        })?
        .unwrap_or_default();

    if !first_chunk.is_empty() {
        let level = state
            .jh_config
            .read()
            .unwrap()
            .as_ref()
            .map(|c| c.danger_level)
            .unwrap_or(crate::file_validation::ProtectionLevel::High);
        match crate::file_validation::validate_file(filename, &first_chunk, level) {
            crate::file_validation::FileValidation::Allowed => {}
            crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } => {
                return Err(AppError::BlockedFileType(
                    crate::file_validation::friendly_block_reason(tier),
                ));
            }
            crate::file_validation::FileValidation::BlockedMagic {
                description: _,
                tier,
            } => {
                return Err(AppError::BlockedFileType(
                    crate::file_validation::friendly_block_reason(tier),
                ));
            }
            crate::file_validation::FileValidation::Empty => {
                return Err(AppError::BlockedFileType(
                    "Empty files are not allowed".into(),
                ));
            }
        }
    }

    total_bytes += first_chunk.len() as i64;
    if total_bytes > max_size {
        return Err(AppError::PayloadTooLarge);
    }
    chunk_count += 1;
    let send_start = std::time::Instant::now();
    if tx.send(Ok(first_chunk)).await.is_err() {
        // Channel closed, push task panicked
        return Err(AppError::TaskPanicked(
            "streaming push task panicked".into(),
        ));
    }
    push_backpressure += send_start.elapsed();

    while let Some(chunk) = field.chunk().await.map_err(|e| {
        tracing::warn!("stream upload chunk read error: {:?}", e);
        AppError::InvalidMultipart(e.body_text())
    })? {
        total_bytes += chunk.len() as i64;
        if total_bytes > max_size {
            return Err(AppError::PayloadTooLarge);
        }
        chunk_count += 1;
        let send_start = std::time::Instant::now();
        if tx.send(Ok(chunk)).await.is_err() {
            break;
        }
        push_backpressure += send_start.elapsed();
    }
    drop(tx);
    let read_time = read_start.elapsed();

    let stream_start = std::time::Instant::now();
    push_handle
        .await
        .map_err(|_| AppError::TaskPanicked("streaming push task panicked".into()))?
        .map_err(AppError::from_juicehost_error)?;
    let push_wait = stream_start.elapsed();

    let read_bps = if read_time.as_secs_f64() > 0.0 {
        total_bytes as f64 / read_time.as_secs_f64()
    } else {
        0.0
    };
    tracing::debug!(
        "  stream: id={} bytes={} chunks={} read={:?} ({:.2} MB/s) \
         backpressure={:?} push_wait={:?}",
        file_id,
        total_bytes,
        chunk_count,
        read_time,
        read_bps / crate::constants::BYTES_PER_MIB,
        push_backpressure,
        push_wait,
    );

    Ok((total_bytes, chunk_count, push_backpressure, push_wait))
}

/// Parse multipart fields into an `UploadParams`, it streams non-gzip file data
/// directly to juicehost and returns the buffered bytes for gzip uploads.
async fn parse_multipart(
    multipart: &mut Multipart,
    state: &Arc<AppState>,
    is_gzip: bool,
    reservation_token: Option<&str>,
    default_capability: String,
) -> Result<UploadParams, AppError> {
    let (default_ttl, allowed_ttl, max_size, danger) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.max_file_size_bytes,
            jh.danger_level,
        )
    };
    let mut ttl_seconds = default_ttl as i64 * crate::constants::SECONDS_PER_HOUR;
    let mut file_id = String::new();
    let mut file_data: Option<Vec<u8>> = None;
    let mut sanitized_filename = String::new();
    let mut mime_type = String::new();
    let mut total_bytes: i64 = 0;
    let mut selected_host: Option<String> = None;
    let mut selected_upload_mode = UploadMode::Standard;
    let mut reservation = None;
    let mut file_capability = default_capability;

    while let Some(field) = multipart.next_field().await.map_err(|e| {
        tracing::warn!("multipart field parse error: {:?}", e);
        AppError::InvalidMultipart(e.body_text())
    })? {
        let name = field.name().unwrap_or("").to_string();
        let field_start = std::time::Instant::now();

        if name == "ttl_hours" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::InvalidMultipart(e.body_text()))?;
            if let Ok(val) = text.parse::<f64>() {
                ttl_seconds = (crate::constants::clamp_to_nearest(val, &allowed_ttl)
                    * crate::constants::SECONDS_PER_HOUR_F64)
                    .round() as i64;
            }
            tracing::debug!("  field ttl_hours took {:?}", field_start.elapsed());
        } else if name == "upload_mode" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::InvalidMultipart(e.body_text()))?;
            selected_upload_mode = UploadMode::from(text.trim());
            tracing::debug!("  field upload_mode took {:?}", field_start.elapsed());
        } else if name == "host" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::InvalidMultipart(e.body_text()))?;
            selected_host = normalize_storage_host(
                Some(text.trim().trim_end_matches('/').to_string()),
                &local_storage_hosts(state),
            );
            tracing::debug!("  field host took {:?}", field_start.elapsed());
        } else if name == "reserve_id" {
            let text = field
                .text()
                .await
                .map_err(|e| AppError::InvalidMultipart(e.body_text()))?;
            let text = text.trim().to_string();
            if !text.is_empty() {
                if !crate::utils::is_valid_id(&text) {
                    return Err(AppError::BadRequest("invalid reserve_id".into()));
                }
                let token = reservation_token.ok_or_else(|| {
                    AppError::Forbidden("reservation delete token required".into())
                })?;
                let lookup_id = text.clone();
                let existing = state
                    .db_call("get_reserved_file", move |db| db::get_file(db, &lookup_id))
                    .await?
                    .ok_or_else(|| AppError::BadRequest("invalid reserve_id".into()))?;
                if existing.status != "uploading" {
                    return Err(AppError::BadRequest("reservation is not uploading".into()));
                }
                if !crate::utils::constant_time_eq(&existing.delete_token, token) {
                    return Err(AppError::Forbidden(
                        "invalid reservation delete token".into(),
                    ));
                }
                file_id = text;
                file_capability = existing.delete_token.clone();
                reservation = Some(existing);
            }
            tracing::debug!("  field reserve_id took {:?}", field_start.elapsed());
        } else if name == "file" {
            let filename = field.file_name().unwrap_or("upload").to_string();
            sanitized_filename = sanitize_filename(&filename);

            if let Some(ref existing) = reservation {
                selected_host = existing.storage_host.clone();
            }

            mime_type = field
                .content_type()
                .map(|m| m.to_string())
                .unwrap_or_else(|| {
                    mime_guess::from_path(&sanitized_filename)
                        .first_or_octet_stream()
                        .to_string()
                });

            if file_id.is_empty() {
                file_id = nanoid::nanoid!(8);
            }

            if is_gzip {
                let (decompressed, bytes, read_time, decomp_time) =
                    decompress_gzip_field(field, max_size).await?;
                total_bytes = bytes;
                let gzip_bps = if (read_time + decomp_time).as_secs_f64() > 0.0 {
                    total_bytes as f64 / (read_time + decomp_time).as_secs_f64()
                } else {
                    0.0
                };
                tracing::debug!(
                    "  field file (gzip): read={:?} decompress={:?} bytes={} {:.2} MB/s",
                    read_time,
                    decomp_time,
                    total_bytes,
                    gzip_bps / crate::constants::BYTES_PER_MIB,
                );
                file_data = Some(decompressed);

                // Validate file type from decompressed data
                if let Some(ref data) = file_data {
                    match crate::file_validation::validate_file(&sanitized_filename, data, danger) {
                        crate::file_validation::FileValidation::Allowed => {}
                        crate::file_validation::FileValidation::BlockedExtension {
                            ext: _,
                            tier,
                        } => {
                            return Err(AppError::BlockedFileType(
                                crate::file_validation::friendly_block_reason(tier),
                            ));
                        }
                        crate::file_validation::FileValidation::BlockedMagic {
                            description: _,
                            tier,
                        } => {
                            return Err(AppError::BlockedFileType(
                                crate::file_validation::friendly_block_reason(tier),
                            ));
                        }
                        crate::file_validation::FileValidation::Empty => {
                            return Err(AppError::BlockedFileType(
                                "Empty files are not allowed".into(),
                            ));
                        }
                    }
                }
            } else {
                let (bytes, _chunks, _backpressure, _push_wait) = stream_upload_to_juicehost(
                    field,
                    state,
                    &file_id,
                    &sanitized_filename,
                    &mime_type,
                    &selected_host,
                    selected_upload_mode,
                    max_size as i64,
                    &file_capability,
                )
                .await?;
                total_bytes = bytes;
            }
            break;
        }
    }

    Ok(UploadParams {
        file_id,
        filename: sanitized_filename,
        mime_type,
        file_data,
        total_bytes,
        selected_host,
        selected_upload_mode,
        ttl_seconds,
        reservation,
        file_capability,
    })
}

/// Push buffered gzip data to juicehost after parsing completes.
async fn push_buffered(state: &Arc<AppState>, params: &UploadParams) -> Result<(), AppError> {
    if let Some(data) = &params.file_data {
        let push_start = std::time::Instant::now();
        crate::juicehost::push_file_to_juicehost(
            state,
            &params.file_id,
            &params.filename,
            &params.mime_type,
            data.clone(),
            params.selected_host.as_deref(),
            Some(&params.file_capability),
        )
        .await
        .map_err(AppError::from_juicehost_error)?;
        tracing::debug!(
            "push_to_juicehost (buffered) took {:?}",
            push_start.elapsed()
        );
    }
    Ok(())
}

/// JSON body for the reserve upload endpoint.
#[derive(serde::Deserialize, ToSchema)]
pub struct ReserveUploadRequest {
    pub filename: Option<String>,
    pub mime_type: Option<String>,
    pub ttl_hours: Option<f64>,
    pub host: Option<String>,
}

/// Reserve an upload slot: allocate a file ID and return the shareable URL
/// before any bytes are transferred and the upload must then be completed via
/// POST /upload with a `reserve_id` form field.
#[utoipa::path(
    post,
    path = "/upload/reserve",
    request_body = ReserveUploadRequest,
    responses(
        (status = 200, description = "Upload slot reserved", body = ReserveResponse),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn reserve_upload_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<ReserveUploadRequest>,
) -> Result<impl IntoResponse, AppError> {
    let (default_ttl, allowed_ttl, danger) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
        )
    };

    let now = chrono::Utc::now().timestamp();
    let delete_token = uuid::Uuid::new_v4().to_string();

    let mut ttl_seconds = default_ttl as i64 * crate::constants::SECONDS_PER_HOUR;
    if let Some(hours) = body.ttl_hours {
        let clamped = crate::constants::clamp_to_nearest(hours, &allowed_ttl);
        ttl_seconds = (clamped * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
    }

    let filename = body.filename.unwrap_or_else(|| "upload".to_string());
    let mime_type = body
        .mime_type
        .unwrap_or_else(|| "application/octet-stream".to_string());

    // Validate filename extension before reserving (use a dummy empty slice for magic byte check,
    // since we only have the filename at reserve time -- full validation happens on upload).
    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    let selected_host = normalize_storage_host(
        body.host
            .map(|h| h.trim().trim_end_matches('/').to_string())
            .filter(|h| !h.is_empty()),
        &local_storage_hosts(&state),
    );

    let file_id = nanoid::nanoid!(8);

    let encrypted_ip = {
        let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key)
    };

    let record = FileRecord::new(
        file_id.clone(),
        filename,
        mime_type,
        0, // size unknown at reserve time
        delete_token.clone(),
        now,
        now + ttl_seconds,
        encrypted_ip,
        selected_host.clone(),
    );

    let record = db::insert_pending_file(&state, record).await?;
    let owned_record = record.clone();
    state
        .db_call("own_reserved_file", move |db| {
            db::add_client_file(db, &user_id, &owned_record)
        })
        .await?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &file_id,
        &record.filename,
    );

    tracing::info!("reserve: id={} host={:?}", file_id, selected_host);

    Ok((
        StatusCode::OK,
        Json(ReserveResponse {
            id: file_id,
            url: public_url,
            delete_token,
            status: "uploading".to_string(),
        }),
    ))
}

/// Reserve a browser-direct upload. Juiceback handles metadata and ownership;
/// the file bytes go directly to juicehost with the returned ticket.
#[utoipa::path(
    post,
    path = "/upload/direct/reserve",
    request_body = DirectUploadReserveRequest,
    responses(
        (status = 200, description = "Direct upload ticket issued", body = DirectUploadReserveResponse),
        (status = 413, description = "File exceeds configured limit"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn direct_upload_reserve_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DirectUploadReserveRequest>,
) -> Result<Json<DirectUploadReserveResponse>, AppError> {
    let (default_ttl, allowed_ttl, danger, max_file_size, public_juicehost_url) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
            jh.max_file_size_bytes,
            state.config.public_juicehost_url.clone(),
        )
    };

    if body.file_size == 0 || body.file_size > max_file_size {
        return Err(AppError::PayloadTooLarge);
    }
    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&body.filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    // Hosts addressing this instance collapse to direct routing (see
    // normalize_storage_host); only genuine third-party hosts are rejected,
    // since cross-instance tickets would not verify there.
    let selected_host = normalize_storage_host(
        body.host
            .as_deref()
            .map(str::trim)
            .filter(|host| !host.is_empty())
            .map(str::to_string),
        &local_storage_hosts(&state),
    );
    if selected_host.is_some() {
        return Err(AppError::BadRequest(
            "direct browser upload does not support custom hosts yet".into(),
        ));
    }

    enforce_mint_limit(&state, &headers, addr)?;

    let now = chrono::Utc::now().timestamp();
    let ticket_ttl_secs = if state.config.dte_enabled {
        dte_ticket_ttl_secs(&state, body.file_size)
    } else {
        900
    };
    let ttl_seconds = body
        .ttl_hours
        .map(|hours| crate::constants::clamp_to_nearest(hours, &allowed_ttl))
        .map(|hours| (hours * crate::constants::SECONDS_PER_HOUR_F64).round() as i64)
        .unwrap_or_else(|| (default_ttl * crate::constants::SECONDS_PER_HOUR_F64).round() as i64);
    let file_id = nanoid::nanoid!(8);
    let delete_token = uuid::Uuid::new_v4().to_string();
    let encrypted_ip = {
        let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key)
    };

    let region_juicehost = region_host(&state, &headers, addr);
    let record = FileRecord::new(
        file_id.clone(),
        body.filename.clone(),
        body.mime_type.clone(),
        body.file_size as i64,
        delete_token.clone(),
        now,
        now + ttl_seconds,
        encrypted_ip,
        region_juicehost.clone(),
    );
    let record = db::insert_pending_file(&state, record).await?;
    let owned_record = record.clone();
    let owner = user_id.clone();
    state
        .db_call("own_direct_upload", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    let ticket_claims = serde_json::json!({
        "sub": "browser",
        "user_id": user_id,
        "iss": crate::auth::ISS_TICKET,
        "file_id": file_id,
        "filename": body.filename,
        "mime_type": body.mime_type,
        "file_size": body.file_size,
        "file_capability": delete_token,
        "iat": now as usize,
        "exp": (now + ticket_ttl_secs) as usize,
    });
    let ticket = encode(
        &Header::default(),
        &ticket_claims,
        &EncodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &file_id,
        &record.filename,
    );
    let upload_url = format!(
        "{}/internal/file/upload/{}",
        region_juicehost
            .as_deref()
            .unwrap_or_else(|| public_juicehost_url.trim_end_matches('/')),
        file_id
    );

    Ok(Json(DirectUploadReserveResponse {
        ticket,
        file_id,
        upload_url,
        url: public_url,
        expires_at: record.expires_at,
    }))
}

/// Complete a browser-direct upload after juicehost has stored the exact body.
/// Ownership and the delete capability are resolved server-side.
#[utoipa::path(
    post,
    path = "/upload/direct/complete",
    request_body = DirectUploadCompleteRequest,
    responses(
        (status = 200, description = "Direct upload completed", body = UploadResponse),
        (status = 401, description = "Invalid upload ticket"),
        (status = 409, description = "Upload is already complete"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn direct_upload_complete_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    Json(body): Json<DirectUploadCompleteRequest>,
) -> Result<Json<UploadResponse>, AppError> {
    use jsonwebtoken::{decode as jwt_decode, Algorithm, DecodingKey, Validation};
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);
    validation.set_issuer(&[crate::auth::ISS_TICKET]);
    let ticket = jwt_decode::<serde_json::Value>(
        &body.ticket,
        &DecodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized("Invalid upload ticket".into()))?
    .claims;
    let ticket_file_id = ticket.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
    let ticket_user_id = ticket.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
    if ticket_file_id != body.file_id || ticket_user_id != user_id {
        return Err(AppError::Unauthorized(
            "Upload ticket does not match session".into(),
        ));
    }

    let file_id = body.file_id;
    let lookup_id = file_id.clone();
    let file = state
        .db_call("get_direct_upload", move |db| db::get_file(db, &lookup_id))
        .await?
        .ok_or(AppError::NotFound)?;
    let owner_id = user_id.clone();
    let owned_id = file_id.clone();
    let owned = state
        .db_call("verify_direct_upload_owner", move |db| {
            db::client_owns_file(db, &owner_id, &owned_id)
        })
        .await?;
    if !owned {
        return Err(AppError::Unauthorized(
            "Upload is not owned by this session".into(),
        ));
    }
    if file.status != "uploading" {
        return Err(AppError::Conflict("upload is already complete".into()));
    }

    let stored_size = crate::juicehost::stat_file_on_juicehost(
        &state,
        &file_id,
        file.storage_host.as_deref(),
        Some(&file.delete_token),
    )
    .await
    .map_err(AppError::JuicehostUnreachable)?
    .ok_or_else(|| AppError::BadRequest("Uploaded file was not found on juicehost".into()))?;
    if stored_size != file.size_bytes as u64 {
        return Err(AppError::BadRequest(format!(
            "Uploaded file size mismatch: expected {} bytes, got {stored_size}",
            file.size_bytes
        )));
    }

    let filename = file.filename.clone();
    let mime_type = file.mime_type.clone();
    let size_bytes = file.size_bytes;
    let reservation_token = file.delete_token.clone();
    let update_id = file_id.clone();
    match state
        .db_call("complete_direct_upload", move |db| {
            db::complete_reservation(
                db,
                &filename,
                &mime_type,
                size_bytes,
                &update_id,
                &reservation_token,
                None,
            )
        })
        .await?
    {
        db::CompleteReservationResult::Completed(_) => {}
        db::CompleteReservationResult::NotFound => return Err(AppError::NotFound),
        db::CompleteReservationResult::NotUploading => {
            return Err(AppError::Conflict("upload is already complete".into()))
        }
        db::CompleteReservationResult::InvalidToken => {
            return Err(AppError::Unauthorized("invalid upload ownership".into()))
        }
    }

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &file.storage_host,
        &file_id,
        &file.filename,
    );
    Ok(Json(UploadResponse {
        id: file_id,
        url: public_url,
        filename: file.filename,
        size_bytes,
        mime_type: file.mime_type,
        expires_at: file.expires_at,
        delete_token: String::new(),
        status: "ready".into(),
    }))
}

/// Reserve a parallel browser-direct upload: one parent record plus a
/// per-part upload ticket per part. The browser PUTs parts straight to
/// juicehost, so no file bytes ever transit juiceback on this path.
#[utoipa::path(
    post,
    path = "/upload/direct/parallel/reserve",
    request_body = DirectParallelReserveRequest,
    responses(
        (status = 200, description = "Parallel direct upload tickets issued", body = DirectParallelReserveResponse),
        (status = 400, description = "Invalid request"),
        (status = 413, description = "File or part size exceeds limits"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn direct_parallel_reserve_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<DirectParallelReserveRequest>,
) -> Result<Json<DirectParallelReserveResponse>, AppError> {
    let (default_ttl, allowed_ttl, danger, max_file_size, public_juicehost_url) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
            jh.max_file_size_bytes,
            state.config.public_juicehost_url.clone(),
        )
    };

    if body.file_size == 0 || body.file_size > max_file_size {
        return Err(AppError::PayloadTooLarge);
    }
    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&body.filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    // 2..=64 parts, each at most DIRECT_MAX_PART_BYTES so every part PUT
    // fits proxied upload caps. Small files are rejected: use single-PUT
    // direct or TUS below the parallel threshold instead.
    let min_parts = (body.file_size.div_ceil(DIRECT_MAX_PART_BYTES) as usize).max(2);
    if min_parts > DIRECT_MAX_PARTS {
        return Err(AppError::PayloadTooLarge);
    }
    let mut num_parts = body.num_parts.unwrap_or_else(|| {
        (body.file_size.div_ceil(32 * 1024 * 1024) as usize).clamp(2, 32)
    });
    num_parts = num_parts.clamp(2, DIRECT_MAX_PARTS).max(min_parts);
    if num_parts > DIRECT_MAX_PARTS {
        return Err(AppError::PayloadTooLarge);
    }
    let part_sizes = direct_part_sizes(body.file_size, num_parts);

    enforce_mint_limit(&state, &headers, addr)?;

    let now = chrono::Utc::now().timestamp();
    let ticket_ttl_secs = if state.config.dte_enabled {
        dte_ticket_ttl_secs(&state, body.file_size)
    } else {
        900
    };
    let ttl_seconds = body
        .ttl_hours
        .map(|hours| crate::constants::clamp_to_nearest(hours, &allowed_ttl))
        .map(|hours| (hours * crate::constants::SECONDS_PER_HOUR_F64).round() as i64)
        .unwrap_or_else(|| (default_ttl * crate::constants::SECONDS_PER_HOUR_F64).round() as i64);
    let file_id = nanoid::nanoid!(8);
    let delete_token = uuid::Uuid::new_v4().to_string();
    let encrypted_ip = {
        let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key)
    };

    let region_juicehost = region_host(&state, &headers, addr);
    let record = FileRecord::new(
        file_id.clone(),
        body.filename.clone(),
        body.mime_type.clone(),
        body.file_size as i64,
        delete_token.clone(),
        now,
        now + ttl_seconds,
        encrypted_ip,
        region_juicehost.clone(),
    );
    let record = db::insert_pending_file(&state, record).await?;
    let owned_record = record.clone();
    let owner = user_id.clone();
    state
        .db_call("own_direct_parallel_upload", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    let upload_base = region_juicehost
        .as_deref()
        .unwrap_or_else(|| public_juicehost_url.trim_end_matches('/'));
    let ticket_secret = state.config.ticket_jwt_secret.clone();
    let mut parts = Vec::with_capacity(num_parts);
    for (i, part_size) in part_sizes.iter().enumerate() {
        let part_id = nanoid::nanoid!(8);
        let ticket_claims = serde_json::json!({
            "sub": "browser",
            "user_id": user_id,
            "iss": crate::auth::ISS_TICKET,
            "file_id": part_id,
            "filename": body.filename,
            "mime_type": body.mime_type,
            "file_size": part_size,
            "file_capability": delete_token,
            "iat": now as usize,
            "exp": (now + ticket_ttl_secs) as usize,
        });
        let ticket = encode(
            &Header::default(),
            &ticket_claims,
            &EncodingKey::from_secret(ticket_secret.as_bytes()),
        )
        .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;
        parts.push(DirectParallelPart {
            part_index: i,
            part_id: part_id.clone(),
            part_size: *part_size,
            ticket,
            upload_url: format!("{upload_base}/internal/file/upload/{part_id}"),
        });
    }

    // Parent ticket authorizes the complete call (same verification shape
    // as single-PUT direct uploads).
    let parent_claims = serde_json::json!({
        "sub": "browser",
        "user_id": user_id,
        "iss": crate::auth::ISS_TICKET,
        "file_id": file_id,
        "filename": body.filename,
        "mime_type": body.mime_type,
        "file_size": body.file_size,
        "file_capability": delete_token,
        "iat": now as usize,
        "exp": (now + ticket_ttl_secs) as usize,
    });
    let parent_ticket = encode(
        &Header::default(),
        &parent_claims,
        &EncodingKey::from_secret(ticket_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &file_id,
        &record.filename,
    );
    Ok(Json(DirectParallelReserveResponse {
        ticket: parent_ticket,
        file_id,
        url: public_url,
        expires_at: record.expires_at,
        parts,
    }))
}

/// Complete a parallel browser-direct upload: verify every part landed on
/// juicehost with the reserved size, concat server-side, then finalize the
/// parent record. Idempotent: a retried complete after success returns the
/// existing URL instead of conflicting.
#[utoipa::path(
    post,
    path = "/upload/direct/parallel/complete",
    request_body = DirectParallelCompleteRequest,
    responses(
        (status = 200, description = "Parallel direct upload completed", body = UploadResponse),
        (status = 401, description = "Invalid upload ticket"),
        (status = 409, description = "Parts missing or size mismatch"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn direct_parallel_complete_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    Json(body): Json<DirectParallelCompleteRequest>,
) -> Result<Json<UploadResponse>, AppError> {
    use jsonwebtoken::{decode as jwt_decode, Algorithm, DecodingKey, Validation};
    let mut validation = Validation::new(Algorithm::HS256);
    validation.set_required_spec_claims(&["exp"]);
    validation.set_issuer(&[crate::auth::ISS_TICKET]);
    let ticket = jwt_decode::<serde_json::Value>(
        &body.ticket,
        &DecodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
        &validation,
    )
    .map_err(|_| AppError::Unauthorized("Invalid upload ticket".into()))?
    .claims;
    let ticket_file_id = ticket.get("file_id").and_then(|v| v.as_str()).unwrap_or("");
    let ticket_user_id = ticket.get("user_id").and_then(|v| v.as_str()).unwrap_or("");
    if ticket_file_id != body.file_id || ticket_user_id != user_id {
        return Err(AppError::Unauthorized(
            "Upload ticket does not match session".into(),
        ));
    }

    let file_id = body.file_id;
    let lookup_id = file_id.clone();
    let file = state
        .db_call("get_direct_parallel_upload", move |db| db::get_file(db, &lookup_id))
        .await?
        .ok_or(AppError::NotFound)?;
    let owner_id = user_id.clone();
    let owned_id = file_id.clone();
    let owned = state
        .db_call("verify_direct_parallel_upload_owner", move |db| {
            db::client_owns_file(db, &owner_id, &owned_id)
        })
        .await?;
    if !owned {
        return Err(AppError::Unauthorized(
            "Upload is not owned by this session".into(),
        ));
    }

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &file.storage_host,
        &file_id,
        &file.filename,
    );
    if file.status != "uploading" {
        // Already finalized (e.g. retried complete after a network blip):
        // return the existing URL instead of failing.
        return Ok(Json(UploadResponse {
            id: file_id,
            url: public_url,
            filename: file.filename,
            size_bytes: file.size_bytes,
            mime_type: file.mime_type,
            expires_at: file.expires_at,
            delete_token: String::new(),
            status: "ready".into(),
        }));
    }

    // The client echoes the part ids it stored; sizes come from storage and
    // must sum to the reserved total with the deterministic split.
    if body.part_ids.len() < 2
        || body.part_ids.len() > DIRECT_MAX_PARTS
        || body.part_ids.iter().any(|id| !crate::utils::is_valid_id(id))
    {
        return Err(AppError::BadRequest("invalid part list".into()));
    }
    let mut seen = std::collections::HashSet::new();
    if body.part_ids.iter().any(|id| !seen.insert(id)) {
        return Err(AppError::BadRequest("duplicate part id".into()));
    }
    let expected = direct_part_sizes(file.size_bytes as u64, body.part_ids.len());
    let mut checked: Vec<String> = Vec::with_capacity(body.part_ids.len());
    for (part_id, want) in body.part_ids.iter().zip(expected.iter()) {
        let stored = crate::juicehost::stat_file_on_juicehost(
            &state,
            part_id,
            file.storage_host.as_deref(),
            Some(&file.delete_token),
        )
        .await
        .map_err(AppError::JuicehostUnreachable)?
        .ok_or_else(|| {
            AppError::Conflict(format!("part {part_id} has not been uploaded yet"))
        })?;
        if stored != *want {
            return Err(AppError::Conflict(format!(
                "part {part_id} size mismatch: expected {want} bytes, got {stored}"
            )));
        }
        checked.push(part_id.clone());
    }

    crate::juicehost::concat_files(
        &state,
        &file_id,
        &file.filename,
        &checked,
        file.storage_host.as_deref(),
        Some(&file.delete_token),
    )
    .await
    .map_err(|e| AppError::Internal(format!("concat failed: {e}")))?;

    let filename = file.filename.clone();
    let mime_type = file.mime_type.clone();
    let size_bytes = file.size_bytes;
    let reservation_token = file.delete_token.clone();
    let update_id = file_id.clone();
    match state
        .db_call("complete_direct_parallel_upload", move |db| {
            db::complete_reservation(
                db,
                &filename,
                &mime_type,
                size_bytes,
                &update_id,
                &reservation_token,
                None,
            )
        })
        .await?
    {
        db::CompleteReservationResult::Completed(_) => {}
        db::CompleteReservationResult::NotFound => return Err(AppError::NotFound),
        db::CompleteReservationResult::NotUploading => {
            return Ok(Json(UploadResponse {
                id: file_id,
                url: public_url,
                filename: file.filename,
                size_bytes,
                mime_type: file.mime_type,
                expires_at: file.expires_at,
                delete_token: String::new(),
                status: "ready".into(),
            }))
        }
        db::CompleteReservationResult::InvalidToken => {
            return Err(AppError::Unauthorized("invalid upload ownership".into()))
        }
    }

    Ok(Json(UploadResponse {
        id: file_id,
        url: public_url,
        filename: file.filename,
        size_bytes,
        mime_type: file.mime_type,
        expires_at: file.expires_at,
        delete_token: String::new(),
        status: "ready".into(),
    }))
}

/// Internal endpoint: return the upload status for a file ID.
/// Called by juicehost to check if a file is still uploading or ready.
#[utoipa::path(
    get,
    path = "/internal/file/{id}/status",
    params(
        ("id" = String, Path, description = "File ID")
    ),
    responses(
        (status = 200, description = "File status"),
        (status = 404, description = "File not found"),
    ),
    tag = "Internal",
)]
#[tracing::instrument(skip_all)]
pub async fn file_status_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    // Require juicehost API key for this internal endpoint
    if state.config.juicehost_api_key.is_empty() {
        return Err(AppError::Forbidden("API key not configured".into()));
    }
    let provided = headers
        .get("x-juicehost-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !crate::utils::constant_time_eq(provided, &state.config.juicehost_api_key) {
        return Err(AppError::Forbidden("invalid API key".into()));
    }

    let id_clone = id.clone();
    let file = state
        .db_call("get_file", move |db| db::get_file(db, &id_clone))
        .await;

    match file {
        Ok(Some(record)) => Ok(Json(serde_json::json!({
            "id": id,
            "status": record.status,
            "filename": record.filename,
            "expires_at": record.expires_at,
        }))),
        Ok(None) => Err(AppError::NotFound),
        Err(e) => Err(e),
    }
}

/// Build the upload response. json or bust unless they want a redirect.
fn build_upload_response(
    headers: &HeaderMap,
    record: &FileRecord,
    public_url: &str,
    wants_html: bool,
    ttl_seconds: i64,
    now: i64,
) -> Result<Response, AppError> {
    if wants_html {
        return Ok(noscript::upload_redirect(
            headers,
            public_url,
            &record.filename,
            &record.delete_token,
            &record.id,
            &record.mime_type,
            record.size_bytes,
            now,
            now + ttl_seconds,
        ));
    }

    let entry = noscript::FileCookieEntry {
        id: record.id.clone(),
        token: record.delete_token.clone(),
    };
    let cookie_hdr = noscript::file_cookie_header(headers, &entry);

    let mut resp = Json(UploadResponse {
        url: public_url.to_string(),
        id: record.id.clone(),
        filename: record.filename.clone(),
        size_bytes: record.size_bytes,
        mime_type: record.mime_type.clone(),
        expires_at: record.expires_at,
        delete_token: record.delete_token.clone(),
        status: record.status.clone(),
    })
    .into_response();

    resp.headers_mut().insert(header::SET_COOKIE, cookie_hdr);
    Ok(resp)
}

#[utoipa::path(
    post,
    path = "/upload",
    request_body(content_type = "multipart/form-data", description = "File upload with optional metadata fields (host, upload_mode, ttl_hours). Completing reserve_id requires x-delete-token."),
    responses(
        (status = 200, description = "File uploaded successfully", body = UploadResponse),
        (status = 413, description = "File too large"),
        (status = 403, description = "Missing or invalid reservation delete token"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn upload_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> Result<Response, AppError> {
    let handler_start = std::time::Instant::now();
    let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
    let encrypted_ip = crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key);
    let hashed_ip = crate::utils::hash_ip_for_ban(&raw_ip, &state.config.ip_pepper);

    // Acquire a semaphore permit to limit concurrent direct uploads
    let _permit = state
        .upload_semaphore
        .acquire()
        .await
        .map_err(|_| AppError::TaskPanicked("upload semaphore closed".into()))?;

    let wants_html = noscript::wants_html(&headers);
    let is_gzip = headers.get("x-file-encoding").and_then(|v| v.to_str().ok()) == Some("gzip");

    let parse_start = std::time::Instant::now();
    let reservation_token = headers
        .get("x-delete-token")
        .and_then(|value| value.to_str().ok());
    let fresh_capability = uuid::Uuid::new_v4().to_string();
    let params = parse_multipart(
        &mut multipart,
        &state,
        is_gzip,
        reservation_token,
        fresh_capability,
    )
    .await?;
    tracing::debug!("multipart parse total took {:?}", parse_start.elapsed());

    let now = chrono::Utc::now().timestamp();

    let record = if let Some(ref existing) = params.reservation {
        push_buffered(&state, &params).await?;

        let update_id = existing.id.clone();
        let delete_token = existing.delete_token.clone();
        let fname = params.filename.clone();
        let mtype = params.mime_type.clone();
        let enc_ip = encrypted_ip.clone();
        let completed = match state
            .db_call("complete_file", move |db| {
                crate::db::complete_reservation(
                    db,
                    &fname,
                    &mtype,
                    params.total_bytes,
                    &update_id,
                    &delete_token,
                    enc_ip.as_deref(),
                )
            })
            .await?
        {
            db::CompleteReservationResult::Completed(record) => record,
            db::CompleteReservationResult::NotFound => {
                return Err(AppError::BadRequest("invalid reserve_id".into()))
            }
            db::CompleteReservationResult::NotUploading => {
                return Err(AppError::BadRequest("reservation is not uploading".into()))
            }
            db::CompleteReservationResult::InvalidToken => {
                return Err(AppError::Forbidden(
                    "invalid reservation delete token".into(),
                ))
            }
        };

        tracing::info!(
            "reserved upload completed: id={} size={}",
            existing.id,
            params.total_bytes
        );
        completed
    } else {
        // Fresh upload (no reservation)
        let record = FileRecord::new(
            params.file_id.clone(),
            params.filename.clone(),
            params.mime_type.clone(),
            params.total_bytes,
            params.file_capability.clone(),
            now,
            now + params.ttl_seconds,
            encrypted_ip.clone(),
            params.selected_host.clone(),
        );

        push_buffered(&state, &params).await?;

        let db_start = std::time::Instant::now();
        let record = db::insert_new_file(&state, record).await?;
        tracing::debug!("db insert took {:?}", db_start.elapsed());
        Box::new(record)
    };

    let owned_record = record.clone();
    state
        .db_call("own_uploaded_file", move |db| {
            db::add_client_file(db, &user_id, &owned_record)
        })
        .await?;

    tracing::info!(
        "upload: id={} size={} mime={} expires_at={} ip={}",
        record.id,
        record.size_bytes,
        record.mime_type,
        record.expires_at,
        crate::utils::truncate_hash(&hashed_ip)
    );

    if params.selected_upload_mode == UploadMode::Quic {
        crate::utils::log_quic_throughput(
            &record.id,
            record.size_bytes as u64,
            handler_start.elapsed(),
            parse_start.elapsed(),
        );
    }

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &record.id,
        &record.filename,
    );

    build_upload_response(
        &headers,
        &record,
        &public_url,
        wants_html,
        params.ttl_seconds,
        now,
    )
}

/// JSON body for the UltraFast reserve endpoint.
#[derive(serde::Deserialize, ToSchema)]
pub struct UltrafastReserveRequest {
    pub filename: String,
    pub mime_type: String,
    pub file_size: u64,
    pub ttl_hours: Option<f64>,
    pub host: Option<String>,
}

/// JSON response for UltraFast reserve.
#[derive(Serialize, ToSchema)]
pub struct UltrafastReserveResponse {
    pub ticket: String,
    pub file_id: String,
    pub url: String,
    pub delete_token: String,
    pub juicehost_url: String,
}

/// Reserve an ultrafast slot and get back a JWT ticket for direct upload
#[utoipa::path(
    post,
    path = "/upload/ultrafast/reserve",
    request_body = UltrafastReserveRequest,
    responses(
        (status = 200, description = "Ticket issued", body = UltrafastReserveResponse),
        (status = 401, description = "Cloudflare Access auth required"),
        (status = 403, description = "No connected devices"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn ultrafast_reserve_handler(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    UserId(user_id): UserId,
    Json(body): Json<UltrafastReserveRequest>,
) -> Result<Json<UltrafastReserveResponse>, AppError> {
    let has_device = state
        .connected_devices
        .get(&user_id)
        .map(|d| !d.value().is_empty())
        .unwrap_or(false);
    if !has_device {
        return Err(AppError::BadRequest(
            "No connected devices. Pair juicebox-plus first.".into(),
        ));
    }

    let (default_ttl, allowed_ttl, danger, max_file_size) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
            jh.max_file_size_bytes,
        )
    };
    if body.file_size == 0 || body.file_size > max_file_size {
        return Err(AppError::PayloadTooLarge);
    }

    enforce_mint_limit(&state, &headers, addr)?;

    let now = chrono::Utc::now().timestamp();
    let ticket_ttl_secs = if state.config.dte_enabled {
        dte_ticket_ttl_secs(&state, body.file_size)
    } else {
        300
    };
    let delete_token = uuid::Uuid::new_v4().to_string();

    let mut ttl_seconds = default_ttl as i64 * crate::constants::SECONDS_PER_HOUR;
    if let Some(hours) = body.ttl_hours {
        let clamped = crate::constants::clamp_to_nearest(hours, &allowed_ttl);
        ttl_seconds = (clamped * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
    }

    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&body.filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    let file_id = nanoid::nanoid!(8);

    // Clone body fields for later use in upload_request
    let body_clone_filename = body.filename.clone();
    let body_clone_mime = body.mime_type.clone();
    let body_clone_size = body.file_size;

    let encrypted_ip = {
        let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key)
    };

    let record = FileRecord::new(
        file_id.clone(),
        body.filename.clone(),
        body.mime_type.clone(),
        body.file_size as i64,
        delete_token.clone(),
        now,
        now + ttl_seconds,
        encrypted_ip,
        None,
    );

    let record = db::insert_pending_file(&state, record).await?;
    let owned_record = record.clone();
    let owner = user_id.clone();
    state
        .db_call("own_ultrafast_reservation", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    // Pick a connected device to embed in the ticket
    let device_id = {
        let guard = state
            .connected_devices
            .get(&user_id)
            .ok_or_else(|| AppError::BadRequest("No connected devices".into()))?;
        let first = guard
            .value()
            .first()
            .ok_or_else(|| AppError::BadRequest("No connected devices".into()))?;
        let locked = first
            .try_lock()
            .map_err(|_| AppError::Internal("Device lock poisoned".into()))?;
        locked.device_id.clone()
    };

    // Sign ticket JWT
    let ticket_claims = serde_json::json!({
        "sub": device_id,
        "user_id": user_id,
        "iss": crate::auth::ISS_TICKET,
        "file_id": file_id,
        "filename": body.filename,
        "mime_type": body.mime_type,
        "file_size": body.file_size,
        "file_capability": delete_token,
        "iat": now as usize,
        "exp": (now + ticket_ttl_secs) as usize,
    });

    let ticket = encode(
        &Header::default(),
        &ticket_claims,
        &EncodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &file_id,
        &record.filename,
    );

    tracing::info!("ultrafast reserve: id={} user={}", file_id, user_id);

    let upload_request = serde_json::json!({
        "type": "upload_request",
        "file_id": file_id,
        "filename": body_clone_filename,
        "mime_type": body_clone_mime,
        "file_size": body_clone_size,
        "ticket": ticket.clone(),
        "juicehost_url": state.config.public_juicehost_url,
        "juiceback_url": state.config.public_base_url,
        "download_url": public_url,
    });

    {
        let guard = state
            .connected_devices
            .get(&user_id)
            .ok_or_else(|| AppError::BadRequest("No connected devices".into()))?;
        let first = guard
            .value()
            .first()
            .ok_or_else(|| AppError::BadRequest("No connected devices".into()))?;
        let locked = first
            .try_lock()
            .map_err(|_| AppError::Internal("Device lock poisoned".into()))?;
        let _ = locked.sender.send(upload_request.to_string()).await;
    }

    Ok(Json(UltrafastReserveResponse {
        ticket,
        file_id,
        url: public_url,
        delete_token,
        juicehost_url: state.config.public_juicehost_url.clone(),
    }))
}

/// JSON body for the UltraFast complete endpoint.
#[derive(serde::Deserialize, ToSchema)]
pub struct UltrafastCompleteRequest {
    pub file_id: String,
    /// Real filename from the device (may differ from the placeholder used during reserve).
    #[serde(default)]
    pub filename: Option<String>,
    /// MIME type from the device.
    #[serde(default)]
    pub mime_type: Option<String>,
    /// File size in bytes from the device.
    #[serde(default)]
    pub file_size: Option<i64>,
    /// JWT ticket that proves the device is authorized for ultrafast
    #[serde(default)]
    pub ticket: Option<String>,
    /// Reservation capability returned by the reserve endpoint.
    #[serde(default)]
    pub delete_token: Option<String>,
}

/// `POST /api/device/upload` Same thing as upload but JWT auth is required (for juicebox-plus it doesn't use standard auth).
#[utoipa::path(
    post,
    path = "/api/device/upload",
    request_body = UltrafastReserveRequest,
    responses(
        (status = 200, description = "Ticket issued", body = UltrafastReserveResponse),
        (status = 401, description = "Device JWT required"),
        (status = 403, description = "Missing or invalid reservation delete token"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn device_upload_handler(
    State(state): State<Arc<AppState>>,
    crate::auth::DeviceAuth(claims): crate::auth::DeviceAuth,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(body): Json<UltrafastReserveRequest>,
) -> Result<Json<UltrafastReserveResponse>, AppError> {
    let device_id = claims.sub;
    let user_id = claims.user_id;

    let (default_ttl, allowed_ttl, danger, max_file_size) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
            jh.max_file_size_bytes,
        )
    };
    if body.file_size == 0 || body.file_size > max_file_size {
        return Err(AppError::PayloadTooLarge);
    }

    let now = chrono::Utc::now().timestamp();
    let delete_token = uuid::Uuid::new_v4().to_string();

    let mut ttl_seconds = default_ttl as i64 * crate::constants::SECONDS_PER_HOUR;
    if let Some(hours) = body.ttl_hours {
        let clamped = crate::constants::clamp_to_nearest(hours, &allowed_ttl);
        ttl_seconds = (clamped * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
    }

    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&body.filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    let file_id = nanoid::nanoid!(8);

    let selected_host = body
        .host
        .as_ref()
        .map(|h| h.trim().trim_end_matches('/').to_string())
        .filter(|h| !h.is_empty());

    let encrypted_ip = {
        let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key)
    };

    let record = FileRecord::new(
        file_id.clone(),
        body.filename.clone(),
        body.mime_type.clone(),
        body.file_size as i64,
        delete_token.clone(),
        now,
        now + ttl_seconds,
        encrypted_ip,
        selected_host.clone(),
    );

    let record = db::insert_pending_file(&state, record).await?;

    let owned_record = record.clone();
    let owner = user_id.clone();
    state
        .db_call("own_device_reservation", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    let ticket_claims = serde_json::json!({
        "sub": device_id,
        "user_id": user_id,
        "iss": crate::auth::ISS_TICKET,
        "file_id": file_id,
        "filename": body.filename,
        "mime_type": body.mime_type,
        "file_size": body.file_size,
        "file_capability": delete_token,
        "iat": now as usize,
        "exp": (now + 300) as usize,
    });

    let ticket = encode(
        &Header::default(),
        &ticket_claims,
        &EncodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
    )
    .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &selected_host,
        &file_id,
        &record.filename,
    );

    tracing::info!("device upload reserve: id={} device={}", file_id, device_id);

    Ok(Json(UltrafastReserveResponse {
        ticket,
        file_id,
        url: public_url,
        delete_token,
        juicehost_url: state.config.public_juicehost_url.clone(),
    }))
}

/// `POST /api/upload/ultrafast/complete` - Device JWT auth required.
///
/// Called by juicebox-plus after uploading a file to juicehost to mark
/// the file as ready.
#[utoipa::path(
    post,
    path = "/upload/ultrafast/complete",
    request_body = UltrafastCompleteRequest,
    responses(
        (status = 200, description = "File marked ready", body = serde_json::Value),
        (status = 401, description = "Device JWT required"),
        (status = 404, description = "File not found"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn ultrafast_complete_handler(
    State(state): State<Arc<AppState>>,
    crate::auth::DeviceAuth(claims): crate::auth::DeviceAuth,
    Json(body): Json<UltrafastCompleteRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    let device_id = claims.sub.clone();
    let device_user_id = claims.user_id.clone();

    // Validate ticket JWT for ownership check (proves this device was authorized for this file).
    if let Some(ref ticket_str) = body.ticket {
        use jsonwebtoken::{decode as jwt_decode, Algorithm, DecodingKey, Validation};

        let mut ticket_validation = Validation::new(Algorithm::HS256);
        ticket_validation.set_required_spec_claims(&["exp"]);
        ticket_validation.set_issuer(&[crate::auth::ISS_TICKET]);

        let ticket = jwt_decode::<serde_json::Value>(
            ticket_str,
            &DecodingKey::from_secret(state.config.ticket_jwt_secret.as_bytes()),
            &ticket_validation,
        )
        .map_err(|e| {
            tracing::warn!("ticket JWT validation failed in complete: {e}");
            AppError::Unauthorized("Invalid upload ticket".into())
        })?;

        let ticket_file_id = ticket
            .claims
            .get("file_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let ticket_user_id = ticket
            .claims
            .get("user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if ticket_file_id != body.file_id {
            tracing::warn!(
                "ticket file_id mismatch in complete: ticket={ticket_file_id} body={}",
                body.file_id
            );
            return Err(AppError::Unauthorized("Ticket file_id mismatch".into()));
        }
        if ticket_user_id != device_user_id {
            tracing::warn!(
                "ticket user_id mismatch in complete: ticket={ticket_user_id} device={device_user_id}"
            );
            return Err(AppError::Unauthorized("Ticket user_id mismatch".into()));
        }
    }

    let file_id = body.file_id;
    let reservation_token = body
        .delete_token
        .as_deref()
        .ok_or_else(|| AppError::Forbidden("reservation delete token required".into()))?;

    // Look up file record for storage_host, delete_token, and expires_at
    let fid = file_id.clone();
    let file = state
        .db_call("get_file_for_complete", move |db| db::get_file(db, &fid))
        .await?
        .ok_or_else(|| AppError::NotFound)?;

    // Prefer body metadata (real values from the device) over JWT claims (which may be placeholders from reserve).
    let real_filename = body
        .filename
        .or(claims.filename)
        .unwrap_or_else(|| file.filename.clone());
    let real_mime = body
        .mime_type
        .or(claims.mime_type)
        .unwrap_or_else(|| file.mime_type.clone());
    let real_size = body
        .file_size
        .or(claims.file_size.map(|s| s as i64))
        .unwrap_or(file.size_bytes);

    // Re-validate the real filename (device-provided, may differ from the reserve
    // placeholder) against the configured danger level before marking ready.
    let danger = state
        .jh_config
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.danger_level)
        .unwrap_or(crate::file_validation::ProtectionLevel::High);
    if danger != crate::file_validation::ProtectionLevel::None {
        if let crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } =
            crate::file_validation::validate_filename(&real_filename, danger)
        {
            return Err(AppError::BlockedFileType(
                crate::file_validation::friendly_block_reason(tier),
            ));
        }
    }

    // Verify the bytes actually landed on juicehost before marking ready.
    // The device uploads to `public_juicehost_url`, which routes to the same
    // juicehost instance that juiceback talks to via `juicehost_url`, so a stat
    // here reflects what the device uploaded.
    match crate::juicehost::stat_file_on_juicehost(
        &state,
        &file_id,
        file.storage_host.as_deref(),
        Some(&file.delete_token),
    )
    .await
    {
        Ok(Some(stored_size)) if stored_size == real_size as u64 => {}
        Ok(Some(stored_size)) => {
            tracing::warn!(
                "ultrafast complete size mismatch: id={} device={} stored={}",
                file_id,
                real_size,
                stored_size
            );
            return Err(AppError::BadRequest(format!(
                "Uploaded file size mismatch: device reported {real_size} bytes but juicehost stored {stored_size}"
            )));
        }
        Ok(None) => {
            tracing::warn!("ultrafast complete: id={} not found on juicehost", file_id);
            return Err(AppError::BadRequest(
                "Uploaded file was not found on juicehost".into(),
            ));
        }
        Err(e) => {
            tracing::warn!("ultrafast complete: id={} stat failed: {}", file_id, e);
            return Err(AppError::JuicehostUnreachable(format!(
                "Could not verify upload on juicehost: {e}"
            )));
        }
    }

    let update_id = file_id.clone();
    let reservation_token = reservation_token.to_string();
    let ufname = real_filename.clone();
    let umime = real_mime.clone();
    let usize = real_size;
    match state
        .db_call("complete_ultrafast", move |db| {
            crate::db::complete_reservation(
                db,
                &ufname,
                &umime,
                usize,
                &update_id,
                &reservation_token,
                None,
            )
        })
        .await?
    {
        db::CompleteReservationResult::Completed(_) => {}
        db::CompleteReservationResult::NotFound => return Err(AppError::NotFound),
        db::CompleteReservationResult::NotUploading => {
            return Err(AppError::BadRequest("reservation is not uploading".into()))
        }
        db::CompleteReservationResult::InvalidToken => {
            return Err(AppError::Forbidden(
                "invalid reservation delete token".into(),
            ))
        }
    }

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &file.storage_host,
        &file_id,
        &real_filename,
    );

    tracing::info!("ultrafast complete: id={} device={}", file_id, device_id);

    Ok(Json(serde_json::json!({
        "url": public_url,
        "id": file_id,
        "filename": real_filename,
        "mime_type": real_mime,
        "size_bytes": real_size,
        "status": "ready",
        "delete_token": file.delete_token,
        "expires_at": file.expires_at,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_empty_string() {
        assert_eq!(sanitize_filename(""), "upload");
    }

    #[test]
    fn sanitize_whitespace_only() {
        assert_eq!(sanitize_filename("   "), "upload");
    }

    #[test]
    fn sanitize_control_chars() {
        let result = sanitize_filename("hello\x00world\x01test");
        assert_eq!(result, "helloworldtest");
    }

    #[test]
    fn sanitize_slashes() {
        let result = sanitize_filename("path/to/file.txt");
        assert_eq!(result, "pathtofile.txt");
    }

    #[test]
    fn sanitize_backslashes() {
        let result = sanitize_filename("path\\to\\file.txt");
        assert_eq!(result, "pathtofile.txt");
    }

    #[test]
    fn sanitize_dotdot_removed() {
        let result = sanitize_filename("file..name.txt");
        assert_eq!(result, "filename.txt");
    }

    #[test]
    fn sanitize_normal_filename() {
        assert_eq!(sanitize_filename("photo.jpg"), "photo.jpg");
    }

    #[test]
    fn sanitize_long_filename_truncated() {
        let long_name = "a".repeat(300);
        let result = sanitize_filename(&long_name);
        assert!(result.len() <= crate::constants::MAX_FILENAME_LEN);
    }

    #[test]
    fn sanitize_path_separators_only() {
        assert_eq!(sanitize_filename("/\\\\\0"), "upload");
    }

    fn compute(size: u64) -> i64 {
        compute_ticket_ttl_secs(
            size,
            crate::constants::DTE_ASSUMED_BPS,
            crate::constants::DTE_SAFETY_MULT,
            crate::constants::DTE_BASE_OVERHEAD_SECS,
            crate::constants::DTE_MIN_TTL_SECS,
            crate::constants::DTE_MAX_TTL_SECS,
        )
    }

    #[test]
    fn ttl_min_clamp_for_tiny_file() {
        let ttl = compute(1024 * 1024);
        assert!(ttl >= crate::constants::DTE_MIN_TTL_SECS, "ttl={ttl}");
    }

    #[test]
    fn ttl_small_file_at_least_ten_minutes() {
        let ttl = compute(1_000_000);
        assert!(ttl >= 600, "ttl={ttl}");
    }

    #[test]
    fn ttl_mid_size_in_range() {
        let ttl = compute(4 * 1024 * 1024 * 1024);
        assert!(ttl > 600 && ttl < 21_600, "ttl={ttl}");
    }

    #[test]
    fn ttl_large_file_clamped_to_max() {
        let ttl = compute(u64::MAX);
        assert_eq!(ttl, crate::constants::DTE_MAX_TTL_SECS);
    }

    #[test]
    fn ttl_max_clamp_for_very_large_file() {
        let ttl = compute(100 * 1024 * 1024 * 1024);
        assert_eq!(ttl, crate::constants::DTE_MAX_TTL_SECS);
    }

    #[test]
    fn ttl_is_monotonic_in_size() {
        let mut prev = 0_i64;
        for size in [1, 1024, 1_000_000, 10_000_000, 100_000_000, 1_000_000_000, u64::MAX] {
            let ttl = compute(size);
            assert!(ttl >= prev, "ttl regressed: {ttl} < {prev} at size {size}");
            prev = ttl;
        }
    }

    #[test]
    fn ttl_zero_size_returns_min() {
        assert_eq!(compute(0), crate::constants::DTE_MIN_TTL_SECS);
    }

    #[test]
    fn direct_part_sizes_match_ceil_split_and_sum_to_total() {
        // Even split.
        assert_eq!(direct_part_sizes(100, 4), vec![25, 25, 25, 25]);
        // Ceil division: first parts take the step, the last takes less.
        assert_eq!(direct_part_sizes(10, 3), vec![4, 4, 2]);
        assert_eq!(direct_part_sizes(72_726_040, 2), vec![36_363_020, 36_363_020]);
        assert_eq!(
            direct_part_sizes(72_726_041, 2),
            vec![36_363_021, 36_363_020]
        );
        // Single part and degenerate inputs.
        assert_eq!(direct_part_sizes(7, 1), vec![7]);
        // Browser must slice identically: cumulative offsets cover everything.
        for (total, n) in [(16_777_216, 2), (100_000_000, 7), (5_368_709_120, 41)] {
            let sizes = direct_part_sizes(total, n);
            assert_eq!(sizes.len(), n);
            assert_eq!(sizes.iter().sum::<u64>(), total);
            assert!(sizes.iter().all(|&s| s > 0));
            let step = total.div_ceil(n as u64);
            assert!(sizes.iter().all(|&s| s <= step));
        }
    }

    #[test]
    fn direct_parts_fit_proxied_upload_caps() {
        // Worst case under the 5 GiB max file size: 64 parts stay small.
        let sizes = direct_part_sizes(5_368_709_120, 64);
        assert!(sizes
            .iter()
            .all(|&s| s <= DIRECT_MAX_PART_BYTES));
    }

    #[test]
    fn normalize_storage_host_collapses_self_hosts_to_direct() {
        let local = [
            "https://f.juicey.dev",
            "https://box.juicey.dev",
            "http://127.0.0.1:6402",
        ];
        // Exact, case, scheme, and slash variants of self all route direct.
        for host in [
            "https://f.juicey.dev",
            "https://f.juicey.dev/",
            "f.juicey.dev",
            "F.JUICEY.DEV",
            "http://f.juicey.dev",
            "https://box.juicey.dev",
            "http://127.0.0.1:6402/",
        ] {
            assert_eq!(
                normalize_storage_host(Some(host.into()), &local),
                None,
                "self host {host} must collapse"
            );
        }
        // Empty stays empty.
        assert_eq!(normalize_storage_host(None, &local), None);
        assert_eq!(normalize_storage_host(Some("   ".into()), &local), None);
        // Genuine third-party hosts pass through untouched.
        assert_eq!(
            normalize_storage_host(Some("https://f-us.juicey.dev".into()), &local),
            Some("https://f-us.juicey.dev".into())
        );
        assert_eq!(
            normalize_storage_host(Some("custom.example.com".into()), &local),
            Some("custom.example.com".into())
        );
    }
}
