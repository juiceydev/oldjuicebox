//! TUS resumable upload protocol endpoints.

use axum::{
    body::Body,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing, Json,
};
use base64::Engine;
use bytes::Bytes;
use dashmap::mapref::entry::Entry;
use futures::StreamExt as _;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;

use crate::db::{self, FileRecord};
use crate::error::AppError;
use crate::state::AppState;
use crate::tus::TusUpload;
use crate::upload_mode::UploadMode;

/// Live tail of a streaming TUS PATCH request body.
type PatchBodyStream =
    std::pin::Pin<Box<dyn futures::Stream<Item = Result<Bytes, axum::Error>> + Send>>;

/// Snapshot of a TUS upload's metadata, taken when the upload completes.
struct TusUploadMeta {
    id: String,
    filename: String,
    mime_type: String,
    total_length: u64,
    delete_token: String,
    encrypted_ip: String,
    ttl_hours: f64,
    storage_host: Option<String>,
    reserve_id: Option<String>,
    reservation_token: Option<String>,
    user_id: String,
}

fn validate_parallel_metadata(
    session_id: Option<&str>,
    part_index: Option<usize>,
    total_parts: Option<usize>,
) -> Result<Option<(&str, usize, usize)>, AppError> {
    match (session_id, part_index, total_parts) {
        (None, None, None) => Ok(None),
        (Some(sid), Some(index), Some(total))
            if !sid.is_empty()
                && sid.len() <= 128
                && total > 0
                && total <= crate::constants::MAX_TUS_PARALLEL_PARTS
                && index < total =>
        {
            Ok(Some((sid, index, total)))
        }
        _ => Err(AppError::BadRequest(
            "invalid parallel upload metadata".into(),
        )),
    }
}

fn parse_parallel_number(value: Option<&str>) -> Result<Option<usize>, AppError> {
    value
        .map(|value| {
            value
                .parse::<usize>()
                .map_err(|_| AppError::BadRequest("invalid parallel upload metadata".into()))
        })
        .transpose()
}

fn checked_upload_offset(offset: u64, chunk_len: u64, total: u64) -> Result<u64, AppError> {
    let new_offset = offset
        .checked_add(chunk_len)
        .ok_or(AppError::PayloadTooLarge)?;
    if new_offset > total {
        return Err(AppError::PayloadTooLarge);
    }
    Ok(new_offset)
}

fn parse_tus_metadata(header: Option<&axum::http::HeaderValue>) -> Vec<(String, String)> {
    let raw = match header.and_then(|v| v.to_str().ok()) {
        Some(v) => v,
        None => return vec![],
    };
    let mut pairs = Vec::new();
    for pair in raw.split(',') {
        let pair = pair.trim();
        if let Some(eq) = pair.find(' ') {
            let key = pair[..eq].to_string();
            let value_b64 = pair[eq + 1..].trim();
            if let Ok(decoded) =
                base64::engine::general_purpose::STANDARD.decode(value_b64.as_bytes())
            {
                if let Ok(value) = String::from_utf8(decoded) {
                    pairs.push((key, value));
                }
            }
        }
    }
    pairs
}

fn find_meta<'a>(metadata: &'a [(String, String)], key: &str) -> Option<&'a str> {
    metadata
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

#[utoipa::path(
    post,
    path = "/api/tus",
    request_body(content_type = "application/json", description = "Empty body. File metadata is sent via headers: Upload-Length, Upload-Metadata (base64-encoded pairs)"),
    responses(
        (status = 201, description = "TUS upload session created", headers(
            ("Location" = String, description = "URL of the created upload resource"),
            ("Tus-Resumable" = String, description = "TUS protocol version"),
        )),
        (status = 413, description = "File too large"),
        (status = 429, description = "Rate limit exceeded or too many TUS sessions"),
    ),
    tag = "Uploads",
)]
#[tracing::instrument(skip_all)]
pub async fn create_upload_handler(
    State(state): State<Arc<AppState>>,
    crate::routes::UserId(user_id): crate::routes::UserId,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Result<Response, AppError> {
    let raw_ip = crate::utils::client_ip(&headers, addr.ip(), &state).to_string();
    let hashed_ip = crate::utils::hash_ip_for_ban(&raw_ip, &state.config.ip_pepper);
    let encrypted_ip =
        crate::utils::encrypt_ip(&raw_ip, &state.config.ip_encryption_key).unwrap_or_default();

    let total_length = headers
        .get("upload-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| AppError::TusMissingLength)?;

    let (max_size, default_ttl, allowed_ttl, _danger) = {
        let guard = state.jh_config.read().unwrap();
        let jh = guard.as_ref().ok_or_else(|| {
            AppError::ServiceUnavailable("juicehost config unavailable (degraded mode)".into())
        })?;
        (
            jh.max_file_size_bytes,
            jh.default_ttl_hours,
            jh.allowed_ttl_hours.clone(),
            jh.danger_level,
        )
    };

    if total_length > max_size {
        return Err(AppError::PayloadTooLarge);
    }

    if state.tus.len() >= crate::constants::MAX_TUS_SESSIONS {
        return Err(AppError::RateLimited);
    }

    let ip_sessions = state
        .tus
        .iter()
        .filter(|entry| entry.hashed_ip == hashed_ip)
        .count();
    if ip_sessions >= crate::constants::MAX_TUS_PER_IP {
        tracing::warn!(
            "tus: ip={} hit per-IP session limit ({})",
            crate::utils::truncate_hash(&hashed_ip),
            crate::constants::MAX_TUS_PER_IP
        );
        return Err(AppError::RateLimited);
    }

    let metadata = parse_tus_metadata(headers.get("upload-metadata"));
    let filename = find_meta(&metadata, "filename")
        .unwrap_or("upload")
        .to_string();
    let mime_type = find_meta(&metadata, "mimetype")
        .unwrap_or("application/octet-stream")
        .to_string();
    let ttl_hours = find_meta(&metadata, "ttl")
        .and_then(|v| v.parse::<f64>().ok())
        .unwrap_or(default_ttl);
    let ttl_hours = crate::constants::clamp_to_nearest(ttl_hours, &allowed_ttl);
    let mut storage_host = crate::routes::upload::normalize_storage_host(
        find_meta(&metadata, "host")
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string()),
        &crate::routes::upload::local_storage_hosts(&state),
    );
    let storage_upload_mode = find_meta(&metadata, "upload_mode")
        .map(UploadMode::from)
        .unwrap_or(UploadMode::Standard);

    // Parallel upload session metadata (optional).
    let session_id = find_meta(&metadata, "session_id").map(|v| v.to_string());
    let part_index = parse_parallel_number(find_meta(&metadata, "part_index"))?;
    let total_parts = parse_parallel_number(find_meta(&metadata, "total_parts"))?;
    let parallel = validate_parallel_metadata(session_id.as_deref(), part_index, total_parts)?;

    // Quick Link reservation ID (optional).
    let reserve_id = find_meta(&metadata, "reserve_id")
        .filter(|v| !v.is_empty())
        .map(|value| {
            if crate::utils::is_valid_id(value) {
                Ok(value.to_string())
            } else {
                Err(AppError::BadRequest("invalid reserve_id".into()))
            }
        })
        .transpose()?;
    let reservation_token = find_meta(&metadata, "delete_token")
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    if let Some(ref reserve_id) = reserve_id {
        let token = reservation_token
            .as_deref()
            .ok_or_else(|| AppError::Forbidden("reservation delete token required".into()))?;
        let lookup_id = reserve_id.clone();
        let reservation = state
            .db_call("get_tus_reservation", move |db| {
                db::get_file(db, &lookup_id)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("invalid reserve_id".into()))?;
        if reservation.status != "uploading" {
            return Err(AppError::BadRequest("reservation is not uploading".into()));
        }
        if !crate::utils::constant_time_eq(&reservation.delete_token, token) {
            return Err(AppError::Forbidden(
                "invalid reservation delete token".into(),
            ));
        }
        storage_host = reservation.storage_host;
    }

    if storage_host.is_none() {
        storage_host = crate::routes::upload::region_host(&state, &headers, addr);
    }

    let id = nanoid::nanoid!(8);
    let mut file_capability = reservation_token
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    if let Some((sid, pi, tp)) = parallel {
        let session = state
            .part_sessions
            .entry(sid.to_string())
            .or_insert_with(|| {
                Arc::new(crate::tus::PartSession {
                    session_id: sid.to_string(),
                    total_parts: tp,
                    filename: filename.clone(),
                    mime_type: mime_type.clone(),
                    hashed_ip: hashed_ip.clone(),
                    storage_host: storage_host.clone(),
                    ttl_hours,
                    reserve_id: reserve_id.clone(),
                    reservation_token: reservation_token.clone(),
                    capability: file_capability.clone(),
                    user_id: user_id.clone(),
                    upload_mode: storage_upload_mode,
                    declared_size: std::sync::atomic::AtomicU64::new(0),
                    completed: std::sync::atomic::AtomicUsize::new(0),
                    part_ids: dashmap::DashMap::new(),
                    part_lengths: dashmap::DashMap::new(),
                    completion_reserve_id: std::sync::Mutex::new(None),
                })
            });
        if session.total_parts != tp
            || session.filename != filename
            || session.mime_type != mime_type
            || session.storage_host != storage_host
            || session.ttl_hours != ttl_hours
            || session.reserve_id != reserve_id
            || session.reservation_token != reservation_token
            || session.upload_mode != storage_upload_mode
        {
            // NB: client IP and user_id are deliberately NOT compared - the
            // client can flap IPv4<->IPv6 between requests and cookieless
            // origins mint a fresh anonymous user per request, so identity
            // fields churn mid-upload while the structural fields above
            // uniquely define the session.
            return Err(AppError::BadRequest(
                "parallel upload session metadata mismatch".into(),
            ));
        }
        file_capability = session.capability.clone();
        match session.part_ids.entry(pi) {
            Entry::Occupied(_) => {
                return Err(AppError::BadRequest(
                    "parallel upload part index already exists".into(),
                ));
            }
            Entry::Vacant(entry) => {
                entry.insert(id.clone());
            }
        }
        if session
            .declared_size
            .fetch_update(
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
                |size| {
                    size.checked_add(total_length)
                        .filter(|sum| *sum <= max_size)
                },
            )
            .is_err()
        {
            session.part_ids.remove(&pi);
            return Err(AppError::PayloadTooLarge);
        }
        session.part_lengths.insert(pi, total_length);
        tracing::debug!(
            "parallel part registered: session={} part={}/{} id={}",
            sid,
            pi + 1,
            tp,
            id
        );
    }

    // Chunks flow from PATCH handlers into the storage task without disk buffering.
    // The push task itself is spawned lazily on the first chunk so a session
    // that is slow to start never trips juicehost's body inactivity deadline.
    let (tx, rx) =
        mpsc::channel::<Result<Bytes, String>>(crate::constants::STREAM_CHANNEL_CAPACITY);

    let upload = TusUpload {
        id: id.clone(),
        offset: 0,
        total_length,
        filename: filename.clone(),
        mime_type: mime_type.clone(),
        ttl_hours,
        created_at: chrono::Utc::now().timestamp(),
        delete_token: file_capability.clone(),
        hashed_ip,
        encrypted_ip,
        storage_host: storage_host.clone(),
        session_id,
        part_index,
        reserve_id,
        reservation_token,
        capability: file_capability.clone(),
        patch_lock: Arc::new(tokio::sync::Mutex::new(())),
        user_id,
        upload_mode: storage_upload_mode,
        push_rx: Some(rx),
    };

    state.tus.insert(id.clone(), upload);
    state.tus_senders.insert(id.clone(), tx);

    let location = format!("/api/tus/{}", id);
    Ok(Response::builder()
        .status(StatusCode::CREATED)
        .header("Location", &location)
        .header("Tus-Resumable", "1.0.0")
        .body(axum::body::Body::empty())
        .expect("Response builder should not fail"))
}

#[utoipa::path(
    get,
    path = "/api/tus/{id}",
    params(
        ("id" = String, Path, description = "The TUS upload session ID"),
    ),
    responses(
        (status = 200, description = "Upload progress", headers(
            ("Upload-Offset" = String, description = "Current byte offset"),
            ("Upload-Length" = String, description = "Total expected size"),
            ("Tus-Resumable" = String, description = "TUS protocol version"),
        )),
        (status = 404, description = "TUS session not found"),
    ),
    tag = "Uploads",
)]
pub async fn get_upload_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let upload = state.tus.get(&id).ok_or(AppError::TusSessionNotFound)?;

    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("Upload-Offset", upload.offset.to_string())
        .header("Upload-Length", upload.total_length.to_string())
        .header("Tus-Resumable", "1.0.0")
        .body(axum::body::Body::empty())
        .expect("Response builder should not fail"))
}

/// Finish a non-parallel TUS upload by closing the sender and writing the DB record
async fn complete_tus_upload(
    state: &Arc<AppState>,
    meta: TusUploadMeta,
) -> Result<serde_json::Value, AppError> {
    // Close the sender side so the push task sees EOF and finishes.
    state.tus_senders.remove(&meta.id);
    await_storage_push(state, &meta.id).await?;
    finish_tus_upload(state, meta).await
}

/// Finalize a TUS upload whose storage push has already been awaited: write the
/// DB record and return the public URL response.
async fn finish_tus_upload(
    state: &Arc<AppState>,
    meta: TusUploadMeta,
) -> Result<serde_json::Value, AppError> {
    if let Some(ref reserve_id) = meta.reserve_id {
        crate::juicehost::rename_file_on_juicehost(
            state,
            &meta.id,
            reserve_id,
            meta.storage_host.as_deref(),
            meta.reservation_token.as_deref(),
        )
        .await
        .map_err(AppError::from_juicehost_error)?;
    }

    let record = if let Some(ref reserve_id) = meta.reserve_id {
        // This was a pre-reserved upload (Quick Link). Update the existing record.
        let rid = reserve_id.clone();
        let reservation_token = meta
            .reservation_token
            .clone()
            .ok_or_else(|| AppError::Forbidden("reservation delete token required".into()))?;
        let update_id = rid;
        let fname = meta.filename.clone();
        let mtype = meta.mime_type.clone();
        let size = meta.total_length as i64;
        let enc_ip = meta.encrypted_ip.clone();
        let completed = match state
            .db_call("complete_file", move |db| {
                crate::db::complete_reservation(
                    db,
                    &fname,
                    &mtype,
                    size,
                    &update_id,
                    &reservation_token,
                    Some(enc_ip.as_str()),
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
            "reserved TUS upload completed: id={} size={}",
            completed.id,
            completed.size_bytes
        );
        completed
    } else {
        let record = FileRecord::from_upload_with_token(
            meta.id.clone(),
            meta.filename,
            meta.mime_type,
            meta.total_length as i64,
            meta.ttl_hours,
            meta.delete_token,
            Some(meta.encrypted_ip),
            meta.storage_host.clone(),
        );
        Box::new(db::insert_new_file(state, record).await?)
    };

    let owned_record = record.clone();
    let owner = meta.user_id.clone();
    state
        .db_call("own_tus_file", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &record.id,
        &record.filename,
    );

    Ok(serde_json::to_value(crate::routes::upload::UploadResponse {
        id: record.id,
        url: public_url,
        filename: record.filename,
        size_bytes: record.size_bytes,
        mime_type: record.mime_type,
        expires_at: record.expires_at,
        delete_token: record.delete_token,
        status: record.status,
    })
    .unwrap_or_default())
}

async fn await_storage_push(state: &Arc<AppState>, upload_id: &str) -> Result<(), AppError> {
    let (_, handle) = match state.push_handles.remove(upload_id) {
        Some(pair) => pair,
        // Push never spawned (no chunk ever arrived); session is gone either way.
        None => return Err(AppError::TusSessionNotFound),
    };
    handle
        .await
        .map_err(|e| AppError::TaskPanicked(format!("TUS storage task for {}: {}", upload_id, e)))?
        .map_err(AppError::from_juicehost_error)
}

/// Spawn the streaming push for an upload once its first chunk arrives.
fn spawn_push_task(state: &Arc<AppState>, id: &str, rx: mpsc::Receiver<Result<Bytes, String>>) {
    let Some(upload) = state.tus.get(id) else {
        return;
    };
    let push_filename = upload.filename.clone();
    let push_mime = upload.mime_type.clone();
    let push_host = upload.storage_host.clone();
    let push_mode = upload.upload_mode;
    let push_capability = upload.capability.clone();
    let sid = upload.session_id.clone();
    let pi = upload.part_index;
    let total_len = upload.total_length;
    drop(upload);

    let push_state = Arc::clone(state);
    let push_id = id.to_string();
    let handle_id = id.to_string();
    let handle = tokio::spawn(async move {
        let result = crate::juicehost::push_file_streaming(
            &push_state,
            &push_id,
            &push_filename,
            &push_mime,
            rx,
            push_host,
            &push_mode,
            Some(&push_capability),
        )
        .await;
        if let Err(ref e) = result {
            tracing::warn!("tus push failed for {}: {}", push_id, e);
            // Free the part slot so the client can rebuild this session cleanly
            // instead of hitting "part index already exists" forever.
            release_parallel_slot(&push_state, &push_id, sid.as_deref(), pi, total_len);
        }
        result
    });
    state.push_handles.insert(handle_id, handle);
}

/// Remove a dead upload and, for parallel parts, release its reserved slot.
fn release_parallel_slot(
    state: &Arc<AppState>,
    id: &str,
    session_id: Option<&str>,
    part_index: Option<usize>,
    total_length: u64,
) {
    state.tus.remove(id);
    state.tus_senders.remove(id);
    state.push_handles.remove(id);
    let (Some(sid), Some(pi)) = (session_id, part_index) else {
        return;
    };
    use std::sync::atomic::Ordering;
    let Some(session) = state.part_sessions.get(sid) else {
        return;
    };
    let ours = session
        .part_ids
        .get(&pi)
        .map(|v| v.value() == id)
        .unwrap_or(false);
    if ours && session.part_ids.remove(&pi).is_some() {
        let _ = session.declared_size.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |size| {
            Some(size.saturating_sub(total_length))
        });
    }
}

/// Check if all parallel parts are complete, and if so, collect ordered part IDs.
fn try_collect_finalized_parts(
    state: &Arc<AppState>,
    session_id: &str,
    total: usize,
) -> Option<(Vec<String>, u64)> {
    let completed = {
        let session = state.part_sessions.get(session_id)?;
        use std::sync::atomic::Ordering;
        session.completed.fetch_add(1, Ordering::SeqCst) + 1
    };

    tracing::info!(
        "parallel part {}/{} complete for session {}",
        completed,
        total,
        session_id
    );

    if completed < total {
        return None;
    }

    let session = state.part_sessions.get(session_id).unwrap();
    let (part_ids, full_size) = collect_ordered_parts(&session, total)?;
    let part_tus_ids: Vec<String> = session.part_ids.iter().map(|e| e.value().clone()).collect();
    drop(session);
    state.part_sessions.remove(session_id);

    for pid in &part_tus_ids {
        state.tus.remove(pid);
        state.tus_senders.remove(pid);
        state.push_handles.remove(pid);
    }

    Some((part_ids, full_size))
}

fn collect_ordered_parts(
    session: &crate::tus::PartSession,
    total: usize,
) -> Option<(Vec<String>, u64)> {
    let ordered_parts: Option<Vec<(String, u64)>> = (0..total)
        .map(|i| {
            Some((
                session.part_ids.get(&i)?.value().clone(),
                *session.part_lengths.get(&i)?.value(),
            ))
        })
        .collect();
    let ordered_parts = ordered_parts?;
    let full_size = ordered_parts
        .iter()
        .try_fold(0_u64, |sum, (_, length)| sum.checked_add(*length))?;
    let part_ids = ordered_parts.into_iter().map(|(id, _)| id).collect();
    Some((part_ids, full_size))
}

/// Concat parts on juicehost and create the merged DB record.
async fn finalize_concat(
    state: &Arc<AppState>,
    meta: &TusUploadMeta,
    session_id: &str,
    part_ids: &[String],
    full_size: u64,
) -> Result<serde_json::Value, AppError> {
    let target_id = if let Some(ref rid) = meta.reserve_id {
        rid.clone()
    } else {
        nanoid::nanoid!(8)
    };
    let filename = meta.filename.clone();

    crate::juicehost::concat_files(
        state,
        &target_id,
        &filename,
        part_ids,
        meta.storage_host.as_deref(),
        Some(&meta.delete_token),
    )
    .await
    .map_err(|e| {
        tracing::error!("concat failed for session {}: {}", session_id, e);
        AppError::Internal(format!("concat failed: {}", e))
    })?;

    let record = if let Some(ref reserve_id) = meta.reserve_id {
        // This was a pre-reserved upload (Quick Link). Update the existing record.
        let rid = reserve_id.clone();
        let reservation_token = meta
            .reservation_token
            .clone()
            .ok_or_else(|| AppError::Forbidden("reservation delete token required".into()))?;
        let update_id = rid;
        let fname = meta.filename.clone();
        let mtype = meta.mime_type.clone();
        let size = full_size as i64;
        let enc_ip = meta.encrypted_ip.clone();
        let completed = match state
            .db_call("complete_file", move |db| {
                crate::db::complete_reservation(
                    db,
                    &fname,
                    &mtype,
                    size,
                    &update_id,
                    &reservation_token,
                    Some(enc_ip.as_str()),
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
            "reserved parallel TUS upload completed: id={} size={}",
            completed.id,
            completed.size_bytes
        );
        completed
    } else {
        let record = FileRecord::from_upload_with_token(
            target_id,
            filename,
            meta.mime_type.clone(),
            full_size as i64,
            meta.ttl_hours,
            meta.delete_token.clone(),
            Some(meta.encrypted_ip.clone()),
            meta.storage_host.clone(),
        );
        Box::new(db::insert_new_file(state, record).await?)
    };

    let owned_record = record.clone();
    let owner = meta.user_id.clone();
    state
        .db_call("own_parallel_tus_file", move |db| {
            db::add_client_file(db, &owner, &owned_record)
        })
        .await?;

    let public_url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &record.id,
        &record.filename,
    );

    Ok(serde_json::to_value(crate::routes::upload::UploadResponse {
        id: record.id,
        url: public_url,
        filename: record.filename,
        size_bytes: record.size_bytes,
        mime_type: record.mime_type,
        expires_at: record.expires_at,
        delete_token: record.delete_token,
        status: record.status,
    })
    .unwrap_or_default())
}

/// Complete a parallel upload session: await push, register part, maybe concat.
async fn complete_parallel_part(
    state: &Arc<AppState>,
    meta: TusUploadMeta,
    session_id: &str,
    part_index: usize,
) -> Result<serde_json::Value, AppError> {
    state.tus_senders.remove(&meta.id);
    await_storage_push(state, &meta.id).await?;

    let session = state.part_sessions.get(session_id).ok_or_else(|| {
        tracing::error!("part session {} not found", session_id);
        AppError::TusSessionNotFound
    })?;
    let total = session.total_parts;
    {
        let mut expected = session.completion_reserve_id.lock().unwrap();
        match expected.as_ref() {
            Some(reserve_id) if reserve_id != &meta.reserve_id => {
                return Err(AppError::BadRequest(
                    "parallel upload reserve_id mismatch".into(),
                ));
            }
            None => *expected = Some(meta.reserve_id.clone()),
            _ => {}
        }
    }
    drop(session);

    // Single-part uploads have nothing to concat: the streamed file already
    // IS the final file. Skip the parallel bookkeeping and finalize directly.
    if total <= 1 {
        state.part_sessions.remove(session_id);
        return finish_tus_upload(state, meta).await;
    }

    match try_collect_finalized_parts(state, session_id, total) {
        Some((part_ids, full_size)) => {
            finalize_concat(state, &meta, session_id, &part_ids, full_size).await
        }
        None => Ok(serde_json::json!({
            "status": "part_complete",
            "part": part_index + 1,
            "total": total,
        })),
    }
}

#[utoipa::path(
    patch,
    path = "/api/tus/{id}",
    params(
        ("id" = String, Path, description = "The TUS upload session ID"),
    ),
    request_body(content_type = "application/offset+octet-stream", description = "Binary chunk of file data; may be gzip-compressed when the X-File-Encoding: gzip header is set (offsets remain in decompressed bytes)"),
    responses(
        (status = 204, description = "Chunk accepted, upload in progress", headers(
            ("Upload-Offset" = String, description = "New byte offset after this chunk"),
            ("Tus-Resumable" = String, description = "TUS protocol version"),
        )),
        (status = 200, description = "Upload complete, file is now live", body = serde_json::Value),
        (status = 404, description = "TUS session not found"),
        (status = 409, description = "Upload offset mismatch"),
        (status = 413, description = "Chunk exceeds file size limit"),
    ),
    tag = "Uploads",
)]
pub async fn patch_upload_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, AppError> {
    let req_offset = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let res = patch_upload_handler_impl(State(state), Path(id.clone()), headers, body).await;
    match res {
        Ok((r, wire_len)) => {
            tracing::info!(
                id = %id,
                req_offset,
                resp_offset = r.headers().get("upload-offset").and_then(|v| v.to_str().ok().map(str::to_string)),
                wire_len,
                status = r.status().as_u16(),
                "tus_patch ok"
            );
            Ok(r)
        }
        Err(e) => {
            let code: u16 = match e {
                AppError::TusOffsetMismatch => 409,
                AppError::TusSessionNotFound => 404,
                AppError::TusMissingOffset => 400,
                AppError::PayloadTooLarge => 413,
                AppError::BlockedFileType(_) => 403,
                AppError::GzipDecodeFailed => 400,
                _ => 500,
            };
            tracing::info!(
                id = %id,
                req_offset,
                status = code,
                "tus_patch err"
            );
            Err(e)
        }
    }
}

/// Tear down a session whose streamed PATCH died mid-body after bytes were
/// already forwarded to juicehost. The stored prefix is truncated, so the
/// client must rebuild the whole part (same offset would append duplicates).
/// Session state is released first, then the push task is given a bounded
/// grace period, then the partial file is best-effort deleted so it cannot
/// be mistaken for a complete part.
///
/// Callers MUST have dropped their `tus_senders` Ref before this: map
/// removal blocks on outstanding readers of the same shard, and the
/// handler must never park (it holds the session patch_lock).
async fn abort_streamed_patch(
    state: &Arc<AppState>,
    id: &str,
    session_id: Option<&str>,
    part_index: Option<usize>,
    total_length: u64,
    storage_host: Option<&str>,
    capability: &str,
) {
    match (session_id, part_index) {
        (Some(sid), Some(pi)) => release_parallel_slot(state, id, Some(sid), Some(pi), total_length),
        _ => {
            state.tus.remove(id);
        }
    }
    state.tus_senders.remove(id);
    if let Some((_, handle)) = state.push_handles.remove(id) {
        // Bounded: a truncated push to local juicehost drains in seconds.
        // An unbounded await here would park the handler (and its
        // patch_lock) forever if the push task ever stopped polling rx.
        match tokio::time::timeout(std::time::Duration::from_secs(60), handle).await
        {
            Ok(_) => {}
            Err(_) => {
                tracing::warn!("abort_streamed_patch: push task did not finish for {}", id);
            }
        }
    }
    let _ =
        crate::juicehost::delete_file_on_juicehost(state, id, storage_host, Some(capability)).await;
}

#[tracing::instrument(skip_all)]
async fn patch_upload_handler_impl(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<(Response, u64), AppError> {
    let upload_offset = headers
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .ok_or_else(|| AppError::TusMissingOffset)?;

    let is_gzip =
        headers.get("x-file-encoding").and_then(|v| v.to_str().ok()) == Some("gzip");

    let reserve_override = headers
        .get("x-reserve-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(|value| {
            if crate::utils::is_valid_id(value) {
                Ok(value.to_string())
            } else {
                Err(AppError::BadRequest("invalid reserve_id".into()))
            }
        })
        .transpose()?;
    let reservation_token_override = headers
        .get("x-delete-token")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string());
    if reserve_override.is_some() && reservation_token_override.is_none() {
        return Err(AppError::Forbidden(
            "reservation delete token required".into(),
        ));
    }
    if let Some(ref reserve_id) = reserve_override {
        let token = reservation_token_override.as_deref().unwrap_or_default();
        let lookup_id = reserve_id.clone();
        let reservation = state
            .db_call("get_tus_override_reservation", move |db| {
                db::get_file(db, &lookup_id)
            })
            .await?
            .ok_or_else(|| AppError::BadRequest("invalid reserve_id".into()))?;
        if reservation.status != "uploading" {
            return Err(AppError::BadRequest("reservation is not uploading".into()));
        }
        if !crate::utils::constant_time_eq(&reservation.delete_token, token) {
            return Err(AppError::Forbidden(
                "invalid reservation delete token".into(),
            ));
        }
        let upload_host = state
            .tus
            .get(&id)
            .ok_or(AppError::TusSessionNotFound)?
            .storage_host
            .clone();
        if reservation.storage_host != upload_host {
            return Err(AppError::BadRequest(
                "reservation storage host does not match TUS session".into(),
            ));
        }
    }

    let patch_lock = state
        .tus
        .get(&id)
        .ok_or(AppError::TusSessionNotFound)?
        .patch_lock
        .clone();
    let _patch_guard = patch_lock.lock().await;

    let (session_id, part_index, filename, total_length, capability, storage_host) = {
        let upload = state.tus.get(&id).ok_or(AppError::TusSessionNotFound)?;
        if upload_offset != upload.offset {
            return Err(AppError::TusOffsetMismatch);
        }
        (
            upload.session_id.clone(),
            upload.part_index,
            upload.filename.clone(),
            upload.total_length,
            upload.capability.clone(),
            upload.storage_host.clone(),
        )
    };
    // This PATCH may not push the offset past the declared total length.
    let remaining = total_length.saturating_sub(upload_offset);

    let danger_level = state
        .jh_config
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.danger_level)
        .unwrap_or(crate::file_validation::ProtectionLevel::High);

    // Validate file type on the first chunk. `validate_file` only sniffs a
    // content prefix, so validating the head of the stream is equivalent to
    // validating a buffered body.
    let validate_head = |head: &[u8]| -> Result<(), AppError> {
        if upload_offset != 0 || head.is_empty() {
            return Ok(());
        }
        match crate::file_validation::validate_file(&filename, head, danger_level) {
            crate::file_validation::FileValidation::Allowed => Ok(()),
            crate::file_validation::FileValidation::BlockedExtension { ext: _, tier } => {
                state.tus.remove(&id);
                Err(AppError::BlockedFileType(
                    crate::file_validation::friendly_block_reason(tier),
                ))
            }
            crate::file_validation::FileValidation::BlockedMagic {
                description: _,
                tier,
            } => {
                state.tus.remove(&id);
                Err(AppError::BlockedFileType(
                    crate::file_validation::friendly_block_reason(tier),
                ))
            }
            crate::file_validation::FileValidation::Empty => {
                state.tus.remove(&id);
                Err(AppError::BlockedFileType(
                    "Empty files are not allowed".into(),
                ))
            }
        }
    };

    // Chunks may arrive gzip-compressed (X-File-Encoding: gzip, one
    // self-contained gzip member per PATCH). Those stay buffered + decoded
    // up front (text only, small); everything else streams frame-by-frame
    // straight into the storage push with no full-body buffering.
    //
    // Frames validated and ready to forward, plus the optional live tail of
    // the request body for large raw PATCHes. `wire_len` counts wire bytes
    // already consumed (for gzip: the compressed size).
    struct PatchWire {
        buffered: Vec<Bytes>,
        live: Option<PatchBodyStream>,
        wire_len: u64,
    }
    let max_wire_bytes = {
        let max_size = state
            .jh_config
            .read()
            .unwrap()
            .as_ref()
            .map(|c| c.max_file_size_bytes)
            .unwrap_or(500 * 1024 * 1024);
        max_size.saturating_add(1024 * 1024).max(1024 * 1024) as usize
    };
    let wire = if is_gzip {
        let raw = axum::body::to_bytes(body, max_wire_bytes)
            .await
            .map_err(|_| AppError::PayloadTooLarge)?;
        if raw.is_empty() {
            PatchWire {
                buffered: Vec::new(),
                live: None,
                wire_len: 0,
            }
        } else {
            use std::io::Read as _;
            let wire_len = raw.len() as u64;
            let mut out = Vec::with_capacity(raw.len() * 4);
            flate2::read::GzDecoder::new(&raw[..])
                .read_to_end(&mut out)
                .map_err(|_| AppError::GzipDecodeFailed)?;
            const MAX_DECOMPRESSED_CHUNK: usize = 256 * 1024 * 1024;
            if out.len() > MAX_DECOMPRESSED_CHUNK {
                return Err(AppError::PayloadTooLarge);
            }
            let out = Bytes::from(out);
            validate_head(&out)?;
            PatchWire {
                buffered: vec![out],
                live: None,
                wire_len,
            }
        }
    } else {
        // Buffer a small sniff prefix for validation, then stream the rest.
        // Nothing is forwarded before the head validates, so a blocked type
        // never stores a single byte.
        const SNIFF_MAX: usize = 8192;
        let mut stream = body.into_data_stream();
        let mut head: Vec<u8> = Vec::new();
        let mut buffered: Vec<Bytes> = Vec::new();
        let mut buffered_len: u64 = 0;
        let mut eof = false;
        loop {
            if head.len() >= SNIFF_MAX {
                break;
            }
            match stream.next().await {
                Some(Ok(frame)) => {
                    if frame.is_empty() {
                        continue;
                    }
                    buffered_len = buffered_len
                        .checked_add(frame.len() as u64)
                        .ok_or(AppError::PayloadTooLarge)?;
                    if buffered_len > remaining {
                        return Err(AppError::PayloadTooLarge);
                    }
                    let take = (SNIFF_MAX - head.len()).min(frame.len());
                    head.extend_from_slice(&frame[..take]);
                    buffered.push(frame);
                }
                Some(Err(_)) => return Err(AppError::Internal("upload body failed".into())),
                None => {
                    eof = true;
                    break;
                }
            }
        }
        validate_head(&head)?;
        PatchWire {
            buffered,
            live: if eof {
                None
            } else {
                Some(Box::pin(stream))
            },
            wire_len: buffered_len,
        }
    };

    // First chunk for this session: hand the receiver to a freshly spawned
    // storage push. Sessions that never reach this point hold no juicehost
    // connection and cannot starve its inactivity deadline.
    {
        let mut upload = state.tus.get_mut(&id).ok_or(AppError::TusSessionNotFound)?;
        if let Some(rx) = upload.push_rx.take() {
            drop(upload);
            spawn_push_task(&state, &id, rx);
        }
    }

    let sender = state
        .tus_senders
        .get(&id)
        .map(|s| s.clone())
        .ok_or(AppError::TusSessionNotFound)?;
    // NOTE: `sender` is an owned clone, deliberately NOT the map Ref. A Ref
    // borrows the DashMap shard for as long as it lives; holding one across
    // minutes of network awaits let two wedged holders starve every runtime
    // worker against a concurrent shard write, freezing accept entirely.
    // Cloned senders keep the channel usable, and map removal still closes
    // it once all clones are dropped.

    // Forward validated frames, then the live tail. Only bytes actually
    // accepted here advance the offset below.
    let send_start = std::time::Instant::now();
    let mut forwarded: u64 = 0;
    let mut wire_len: u64 = wire.wire_len;
    let mut push_died = false;
    for msg in wire.buffered {
        forwarded = forwarded
            .checked_add(msg.len() as u64)
            .ok_or(AppError::PayloadTooLarge)?;
        if forwarded > remaining {
            if forwarded == msg.len() as u64 {
                // Nothing stored yet (e.g. oversize gzip member): plain 413
                // so the client can retry a smaller chunk.
                return Err(AppError::PayloadTooLarge);
            }
            // Bytes already stored: the part must be rebuilt. Drop the
            // sender Ref BEFORE tearing down (see abort_streamed_patch).
            drop(sender);
            abort_streamed_patch(
                &state,
                &id,
                session_id.as_deref(),
                part_index,
                total_length,
                storage_host.as_deref(),
                &capability,
            )
            .await;
            return Err(AppError::PayloadTooLarge);
        }
        if sender.send(Ok(msg)).await.is_err() {
            push_died = true;
            break;
        }
    }
    if !push_died {
        if let Some(mut live) = wire.live {
            while let Some(frame) = live.next().await {
                match frame {
                    Ok(chunk) => {
                        if chunk.is_empty() {
                            continue;
                        }
                        wire_len = wire_len.saturating_add(chunk.len() as u64);
                        forwarded = forwarded
                            .checked_add(chunk.len() as u64)
                            .ok_or(AppError::PayloadTooLarge)?;
                        if forwarded > remaining {
                            drop(sender);
                            abort_streamed_patch(
                                &state,
                                &id,
                                session_id.as_deref(),
                                part_index,
                                total_length,
                                storage_host.as_deref(),
                                &capability,
                            )
                            .await;
                            return Err(AppError::PayloadTooLarge);
                        }
                        if sender.send(Ok(chunk)).await.is_err() {
                            push_died = true;
                            break;
                        }
                    }
                    Err(_) => {
                        // Body died mid-chunk. Anything already forwarded is
                        // stored under this id, so the part must be rebuilt
                        // from scratch rather than retried at this offset.
                        drop(sender);
                        if forwarded > 0 {
                            abort_streamed_patch(
                                &state,
                                &id,
                                session_id.as_deref(),
                                part_index,
                                total_length,
                                storage_host.as_deref(),
                                &capability,
                            )
                            .await;
                        }
                        return Err(AppError::Internal("upload body failed".into()));
                    }
                }
            }
        }
    }
    if push_died {
        drop(sender);
        state.tus_senders.remove(&id);
        await_storage_push(&state, &id).await?;
        return Err(AppError::TusSessionNotFound);
    }
    tracing::debug!(
        "tus patch: chunk_stream took {:?} ({} bytes)",
        send_start.elapsed(),
        forwarded
    );
    drop(sender);

    let new_offset = checked_upload_offset(upload_offset, forwarded, total_length)?;

    let (is_complete, completed_meta) = {
        let mut upload = state.tus.get_mut(&id).ok_or(AppError::TusSessionNotFound)?;
        upload.offset = new_offset;
        let done = upload.offset >= upload.total_length;
        if done {
            let meta = TusUploadMeta {
                id: upload.id.clone(),
                filename: upload.filename.clone(),
                mime_type: upload.mime_type.clone(),
                total_length: upload.total_length,
                delete_token: upload.delete_token.clone(),
                encrypted_ip: upload.encrypted_ip.clone(),
                ttl_hours: upload.ttl_hours,
                storage_host: upload.storage_host.clone(),
                reserve_id: reserve_override
                    .clone()
                    .or_else(|| upload.reserve_id.clone()),
                reservation_token: reservation_token_override
                    .clone()
                    .or_else(|| upload.reservation_token.clone()),
                user_id: upload.user_id.clone(),
            };
            (true, Some(meta))
        } else {
            (false, None)
        }
    };

    if is_complete {
        if let Some(meta) = completed_meta {
            // Check if this is a parallel part.
            if let (Some(ref sid), Some(pi)) = (&session_id, part_index) {
                state.tus.remove(&id);
                let response = complete_parallel_part(&state, meta, sid, pi).await?;
                let status = if response.get("url").is_some() {
                    StatusCode::OK
                } else {
                    StatusCode::ACCEPTED
                };
                return Ok(((status, Json(response)).into_response(), wire_len));
            } else {
                state.tus.remove(&id);
                let response = complete_tus_upload(&state, meta).await?;
                return Ok((Json(response).into_response(), wire_len));
            }
        }
    }

    Ok((
        Response::builder()
            .status(StatusCode::NO_CONTENT)
            .header("Upload-Offset", new_offset.to_string())
            .header("Tus-Resumable", "1.0.0")
            .body(axum::body::Body::empty())
            .expect("Response builder should not fail"),
        wire_len,
    ))
}

#[utoipa::path(
    options,
    path = "/api/tus",
    responses(
        (status = 204, description = "TUS capabilities", headers(
            ("Tus-Resumable" = String, description = "TUS protocol version"),
            ("Tus-Version" = String, description = "Supported TUS version"),
            ("Tus-Extension" = String, description = "Supported TUS extensions"),
        )),
    ),
    tag = "Uploads",
)]
pub async fn options_handler() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Tus-Resumable", "1.0.0")
        .header("Tus-Version", "1.0.0")
        .header("Tus-Extension", "creation,termination")
        .body(axum::body::Body::empty())
        .expect("Response builder should not fail")
}

#[utoipa::path(
    delete,
    path = "/api/tus/{id}",
    params(
        ("id" = String, Path, description = "The TUS upload session ID"),
    ),
    responses(
        (status = 204, description = "TUS session deleted and temp file removed"),
        (status = 404, description = "TUS session not found"),
    ),
    tag = "Uploads",
)]
pub async fn delete_upload_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let existing = state.tus.get(&id).ok_or(AppError::TusSessionNotFound)?;
    let sid = existing.session_id.clone();
    let pi = existing.part_index;
    let len = existing.total_length;
    drop(existing);
    release_parallel_slot(&state, &id, sid.as_deref(), pi, len);
    Ok(Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header("Tus-Resumable", "1.0.0")
        .body(axum::body::Body::empty())
        .expect("Response builder should not fail"))
}

pub fn tus_routes() -> axum::Router<Arc<AppState>> {
    axum::Router::new()
        .route(
            "/api/tus",
            routing::post(create_upload_handler).options(options_handler),
        )
        .route(
            "/api/tus/:id",
            routing::get(get_upload_handler)
                .patch(patch_upload_handler)
                .delete(delete_upload_handler),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tus_metadata_empty() {
        let result = parse_tus_metadata(None);
        assert!(result.is_empty());
    }

    #[test]
    fn parse_tus_metadata_single_pair() {
        let header = axum::http::HeaderValue::from_static("filename dGVzdC50eHQ=");
        let result = parse_tus_metadata(Some(&header));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "filename");
        assert_eq!(result[0].1, "test.txt");
    }

    #[test]
    fn parse_tus_metadata_multiple_pairs() {
        let header = axum::http::HeaderValue::from_static(
            "filename dGVzdC50eHQ=, mimetype dGV4dC9wbGFpbg==",
        );
        let result = parse_tus_metadata(Some(&header));
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "filename");
        assert_eq!(result[0].1, "test.txt");
        assert_eq!(result[1].0, "mimetype");
        assert_eq!(result[1].1, "text/plain");
    }

    #[test]
    fn parse_tus_metadata_invalid_base64() {
        let header = axum::http::HeaderValue::from_static("key !!!invalid!!!");
        let result = parse_tus_metadata(Some(&header));
        assert!(result.is_empty());
    }

    #[test]
    fn find_meta_found() {
        let meta = vec![
            ("filename".into(), "test.txt".into()),
            ("mimetype".into(), "text/plain".into()),
        ];
        assert_eq!(find_meta(&meta, "filename"), Some("test.txt"));
    }

    #[test]
    fn find_meta_not_found() {
        let meta = vec![("filename".into(), "test.txt".into())];
        assert_eq!(find_meta(&meta, "mimetype"), None);
    }

    #[test]
    fn find_meta_empty() {
        assert_eq!(find_meta(&[], "filename"), None);
    }

    #[test]
    fn parallel_metadata_requires_complete_bounded_tuple() {
        assert!(validate_parallel_metadata(None, None, None)
            .unwrap()
            .is_none());
        assert!(validate_parallel_metadata(Some("session"), Some(0), Some(4)).is_ok());
        assert!(validate_parallel_metadata(Some("session"), None, Some(4)).is_err());
        assert!(validate_parallel_metadata(Some("session"), Some(4), Some(4)).is_err());
        assert!(validate_parallel_metadata(
            Some("session"),
            Some(0),
            Some(crate::constants::MAX_TUS_PARALLEL_PARTS + 1),
        )
        .is_err());
        assert!(parse_parallel_number(Some("not-a-number")).is_err());
    }

    #[test]
    fn ordered_parts_use_actual_declared_lengths() {
        let session = crate::tus::PartSession {
            session_id: "session".into(),
            total_parts: 3,
            filename: "file.bin".into(),
            mime_type: "application/octet-stream".into(),
            hashed_ip: "hash".into(),
            storage_host: None,
            ttl_hours: 24.0,
            reserve_id: None,
            reservation_token: None,
            capability: "capability".into(),
            user_id: "user-1".into(),
            upload_mode: UploadMode::Standard,
            declared_size: std::sync::atomic::AtomicU64::new(17),
            completed: std::sync::atomic::AtomicUsize::new(3),
            part_ids: dashmap::DashMap::new(),
            part_lengths: dashmap::DashMap::new(),
            completion_reserve_id: std::sync::Mutex::new(None),
        };
        session.part_ids.insert(2, "part-c".into());
        session.part_ids.insert(0, "part-a".into());
        session.part_ids.insert(1, "part-b".into());
        session.part_lengths.insert(0, 8);
        session.part_lengths.insert(1, 8);
        session.part_lengths.insert(2, 1);

        let (ids, size) = collect_ordered_parts(&session, 3).unwrap();
        assert_eq!(ids, vec!["part-a", "part-b", "part-c"]);
        assert_eq!(size, 17);
    }

    #[test]
    fn upload_offset_is_checked_before_streaming() {
        assert_eq!(checked_upload_offset(4, 6, 10).unwrap(), 10);
        assert!(matches!(
            checked_upload_offset(4, 7, 10),
            Err(AppError::PayloadTooLarge)
        ));
        assert!(matches!(
            checked_upload_offset(u64::MAX, 1, u64::MAX),
            Err(AppError::PayloadTooLarge)
        ));
    }
}
