//! glues every HTTP handler together into one router with middleware.
//! its like the conductor of an orchestra but the orchestra is HTTP requests

use axum::{
    async_trait,
    body::Body,
    extract::{DefaultBodyLimit, FromRequestParts, Query, State},
    http::{header, request::Parts, HeaderMap, HeaderValue, Method, Request, Response, StatusCode},
    middleware,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use std::net::SocketAddr;
use sentry::integrations::tower::NewSentryLayer;
use serde_json::json;
use std::sync::Arc;
use tower_governor::{governor::GovernorConfigBuilder, GovernorLayer};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use utoipa::OpenApi;

use crate::db;
use crate::error::AppError;
use crate::routes::api_doc::ApiDoc;
use crate::state::AppState;
use crate::utils::{ban_check_middleware, client_ip_middleware, TrustedClientIpKeyExtractor};

/// Axum extractor for user identity that gets injected by middleware and used by handlers
#[derive(Clone)]
pub struct UserId(pub String);

#[derive(Debug)]
pub struct UserIdMissing;

impl IntoResponse for UserIdMissing {
    fn into_response(self) -> Response<Body> {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            "UserId not set by middleware",
        )
            .into_response()
    }
}

#[async_trait]
impl<S: Send + Sync> FromRequestParts<S> for UserId {
    type Rejection = UserIdMissing;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        parts
            .extensions
            .get::<UserId>()
            .cloned()
            .ok_or(UserIdMissing)
    }
}

/// Resolve an opaque server-side session, creating one when necessary.
async fn user_identity_middleware(
    State(state): State<Arc<AppState>>,
    mut req: Request<Body>,
    next: middleware::Next,
) -> Response<Body> {
    let now = chrono::Utc::now().timestamp();
    let session_token = crate::auth::cookie_value(req.headers(), crate::auth::SESSION_COOKIE_NAME);
    let user_id = if let Some(token) = session_token.as_ref() {
        let hash = crate::auth::session_token_hash(token);
        state
            .db_call("resolve_session", move |db| {
                db::resolve_session(db, &hash, now)
            })
            .await
            .ok()
            .flatten()
    } else {
        None
    };
    let needs_cookie = user_id.is_none();
    let user_id = user_id.unwrap_or_else(|| {
        crate::auth::verify_legacy_user_cookie(req.headers(), &state.config.jwt_secret)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
    });
    let new_token = if needs_cookie {
        let token = crate::auth::new_session_token();
        let hash = crate::auth::session_token_hash(&token);
        let uid = user_id.clone();
        if state
            .db_call("create_session", move |db| {
                db::create_session(db, &hash, &uid, now, now + crate::auth::SESSION_MAX_AGE)
            })
            .await
            .is_ok()
        {
            Some(token)
        } else {
            None
        }
    } else {
        None
    };
    let had_legacy =
        crate::auth::cookie_value(req.headers(), crate::auth::LEGACY_USER_COOKIE_NAME).is_some();

    req.extensions_mut().insert(UserId(user_id.clone()));

    let mut response = next.run(req).await;

    if let Some(token) = new_token {
        let cookie_val = crate::auth::create_session_cookie(&token, state.config.secure_cookies);
        if let Ok(header_val) = HeaderValue::from_str(&cookie_val) {
            response
                .headers_mut()
                .append(header::SET_COOKIE, header_val);
        }
    }
    if had_legacy {
        if let Ok(value) = HeaderValue::from_str(&crate::auth::clear_legacy_user_cookie(
            state.config.secure_cookies,
        )) {
            response.headers_mut().append(header::SET_COOKIE, value);
        }
    }

    response
}

pub mod admin;
pub mod api_doc;
pub mod fetch;
pub mod manage;
pub mod noscript;
pub mod pairing;
pub mod presence;
pub mod register;
pub mod tus;
pub mod upload;

/// Middleware that applies security-related response headers.
#[cfg(feature = "quic")]
async fn add_security_headers(req: Request<Body>, next: middleware::Next) -> impl IntoResponse {
    juiceutils::add_security_headers(req, next).await
}

#[cfg(not(feature = "quic"))]
async fn add_security_headers(req: Request<Body>, next: middleware::Next) -> impl IntoResponse {
    let mut response = next.run(req).await;
    let headers = response.headers_mut();

    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'",
        ),
    );
    headers.insert(
        "referrer-policy",
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static("geolocation=(), microphone=(), camera=()"),
    );

    response
}

/// tower_governor answers 429s with its own plain-text body, which no client
/// understands. Rewrite those responses as our RATE_LIMITED AppError JSON while
/// keeping the governor's x-ratelimit-after hint (mirrored to Retry-After).
async fn rate_limit_error_mapper(req: Request<Body>, next: middleware::Next) -> Response<Body> {
    let response = next.run(req).await;
    if response.status() != StatusCode::TOO_MANY_REQUESTS {
        return response;
    }
    let mut mapped = AppError::RateLimited.into_response();
    if let Some(after) = response.headers().get("x-ratelimit-after").cloned() {
        mapped.headers_mut().insert("x-ratelimit-after", after.clone());
        mapped.headers_mut().insert(header::RETRY_AFTER, after);
    }
    mapped
}

/// Wrap a request body so stalls fail instead of parking the handler:
/// any `inactivity` window with zero bytes (or `total` overall) turns the
/// stream into an error. Upload handlers already treat body errors as
/// retryable session failures, so a half-dead peer (headers sent, body
/// never arrives, connection never closed) tears its session down instead
/// of holding a handler task — and its locks — forever.
pub(crate) fn deadline_body(
    body: Body,
    inactivity: std::time::Duration,
    total: std::time::Duration,
) -> Body {
    let deadline = tokio::time::Instant::now() + total;
    let stream = futures::stream::unfold(Some(body.into_data_stream()), move |state| async move {
        let mut stream = state?;
        let wait = inactivity.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
        if wait.is_zero() {
            return Some((
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request body total deadline exceeded",
                )),
                None,
            ));
        }
        match tokio::time::timeout(wait, futures::StreamExt::next(&mut stream)).await {
            Ok(Some(Ok(chunk))) => Some((Ok(chunk), Some(stream))),
            Ok(Some(Err(error))) => Some((Err(std::io::Error::other(error)), None)),
            Ok(None) => None,
            Err(_) => Some((
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "request body inactivity deadline exceeded",
                )),
                None,
            )),
        }
    });
    Body::from_stream(stream)
}

/// Middleware for the byte-ingest routers (multipart + TUS): bodies must
/// keep moving. 120s of silence means the peer is gone; the 6h total is a
/// backstop only (slow-but-moving uploads must never trip it).
async fn request_body_deadline(req: Request<Body>, next: middleware::Next) -> axum::response::Response {
    let (parts, body) = req.into_parts();
    let body = deadline_body(
        body,
        std::time::Duration::from_secs(120),
        std::time::Duration::from_secs(6 * 3600),
    );
    next.run(Request::from_parts(parts, body)).await
}

/// Wire up every route + middleware. upload, tus, manage, admin, the works.
pub fn build_router(state: Arc<AppState>) -> Router {
    let mut upload_builder = GovernorConfigBuilder::default();
    upload_builder.period(std::time::Duration::from_secs_f64(
        60.0 / state.config.rate_limit_per_minute.max(1) as f64,
    ));
    upload_builder.burst_size(state.config.rate_limit_per_minute.max(1));
    let upload_governor_conf = Arc::new(
        upload_builder
            .key_extractor(TrustedClientIpKeyExtractor::new(
                state.config.trusted_proxy_cidrs.clone(),
            ))
            .finish()
            .unwrap(),
    );

    let mut action_builder = GovernorConfigBuilder::default();
    action_builder.period(std::time::Duration::from_secs(6));
    action_builder.burst_size(10);
    let action_governor_conf = Arc::new(
        action_builder
            .key_extractor(TrustedClientIpKeyExtractor::new(
                state.config.trusted_proxy_cidrs.clone(),
            ))
            .finish()
            .unwrap(),
    );
    let mut sse_builder = GovernorConfigBuilder::default();
    sse_builder.period(std::time::Duration::from_secs_f64(
        60.0 / crate::constants::MAX_SSE_CONNECTIONS_PER_IP as f64,
    ));
    sse_builder.burst_size(crate::constants::MAX_SSE_CONNECTIONS_PER_IP);
    let sse_governor_conf = Arc::new(
        sse_builder
            .key_extractor(TrustedClientIpKeyExtractor::new(
                state.config.trusted_proxy_cidrs.clone(),
            ))
            .finish()
            .unwrap(),
    );

    // Cobalt fetches are expensive (media downloads); 3 per minute per IP.
    let mut fetch_builder = GovernorConfigBuilder::default();
    fetch_builder.period(std::time::Duration::from_secs_f64(
        60.0 / crate::constants::COBALT_FETCH_RATE_LIMIT_PER_MINUTE as f64,
    ));
    fetch_builder.burst_size(crate::constants::COBALT_FETCH_RATE_LIMIT_PER_MINUTE);
    let fetch_governor_conf = Arc::new(
        fetch_builder
            .key_extractor(TrustedClientIpKeyExtractor::new(
                state.config.trusted_proxy_cidrs.clone(),
            ))
            .finish()
            .unwrap(),
    );

    let cors = {
        let origins: Vec<HeaderValue> = state
            .config
            .cors_origins
            .iter()
            .filter_map(|o| match o.parse::<HeaderValue>() {
                Ok(v) => Some(v),
                Err(_) => {
                    tracing::warn!("invalid CORS origin ignored: {}", o);
                    None
                }
            })
            .collect();
        CorsLayer::new()
            .allow_origin(origins)
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE, Method::OPTIONS])
            .allow_credentials(true)
            .allow_headers([
                header::CONTENT_TYPE,
                header::AUTHORIZATION,
                header::HeaderName::from_static("x-file-encoding"),
                header::HeaderName::from_static("upload-length"),
                header::HeaderName::from_static("upload-offset"),
                header::HeaderName::from_static("upload-metadata"),
                header::HeaderName::from_static("tus-resumable"),
                header::HeaderName::from_static("x-delete-token"),
            ])
    };

    let max_size_bytes = state
        .jh_config
        .read()
        .unwrap()
        .as_ref()
        .map(|c| c.max_file_size_bytes as usize)
        .unwrap_or(500 * 1024 * 1024);

    let upload_router = Router::new()
        .route("/", post(upload::upload_handler))
        .route("/reserve", post(upload::reserve_upload_handler))
        .route(
            "/direct/reserve",
            post(upload::direct_upload_reserve_handler),
        )
        .route(
            "/direct/complete",
            post(upload::direct_upload_complete_handler),
        )
        .route(
            "/direct/parallel/reserve",
            post(upload::direct_parallel_reserve_handler),
        )
        .route(
            "/direct/parallel/complete",
            post(upload::direct_parallel_complete_handler),
        )
        .route(
            "/ultrafast/reserve",
            post(upload::ultrafast_reserve_handler),
        )
        .route(
            "/ultrafast/complete",
            post(upload::ultrafast_complete_handler),
        )
        .layer(GovernorLayer {
            config: upload_governor_conf,
        })
        .layer(middleware::from_fn(rate_limit_error_mapper))
        .layer(middleware::from_fn(request_body_deadline))
        .layer(DefaultBodyLimit::max(
            max_size_bytes + crate::constants::UPLOAD_SIZE_OVERHEAD,
        ));

    let tus_router = tus::tus_routes()
        .layer(middleware::from_fn(request_body_deadline))
        .layer(DefaultBodyLimit::max(
            max_size_bytes + crate::constants::UPLOAD_SIZE_OVERHEAD,
        ));

    let abuse_routes = Router::new()
        .route("/api/report", post(noscript::report_submit_handler))
        .route("/api/feedback", post(noscript::feedback_submit_handler))
        .route("/api/pair/generate", post(pairing::generate_code))
        .route("/api/pair/verify", post(pairing::verify_code))
        .layer(GovernorLayer {
            config: action_governor_conf,
        });

    let small_payload_routes = Router::new()
        .route("/api/owned-files", post(manage::owned_files_handler))
        .route(
            "/api/client-files",
            get(manage::get_client_files_handler).post(manage::put_client_files_handler),
        )
        .route(
            "/api/client-files/migrate",
            post(manage::put_client_files_handler),
        )
        .route("/api/register", post(register::register_handler))
        .merge(abuse_routes)
        .layer(DefaultBodyLimit::max(64 * 1024));

    let presence_route = Router::new()
        .route("/api/presence", get(presence::presence_sse))
        .layer(GovernorLayer {
            config: sse_governor_conf,
        });

    // Only the start endpoint is rate limited; status polling must stay cheap
    // for the browser's 2s poll loop.
    let fetch_start_route = Router::new()
        .route("/api/fetch", post(fetch::fetch_start_handler))
        .layer(GovernorLayer {
            config: fetch_governor_conf,
        })
        .layer(DefaultBodyLimit::max(64 * 1024));

    // Service list is served from a server-side cache; no rate limit needed.
    let fetch_services_route = Router::new()
        .route("/api/fetch/services", get(fetch::fetch_services_handler))
        .layer(DefaultBodyLimit::max(64 * 1024));

    Router::new()
        .nest("/upload", upload_router)
        .merge(tus_router)
        .route("/file/:id/info", get(manage::file_info_handler))
        .route("/file/:id/renew", post(manage::renew_file_id_handler))
        .route("/file/:id", delete(manage::delete_file_handler))
        // internal endpoints (for juicehost)
        .route(
            "/internal/file/:id/status",
            get(upload::file_status_handler),
        )
        .route(
            "/internal/alias/:old_id",
            get(manage::resolve_alias_handler),
        )
        .route("/internal/ban-snapshot", get(ban_snapshot_handler))
        // no-JS HTML endpoints
        .route("/file/:id/delete", post(manage::delete_file_form_handler))
        .route("/file/:id/rename", post(manage::rename_file_form_handler))
        .merge(small_payload_routes)
        .route("/api/health", get(health_handler))
        .route("/api/config", get(config_handler))
        .route("/api/ip", get(ip_handler))
        .route("/api/fetch/:job_id", get(fetch::fetch_status_handler))
        .route("/api/validate-host", post(validate_host_handler))
        .route("/api/announcement", get(announcement_handler))
        .route("/api/ban-status", get(ban_status_handler))
        .route("/api/openapi.json", get(openapi_json_handler))
        // juicebox-plus pairing
        .route("/api/device", get(pairing::list_devices))
        .route("/api/device/:id", delete(pairing::unpair_device))
        // device WebSocket + presence SSE
        .route("/api/device/ws", get(presence::device_ws))
        .route("/api/device/status", get(presence::device_status))
        .route(
            "/api/device/connected",
            get(presence::list_connected_devices),
        )
        .route("/api/device/ping", post(presence::ping_device))
        .route("/api/device/upload", post(upload::device_upload_handler))
        .merge(fetch_start_route)
        .merge(fetch_services_route)
        .merge(presence_route)
        .merge(admin::admin_routes(&state.config.trusted_proxy_cidrs))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            ban_check_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            client_ip_middleware,
        ))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&state),
            user_identity_middleware,
        ))
        .layer(middleware::from_fn(add_security_headers))
        .layer(
            TraceLayer::new_for_http().make_span_with(|request: &Request<Body>| {
                tracing::info_span!(
                    "http.request",
                    method = %request.method(),
                    path = request.uri().path(),
                    version = ?request.version(),
                    client_ip = tracing::field::Empty,
                )
            }),
        )
        .layer(NewSentryLayer::<Request<Body>>::new_from_top())
        .layer(cors)
        .with_state(state)
}

#[utoipa::path(
    get,
    path = "/api/health",
    responses(
        (status = 200, description = "Service health status (juicehost status is probed with a short timeout)", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn health_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    let mut body = json!({
        "status": "ok",
        "juiceback": "ok",
    });

    // Report juicehost liveness from a real probe. Skip when we are the probed
    // side (see juicehost's /api/health) to avoid an infinite probe loop.
    if !headers.contains_key("x-health-probe") {
        body["juicehost"] = json!(probe_juicehost(&state).await);
    }

    sentry::metrics::counter("juiceback.health", 1).capture();

    Json(body)
}

/// Probe the juicehost health endpoint with a short timeout.
async fn probe_juicehost(state: &Arc<AppState>) -> &'static str {
    if state.config.juicehost_url.is_empty() {
        return "unknown";
    }
    let url = format!(
        "{}/api/health",
        state.config.juicehost_url.trim_end_matches('/')
    );
    let Ok(resp) = state
        .http
        .get(&url)
        .headers(state.juicehost_headers.clone())
        .header("x-health-probe", "1")
        .timeout(std::time::Duration::from_secs(2))
        .send()
        .await
    else {
        return "unreachable";
    };
    if !resp.status().is_success() {
        return "unreachable";
    }
    match resp.json::<serde_json::Value>().await {
        Ok(v) if v.get("status").and_then(|s| s.as_str()) == Some("ok") => "ok",
        _ => "degraded",
    }
}

#[utoipa::path(
    get,
    path = "/api/config",
    responses(
        (status = 200, description = "Public server configuration", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn config_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let jh = state.jh_config.read().unwrap();

    match jh.as_ref() {
        Some(cfg) => {
            let danger_level_str = cfg.danger_level.as_str();
            Json(json!({
                "status": "ok",
                "max_file_size_bytes": cfg.max_file_size_bytes,
                "default_ttl_hours": cfg.default_ttl_hours,
                "allowed_ttl_hours": cfg.allowed_ttl_hours,
                "danger_level": danger_level_str,
                "quick_link": cfg.quick_link,
                "custom_id": cfg.custom_id_enabled,
                "ultrafast": cfg.ultrafast,
                "direct_upload": state.config.direct_upload_enabled
                    && !state.config.public_juicehost_url.is_empty(),
                "upload_mode": if state.config.dte_enabled
                    && state.config.direct_upload_enabled
                    && !state.config.public_juicehost_url.is_empty()
                {
                    "direct-prefer"
                } else {
                    "standard"
                },
                "cobalt": state.config.cobalt_enabled,
                "fetch_rate_limit_per_minute": crate::constants::COBALT_FETCH_RATE_LIMIT_PER_MINUTE,
                "public_base_url": state.config.public_base_url,
                "rate_limit_per_minute": state.config.rate_limit_per_minute,
                "quic": cfg!(feature = "quic"),
                "juicehost_url": state.config.juicehost_url,
            }))
        }
        None => {
            // Degraded mode: return error status so frontend shows offline banner
            Json(json!({
                "status": "degraded",
                "error": "juicehost config unavailable",
            }))
        }
    }
}

#[utoipa::path(
    get,
    path = "/api/ip",
    responses(
        (status = 200, description = "Client IP address and version", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn ip_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<SocketAddr>,
) -> Json<serde_json::Value> {
    let ip = crate::utils::client_ip(&headers, peer.ip(), &state);
    Json(json!({
        "ip": ip.to_string(),
        "version": if ip.is_ipv6() { "ipv6" } else { "ipv4" },
    }))
}

/// Validate that a custom juicehost URL is reachable and returns valid config.
///
/// Every call also records the host in the known-hosters table (first use
/// creates the entry), and banned hosts are rejected up front.
#[utoipa::path(
    post,
    path = "/api/validate-host",
    request_body(content = serde_json::Value, description = "JSON with a \"host\" field containing the juicehost URL"),
    responses(
        (status = 200, description = "Host validation result", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn validate_host_handler(
    State(state): State<Arc<AppState>>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, AppError> {
    let host = body
        .get("host")
        .and_then(|v| v.as_str())
        .ok_or_else(|| AppError::BadRequest("host field required".into()))?;

    let target = crate::juicehost::custom_juicehost_target(host)
        .await
        .map_err(AppError::BadRequest)?;
    let host = target.base_url.clone();

    // Reject banned hosts before touching them.
    let host_key = host.to_string();
    let banned = state
        .db_call("get_hoster", move |db| crate::db::get_hoster(db, &host_key))
        .await?
        .map(|h| h.banned)
        .unwrap_or(false);

    if banned {
        let host_key = host.to_string();
        let _ = state
            .db_call("upsert_hoster", move |db| {
                crate::db::upsert_hoster(db, &host_key, "banned", "")
            })
            .await;
        return Ok(Json(json!({
            "status": "banned",
            "host": host,
        })));
    }

    let url = format!("{}/api/config", target.base_url);
    let resp = match target.client.get(&url).headers(target.headers).send().await {
        Ok(resp) => resp,
        Err(e) => {
            let msg = format!("failed to reach host: {}", e);
            let host_key = host.to_string();
            let value = msg.clone();
            let _ = state
                .db_call("upsert_hoster", move |db| {
                    crate::db::upsert_hoster(db, &host_key, "error", &value)
                })
                .await;
            return Ok(Json(json!({
                "status": "error",
                "host": host,
                "error": msg,
            })));
        }
    };

    if !resp.status().is_success() {
        let msg = format!("host returned status {}", resp.status());
        let host_key = host.to_string();
        let value = msg.clone();
        let _ = state
            .db_call("upsert_hoster", move |db| {
                crate::db::upsert_hoster(db, &host_key, "error", &value)
            })
            .await;
        return Ok(Json(json!({
            "status": "error",
            "host": host,
            "error": msg,
        })));
    }

    let cfg: serde_json::Value = match resp.json().await {
        Ok(cfg) => cfg,
        Err(e) => {
            let msg = format!("invalid config response: {}", e);
            let host_key = host.to_string();
            let value = msg.clone();
            let _ = state
                .db_call("upsert_hoster", move |db| {
                    crate::db::upsert_hoster(db, &host_key, "error", &value)
                })
                .await;
            return Ok(Json(json!({
                "status": "error",
                "host": host,
                "error": msg,
            })));
        }
    };

    let host_key = host.to_string();
    let _ = state
        .db_call("upsert_hoster", move |db| {
            crate::db::upsert_hoster(db, &host_key, "ok", "")
        })
        .await;

    Ok(Json(json!({
        "status": "valid",
        "host": host,
        "config": cfg,
        "max_file_size_bytes": cfg.get("max_file_size_bytes").and_then(|v| v.as_u64()).unwrap_or(0),
        "default_ttl_hours": cfg.get("default_ttl_hours").and_then(|v| v.as_f64()),
        "allowed_ttl_hours": cfg.get("allowed_ttl_hours"),
        "danger_level": cfg.get("danger_level").and_then(|v| v.as_str()).unwrap_or("high"),
        "quick_link": cfg.get("quick_link").and_then(|v| v.as_bool()),
        "custom_id": cfg.get("custom_id").and_then(|v| v.as_bool()),
        "ultrafast": cfg.get("ultrafast").and_then(|v| v.as_bool()),
        "quic": cfg.get("quic").and_then(|v| v.as_bool()),
        "cobalt": cfg.get("cobalt").and_then(|v| v.as_bool()),
        "upload_mode": cfg.get("upload_mode").and_then(|v| v.as_str()),
    })))
}

#[utoipa::path(
    get,
    path = "/api/announcement",
    responses(
        (status = 200, description = "Active site announcement, or null", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn announcement_handler(State(state): State<Arc<AppState>>) -> Json<serde_json::Value> {
    let announcement = state
        .db_call("get_active_announcement", |db| {
            crate::db::get_active_announcement(db)
        })
        .await
        .ok()
        .flatten();

    match announcement {
        Some(a) => Json(json!({
            "message": a.message,
            "link_url": a.link_url,
            "mode": a.mode,
        })),
        None => Json(json!(null)),
    }
}

#[utoipa::path(
    get,
    path = "/api/ban-status",
    params(
        ("ip" = String, Query, description = "IP address to check"),
    ),
    responses(
        (status = 200, description = "Ban status for the given IP", body = serde_json::Value),
    ),
    tag = "General",
)]
pub async fn ban_status_handler(
    State(state): State<Arc<AppState>>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Result<Json<serde_json::Value>, AppError> {
    let ip_str = params
        .get("ip")
        .filter(|s| !s.is_empty())
        .cloned()
        .ok_or_else(|| AppError::BadRequest("ip parameter required".into()))?;

    let hashed = crate::utils::hash_ip_for_ban(&ip_str, &state.config.ip_pepper);

    let ban = state
        .db_call("get_ban_record", move |db| {
            crate::db::get_ban_record(db, &hashed)
        })
        .await
        .ok()
        .flatten();

    match ban {
        Some(b) => Ok(Json(json!({
            "banned": true,
            "reason": b.reason,
            "banned_at": b.banned_at,
        }))),
        None => Ok(Json(json!({ "banned": false }))),
    }
}

/// Internal endpoint for juicehost: returns the current ban hashes plus the
/// pepper juiceback uses, so a securely-linked juicehost can enforce the same
/// ban list without a database. Authenticated with the shared juicehost API key.
#[utoipa::path(
    get,
    path = "/internal/ban-snapshot",
    responses(
        (status = 200, description = "Ban hash list and pepper", body = serde_json::Value),
        (status = 403, description = "Invalid or missing API key"),
    ),
    tag = "Internal",
    security(),
)]
pub async fn ban_snapshot_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>, AppError> {
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

    let hashes: Vec<String> = state.banned_ips.iter().map(|e| e.key().clone()).collect();
    let pepper = state.config.ip_pepper.clone();

    tracing::debug!("internal ban snapshot: {} hashes", hashes.len());

    Ok(Json(json!({
        "pepper": pepper,
        "hashes": hashes,
        "count": hashes.len(),
    })))
}

async fn openapi_json_handler() -> (axum::http::header::HeaderMap, String) {
    let json = serde_json::to_string_pretty(&ApiDoc::openapi()).unwrap_or_default();
    let mut headers = axum::http::header::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/json"),
    );
    (headers, json)
}
