//! juiceback, the entire backend of juicebox
// "Well, well, well. Welcome to MY LAIR!" - Wheatley from Portal 2

use mimalloc::MiMalloc;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
#[cfg(feature = "quic")]
use tokio::sync::Notify;
use tracing_subscriber::{fmt::format::FmtSpan, layer::SubscriberExt, util::SubscriberInitExt};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

use juiceback::config::Config;
use juiceback::state::AppState;

#[derive(Debug)]
struct SqlitePragmas;

impl r2d2::CustomizeConnection<rusqlite::Connection, rusqlite::Error> for SqlitePragmas {
    fn on_acquire(&self, conn: &mut rusqlite::Connection) -> Result<(), rusqlite::Error> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )
    }
}

async fn shutdown_signal() {
    juiceutils::shutdown_signal("juiceback").await
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() > 1 && args[1] == "hash-password" {
        let password = if args.get(2).map(|s| s.as_str()) == Some("--stdin") {
            let mut input = String::new();
            std::io::Read::read_to_string(&mut std::io::stdin(), &mut input)
                .expect("Failed to read password from stdin");
            input.trim().to_string()
        } else {
            args.get(2).expect("usage: juiceback hash-password <password> OR echo 'password' | juiceback hash-password --stdin").to_string()
        };
        let hash = juiceback::auth::hash_password(&password).expect("hashing failed");
        println!("{}", hash);
        return;
    }

    dotenvy::dotenv().ok();

    // Initialize Sentry before the tokio runtime so all threads inherit the Hub.
    // Uses SENTRY_DSN_JUICEBACK if set, otherwise falls back to SENTRY_DSN.
    let _sentry_guard = std::env::var("SENTRY_DSN_JUICEBACK")
        .or_else(|_| std::env::var("SENTRY_DSN"))
        .ok()
        .filter(|dsn| !dsn.is_empty())
        .map(|dsn| {
            sentry::init((
                dsn.as_str(),
                sentry::ClientOptions {
                    release: sentry::release_name!(),
                    environment: Some(
                        std::env::var("SENTRY_ENVIRONMENT")
                            .unwrap_or_else(|_| "production".into())
                            .into(),
                    ),
                    traces_sample_rate: std::env::var("SENTRY_TRACES_SAMPLE_RATE")
                        .ok()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(0.05),
                    send_default_pii: false,
                    ..Default::default()
                },
            ))
        });

    let config = Config::load().expect("Failed to load configuration");

    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| config.log_level.clone().into()),
        )
        .with(tracing_subscriber::fmt::layer().with_span_events(FmtSpan::CLOSE))
        .with(sentry_tracing::layer())
        .init();

    tracing::info!("juiceback v{} starting", env!("CARGO_PKG_VERSION"));
    tracing::info!("db: {}", config.database_path);
    tracing::info!("juicehost_url: {}", config.juicehost_url);
    tracing::info!("public_juicehost_url: {}", config.public_juicehost_url);
    if config.cobalt_enabled {
        tracing::info!("cobalt fetching enabled ({})", config.cobalt_api_url);
    } else {
        tracing::info!("cobalt fetching disabled (set COBALT_ENABLED=true to enable)");
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("Failed to create tokio runtime");

    runtime.block_on(async {
        let manager = r2d2_sqlite::SqliteConnectionManager::file(&config.database_path);
        let pool = r2d2::Pool::builder()
            .max_size(config.db_pool_size)
            .connection_timeout(Duration::from_secs(5))
            .connection_customizer(Box::new(SqlitePragmas))
            .build(manager)
            .expect("Failed to create database connection pool");

        {
            let conn = pool.get().expect("Failed to get connection for initialization");
            juiceback::db::init_db(&conn).expect("Failed to initialize database schema");
            juiceback::db::migrate_raw_ips_to_encrypted(
                &conn,
                &config.ip_encryption_key,
                &config.ip_pepper,
            ).expect("Failed to migrate raw IPs to encrypted form");
        }
        tracing::info!("Database initialized");

        // Build the reqwest client and juicehost headers once at startup.
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(juiceback::constants::HTTP_CONNECT_TIMEOUT_SECS))
            .pool_idle_timeout(Duration::from_secs(juiceback::constants::HTTP_IDLE_TIMEOUT_SECS))
            .pool_max_idle_per_host(16)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("Failed to build reqwest client");

        let mut juicehost_headers = reqwest::header::HeaderMap::new();
        if !config.juicehost_api_key.is_empty() {
            juicehost_headers.insert(
                "x-juicehost-api-key",
                config.juicehost_api_key.parse().expect("juicehost API key must be valid ASCII"),
            );
        }
        if !config.juiceback_origin.is_empty() {
            juicehost_headers.insert(
                "x-juiceback-origin",
                config.juiceback_origin.parse().expect("juiceback origin must be valid ASCII"),
            );
        }

        // Fetch juicehost config with startup retries (degraded mode if all fail).
        let mut jh_config: Option<juiceback::juicehost::JuicehostConfig> = None;
        {
            let mut last_err = String::new();
            for attempt in 0..juiceback::constants::STARTUP_CONFIG_RETRIES {
                match juiceback::juicehost::fetch_juicehost_config(
                    &http,
                    &config.juicehost_url,
                    &juicehost_headers,
                ).await {
                    Ok(cfg) => {
                        tracing::info!("juicehost config fetched successfully on attempt {}", attempt + 1);
                        jh_config = Some(cfg);
                        break;
                    }
                    Err(e) => {
                        let delay = juiceback::constants::STARTUP_BACKOFF_BASE_SECS.pow(attempt + 1);
                        tracing::warn!(
                            "juicehost config fetch attempt {}/{} failed: {} (retrying in {}s)",
                            attempt + 1,
                            juiceback::constants::STARTUP_CONFIG_RETRIES,
                            e,
                            delay,
                        );
                        last_err = e;
                        tokio::time::sleep(Duration::from_secs(delay)).await;
                    }
                }
            }

            if jh_config.is_none() {
                tracing::error!(
                    "juicehost config fetch failed after {} attempts ({}). Starting in degraded mode.",
                    juiceback::constants::STARTUP_CONFIG_RETRIES,
                    last_err,
                );
            }
        }

        let state = AppState::new(pool, config.clone(), http.clone(), juicehost_headers.clone(), jh_config);

        // bg refresh
        {
            let refresh_state = Arc::clone(&state);
            let refresh_http = http.clone();
            let refresh_url = config.juicehost_url.clone();
            let refresh_headers = juicehost_headers.clone();
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(
                        juiceback::constants::DEGRADED_RETRY_INTERVAL_SECS,
                    ))
                    .await;
                    match juiceback::juicehost::fetch_juicehost_config(
                        &refresh_http,
                        &refresh_url,
                        &refresh_headers,
                    )
                    .await
                    {
                        Ok(cfg) => {
                            tracing::debug!("juicehost config refreshed");
                            *refresh_state.jh_config.write().unwrap() = Some(Arc::new(cfg));
                        }
                        Err(e) => {
                            tracing::debug!(
                                "juicehost config refresh failed (keeping last known value): {}",
                                e
                            );
                        }
                    }
                }
            });
        }

        state.reload_banned_ips();

        let state_cleanup = Arc::clone(&state);
        tokio::spawn(async move {
            juiceback::jobs::cleanup::run_cleanup_loop(state_cleanup).await;
        });

        // Resource watchdog: log internal upload-state sizes every 30s. If
        // the server ever stops accepting again, these trends identify the
        // piling component (sessions vs senders vs push tasks vs permits)
        // instead of leaving only a silent wedge to dissect.
        let state_watchdog = Arc::clone(&state);
        tokio::spawn(async move {
            let mut ticker =
                tokio::time::interval(std::time::Duration::from_secs(30));
            loop {
                ticker.tick().await;
                tracing::info!(
                    "watchdog: tus_sessions={} tus_senders={} push_tasks={} part_sessions={} upload_permits={} presence_permits={}",
                    state_watchdog.tus.len(),
                    state_watchdog.tus_senders.len(),
                    state_watchdog.push_handles.len(),
                    state_watchdog.part_sessions.len(),
                    state_watchdog.upload_semaphore.available_permits(),
                    state_watchdog.presence_semaphore.available_permits(),
                );
            }
        });

        if state.config.dte_enabled {
            let state_prune = Arc::clone(&state);
            tokio::spawn(async move {
                juiceback::jobs::cleanup::run_mint_limiter_prune_loop(state_prune).await;
            });
        }

        let app = juiceback::routes::build_router(Arc::clone(&state));

        let addr = format!("{}:{}", state.config.host, state.config.port);
        let listener = TcpListener::bind(&addr)
            .await
            .expect("Failed to bind to address");

        #[cfg(feature = "quic")]
        {
            let shutdown = Arc::new(Notify::new());
            let quic_shutdown = Arc::clone(&shutdown);

            let quic_listen: std::net::SocketAddr = format!("{}:{}", state.config.host, state.config.quic_port)
                .parse()
                .expect("Invalid QUIC address");

            let quic_router = juiceback::routes::build_router(Arc::clone(&state));
            let quic_cert = state.config.quic_cert_path.clone();

            tokio::select! {
                _ = async {
                    juiceutils::start_quic_server(quic_router, quic_listen, quic_shutdown, "juiceback", quic_cert).await;
                } => {}
                _ = async {
                    tracing::info!("Listening on http://{}", addr);
                    axum::serve(
                        listener,
                        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
                    )
                    .with_graceful_shutdown(shutdown_signal())
                    .await
                    .expect("Server failed");
                } => {}
                _ = shutdown_signal() => {
                    tracing::info!("juiceback shutting down...");
                    shutdown.notify_one();
                }
            }
        }

        #[cfg(not(feature = "quic"))]
        {
            tracing::info!("Listening on http://{}", addr);
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(shutdown_signal())
            .await
            .expect("Server failed");
        }
    });

    // Flush any remaining Sentry events before exit
    if let Some(client) = sentry::Hub::current().client() {
        client.close(Some(Duration::from_secs(2)));
    }
}
