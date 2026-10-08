//! Admin panel: login JWT sessions blablabla

use axum::http::request::Parts;
use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts, Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Json, Response},
    Form, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use utoipa::ToSchema;

use crate::auth;
use crate::db;
use crate::error::AppError;
use crate::state::AppState;
use crate::utils::{ClientIp, TrustedClientIpKeyExtractor};

/// Constants for account lockout
const MAX_FAILURES: u32 = 5;
const LOCKOUT_SECS: i64 = 600;

/// Typed extractor that verifies the admin JWT from the cookie.
#[derive(Clone)]
pub struct AdminUser {
    pub claims: auth::AdminClaims,
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for AdminUser
where
    Arc<AppState>: FromRef<S>,
{
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let app_state = Arc::<AppState>::from_ref(state);

        let token = extract_jwt(&parts.headers)
            .ok_or_else(|| AppError::Unauthorized("not authenticated".into()))?;

        let secret = app_state.config.jwt_secret.clone();
        let claims = tokio::task::spawn_blocking(move || auth::verify_jwt(&token, &secret))
            .await
            .map_err(|_| AppError::Internal("JWT verification task panicked".into()))?
            .map_err(|_| AppError::Unauthorized("invalid token".into()))?;

        let username = claims.sub.clone();
        if !app_state
            .db_call("authenticate_admin", move |db| {
                Ok(db::get_admin_by_username(db, &username)?.is_some())
            })
            .await?
        {
            return Err(AppError::Unauthorized("admin no longer exists".into()));
        }

        Ok(AdminUser { claims })
    }
}

#[derive(Deserialize, ToSchema)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

#[derive(Serialize, ToSchema)]
pub struct LoginResponse {
    pub username: String,
}

#[derive(Serialize, ToSchema)]
pub struct CheckResponse {
    pub ok: bool,
    pub username: String,
}

#[derive(Serialize, ToSchema)]
pub struct AdminFileEntry {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub uploaded_at: i64,
    pub expires_at: i64,
    pub uploader_ip: Option<String>,
    pub uploader_ip_hash: Option<String>,
    pub url: String,
    pub storage_host: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct AdminReportEntry {
    pub id: i64,
    pub file_url: String,
    pub reason: String,
    pub details: String,
    pub reporter_ip: Option<String>,
    pub reporter_ip_hash: Option<String>,
    pub email: Option<String>,
    pub created_at: i64,
}

#[derive(Serialize, ToSchema)]
pub struct AdminFeedbackEntry {
    pub id: i64,
    pub message: String,
    pub email: Option<String>,
    pub reporter_ip: Option<String>,
    pub reporter_ip_hash: Option<String>,
    pub created_at: i64,
}

#[derive(Deserialize)]
pub struct ListParams {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_sort")]
    pub sort: String,
    #[serde(default = "default_dir")]
    pub dir: String,
    #[serde(default = "default_offset")]
    pub offset: i64,
    #[serde(default = "default_limit")]
    pub limit: i64,
}

fn default_sort() -> String {
    "uploaded_at".to_string()
}
fn default_dir() -> String {
    "desc".to_string()
}
fn default_offset() -> i64 {
    0
}
fn default_limit() -> i64 {
    50
}

#[derive(Serialize)]
pub struct PaginatedResponse<T: Serialize> {
    pub items: Vec<T>,
    pub total: i64,
}

/// Paginated list of files (admin view).
#[derive(Serialize, ToSchema)]
pub struct AdminFilesResponse {
    pub items: Vec<AdminFileEntry>,
    pub total: i64,
}

/// Paginated list of abuse reports (admin view).
#[derive(Serialize, ToSchema)]
pub struct AdminReportsResponse {
    pub items: Vec<AdminReportEntry>,
    pub total: i64,
}

/// Paginated list of user feedback (admin view).
#[derive(Serialize, ToSchema)]
pub struct AdminFeedbackResponse {
    pub items: Vec<AdminFeedbackEntry>,
    pub total: i64,
}

/// Paginated list of banned IPs (admin view).
#[derive(Serialize, ToSchema)]
pub struct AdminBansResponse {
    pub items: Vec<db::BanRecord>,
    pub total: i64,
}

/// Paginated list of known custom hosts (admin view).
#[derive(Serialize, ToSchema)]
pub struct AdminHostersResponse {
    pub items: Vec<db::HosterRecord>,
    pub total: i64,
}

pub fn admin_routes(trusted_proxy_cidrs: &[juiceutils::proxy::IpCidr]) -> Router<Arc<AppState>> {
    let login_limiter = {
        let mut builder = GovernorConfigBuilder::default();
        builder.period(std::time::Duration::from_secs_f64(
            crate::constants::ADMIN_RATE_LIMIT_WINDOW_SECS as f64
                / crate::constants::ADMIN_RATE_LIMIT_BURST as f64,
        ));
        builder.burst_size(crate::constants::ADMIN_RATE_LIMIT_BURST);
        let conf = Arc::new(
            builder
                .key_extractor(TrustedClientIpKeyExtractor::new(
                    trusted_proxy_cidrs.to_vec(),
                ))
                .finish()
                .unwrap(),
        );
        Router::new()
            .route("/api/admin/login", axum::routing::post(login_handler))
            .layer(GovernorLayer { config: conf })
    };

    Router::new()
        .merge(login_limiter)
        .route(
            "/api/admin/logout",
            axum::routing::get(logout_handler).post(logout_handler),
        )
        .route("/api/admin/check", axum::routing::get(check_handler))
        .route("/api/admin/files", axum::routing::get(files_handler))
        .route(
            "/api/admin/file/:id",
            axum::routing::delete(delete_file_handler),
        )
        .route("/api/admin/reports", axum::routing::get(reports_handler))
        .route(
            "/api/admin/report/:id",
            axum::routing::delete(delete_report_handler),
        )
        .route(
            "/api/admin/bans",
            axum::routing::get(list_bans_handler).post(ban_ip_handler),
        )
        .route(
            "/api/admin/bans/export",
            axum::routing::get(export_bans_handler),
        )
        .route(
            "/api/admin/bans/import",
            axum::routing::post(import_bans_handler),
        )
        .route(
            "/api/admin/ban/:ip",
            axum::routing::delete(unban_ip_handler),
        )
        .route(
            "/api/admin/announcement",
            axum::routing::get(get_announcement_handler).put(put_announcement_handler),
        )
        .route(
            "/api/admin/feedback",
            axum::routing::get(list_feedback_handler),
        )
        .route(
            "/api/admin/feedback/:id",
            axum::routing::delete(delete_feedback_handler),
        )
        .route(
            "/api/admin/hosters",
            axum::routing::get(list_hosters_handler),
        )
        .route(
            "/api/admin/hoster/ban",
            axum::routing::post(ban_hoster_handler),
        )
        .route(
            "/api/admin/hoster/unban",
            axum::routing::post(unban_hoster_handler),
        )
}

#[utoipa::path(
    post,
    path = "/api/admin/logout",
    responses(
        (status = 302, description = "Redirects to /admin/login and clears the JWT cookie"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn logout_handler(State(state): State<Arc<AppState>>) -> Response {
    let cookie = format!(
        "token=; Path=/; Max-Age=0; HttpOnly; SameSite=Strict{}",
        if state.config.secure_cookies {
            "; Secure"
        } else {
            ""
        }
    );
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, "/admin/login"),
            (header::SET_COOKIE, cookie.as_str()),
        ],
    )
        .into_response()
}

#[utoipa::path(
    post,
    path = "/api/admin/login",
    request_body(content_type = "application/x-www-form-urlencoded", content = inline(LoginForm), description = "Admin username and password"),
    responses(
        (status = 200, description = "Login successful, sets JWT cookie", body = LoginResponse),
        (status = 401, description = "Invalid credentials"),
        (status = 429, description = "Rate limit exceeded"),
    ),
    tag = "Admin",
)]
pub async fn login_handler(
    State(state): State<Arc<AppState>>,
    axum::extract::Extension(client_ip): axum::extract::Extension<ClientIp>,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let username = form.username.trim().to_lowercase();
    if username.is_empty() || form.password.is_empty() {
        return Err(AppError::BadRequest(
            "username and password are required".into(),
        ));
    }

    // Check account lockout from DB: 5 failures within 10 minutes triggers lockout
    let username_for_lockout = username.clone();
    let ip_hash = crate::utils::hash_ip_for_ban(&client_ip.0.to_string(), &state.config.ip_pepper);
    let ip_hash_for_lockout = ip_hash.clone();
    let failed_count = state
        .db_call("count_failed_logins", move |db| {
            db::count_failed_logins_for_ip(
                db,
                &username_for_lockout,
                &ip_hash_for_lockout,
                LOCKOUT_SECS,
            )
        })
        .await
        .unwrap_or(0);

    if failed_count >= MAX_FAILURES {
        tracing::warn!(
            "admin login: account '{}' is locked out ({} failures in {}s)",
            username,
            failed_count,
            LOCKOUT_SECS
        );
        return Err(AppError::TooManyRequests(
            "account temporarily locked due to too many failed attempts".into(),
        ));
    }
    if failed_count > 0 {
        tokio::time::sleep(std::time::Duration::from_millis(
            200_u64.saturating_mul(1_u64 << failed_count.min(4)),
        ))
        .await;
    }

    let username_for_db = username.clone();
    let user = state
        .db_call("get_admin_by_username", move |db| {
            db::get_admin_by_username(db, &username_for_db)
        })
        .await?;
    let Some(user) = user else {
        let uname = username.clone();
        let missing_user_ip_hash = ip_hash.clone();
        state
            .db_call("record_failed_login", move |db| {
                db::record_failed_login_for_ip(db, &uname, &missing_user_ip_hash)
            })
            .await?;
        return Err(AppError::Unauthorized("invalid credentials".into()));
    };

    let password = form.password.clone();
    let hash = user.password_hash.clone();
    let valid = tokio::task::spawn_blocking(move || auth::verify_password(&password, &hash))
        .await
        .map_err(|_| AppError::Internal("password verification task panicked".into()))?
        .map_err(AppError::Internal)?;

    if !valid {
        let uname = username.clone();
        let failed_ip_hash = ip_hash.clone();
        state
            .db_call("record_failed_login", move |db| {
                db::record_failed_login_for_ip(db, &uname, &failed_ip_hash)
            })
            .await?;
        return Err(AppError::Unauthorized("invalid credentials".into()));
    }

    // Successful login: clear failed attempts from DB
    let uname = username.clone();
    let success_ip_hash = ip_hash.clone();
    state
        .db_call("clear_failed_logins", move |db| {
            db::clear_failed_logins_for_ip(db, &uname, &success_ip_hash)
        })
        .await?;

    let username2 = user.username.clone();
    let secret = state.config.jwt_secret.clone();
    let token = tokio::task::spawn_blocking(move || auth::create_jwt(&username2, &secret))
        .await
        .map_err(|_| AppError::Internal("jwt signing task panicked".into()))?
        .map_err(AppError::Internal)?;

    let cookie = format!(
        "token={}; Path=/; Max-Age={}; HttpOnly; SameSite=Strict{}",
        token,
        crate::constants::ADMIN_JWT_COOKIE_MAX_AGE_SECS,
        if state.config.secure_cookies {
            "; Secure"
        } else {
            ""
        }
    );

    let body = Json(LoginResponse {
        username: user.username,
    });

    Ok((StatusCode::OK, [(header::SET_COOKIE, cookie)], body).into_response())
}

#[utoipa::path(
    get,
    path = "/api/admin/check",
    responses(
        (status = 200, description = "JWT is valid", body = CheckResponse),
        (status = 401, description = "Not authenticated or invalid token"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn check_handler(_admin: AdminUser) -> Result<Json<CheckResponse>, AppError> {
    Ok(Json(CheckResponse {
        ok: true,
        username: _admin.claims.sub,
    }))
}

#[utoipa::path(
    get,
    path = "/api/admin/files",
    params(
        ("q" = String, Query, description = "Search query"),
        ("sort" = String, Query, description = "Sort column"),
        ("dir" = String, Query, description = "Sort direction"),
        ("offset" = i64, Query, description = "Offset"),
        ("limit" = i64, Query, description = "Limit"),
    ),
    responses(
        (status = 200, description = "Paginated file list", body = AdminFilesResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn files_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<AdminFilesResponse>, AppError> {
    let search = params.q.clone();
    let sort = params.sort.clone();
    let dir = params.dir.clone();
    let offset = params.offset;
    let limit = params.limit.min(200);

    let (records, total) = state
        .db_call("list_files_paginated", move |db| {
            db::list_files_paginated(db, &search, &sort, &dir, offset, limit)
        })
        .await?;

    let files: Vec<AdminFileEntry> = records
        .into_iter()
        .map(|r| {
            let url = crate::utils::public_url(
                &state.config.public_base_url,
                &r.storage_host,
                &r.id,
                &r.filename,
            );
            let decrypted_ip = r
                .uploader_ip
                .as_ref()
                .and_then(|enc| crate::utils::decrypt_ip(enc, &state.config.ip_encryption_key));
            let ip_hash = decrypted_ip.as_ref().map(|ip| {
                let h = crate::utils::hash_ip_for_ban(ip, &state.config.ip_pepper);
                crate::utils::truncate_hash(&h).to_string()
            });
            AdminFileEntry {
                id: r.id,
                filename: r.filename,
                mime_type: r.mime_type,
                size_bytes: r.size_bytes,
                uploaded_at: r.uploaded_at,
                expires_at: r.expires_at,
                uploader_ip: decrypted_ip,
                uploader_ip_hash: ip_hash,
                url,
                storage_host: r.storage_host,
            }
        })
        .collect();

    Ok(Json(AdminFilesResponse {
        items: files,
        total,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/admin/file/{id}",
    params(
        ("id" = String, Path, description = "The file ID to delete"),
    ),
    responses(
        (status = 204, description = "File deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "File not found"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn delete_file_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let id2 = id.clone();
    let record = state
        .db_call("get_file", move |db| db::get_file(db, &id2))
        .await?
        .ok_or(AppError::NotFound)?;

    let host_opt = record.storage_host.as_deref();
    crate::juicehost::delete_file_on_juicehost(&state, &id, host_opt, Some(&record.delete_token))
        .await
        .map_err(AppError::from_juicehost_error)?;

    let id3 = id.clone();
    state
        .db_call("delete_file", move |db| db::delete_file(db, &id3))
        .await?;

    crate::cloudflare::purge_file(&state.config, &id, &record.filename, &record.storage_host);

    tracing::info!("admin delete: id={}", id);

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/admin/reports",
    params(
        ("q" = String, Query, description = "Search query"),
        ("sort" = String, Query, description = "Sort column"),
        ("dir" = String, Query, description = "Sort direction"),
        ("offset" = i64, Query, description = "Offset"),
        ("limit" = i64, Query, description = "Limit"),
    ),
    responses(
        (status = 200, description = "Paginated report list", body = AdminReportsResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn reports_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<AdminReportsResponse>, AppError> {
    let search = params.q.clone();
    let sort = params.sort.clone();
    let dir = params.dir.clone();
    let offset = params.offset;
    let limit = params.limit.min(200);

    let (records, total) = state
        .db_call("list_reports_paginated", move |db| {
            db::list_reports_paginated(db, &search, &sort, &dir, offset, limit)
        })
        .await?;

    let reports: Vec<AdminReportEntry> = records
        .into_iter()
        .map(|r| {
            let decrypted_ip = r
                .reporter_ip
                .as_ref()
                .and_then(|enc| crate::utils::decrypt_ip(enc, &state.config.ip_encryption_key));
            let ip_hash = decrypted_ip.as_ref().map(|ip| {
                let h = crate::utils::hash_ip_for_ban(ip, &state.config.ip_pepper);
                crate::utils::truncate_hash(&h).to_string()
            });
            AdminReportEntry {
                id: r.id,
                file_url: r.file_url,
                reason: r.reason,
                details: r.details,
                reporter_ip: decrypted_ip,
                reporter_ip_hash: ip_hash,
                email: r.email,
                created_at: r.created_at,
            }
        })
        .collect();

    Ok(Json(AdminReportsResponse {
        items: reports,
        total,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/admin/report/{id}",
    params(
        ("id" = i64, Path, description = "The report ID to delete"),
    ),
    responses(
        (status = 204, description = "Report deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Report not found"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn delete_report_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let deleted = state
        .db_call("delete_report", move |db| db::delete_report(db, id))
        .await?;

    if !deleted {
        return Err(AppError::NotFound);
    }

    tracing::info!("admin delete report: id={}", id);

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/admin/feedback",
    params(
        ("q" = String, Query, description = "Search query"),
        ("sort" = String, Query, description = "Sort column"),
        ("dir" = String, Query, description = "Sort direction"),
        ("offset" = i64, Query, description = "Offset"),
        ("limit" = i64, Query, description = "Limit"),
    ),
    responses(
        (status = 200, description = "Paginated feedback list", body = AdminFeedbackResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn list_feedback_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<AdminFeedbackResponse>, AppError> {
    let search = params.q.clone();
    let sort = params.sort.clone();
    let dir = params.dir.clone();
    let offset = params.offset;
    let limit = params.limit.min(200);

    let (records, total) = state
        .db_call("list_feedback_paginated", move |db| {
            db::list_feedback_paginated(db, &search, &sort, &dir, offset, limit)
        })
        .await?;

    let entries: Vec<AdminFeedbackEntry> = records
        .into_iter()
        .map(|r| {
            let decrypted_ip = r
                .reporter_ip
                .as_ref()
                .and_then(|enc| crate::utils::decrypt_ip(enc, &state.config.ip_encryption_key));
            let ip_hash = decrypted_ip.as_ref().map(|ip| {
                let h = crate::utils::hash_ip_for_ban(ip, &state.config.ip_pepper);
                crate::utils::truncate_hash(&h).to_string()
            });
            AdminFeedbackEntry {
                id: r.id,
                message: r.message,
                email: r.email,
                reporter_ip: decrypted_ip,
                reporter_ip_hash: ip_hash,
                created_at: r.created_at,
            }
        })
        .collect();

    Ok(Json(AdminFeedbackResponse {
        items: entries,
        total,
    }))
}

#[utoipa::path(
    delete,
    path = "/api/admin/feedback/{id}",
    params(
        ("id" = i64, Path, description = "The feedback ID to delete"),
    ),
    responses(
        (status = 204, description = "Feedback deleted"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Feedback not found"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn delete_feedback_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Path(id): Path<i64>,
) -> Result<StatusCode, AppError> {
    let deleted = state
        .db_call("delete_feedback", move |db| db::delete_feedback(db, id))
        .await?;

    if !deleted {
        return Err(AppError::NotFound);
    }

    tracing::info!("admin delete feedback: id={}", id);

    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get,
    path = "/api/admin/bans",
    params(
        ("q" = String, Query, description = "Search query"),
        ("sort" = String, Query, description = "Sort column"),
        ("dir" = String, Query, description = "Sort direction"),
        ("offset" = i64, Query, description = "Offset"),
        ("limit" = i64, Query, description = "Limit"),
    ),
    responses(
        (status = 200, description = "Paginated ban list", body = AdminBansResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn list_bans_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<AdminBansResponse>, AppError> {
    let search = params.q.clone();
    let sort = params.sort.clone();
    let dir = params.dir.clone();
    let offset = params.offset;
    let limit = params.limit.min(200);

    let (records, total) = state
        .db_call("list_bans_paginated", move |db| {
            db::list_bans_paginated(db, &search, &sort, &dir, offset, limit)
        })
        .await?;

    Ok(Json(AdminBansResponse {
        items: records,
        total,
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct BanIpPayload {
    /// Pre-computed HMAC hash from click-to-ban if you have one
    #[serde(default)]
    pub hash: Option<String>,
    /// Raw IP address that gets hashed server-side before storage if hash isn't provided
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub reason: String,
}

#[derive(Serialize, ToSchema)]
pub struct BanExportEntry {
    /// Pre-computed HMAC hash (this is what the DB stores, never a raw IP)
    pub hash: String,
    pub reason: String,
    pub banned_by: String,
    pub banned_at: i64,
}

#[derive(Serialize, ToSchema)]
pub struct BanExportResponse {
    pub entries: Vec<BanExportEntry>,
    pub count: usize,
}

#[derive(Deserialize, ToSchema)]
pub struct BanImportEntry {
    /// Pre-computed HMAC hash; stored as-is
    #[serde(default)]
    pub hash: Option<String>,
    /// Raw IP address; hashed server-side before storage
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub banned_by: Option<String>,
}

#[derive(Deserialize, ToSchema)]
pub struct BanImportPayload {
    pub entries: Vec<BanImportEntry>,
}

#[derive(Serialize, ToSchema)]
pub struct BanImportResponse {
    pub imported: usize,
    #[serde(default)]
    pub errors: Vec<String>,
}

#[utoipa::path(
    post,
    path = "/api/admin/bans",
    request_body(content = BanIpPayload, description = "IP address and optional reason to ban"),
    responses(
        (status = 201, description = "IP banned"),
        (status = 400, description = "IP is required"),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn ban_ip_handler(
    admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BanIpPayload>,
) -> Result<StatusCode, AppError> {
    let ban_hash = if let Some(ref hash) = payload.hash {
        // Pre-computed hash from click-to-ban
        hash.trim().to_string()
    } else if let Some(ref ip) = payload.ip {
        // Raw IP - hash server-side
        crate::utils::hash_ip_for_ban(ip.trim(), &state.config.ip_pepper)
    } else {
        return Err(AppError::BadRequest("either hash or ip is required".into()));
    };

    if ban_hash.is_empty() {
        return Err(AppError::BadRequest("hash or ip is required".into()));
    }

    let reason = if payload.reason.trim().is_empty() {
        "no reason given".to_string()
    } else {
        payload.reason.trim().to_string()
    };

    state.ban_ip(&ban_hash, &reason, &admin.claims.sub).await?;

    tracing::info!(
        "admin banned hash={} reason={} by={}",
        crate::utils::truncate_hash(&ban_hash),
        reason,
        admin.claims.sub
    );

    Ok(StatusCode::CREATED)
}

#[utoipa::path(
    delete,
    path = "/api/admin/ban/{ip}",
    params(
        ("ip" = String, Path, description = "The IP address to unban"),
    ),
    responses(
        (status = 204, description = "IP unbanned"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "IP not found in ban list"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn unban_ip_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Path(ip): Path<String>,
) -> Result<StatusCode, AppError> {
    let deleted = state.unban_ip(&ip).await?;

    if !deleted {
        return Err(AppError::NotFound);
    }

    tracing::info!("admin unbanned hash={}", crate::utils::truncate_hash(&ip));

    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize, ToSchema)]
pub struct BanHosterPayload {
    /// Hostname of the custom juicehost to ban
    pub host: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Deserialize, ToSchema)]
pub struct UnbanHosterPayload {
    /// Hostname of the custom juicehost to unban
    pub host: String,
}

#[utoipa::path(
    get,
    path = "/api/admin/hosters",
    params(
        ("q" = String, Query, description = "Search query"),
        ("sort" = String, Query, description = "Sort column"),
        ("dir" = String, Query, description = "Sort direction"),
        ("offset" = i64, Query, description = "Offset"),
        ("limit" = i64, Query, description = "Limit"),
    ),
    responses(
        (status = 200, description = "Paginated list of known custom hosts", body = AdminHostersResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn list_hosters_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Query(params): Query<ListParams>,
) -> Result<Json<AdminHostersResponse>, AppError> {
    let search = params.q.clone();
    let sort = params.sort.clone();
    let dir = params.dir.clone();
    let offset = params.offset;
    let limit = params.limit.min(200);

    let (records, total) = state
        .db_call("list_hosters_paginated", move |db| {
            db::list_hosters_paginated(db, &search, &sort, &dir, offset, limit)
        })
        .await?;

    Ok(Json(AdminHostersResponse {
        items: records,
        total,
    }))
}

#[utoipa::path(
    post,
    path = "/api/admin/hoster/ban",
    request_body(content = BanHosterPayload, description = "Host to ban and optional reason"),
    responses(
        (status = 200, description = "Host banned"),
        (status = 400, description = "Host is required"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Host not found in known hosters"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn ban_hoster_handler(
    admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BanHosterPayload>,
) -> Result<StatusCode, AppError> {
    let host = payload.host.trim().trim_end_matches('/').to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest("host is required".into()));
    }

    let reason = if payload.reason.trim().is_empty() {
        "no reason given".to_string()
    } else {
        payload.reason.trim().to_string()
    };
    let banned_by = admin.claims.sub.clone();
    let host_value = host.clone();
    let reason_value = reason.clone();
    let banned_by_value = banned_by.clone();

    let updated = state
        .db_call("set_hoster_banned", move |db| {
            db::set_hoster_banned(db, &host_value, true, &reason_value, &banned_by_value)
        })
        .await?;
    if !updated {
        return Err(AppError::NotFound);
    }

    tracing::info!(
        "admin banned host={} reason={} by={}",
        host,
        reason,
        banned_by
    );

    Ok(StatusCode::OK)
}

#[utoipa::path(
    post,
    path = "/api/admin/hoster/unban",
    request_body(content = UnbanHosterPayload, description = "Host to unban"),
    responses(
        (status = 200, description = "Host unbanned"),
        (status = 400, description = "Host is required"),
        (status = 401, description = "Not authenticated"),
        (status = 404, description = "Host not found in known hosters"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn unban_hoster_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<UnbanHosterPayload>,
) -> Result<StatusCode, AppError> {
    let host = payload.host.trim().trim_end_matches('/').to_string();
    if host.is_empty() {
        return Err(AppError::BadRequest("host is required".into()));
    }

    let host_value = host.clone();

    let updated = state
        .db_call("set_hoster_banned", move |db| {
            db::set_hoster_banned(db, &host_value, false, "", "")
        })
        .await?;
    if !updated {
        return Err(AppError::NotFound);
    }

    tracing::info!("admin unbanned host={}", host);

    Ok(StatusCode::OK)
}

#[utoipa::path(
    get,
    path = "/api/admin/bans/export",
    responses(
        (status = 200, description = "Full ban list as a JSON document that can be re-imported"),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn export_bans_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<BanExportResponse>, AppError> {
    let records = state
        .db_call("list_banned_ips", db::list_banned_ips)
        .await?;

    let entries = records
        .into_iter()
        .map(|r| BanExportEntry {
            hash: r.ip,
            reason: r.reason,
            banned_by: r.banned_by,
            banned_at: r.banned_at,
        })
        .collect::<Vec<_>>();
    let count = entries.len();

    tracing::info!("admin exported {} bans", count);

    Ok(Json(BanExportResponse { entries, count }))
}

#[utoipa::path(
    post,
    path = "/api/admin/bans/import",
    request_body(content = BanImportPayload, description = "A list of bans to import. Each entry needs `hash` (pre-computed) or `ip` (raw IP, hashed server-side)."),
    responses(
        (status = 201, description = "Bans imported"),
        (status = 400, description = "No valid entries to import"),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn import_bans_handler(
    admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<BanImportPayload>,
) -> Result<(StatusCode, Json<BanImportResponse>), AppError> {
    if payload.entries.is_empty() {
        return Err(AppError::BadRequest(
            "entries must contain at least one ban".into(),
        ));
    }

    let mut bans = Vec::with_capacity(payload.entries.len());
    let mut errors = Vec::new();

    for (idx, entry) in payload.entries.into_iter().enumerate() {
        let ban_hash = if let Some(ref hash) = entry.hash {
            hash.trim().to_string()
        } else if let Some(ref ip) = entry.ip {
            crate::utils::hash_ip_for_ban(ip.trim(), &state.config.ip_pepper)
        } else {
            errors.push(format!("entry {}: missing hash or ip", idx + 1));
            continue;
        };

        if ban_hash.is_empty() {
            errors.push(format!("entry {}: empty hash or ip", idx + 1));
            continue;
        }

        let reason = if entry.reason.trim().is_empty() {
            "no reason given".to_string()
        } else {
            entry.reason.trim().to_string()
        };

        let banned_by = entry.banned_by.unwrap_or_default();
        let banned_by = if banned_by.trim().is_empty() {
            admin.claims.sub.clone()
        } else {
            banned_by.trim().to_string()
        };

        bans.push(db::ImportBan {
            ip: ban_hash,
            reason,
            banned_by,
        });
    }

    if bans.is_empty() {
        let suffix = if errors.is_empty() {
            String::new()
        } else {
            format!(": {}", errors.join("; "))
        };
        return Err(AppError::BadRequest(format!(
            "no valid entries to import{suffix}"
        )));
    }

    let imported = state.import_bans(bans).await?;

    tracing::info!(
        "admin imported {} bans ({} errors) by {}",
        imported,
        errors.len(),
        admin.claims.sub
    );

    Ok((
        StatusCode::CREATED,
        Json(BanImportResponse { imported, errors }),
    ))
}

// == announcement ==

#[derive(Serialize, ToSchema)]
pub struct AnnouncementResponse {
    pub id: i64,
    pub message: String,
    pub link_url: String,
    pub mode: String,
    pub is_active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Deserialize, ToSchema)]
pub struct AnnouncementPayload {
    pub message: String,
    #[serde(default)]
    pub link_url: String,
    #[serde(default = "default_warning")]
    pub mode: String,
    #[serde(default = "default_true")]
    pub is_active: bool,
}

fn default_true() -> bool {
    true
}

fn default_warning() -> String {
    "warning".to_string()
}

#[utoipa::path(
    get,
    path = "/api/admin/announcement",
    responses(
        (status = 200, description = "Current announcement", body = AnnouncementResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn get_announcement_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let announcement = state
        .db_call("get_announcement", db::get_announcement)
        .await?;

    match announcement {
        Some(a) => Ok(Json(serde_json::json!({
            "id": a.id,
            "message": a.message,
            "link_url": a.link_url,
            "mode": a.mode,
            "is_active": a.is_active,
            "created_at": a.created_at,
            "updated_at": a.updated_at,
        }))),
        None => Ok(Json(serde_json::json!(null))),
    }
}

#[utoipa::path(
    put,
    path = "/api/admin/announcement",
    request_body(content = AnnouncementPayload, description = "Announcement to set"),
    responses(
        (status = 200, description = "Announcement updated", body = AnnouncementResponse),
        (status = 401, description = "Not authenticated"),
    ),
    security(),
    tag = "Admin",
)]
pub async fn put_announcement_handler(
    _admin: AdminUser,
    State(state): State<Arc<AppState>>,
    Json(payload): Json<AnnouncementPayload>,
) -> Result<Json<serde_json::Value>, AppError> {
    let message = payload.message.trim().to_string();
    let link_url = payload.link_url.trim().to_string();
    let mode = payload.mode.trim().to_string();

    if message.len() > 500 {
        return Err(AppError::BadRequest(
            "message must be 500 characters or fewer".into(),
        ));
    }

    if !link_url.is_empty() && !link_url.starts_with("http://") && !link_url.starts_with("https://")
    {
        return Err(AppError::BadRequest(
            "link_url must start with http:// or https://".into(),
        ));
    }
    if link_url.len() > 2048 {
        return Err(AppError::BadRequest(
            "link_url must be 2048 characters or fewer".into(),
        ));
    }

    let is_active = payload.is_active;

    let announcement = state
        .db_call("upsert_announcement", move |db| {
            db::upsert_announcement(db, &message, &link_url, &mode, is_active)
        })
        .await?;

    tracing::info!(
        "admin updated announcement: id={} active={}",
        announcement.id,
        announcement.is_active
    );

    crate::cloudflare::purge_urls(
        Arc::clone(&state.config),
        vec![
            format!("{}/", state.config.juiceback_origin),
            state.config.juiceback_origin.clone(),
        ],
    )
    .await;

    Ok(Json(serde_json::json!({
        "id": announcement.id,
        "message": announcement.message,
        "link_url": announcement.link_url,
        "mode": announcement.mode,
        "is_active": announcement.is_active,
        "created_at": announcement.created_at,
        "updated_at": announcement.updated_at,
    })))
}

fn extract_jwt(headers: &HeaderMap) -> Option<String> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    for pair in cookie.split(';') {
        let pair = pair.trim();
        if let Some(value) = pair.strip_prefix("token=") {
            return Some(value.to_string());
        }
    }
    None
}
