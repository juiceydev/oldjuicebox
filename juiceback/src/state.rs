//! the shared state every request handler can access

use dashmap::DashMap;

use crate::config::Config;
use crate::error::AppError;
use crate::juicehost::JuicehostConfig;
use crate::tus::{PartSessionMap, TusMap, TusSenderMap};
use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;
use std::sync::Arc;

/// A pool of SQLite connections, managed by r2d2.
pub type DbPool = Pool<SqliteConnectionManager>;

/// A connected device's WebSocket handle.
pub struct ConnectedDevice {
    pub device_id: String,
    pub device_name: String,
    pub sender: tokio::sync::mpsc::Sender<String>,
    pub connected_at: std::time::Instant,
    pub last_heartbeat: std::time::Instant,
}

/// Presence events broadcast to SSE listeners.
#[derive(Clone, Debug)]
pub enum PresenceEvent {
    DeviceConnected {
        device_id: String,
        device_name: String,
    },
    DeviceDisconnected {
        device_id: String,
    },
    PingResponse {
        device_id: String,
    },
}

/// Shared state that every axum handler can access.
#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: Arc<Config>,
    pub http: reqwest::Client,
    pub tus: TusMap,
    /// Per-upload mpsc senders for TUS streaming (chunks flow directly to juicehost).
    pub tus_senders: TusSenderMap,
    /// Tracks multi-part parallel upload sessions.
    pub part_sessions: PartSessionMap,
    /// TUS storage tasks, awaited before publishing uploads or concatenating parts.
    pub push_handles: Arc<DashMap<String, tokio::task::JoinHandle<Result<(), String>>>>,
    /// In-memory set of banned IPs for fast O(1) lookups in middleware.
    pub banned_ips: DashMap<String, ()>,
    /// Failed admin login attempts: username -> (count, first_failure_time).
    pub failed_logins: DashMap<String, (u32, std::time::Instant)>,
    /// Pre-built juicehost authentication headers (built once at startup).
    pub juicehost_headers: reqwest::header::HeaderMap,
    /// Semaphore limiting concurrent direct (non-TUS) uploads.
    pub upload_semaphore: Arc<tokio::sync::Semaphore>,
    /// Juicehost-provided config (file size limits, TTL, danger level, etc.).
    /// `None` when juicehost is unreachable (degraded mode).
    pub jh_config: Arc<std::sync::RwLock<Option<Arc<JuicehostConfig>>>>,
    /// Persistent QUIC client endpoint for connection reuse.
    #[cfg(feature = "quic")]
    pub quic_endpoint: std::sync::Arc<tokio::sync::OnceCell<h3_quinn::quinn::Endpoint>>,
    /// Path to the QUIC TLS certificate for cert pinning.
    #[cfg(feature = "quic")]
    pub quic_cert_path: Option<std::path::PathBuf>,
    /// Connected device WebSocket connections, keyed by user_id.
    pub connected_devices: DashMap<String, Vec<Arc<tokio::sync::Mutex<ConnectedDevice>>>>,
    /// Broadcast senders for presence events, keyed by user_id.
    pub presence_listeners: DashMap<String, tokio::sync::broadcast::Sender<PresenceEvent>>,
    pub presence_semaphore: Arc<tokio::sync::Semaphore>,
    pub presence_by_ip: DashMap<std::net::IpAddr, usize>,
    pub notification_semaphore: Arc<tokio::sync::Semaphore>,
    pub mint_limiter: Arc<crate::mint_limiter::MintLimiter>,
}

impl AppState {
    /// Build a new AppState with the given configuration values.
    pub fn new(
        pool: DbPool,
        config: Config,
        http: reqwest::Client,
        juicehost_headers: reqwest::header::HeaderMap,
        jh_config: Option<JuicehostConfig>,
    ) -> Arc<Self> {
        #[cfg(feature = "quic")]
        let quic_cert_path = config.quic_cert_path.clone();

        let mint_limiter = Arc::new(crate::mint_limiter::MintLimiter::new(
            config.dte_mint_limit,
            config.dte_mint_window_secs,
            config.dte_mint_burst,
        ));

        Arc::new(Self {
            db: pool,
            config: Arc::new(config),
            http,
            tus: crate::tus::new_tus_state(),
            tus_senders: crate::tus::new_tus_sender_map(),
            part_sessions: crate::tus::new_part_session_map(),
            push_handles: Arc::new(DashMap::new()),
            banned_ips: DashMap::new(),
            failed_logins: DashMap::new(),
            juicehost_headers,
            upload_semaphore: Arc::new(tokio::sync::Semaphore::new(16)),
            jh_config: Arc::new(std::sync::RwLock::new(jh_config.map(Arc::new))),
            #[cfg(feature = "quic")]
            quic_endpoint: std::sync::Arc::new(tokio::sync::OnceCell::new()),
            #[cfg(feature = "quic")]
            quic_cert_path,
            connected_devices: DashMap::new(),
            presence_listeners: DashMap::new(),
            presence_semaphore: Arc::new(tokio::sync::Semaphore::new(
                crate::constants::MAX_SSE_CONNECTIONS,
            )),
            presence_by_ip: DashMap::new(),
            notification_semaphore: Arc::new(tokio::sync::Semaphore::new(8)),
            mint_limiter,
        })
    }

    /// Check if an IP is in the banned set.
    pub fn is_banned(&self, ip: &str) -> bool {
        self.banned_ips.contains_key(ip)
    }

    /// Reload the banned IPs set from the database.
    pub fn reload_banned_ips(&self) {
        if let Ok(conn) = self.db.get() {
            if let Ok(ips) = crate::db::load_banned_ips_set(&conn) {
                self.banned_ips.clear();
                for ip in ips {
                    self.banned_ips.insert(ip, ());
                }
                tracing::info!("reloaded {} banned IPs", self.banned_ips.len());
            }
        }
    }

    /// Ban an IP: write to DB and update in-memory cache atomically.
    pub async fn ban_ip(
        self: &Arc<Self>,
        ip: &str,
        reason: &str,
        banned_by: &str,
    ) -> Result<(), AppError> {
        let ip_c = ip.to_string();
        let reason_c = reason.to_string();
        let banned_by_c = banned_by.to_string();
        self.db_call("ban_ip", move |db| {
            crate::db::ban_ip(db, &ip_c, &reason_c, &banned_by_c)
        })
        .await?;
        self.banned_ips.insert(ip.to_string(), ());
        Ok(())
    }

    /// Unban an IP: remove from DB and update in-memory cache atomically.
    pub async fn unban_ip(self: &Arc<Self>, ip: &str) -> Result<bool, AppError> {
        let ip_c = ip.to_string();
        let deleted = self
            .db_call("unban_ip", move |db| crate::db::unban_ip(db, &ip_c))
            .await?;
        if deleted {
            self.banned_ips.remove(ip);
        }
        Ok(deleted)
    }

    /// Import many bans: write to DB in one transaction and update the in-memory cache.
    pub async fn import_bans(
        self: &Arc<Self>,
        bans: Vec<crate::db::ImportBan>,
    ) -> Result<usize, AppError> {
        let hashes: Vec<String> = bans.iter().map(|b| b.ip.clone()).collect();
        let count = self
            .db_call("bulk_ban_ips", move |db| crate::db::bulk_ban_ips(db, &bans))
            .await?;
        for h in hashes {
            self.banned_ips.insert(h, ());
        }
        Ok(count)
    }

    /// Run a database query on a background thread via r2d2 pool.
    pub async fn db_call<T, F>(self: &Arc<Self>, name: &'static str, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&rusqlite::Connection) -> Result<T, rusqlite::Error> + Send + 'static,
        T: Send + 'static,
    {
        let pool = self.db.clone();
        tokio::task::spawn_blocking(move || {
            let conn = pool
                .get()
                .map_err(|e| AppError::DbPoolError(e.to_string()))?;
            f(&conn).map_err(AppError::DatabaseError)
        })
        .await
        .map_err(|_| AppError::TaskPanicked(format!("db task '{}' panicked", name)))?
    }
}
