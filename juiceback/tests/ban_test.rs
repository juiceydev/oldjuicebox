use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Request, StatusCode};
use std::net::SocketAddr;
use std::sync::Arc;
use tower::ServiceExt;

use juiceback::config::Config;
use juiceback::routes::build_router;
use juiceback::state::AppState;

fn with_connect_info(mut request: Request<Body>) -> Request<Body> {
    request
        .extensions_mut()
        .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 0))));
    request
}

fn test_config() -> Config {
    Config {
        host: "127.0.0.1".into(),
        port: 6401,
        quic_port: 6402,
        database_path: ":memory:".into(),
        rate_limit_per_minute: 100,
        db_pool_size: 1,
        public_base_url: "http://localhost:6402".into(),
        log_level: "info".into(),
        cleanup_interval_minutes: 30,
        juicehost_api_key: "test-key".into(),
        juicehost_url: "http://127.0.0.1:6402".into(),
        public_juicehost_url: "http://localhost:6402".into(),
        juiceback_origin: "http://127.0.0.1:6401".into(),
        jwt_secret: "test-secret".into(),
        cors_origins: vec!["http://localhost:6400".into()],
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
        trusted_proxy_cidrs: juiceutils::proxy::parse_trusted_proxy_cidrs("127.0.0.1/32").unwrap(),
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
        dte_assumed_bps: juiceback::constants::DTE_ASSUMED_BPS,
        dte_safety_mult: juiceback::constants::DTE_SAFETY_MULT,
        dte_base_overhead_secs: juiceback::constants::DTE_BASE_OVERHEAD_SECS,
        dte_min_ttl_secs: juiceback::constants::DTE_MIN_TTL_SECS,
        dte_max_ttl_secs: juiceback::constants::DTE_MAX_TTL_SECS,
        dte_mint_limit: juiceback::constants::DTE_MINT_LIMIT,
        dte_mint_window_secs: juiceback::constants::DTE_MINT_WINDOW_SECS,
        dte_mint_burst: juiceback::constants::DTE_MINT_BURST,
        region_public_juicehosts: std::collections::HashMap::new(),
    }
}

fn test_jh_config() -> juiceback::juicehost::JuicehostConfig {
    juiceback::juicehost::JuicehostConfig {
        max_file_size_bytes: 524288000,
        default_ttl_hours: 72.0,
        allowed_ttl_hours: vec![0.5, 1.0, 6.0, 12.0, 24.0, 72.0, 168.0],
        danger_level: juiceback::file_validation::ProtectionLevel::High,
        quick_link: false,
        custom_id_enabled: true,
        ultrafast: false,
    }
}

fn test_state() -> Arc<AppState> {
    let config = test_config();
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
    juiceback::db::init_db(&pool.get().unwrap()).unwrap();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-juicehost-api-key", "test-key".parse().unwrap());
    let http = reqwest::Client::new();

    AppState::new(pool, config, http, headers, Some(test_jh_config()))
}

#[tokio::test]
async fn banned_ip_returns_403() {
    let state = test_state();
    let hashed = juiceback::utils::hash_ip_for_ban("192.168.1.100", &state.config.ip_pepper);
    state.banned_ips.insert(hashed, ());

    let app = build_router(state);
    let resp = app
        .oneshot(with_connect_info(
            Request::builder()
                .uri("/api/health")
                .header("x-forwarded-for", "192.168.1.100")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unban_ip_allows_access() {
    let state = test_state();
    let hashed = juiceback::utils::hash_ip_for_ban("192.168.1.100", &state.config.ip_pepper);
    state.banned_ips.insert(hashed.clone(), ());
    state.banned_ips.remove(&hashed);

    let app = build_router(state);
    let resp = app
        .oneshot(with_connect_info(
            Request::builder()
                .uri("/api/health")
                .header("x-forwarded-for", "192.168.1.100")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn banned_ip_can_still_check_ban_status() {
    let state = test_state();
    let hashed = juiceback::utils::hash_ip_for_ban("192.168.1.100", &state.config.ip_pepper);
    state
        .ban_ip(&hashed, "spam", "admin")
        .await
        .expect("ban_ip should succeed");

    let app = build_router(state);
    // The /banned page depends on /api/ban-status, so it must stay reachable
    // even for banned IPs (other endpoints still 403).
    let resp = app
        .oneshot(with_connect_info(
            Request::builder()
                .uri("/api/ban-status?ip=192.168.1.100")
                .header("x-forwarded-for", "192.168.1.100")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["banned"], true);
}

#[tokio::test]
async fn ban_snapshot_requires_api_key_and_returns_hashes() {
    let state = test_state();
    let hashed = juiceback::utils::hash_ip_for_ban("192.168.1.100", &state.config.ip_pepper);
    state.ban_ip(&hashed, "spam", "admin").await.unwrap();

    let app = build_router(state);

    // No API key -> 403.
    let resp = app
        .clone()
        .oneshot(with_connect_info(
            Request::builder()
                .uri("/internal/ban-snapshot")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::FORBIDDEN);

    // Valid API key -> pepper + hashes.
    let resp = app
        .oneshot(with_connect_info(
            Request::builder()
                .uri("/internal/ban-snapshot")
                .header("x-juicehost-api-key", "test-key")
                .body(Body::empty())
                .unwrap(),
        ))
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), 8192).await.unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["pepper"], "test_pepper");
    assert_eq!(json["count"], 1);
    assert_eq!(json["hashes"][0], hashed);
}
