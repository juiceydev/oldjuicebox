//! endpoints for getting file info, deleting, renewing the ID, and looking up
//! owned files. the bread and butter of file management fr fr

use axum::{
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Redirect, Response},
    Form, Json,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use crate::db;
use crate::db::ClientFileRecord;
use crate::db::FileRecord;
use crate::error::AppError;
use crate::routes::noscript;
use crate::routes::UserId;
use crate::state::AppState;
use crate::utils::constant_time_eq;

/// Form fields for no-JS file rename.
#[derive(Deserialize)]
pub struct RenameForm {
    pub token: String,
    pub custom_id: String,
}

/// Request body for the owned-files bulk lookup endpoint.
#[derive(Deserialize, ToSchema)]
pub struct OwnedFilesRequest {
    pub pairs: Vec<FilePair>,
}

/// An ID and delete-token pair to prove you own a file.
#[derive(Deserialize, ToSchema)]
pub struct FilePair {
    pub id: String,
    pub token: String,
}

/// Response containing the caller's owned files.
#[derive(Serialize, ToSchema)]
pub struct OwnedFilesResponse {
    pub files: Vec<FileInfoResponse>,
}

#[utoipa::path(
    post,
    path = "/api/owned-files",
    request_body(content = OwnedFilesRequest, description = "List of file IDs and delete tokens you own"),
    responses(
        (status = 200, description = "List of files you own that havent expired yet", body = OwnedFilesResponse),
    ),
    tag = "Files",
)]
pub async fn owned_files_handler(
    State(state): State<Arc<AppState>>,
    Json(payload): Json<OwnedFilesRequest>,
) -> Result<Json<OwnedFilesResponse>, AppError> {
    let now = chrono::Utc::now().timestamp();

    let ids: Vec<String> = payload.pairs.iter().map(|p| p.id.clone()).collect();
    let token_map: std::collections::HashMap<String, &str> = payload
        .pairs
        .iter()
        .map(|p| (p.id.clone(), p.token.as_str()))
        .collect();

    let records = state
        .db_call("get_files_by_ids_resolving_aliases", move |db| {
            db::get_files_by_ids_resolving_aliases(db, &ids)
        })
        .await?;

    let files: Vec<FileInfoResponse> = records
        .into_iter()
        .filter(|(submitted_id, r)| {
            r.expires_at >= now
                && token_map
                    .get(submitted_id)
                    .map(|t| constant_time_eq(&r.delete_token, t))
                    .unwrap_or(false)
        })
        .map(|(submitted_id, r)| {
            let mut info = FileInfoResponse::from_record(r, &state.config.public_base_url);
            info.id = submitted_id;
            info
        })
        .collect();

    Ok(Json(OwnedFilesResponse { files }))
}

/// Body for the per-client file registry.
#[derive(Deserialize, ToSchema)]
pub struct ClientFilesRequest {
    pub files: Vec<ClientFileRecord>,
}

/// Response listing a client's registered files.
#[derive(Serialize, ToSchema)]
pub struct ClientFilesResponse {
    pub files: Vec<ClientFileRecord>,
}

/// Maximum number of files one client can register (abuse guard).
const CLIENT_FILES_MAX: usize = 500;

/// One-release import of locally-verifiable legacy browser capabilities.
///
/// The browser is the source of truth — the `jb_files` cookie is capped at
/// ~4KB, but this registry has no size limit, so the full upload list (with
/// metadata) survives for no-JS SSR even when the default juiceback can't
/// resolve the ids (custom-host / other-backend files). The client is
/// identified by the `jb_uid` cookie via the user-identity middleware.
#[utoipa::path(
    post,
    path = "/api/client-files",
    request_body(content = ClientFilesRequest, description = "The client's full upload list (id + delete token + display metadata)"),
    responses(
        (status = 200, description = "Registry replaced", body = ClientFilesResponse),
        (status = 400, description = "Too many files"),
    ),
    tag = "Files",
)]
pub async fn put_client_files_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    Json(payload): Json<ClientFilesRequest>,
) -> Result<Json<serde_json::Value>, AppError> {
    if payload.files.len() > CLIENT_FILES_MAX {
        return Err(AppError::BadRequest(format!(
            "Too many files: {} (max {CLIENT_FILES_MAX})",
            payload.files.len()
        )));
    }
    let key = user_id;
    let count = state
        .db_call("import_verified_client_files", move |db| {
            db::import_verified_client_files(db, &key, &payload.files)
        })
        .await?;
    Ok(Json(serde_json::json!({ "count": count })))
}

/// Fetch the caller's registered file list (used by SSR /files).
#[utoipa::path(
    get,
    path = "/api/client-files",
    responses(
        (status = 200, description = "The client's registered files", body = ClientFilesResponse),
    ),
    tag = "Files",
)]
pub async fn get_client_files_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
) -> Result<Json<ClientFilesResponse>, AppError> {
    let key = user_id.clone();
    let files = state
        .db_call("get_client_files", move |db| db::get_client_files(db, &key))
        .await?;
    Ok(Json(ClientFilesResponse { files }))
}

/// Public metadata returned by the file-info and owned-files endpoints.
#[derive(Serialize, ToSchema)]
pub struct FileInfoResponse {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub uploaded_at: i64,
    pub expires_at: i64,
    pub url: String,
    pub storage_host: Option<String>,
    pub status: String,
}

impl FileInfoResponse {
    /// Build a response from a database record, computing the public URL.
    pub fn from_record(record: FileRecord, public_base_url: &str) -> Self {
        let url = crate::utils::public_url(
            public_base_url,
            &record.storage_host,
            &record.id,
            &record.filename,
        );
        Self {
            id: record.id,
            filename: record.filename,
            mime_type: record.mime_type,
            size_bytes: record.size_bytes,
            uploaded_at: record.uploaded_at,
            expires_at: record.expires_at,
            url,
            storage_host: record.storage_host,
            status: record.status,
        }
    }
}

/// Response returned after a successful file-ID renewal.
#[derive(Serialize, ToSchema)]
pub struct RenewResponse {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub expires_at: i64,
    pub custom: bool,
}

/// Request body for file-ID renewal where you can pass custom_id to set a URL slug
#[derive(Deserialize, ToSchema, Default)]
pub struct RenewRequest {
    pub custom_id: Option<String>,
}

#[utoipa::path(
    get,
    path = "/file/{id}/info",
    params(
        ("id" = String, Path, description = "The file ID"),
    ),
    responses(
        (status = 200, description = "File metadata", body = FileInfoResponse),
        (status = 404, description = "File not found"),
        (status = 410, description = "File has expired"),
    ),
    tag = "Files",
)]
#[tracing::instrument(skip_all)]
pub async fn file_info_handler(
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<(HeaderMap, Json<FileInfoResponse>), AppError> {
    // Follow aliases so stale entries (e.g. a pre-rename id left in a cookie)
    // self-heal: info reports the file's current id and url.
    let id2 = id.clone();
    let resolved = state
        .db_call("resolve_alias", move |db| db::resolve_alias(db, &id2))
        .await?
        .unwrap_or(id.clone());
    let resolved2 = resolved.clone();
    let record = state
        .db_call("get_file", move |db| db::get_file(db, &resolved2))
        .await?
        .ok_or(AppError::NotFound)?;

    let now = chrono::Utc::now().timestamp();
    if record.expires_at < now {
        return Err(AppError::Gone);
    }

    let mut headers = HeaderMap::new();
    headers.insert("cache-control", "no-store".parse().unwrap());
    Ok((
        headers,
        Json(FileInfoResponse::from_record(
            record,
            &state.config.public_base_url,
        )),
    ))
}

/// POST /file/:id/delete via a no-JS form that redirects to /files
#[utoipa::path(
    post,
    path = "/file/{id}/delete",
    params(
        ("id" = String, Path, description = "The file ID"),
    ),
    request_body(content_type = "application/x-www-form-urlencoded", description = "Form with a 'token' field containing the delete token"),
    responses(
        (status = 302, description = "Redirects to /files.html?deleted={id}"),
        (status = 403, description = "Invalid delete token"),
        (status = 404, description = "File not found"),
    ),
    tag = "Files",
)]
pub async fn delete_file_form_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    Path(id): Path<String>,
    Form(_params): Form<noscript::DeleteForm>,
) -> Result<Redirect, AppError> {
    delete_file_inner(&state, &user_id, &id, None).await?;
    Ok(Redirect::to(&format!("/files.html?deleted={}", id)))
}

async fn delete_file_inner(
    state: &Arc<AppState>,
    user_id: &str,
    id: &str,
    compatibility_token: Option<&str>,
) -> Result<(), AppError> {
    let id2 = id.to_string();
    let record = state
        .db_call("get_file", move |db| db::get_file(db, &id2))
        .await?
        .ok_or(AppError::NotFound)?;

    let uid = user_id.to_string();
    let owned_id = id.to_string();
    let owned = state
        .db_call("client_owns_file", move |db| {
            db::client_owns_file(db, &uid, &owned_id)
        })
        .await?;
    let token_valid = compatibility_token
        .map(|token| constant_time_eq(&record.delete_token, token))
        .unwrap_or(false);
    if !owned && !token_valid {
        return Err(AppError::Forbidden(
            "file is not owned by this session".into(),
        ));
    }

    let host_opt = record.storage_host.as_deref();
    crate::juicehost::delete_file_on_juicehost(state, id, host_opt, Some(&record.delete_token))
        .await
        .map_err(AppError::from_juicehost_error)?;

    let id3 = id.to_string();
    state
        .db_call("delete_file", move |db| db::delete_file(db, &id3))
        .await?;
    let owner = user_id.to_string();
    let owned_id = id.to_string();
    state
        .db_call("remove_client_file", move |db| {
            db::remove_client_file(db, &owner, &owned_id)
        })
        .await?;

    crate::cloudflare::purge_file(&state.config, id, &record.filename, &record.storage_host);

    tracing::info!("delete: id={}", id);
    Ok(())
}

#[utoipa::path(
    delete,
    path = "/file/{id}",
    params(
        ("id" = String, Path, description = "The file ID"),
    ),
    request_body(content_type = "application/json", content = inline(serde_json::Value), description = "Set the x-delete-token header instead of a body"),
    responses(
        (status = 204, description = "File deleted"),
        (status = 403, description = "Invalid delete token"),
        (status = 404, description = "File not found"),
    ),
    tag = "Files",
)]
pub async fn delete_file_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let token = headers.get("x-delete-token").and_then(|v| v.to_str().ok());

    delete_file_inner(&state, &user_id, &id, token).await?;

    Ok(StatusCode::NO_CONTENT)
}

/// Rotate a file ID by proving ownership with the delete token, pass custom_id for a slug or leave it empty for a nanoid
#[utoipa::path(
    post,
    path = "/file/{id}/renew",
    params(
        ("id" = String, Path, description = "The current file ID"),
    ),
    request_body(content = RenewRequest, description = "Optional custom_id and x-delete-token header"),
    responses(
        (status = 200, description = "File ID renewed", body = RenewResponse),
        (status = 400, description = "Invalid custom ID"),
        (status = 403, description = "Invalid delete token"),
        (status = 404, description = "File not found"),
        (status = 409, description = "Custom ID already taken"),
        (status = 410, description = "File has expired"),
    ),
    tag = "Files",
)]
pub async fn renew_file_id_handler(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    headers: HeaderMap,
    Path(id): Path<String>,
    body: Option<Json<RenewRequest>>,
) -> Result<Json<RenewResponse>, AppError> {
    let token = headers.get("x-delete-token").and_then(|v| v.to_str().ok());
    let id2 = id.clone();
    let record = state
        .db_call("get_file", move |db| db::get_file(db, &id2))
        .await?
        .ok_or(AppError::NotFound)?;

    let now = chrono::Utc::now().timestamp();
    if record.expires_at < now {
        return Err(AppError::Gone);
    }

    let owner = user_id.clone();
    let owner_file = id.clone();
    let owned = state
        .db_call("client_owns_file", move |db| {
            db::client_owns_file(db, &owner, &owner_file)
        })
        .await?;
    let token_valid = token
        .map(|token| constant_time_eq(&record.delete_token, token))
        .unwrap_or(false);
    if !owned && !token_valid {
        return Err(AppError::Forbidden(
            "file is not owned by this session".into(),
        ));
    }

    let custom_id_raw = body.and_then(|Json(r)| r.custom_id).unwrap_or_default();

    let (new_id, is_custom) = if !custom_id_raw.is_empty() {
        let normalized = crate::utils::normalize_custom_id(&custom_id_raw);
        if !crate::utils::is_valid_id(&normalized) {
            return Err(AppError::BadRequest(format!(
                "invalid custom ID: must be {}-{} chars, alphanumeric, hyphens, or underscores",
                crate::constants::MIN_CUSTOM_ID_LEN,
                crate::constants::MAX_CUSTOM_ID_LEN,
            )));
        }
        let new_id2 = normalized.clone();
        let exists = state
            .db_call("get_file", move |db| db::get_file(db, &new_id2))
            .await?;
        if exists.is_some() {
            return Err(AppError::Conflict("custom ID is already taken".into()));
        }
        (normalized, true)
    } else {
        (nanoid::nanoid!(8), false)
    };

    // Alias old ID before renaming, so old URLs still work.
    let alias_old = id.clone();
    let alias_new = new_id.clone();
    let _ = state
        .db_call("add_alias", move |db| {
            db::add_alias(db, &alias_old, &alias_new)
        })
        .await;

    let old_id = id.clone();
    let new_id2 = new_id.clone();
    let storage_path = format!("remote-{}", new_id2);
    let updated = state
        .db_call("renew_file_id", move |db| {
            db::renew_file_id(db, &old_id, &new_id2, &storage_path)
        })
        .await?;

    if !updated {
        return Err(AppError::NotFound);
    }

    let owner = user_id;
    let old_owned_id = id.clone();
    let new_owned_id = new_id.clone();
    state
        .db_call("update_client_file_id", move |db| {
            db::update_client_file_id(db, &owner, &old_owned_id, &new_owned_id)
        })
        .await?;

    // Tell juicehost to rename the file
    let host_opt = record.storage_host.as_deref();
    if let Err(e) = crate::juicehost::rename_file_on_juicehost(
        &state,
        &id,
        &new_id,
        host_opt,
        Some(&record.delete_token),
    )
    .await
    {
        tracing::error!(
            "renew: juicehost rename failed for id={}, rolling back: {}",
            id,
            e
        );
        rollback_renew_file_id(&state, &new_id, &id).await;
        return Err(AppError::JuicehostRejected(e));
    }

    let url = crate::utils::public_url(
        &state.config.public_base_url,
        &record.storage_host,
        &new_id,
        &record.filename,
    );

    crate::cloudflare::purge_file(&state.config, &id, &record.filename, &record.storage_host);

    tracing::info!(
        "renew: old_id={} new_id={} custom={}",
        id,
        new_id,
        is_custom
    );

    Ok(Json(RenewResponse {
        id: new_id,
        url,
        filename: record.filename,
        expires_at: record.expires_at,
        custom: is_custom,
    }))
}

async fn rollback_renew_file_id(state: &Arc<AppState>, old_id: &str, new_id: &str) {
    let old_id = old_id.to_string();
    let new_id = new_id.to_string();
    let alias_id = new_id.clone();
    let storage_path = format!("remote-{}", new_id);
    let _ = state
        .db_call("renew_file_id_rollback", move |db| {
            db::renew_file_id(db, &old_id, &new_id, &storage_path)
        })
        .await;
    // Clean up the stale alias so old URLs don't redirect to a non-existent ID.
    let _ = state
        .db_call("delete_alias", move |db| db::delete_alias(db, &alias_id))
        .await;
}

/// POST /file/:id/rename via a no-JS form that redirects to /files
/// Accepts a form with `token` and `custom_id` fields.
///
/// Keeps the client's list in sync: the `jb_files` cookie entry swaps its id
/// (a rename keeps the delete token) and the `client_files` registry row is
/// re-keyed, so no-JS /files still lists the file under its new URL.
pub async fn rename_file_form_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    UserId(user_id): UserId,
    Path(id): Path<String>,
    Form(form): Form<RenameForm>,
) -> Result<Response, AppError> {
    let id2 = id.clone();
    let record = state
        .db_call("get_file", move |db| db::get_file(db, &id2))
        .await?
        .ok_or(AppError::NotFound)?;

    let now = chrono::Utc::now().timestamp();
    if record.expires_at < now {
        return Err(AppError::Gone);
    }

    let owner = user_id.clone();
    let owner_file = id.clone();
    let owned = state
        .db_call("client_owns_file", move |db| {
            db::client_owns_file(db, &owner, &owner_file)
        })
        .await?;
    if !owned {
        return Err(AppError::Forbidden(
            "file is not owned by this session".into(),
        ));
    }

    let new_id = if form.custom_id.is_empty() {
        nanoid::nanoid!(8)
    } else {
        let normalized = crate::utils::normalize_custom_id(&form.custom_id);
        if !crate::utils::is_valid_id(&normalized) {
            return Err(AppError::BadRequest(format!(
                "invalid custom ID: must be {}-{} chars, alphanumeric, hyphens, or underscores",
                crate::constants::MIN_CUSTOM_ID_LEN,
                crate::constants::MAX_CUSTOM_ID_LEN,
            )));
        }
        let new_id2 = normalized.clone();
        let exists = state
            .db_call("get_file", move |db| db::get_file(db, &new_id2))
            .await?;
        if exists.is_some() {
            return Err(AppError::Conflict("custom ID is already taken".into()));
        }
        normalized
    };

    let old_id = id.clone();
    let new_id2 = new_id.clone();
    let storage_path = format!("remote-{}", new_id2);

    // Alias old ID before renaming.
    let alias_old = id.clone();
    let alias_new = new_id.clone();
    let _ = state
        .db_call("add_alias", move |db| {
            db::add_alias(db, &alias_old, &alias_new)
        })
        .await;

    let updated = state
        .db_call("renew_file_id", move |db| {
            db::renew_file_id(db, &old_id, &new_id2, &storage_path)
        })
        .await?;

    if !updated {
        return Err(AppError::NotFound);
    }

    let host_opt = record.storage_host.as_deref();
    if let Err(e) = crate::juicehost::rename_file_on_juicehost(
        &state,
        &id,
        &new_id,
        host_opt,
        Some(&record.delete_token),
    )
    .await
    {
        tracing::error!(
            "rename form: juicehost rename failed for id={}, rolling back: {}",
            id,
            e
        );
        rollback_renew_file_id(&state, &new_id, &id).await;
        return Err(AppError::JuicehostRejected(e));
    }

    // Keep the client's file list consistent: swap old id -> new id in the
    // jb_files cookie (token is unchanged by a rename) and the client_files
    // registry, so no-JS /files renders the file under its new URL.
    let cookie_hdr =
        noscript::rename_file_cookie_header(&headers, &id, &new_id, &record.delete_token);
    let client_key = user_id.clone();
    let old_id = id.clone();
    let new_id2 = new_id.clone();
    let _ = state
        .db_call("update_client_file_id", move |db| {
            db::update_client_file_id(db, &client_key, &old_id, &new_id2)
        })
        .await;

    tracing::info!("rename form: old_id={} new_id={}", id, new_id);
    let mut response = Redirect::to("/files.html?renamed=1").into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, cookie_hdr);
    Ok(response)
}

/// GET /internal/alias/:old_id is what juicehost calls to resolve old file IDs
pub async fn resolve_alias_handler(
    State(state): State<Arc<AppState>>,
    Path(old_id): Path<String>,
) -> Result<Json<serde_json::Value>, AppError> {
    let old_id2 = old_id.clone();
    let resolved = state
        .db_call("resolve_alias", move |db| db::resolve_alias(db, &old_id2))
        .await?;

    match resolved {
        Some(new_id) if new_id != old_id => {
            // Look up the current file to build the redirect URL
            let id = new_id.clone();
            let record = state
                .db_call("get_file", move |db| db::get_file(db, &id))
                .await?;
            if let Some(rec) = record {
                let url = crate::utils::public_url(
                    &state.config.public_base_url,
                    &rec.storage_host,
                    &new_id,
                    &rec.filename,
                );
                Ok(Json(serde_json::json!({ "new_id": new_id, "url": url })))
            } else {
                Err(AppError::NotFound)
            }
        }
        _ => Err(AppError::NotFound),
    }
}
