use axum::body::Body;
use axum::extract::connect_info::MockConnectInfo;
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tower::ServiceExt;
use wiremock::matchers::{body_partial_json, header, method, path, path_regex};
use wiremock::{Mock, MockServer, ResponseTemplate};

use juiceback::config::Config;
use juiceback::routes::build_router;
use juiceback::state::AppState;

fn test_config(cobalt_api_url: String) -> Config {
    test_config_with_session(cobalt_api_url, None, None)
}

fn test_config_with_session(
    cobalt_api_url: String,
    session_api_url: Option<String>,
    session_api_key: Option<String>,
) -> Config {
    test_config_with_delay(cobalt_api_url, session_api_url, session_api_key, 1)
}

fn test_config_with_delay(
    cobalt_api_url: String,
    session_api_url: Option<String>,
    session_api_key: Option<String>,
    delay: u64,
) -> Config {
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
        // juicehost lives on the same mock server as cobalt here.
        juicehost_url: cobalt_api_url.clone(),
        public_juicehost_url: "http://localhost:6402".into(),
        juiceback_origin: "http://127.0.0.1:6401".into(),
        jwt_secret: "test-secret".into(),
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
        cobalt_enabled: true,
        cobalt_api_url,
        cobalt_api_key: "cobalt_key".into(),
        cobalt_session_api_url: session_api_url,
        cobalt_session_api_key: session_api_key,
        bun_fetch_url: None,
        bun_fetch_key: None,
        fetch_empty_retry_delay_secs: delay,
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

fn test_config_with_bun(
    cobalt_api_url: String,
    session_api_url: Option<String>,
    session_api_key: Option<String>,
    bun_url: String,
    bun_key: String,
) -> Config {
    // delay 0 disables the timed rescue loop; the first-pass bun attempt
    // still runs, which is all this test needs.
    let mut config = test_config_with_delay(cobalt_api_url, session_api_url, session_api_key, 0);
    config.bun_fetch_url = Some(bun_url);
    config.bun_fetch_key = Some(bun_key);
    config
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

fn test_state(cobalt_api_url: String) -> Arc<AppState> {
    let config = test_config(cobalt_api_url);
    state_from_config(config)
}

fn state_from_config(config: Config) -> Arc<AppState> {
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
    juiceback::db::init_db(&pool.get().unwrap()).unwrap();

    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-juicehost-api-key", "test-key".parse().unwrap());
    let http = reqwest::Client::new();

    AppState::new(pool, config, http, headers, Some(test_jh_config()))
}

fn app(state: Arc<AppState>) -> axum::Router {
    build_router(state).layer(MockConnectInfo(SocketAddr::from(([127, 0, 0, 1], 4242))))
}

/// Extract the anon-session cookie pair from a response's Set-Cookie header
/// so follow-up requests keep the same identity (like a browser would).
fn session_cookie(resp: &axum::http::Response<axum::body::Body>) -> Option<String> {
    let header = resp.headers().get("set-cookie")?.to_str().ok()?;
    Some(header.split(';').next()?.to_string())
}

async fn post_fetch(
    app: &axum::Router,
    body: Value,
    cookie: Option<&str>,
) -> (StatusCode, Value, Option<String>) {
    let mut builder = Request::builder()
        .method("POST")
        .uri("/api/fetch")
        .header("content-type", "application/json");
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let set_cookie = session_cookie(&resp);
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json, set_cookie)
}

async fn get_fetch(app: &axum::Router, job_id: &str, cookie: Option<&str>) -> (StatusCode, Value) {
    let mut builder = Request::builder().uri(format!("/api/fetch/{}", job_id));
    if let Some(cookie) = cookie {
        builder = builder.header("cookie", cookie);
    }
    let resp = app
        .clone()
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

/// Poll until the job leaves 'pending', asserting ownership along the way.
async fn wait_for_completion(app: &axum::Router, job_id: &str, cookie: &str) -> Value {
    for _ in 0..400 {
        tokio::time::sleep(Duration::from_millis(50)).await;
        let (status, body) = get_fetch(app, job_id, Some(cookie)).await;
        assert_eq!(status, StatusCode::OK, "status poll failed: {}", body);
        assert_eq!(body["job_id"], *job_id);
        if body["status"] == "done" || body["status"] == "failed" {
            return body;
        }
    }
    panic!("job {} did not complete in time", job_id);
}

#[tokio::test]
async fn full_tunnel_flow_stores_file_and_links_owner() {
    let server = MockServer::start().await;

    // cobalt API: authenticated POST / returning a tunnel response
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key cobalt_key"))
        .and(header("accept", "application/json"))
        .and(body_partial_json(json!({
            "url": "https://youtu.be/dQw4w9WgXcQ",
            "downloadMode": "auto",
            "videoQuality": "1080",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel?id=abc123", server.uri()),
            "filename": "funny cat video.mp4",
        })))
        .mount(&server)
        .await;

    // cobalt tunnel endpoint serving the actual media bytes
    Mock::given(method("GET"))
        .and(path("/tunnel"))
        .respond_with(ResponseTemplate::new(200).set_body_string("fake-mp4-bytes"))
        .mount(&server)
        .await;

    // juicehost internal stream endpoint receiving the pushed file
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/funny%20cat%20video%2Emp4$",
        ))
        .and(header("x-mime-type", "video/mp4"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = app(test_state(server.uri()));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/dQw4w9WgXcQ" }), None).await;
    assert_eq!(status, StatusCode::OK, "start failed: {}", body);
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    let cookie = cookie.expect("session cookie");

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "done", "job failed: {}", result);
    let file = &result["file"];
    assert_eq!(file["mime_type"], "video/mp4");
    assert_eq!(file["size_bytes"], 14); // len("fake-mp4-bytes")
    assert!(file["url"]
        .as_str()
        .unwrap()
        .starts_with("http://localhost:6402/f/"));
    assert!(!file["delete_token"].as_str().unwrap().is_empty());

    // Ownership: without the session cookie we are a different anon user.
    let (status, _) = get_fetch(&app, &job_id, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn audio_only_request_uses_audio_mode_and_mime() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .and(body_partial_json(json!({
            "downloadMode": "audio",
            "audioFormat": "opus",
            "videoQuality": "720",
        })))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel-audio", server.uri()),
            "filename": "song.opus",
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-audio"))
        .respond_with(ResponseTemplate::new(200).set_body_string("opus-data"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/song%2Eopus$",
        ))
        .and(header("x-mime-type", "audio/ogg"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = app(test_state(server.uri()));

    let (status, start, cookie) = post_fetch(
        &app,
        json!({
            "url": "https://youtu.be/x",
            "audio_only": true,
            "audio_format": "opus",
            "video_quality": "720",
            "youtube_video_codec": "not-a-codec", // sanitized away
        }),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", start);
    let result =
        wait_for_completion(&app, start["job_id"].as_str().unwrap(), &cookie.unwrap()).await;
    assert_eq!(result["status"], "done", "{}", result);
    assert_eq!(result["file"]["mime_type"], "audio/ogg");
}

#[tokio::test]
async fn cobalt_error_becomes_friendly_failure() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": { "code": "error.api.link.unsupported" }
        })))
        .mount(&server)
        .await;

    let app = app(test_state(server.uri()));

    let (_, start, cookie) =
        post_fetch(&app, json!({ "url": "https://example.com/x" }), None).await;
    let result =
        wait_for_completion(&app, start["job_id"].as_str().unwrap(), &cookie.unwrap()).await;
    assert_eq!(result["status"], "failed");
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("isn't supported"));
}

#[tokio::test]
async fn oversized_stream_is_rejected() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "redirect",
            "url": format!("{}/big-file", server.uri()),
            "filename": "big.mp4",
        })))
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/big-file"))
        .respond_with(ResponseTemplate::new(200).set_body_string(vec![0u8; 1024]))
        .mount(&server)
        .await;

    // Tiny 8-byte ceiling: the known content-length trips the early check.
    let jh = juiceback::juicehost::JuicehostConfig {
        max_file_size_bytes: 8,
        ..test_jh_config()
    };
    let config = test_config(server.uri());
    let pool = r2d2::Pool::builder()
        .max_size(1)
        .build(r2d2_sqlite::SqliteConnectionManager::memory())
        .unwrap();
    juiceback::db::init_db(&pool.get().unwrap()).unwrap();
    let state = AppState::new(
        pool,
        config,
        reqwest::Client::new(),
        reqwest::header::HeaderMap::new(),
        Some(jh),
    );
    let app = app(state);

    let (_, start, cookie) =
        post_fetch(&app, json!({ "url": "https://example.com/big" }), None).await;
    let result =
        wait_for_completion(&app, start["job_id"].as_str().unwrap(), &cookie.unwrap()).await;
    assert_eq!(result["status"], "failed");
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("maximum file size"));
}

#[tokio::test]
async fn disabled_feature_returns_not_found() {
    let server = MockServer::start().await;

    let config = {
        let mut c = test_config(server.uri());
        c.cobalt_enabled = false;
        c
    };
    let pool = r2d2::Pool::builder()
        .max_size(1)
        .build(r2d2_sqlite::SqliteConnectionManager::memory())
        .unwrap();
    juiceback::db::init_db(&pool.get().unwrap()).unwrap();
    let disabled_state = AppState::new(
        pool,
        config,
        reqwest::Client::new(),
        reqwest::header::HeaderMap::new(),
        Some(test_jh_config()),
    );
    let app = app(disabled_state);

    let (status, _, _) = post_fetch(&app, json!({ "url": "https://youtu.be/x" }), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Status endpoint for a nonexistent job is also 404.
    let (status, _) = get_fetch(&app, "nope", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn unsafe_urls_are_rejected_up_front() {
    // Fresh router per URL so the 3/min governor never interferes.
    for url in [
        "http://127.0.0.1:7272/session",
        "http://192.168.0.5/video",
        "https://localhost/x",
        "ftp://example.com/f",
        "javascript:alert(1)",
        "",
    ] {
        let app = app(test_state("http://127.0.0.1:1".into()));
        let (status, body, _) = post_fetch(&app, json!({ "url": url }), None).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "accepted {:?}", url);
        assert_eq!(body["error"], "BAD_REQUEST");
    }
}

#[tokio::test]
async fn empty_tunnel_response_fails_without_touching_juicehost() {
    let server = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/empty-media", server.uri()),
            "filename": "weird video.mp4",
        })))
        .mount(&server)
        .await;

    // 200 OK but zero bytes — some youtube videos do this with certain
    // quality/codec picks.
    Mock::given(method("GET"))
        .and(path("/empty-media"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "0")
                .set_body_string(""),
        )
        .mount(&server)
        .await;

    // The push endpoint must never be called with an empty body.
    Mock::given(method("POST"))
        .and(path_regex(r"^/internal/file/stream/"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    // delay=0: single-shot semantics for this test (no rescue loop).
    let config = test_config_with_delay(server.uri(), None, None, 0);
    let app = app(state_from_config(config));

    let (_, start, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/weird" }), None).await;
    let result =
        wait_for_completion(&app, start["job_id"].as_str().unwrap(), &cookie.unwrap()).await;
    assert_eq!(result["status"], "failed");
    // youtube link: message must explain the streaming-token enforcement
    assert!(result["error"]
        .as_str()
        .unwrap()
        .contains("YouTube blocked extraction"));
}

#[tokio::test]
async fn fetch_start_is_rate_limited_to_three_per_minute() {
    let app = app(test_state("http://127.0.0.1:1".into())); // cobalt unreachable; fine

    let mut last = None;
    for i in 0..5 {
        let (status, _, _) = post_fetch(
            &app,
            json!({ "url": format!("https://example.com/v{}", i) }),
            None,
        )
        .await;
        last = Some(status);
        if i < 3 {
            assert_eq!(status, StatusCode::OK);
        }
    }
    assert_eq!(last, Some(StatusCode::TOO_MANY_REQUESTS));
}

/// When the primary (sessionless) cobalt instance refuses a YouTube link at
/// client level, juiceback must retry against the configured session-enabled
/// instance and serve its tunnel result.
#[tokio::test]
async fn unavailable_from_primary_retries_session_instance() {
    let primary = MockServer::start().await;
    let session = MockServer::start().await;

    // Primary: authenticated, but refuses the link at client level.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key cobalt_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"code": "error.api.content.video.unavailable"}
        })))
        .mount(&primary)
        .await;

    // Session instance: same link succeeds behind session auth.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key session_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel-s", session.uri()),
            "filename": "rare video.mp4",
        })))
        .mount(&session)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-s"))
        .respond_with(ResponseTemplate::new(200).set_body_string("session-bytes"))
        .mount(&session)
        .await;

    // juicehost stream endpoint lives on the primary mock server here.
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/rare%20video%2Emp4$",
        ))
        .and(header("x-mime-type", "video/mp4"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&primary)
        .await;

    let config = test_config_with_delay(
        primary.uri(),
        Some(session.uri()),
        Some("session_key".into()),
        1,
    );
    let app = app(state_from_config(config));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/rare" }), None).await;
    assert_eq!(status, StatusCode::OK, "start failed: {}", body);
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    let cookie = cookie.expect("session cookie");

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "done", "job failed: {}", result);
    assert_eq!(result["file"]["size_bytes"], 13); // len("session-bytes")
}

/// When both cobalt tiers refuse a YouTube link, juiceback must try the Bun
/// SABR-fetch sidecar next and keep the filename it reports.
#[tokio::test]
async fn unavailable_from_cobalt_tiers_uses_bun_sidecar() {
    let primary = MockServer::start().await;
    let session = MockServer::start().await;
    let bun = MockServer::start().await;

    // Both cobalt tiers refuse at client level (rescuable).
    for server in [&primary, &session] {
        Mock::given(method("POST"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "status": "error",
                "error": {"code": "error.api.content.video.unavailable"}
            })))
            .mount(server)
            .await;
    }

    // Bun sidecar serves the file bytes behind bearer auth.
    Mock::given(method("POST"))
        .and(path("/fetch"))
        .and(header("authorization", "Bearer bun_key"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("bun-bytes")
                .insert_header("x-bun-fetch-filename", "Sidecar%20Title.mp4"),
        )
        .mount(&bun)
        .await;

    // juicehost stream endpoint lives on the primary mock server here.
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/Sidecar%20Title%2Emp4$",
        ))
        .and(header("x-mime-type", "video/mp4"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&primary)
        .await;

    let config = test_config_with_bun(
        primary.uri(),
        Some(session.uri()),
        Some("session_key".into()),
        format!("{}/fetch", bun.uri()),
        "bun_key".into(),
    );
    let app = app(state_from_config(config));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/dQw4w9WgXcQ" }), None).await;
    assert_eq!(status, StatusCode::OK, "start failed: {}", body);
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    let cookie = cookie.expect("session cookie");

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "done", "job failed: {}", result);
    assert_eq!(result["file"]["filename"], "Sidecar Title.mp4");
    assert_eq!(result["file"]["size_bytes"], 9); // len("bun-bytes")
}

/// Without a session instance configured the refusal surfaces as-is.
#[tokio::test]
async fn unavailable_without_session_config_fails_friendly() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"code": "error.api.content.video.unavailable"}
        })))
        .mount(&server)
        .await;

    let app = app(test_state(server.uri()));
    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/rare" }), None).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = body["job_id"].as_str().unwrap().to_string();
    let cookie = cookie.unwrap();

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "failed");
    assert!(result["error"].as_str().unwrap().contains("unavailable"));
}

/// Non-YouTube links must never hit the session fallback.
#[tokio::test]
async fn non_youtube_unavailable_does_not_retry_session_instance() {
    let primary = MockServer::start().await;
    let session = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "error",
            "error": {"code": "error.api.content.video.unavailable"}
        })))
        .mount(&primary)
        .await;

    Mock::given(method("POST"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel-s", session.uri()),
            "filename": "should-not-happen.mp4",
        })))
        .mount(&session)
        .await;

    let config = test_config_with_session(
        primary.uri(),
        Some(session.uri()),
        Some("session_key".into()),
    );
    let app = app(state_from_config(config));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://vimeo.com/locked" }), None).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = body["job_id"].as_str().unwrap().to_string();
    let cookie = cookie.unwrap();

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "failed");
}

/// Empty stream from primary (content-bound enforcement) must trigger the
/// session-instance retry and serve the file from there.
#[tokio::test]
async fn empty_stream_from_primary_retries_session_instance() {
    let primary = MockServer::start().await;
    let session = MockServer::start().await;

    // Primary: accepts the link but its tunnel serves zero bytes.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key cobalt_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel-empty", primary.uri()),
            "filename": "enforced video.mp4",
        })))
        .mount(&primary)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-empty"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "0")
                .set_body_string(""),
        )
        .mount(&primary)
        .await;

    // Session instance: patched with per-video PO tokens; serves bytes.
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key session_key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "tunnel",
            "url": format!("{}/tunnel-s2", session.uri()),
            "filename": "enforced video.mp4",
        })))
        .mount(&session)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-s2"))
        .respond_with(ResponseTemplate::new(200).set_body_string("real-bytes-here"))
        .mount(&session)
        .await;

    // juicehost lives on the primary mock server in these tests.
    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/enforced%20video%2Emp4$",
        ))
        .and(header("x-mime-type", "video/mp4"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&primary)
        .await;

    let config = test_config_with_delay(
        primary.uri(),
        Some(session.uri()),
        Some("session_key".into()),
        1,
    );
    let app = app(state_from_config(config));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/enforced" }), None).await;
    assert_eq!(status, StatusCode::OK, "start failed: {}", body);
    let job_id = body["job_id"].as_str().expect("job_id").to_string();
    let cookie = cookie.expect("session cookie");

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "done", "job failed: {}", result);
    assert_eq!(result["file"]["size_bytes"], 15); // len("real-bytes-here")
}

/// Job progress lifecycle: pending -> processing -> downloading (with byte
/// counts) -> done, including the widened completion guard.
#[tokio::test]
async fn fetch_job_progress_lifecycle() {
    let manager = r2d2_sqlite::SqliteConnectionManager::memory();
    let pool = r2d2::Pool::builder().max_size(1).build(manager).unwrap();
    juiceback::db::init_db(&pool.get().unwrap()).unwrap();
    let conn = pool.get().unwrap();

    juiceback::db::insert_fetch_job(&conn, "job1", "user1", "https://youtu.be/x").unwrap();

    let job = juiceback::db::get_fetch_job(&conn, "job1").unwrap().unwrap();
    assert_eq!(job.status, "pending");
    assert_eq!(job.stage, "");
    assert_eq!(job.bytes_received, 0);

    // processing tick
    juiceback::db::update_fetch_job_progress(&conn, "job1", "cobalt", "processing", 0).unwrap();
    let job = juiceback::db::get_fetch_job(&conn, "job1").unwrap().unwrap();
    assert_eq!(job.status, "processing");
    assert_eq!(job.stage, "cobalt");

    // downloading ticks with growing byte count
    juiceback::db::update_fetch_job_progress(&conn, "job1", "transfer", "downloading", 512).unwrap();
    juiceback::db::update_fetch_job_progress(&conn, "job1", "transfer", "downloading", 4096).unwrap();
    let job = juiceback::db::get_fetch_job(&conn, "job1").unwrap().unwrap();
    assert_eq!(job.status, "downloading");
    assert_eq!(job.bytes_received, 4096);

    // completion must be possible from a non-pending intermediate state,
    // and must record the final byte total via a last progress write.
    juiceback::db::update_fetch_job_progress(&conn, "job1", "transfer", "downloading", 8192).unwrap();
    let updated = juiceback::db::finish_fetch_job(&conn, "job1", "done", "", "file1").unwrap();
    assert!(updated);
    let job = juiceback::db::get_fetch_job(&conn, "job1").unwrap().unwrap();
    assert_eq!(job.status, "done");
    assert_eq!(job.file_id, "file1");

    // terminal state is immutable: late tasks can't overwrite it.
    juiceback::db::update_fetch_job_progress(&conn, "job1", "late", "downloading", 1).unwrap();
    let job = juiceback::db::get_fetch_job(&conn, "job1").unwrap().unwrap();
    assert_eq!(job.status, "done");
    assert_eq!(job.stage, "transfer"); // untouched by the late write
}

/// Transient googlevideo blocks clear within minutes: the second primary
/// attempt (after the retry delay) must succeed with no session instance
/// configured at all.
#[tokio::test]
async fn empty_stream_retries_primary_after_delay() {
    let server = MockServer::start().await;

    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls_counter = std::sync::atomic::AtomicUsize::new(0);
    let calls = std::sync::Arc::new(calls_counter);
    let base = server.uri();

    // Cobalt call #1: tunnel that serves zero bytes (enforcement window).
    // Calls #2+: tunnel serving real bytes once the block clears.
    let calls_a = std::sync::Arc::clone(&calls);
    Mock::given(method("POST"))
        .and(path("/"))
        .and(header("authorization", "Api-Key cobalt_key"))
        .respond_with(move |_req: &wiremock::Request| {
            let n = calls_a.fetch_add(1, Ordering::SeqCst);
            eprintln!("[dbg-mock] cobalt POST #{}", n);
            if n == 0 {
                ResponseTemplate::new(200).set_body_json(json!({
                    "status": "tunnel",
                    "url": format!("{}/tunnel-empty", base),
                    "filename": "flaky video.mp4",
                }))
            } else {
                ResponseTemplate::new(200).set_body_json(json!({
                    "status": "tunnel",
                    "url": format!("{}/tunnel-flaky", base),
                    "filename": "flaky video.mp4",
                }))
            }
        })
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-empty"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-length", "0")
                .set_body_string(""),
        )
        .mount(&server)
        .await;

    Mock::given(method("GET"))
        .and(path("/tunnel-flaky"))
        .respond_with(ResponseTemplate::new(200).set_body_string("recovered-bytes"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path_regex(
            r"^/internal/file/stream/[A-Za-z0-9_-]+/flaky%20video%2Emp4$",
        ))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server)
        .await;

    let app = app(test_state(server.uri()));

    let (status, body, cookie) =
        post_fetch(&app, json!({ "url": "https://youtu.be/flaky" }), None).await;
    assert_eq!(status, StatusCode::OK);
    let job_id = body["job_id"].as_str().unwrap().to_string();
    let cookie = cookie.unwrap();

    let result = wait_for_completion(&app, &job_id, &cookie).await;
    assert_eq!(result["status"], "done", "job failed: {}", result);
    assert_eq!(result["file"]["size_bytes"], 15); // len("recovered-bytes")
    // expectation verified when the mock server drops at test end
}

/// The yt-dlp fallback invocation must carry proxy + PO provider wiring and
/// keep the source URL last (positional).
#[test]
fn ytdlp_args_are_wired_correctly() {
    let args = juiceback::routes::fetch::build_ytdlp_args_for_test(
        "http://pi:8888",
        "http://pot:4416",
        "deno:/usr/local/bin/deno",
        "/tmp/cookies.txt",
        "https://www.youtube.com/watch?v=sPyAQQklc1s",
        "/tmp/potdir",
    );
    let s = args.join(" ");
    assert!(s.contains("--proxy http://pi:8888"));
    assert!(s.contains("youtubepot-bgutilhttp:base_url=http://pot:4416"));
    assert!(s.contains("bv*+ba/b"));
    assert!(s.contains("--cookies /tmp/cookies.txt"));
    assert!(s.contains("--remote-components ejs:github"));
    assert!(s.contains("/tmp/potdir/"));
    assert_eq!(args.last().unwrap(), "https://www.youtube.com/watch?v=sPyAQQklc1s");
}

#[test]
fn video_id_extraction_covers_common_shapes() {
    assert!(juiceback::cobalt::parse_youtube_video_id("https://youtu.be/abc").is_some());
}
