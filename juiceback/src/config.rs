//! config loaded from env vars. very straightforward, nothing to see here

use std::collections::HashMap;

/// Holds every setting juiceback needs to run.
///
/// All values come from environment variables with sensible fallbacks.
/// Check out Config::load to see what variables are available.
#[derive(Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub quic_port: u16,
    pub database_path: String,
    pub rate_limit_per_minute: u32,
    pub db_pool_size: u32,
    pub public_base_url: String,
    pub log_level: String,
    pub cleanup_interval_minutes: u64,
    pub juicehost_api_key: String,
    pub juicehost_url: String,
    /// Public URL that remote devices can use to reach juicehost. Falls back to
    /// PUBLIC_BASE_URL when PUBLIC_JUICEHOST_URL is unset. Must NOT be a loopback
    /// address or remote devices will try to upload to their own localhost.
    pub public_juicehost_url: String,
    pub juiceback_origin: String,
    pub jwt_secret: String,
    /// AES-256-GCM key for encrypting IP addresses before storage (hex-encoded).
    pub ip_encryption_key: String,
    /// Secret pepper for HMAC ban-lookup digests.
    pub ip_pepper: String,
    /// Comma-separated list of allowed CORS origins.
    pub cors_origins: Vec<String>,
    /// Webhook URL for report notifications (Discord, Slack, etc.).
    pub report_webhook_url: Option<String>,
    /// SMTP host for report email notifications.
    pub smtp_host: Option<String>,
    /// SMTP port (default 465 for TLS).
    pub smtp_port: Option<u16>,
    /// SMTP username.
    pub smtp_username: Option<String>,
    /// SMTP password.
    pub smtp_password: Option<String>,
    /// Email address to send report notifications to.
    pub report_email_recipient: Option<String>,
    /// Email address reports appear to come from.
    pub report_email_sender: Option<String>,
    /// Path to the QUIC TLS certificate file for cert pinning.
    pub quic_cert_path: Option<std::path::PathBuf>,
    /// CIDRs whose peers are allowed to supply forwarding headers.
    pub trusted_proxy_cidrs: Vec<juiceutils::proxy::IpCidr>,
    pub report_retention_days: u64,
    pub feedback_retention_days: u64,
    /// Cloudflare API token for cache purging (optional).
    pub cf_api_token: Option<String>,
    /// Cloudflare zone ID for cache purging (optional).
    pub cf_zone_id: Option<String>,
    /// JWT signing secret for upload tickets shared with juicehost. Falls back to jwt_secret if unset.
    pub ticket_jwt_secret: String,
    /// Whether authentication and ownership cookies require HTTPS.
    pub secure_cookies: bool,
    /// Whether browser uploads may bypass the Node/Juiceback byte relay.
    pub direct_upload_enabled: bool,
    /// Whether the JuiceBox x Cobalt.Tools URL-fetch feature is enabled (default off).
    pub cobalt_enabled: bool,
    /// Base URL of the self-hosted cobalt API instance.
    pub cobalt_api_url: String,
    /// Cobalt Api-Key auth token. Required when cobalt_enabled is true.
    pub cobalt_api_key: String,
    /// Optional second cobalt instance with YouTube session auth (cookies +
    /// poToken provider). Used as a fallback when the primary instance
    /// refuses a YouTube link at client level (private/age/region-locked).
    pub cobalt_session_api_url: Option<String>,
    /// Api-Key auth token for the session instance.
    pub cobalt_session_api_key: Option<String>,
    /// Optional Bun SABR-fetch sidecar (juicebox-bun-fetch): runs yt-dlp
    /// with Bun JS challenges + per-video PO tokens, ideally on the
    /// residential secondary box. Tried before the local yt-dlp tier.
    pub bun_fetch_url: Option<String>,
    /// Bearer token for the Bun sidecar.
    pub bun_fetch_key: Option<String>,
    /// Seconds between rescue-ladder passes for enforcement-gated YouTube
    /// links (flapping windows often open within seconds-minutes).
    pub fetch_empty_retry_delay_secs: u64,
    pub dte_enabled: bool,
    pub dte_assumed_bps: f64,
    pub dte_safety_mult: f64,
    pub dte_base_overhead_secs: i64,
    pub dte_min_ttl_secs: i64,
    pub dte_max_ttl_secs: i64,
    pub dte_mint_limit: u32,
    pub dte_mint_window_secs: u64,
    pub dte_mint_burst: u32,
    pub region_public_juicehosts: HashMap<String, String>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("quic_port", &self.quic_port)
            .field("database_path", &self.database_path)
            .field("rate_limit_per_minute", &self.rate_limit_per_minute)
            .field("db_pool_size", &self.db_pool_size)
            .field("public_base_url", &self.public_base_url)
            .field("log_level", &self.log_level)
            .field("cleanup_interval_minutes", &self.cleanup_interval_minutes)
            .field("juicehost_api_key", &"[REDACTED]")
            .field("juicehost_url", &self.juicehost_url)
            .field("public_juicehost_url", &self.public_juicehost_url)
            .field("juiceback_origin", &self.juiceback_origin)
            .field("jwt_secret", &"[REDACTED]")
            .field("ip_encryption_key", &"[REDACTED]")
            .field("ip_pepper", &"[REDACTED]")
            .field("cors_origins", &self.cors_origins)
            .field(
                "report_webhook_url",
                &self.report_webhook_url.as_ref().map(|_| "[REDACTED]"),
            )
            .field("smtp_host", &self.smtp_host)
            .field("smtp_port", &self.smtp_port)
            .field("smtp_username", &self.smtp_username)
            .field(
                "smtp_password",
                &self.smtp_password.as_ref().map(|_| "[REDACTED]"),
            )
            .field("report_email_recipient", &self.report_email_recipient)
            .field("report_email_sender", &self.report_email_sender)
            .field("trusted_proxy_cidrs", &self.trusted_proxy_cidrs)
            .field("report_retention_days", &self.report_retention_days)
            .field("feedback_retention_days", &self.feedback_retention_days)
            .field(
                "cf_api_token",
                &self.cf_api_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "cf_zone_id",
                &self.cf_zone_id.as_ref().map(|_| "[REDACTED]"),
            )
            .field("ticket_jwt_secret", &"[REDACTED]")
            .field("secure_cookies", &self.secure_cookies)
            .field("direct_upload_enabled", &self.direct_upload_enabled)
            .field("cobalt_enabled", &self.cobalt_enabled)
            .field("cobalt_api_url", &self.cobalt_api_url)
            .field("cobalt_api_key", &"[REDACTED]")
            .field("cobalt_session_api_url", &self.cobalt_session_api_url)
            .field(
                "cobalt_session_api_key",
                &self.cobalt_session_api_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("bun_fetch_url", &self.bun_fetch_url)
            .field(
                "bun_fetch_key",
                &self.bun_fetch_key.as_ref().map(|_| "[REDACTED]"),
            )
            .field("dte_enabled", &self.dte_enabled)
            .field("dte_assumed_bps", &self.dte_assumed_bps)
            .field("dte_safety_mult", &self.dte_safety_mult)
            .field("dte_base_overhead_secs", &self.dte_base_overhead_secs)
            .field("dte_min_ttl_secs", &self.dte_min_ttl_secs)
            .field("dte_max_ttl_secs", &self.dte_max_ttl_secs)
            .field("dte_mint_limit", &self.dte_mint_limit)
            .field("dte_mint_window_secs", &self.dte_mint_window_secs)
            .field("dte_mint_burst", &self.dte_mint_burst)
            .field("region_public_juicehosts", &self.region_public_juicehosts)
            .finish()
    }
}

impl Config {
    /// Load settings from the environment. see env vars below if you care.
    pub fn load() -> Result<Self, String> {
        let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
        let port = std::env::var("PORT")
            .unwrap_or_else(|_| "6401".to_string())
            .parse::<u16>()
            .map_err(|e| format!("Invalid PORT: {}", e))?;

        let quic_port = std::env::var("QUIC_PORT")
            .ok()
            .and_then(|p| p.parse::<u16>().ok())
            .unwrap_or(port + 1);

        let database_path =
            std::env::var("DATABASE_PATH").unwrap_or_else(|_| "./delta.db".to_string());

        let rate_limit_per_minute = std::env::var("RATE_LIMIT_PER_MINUTE")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u32>()
            .map_err(|e| format!("Invalid RATE_LIMIT_PER_MINUTE: {}", e))?;

        let db_pool_size = std::env::var("DB_POOL_SIZE")
            .unwrap_or_else(|_| "8".to_string())
            .parse::<u32>()
            .map_err(|e| format!("Invalid DB_POOL_SIZE: {}", e))?;

        let public_base_url = std::env::var("PUBLIC_BASE_URL")
            .unwrap_or_else(|_| "http://localhost:6402".to_string())
            .trim_end_matches('/')
            .to_string();

        let log_level = std::env::var("LOG_LEVEL").unwrap_or_else(|_| "info".to_string());

        let cleanup_interval_minutes = std::env::var("CLEANUP_INTERVAL_MINUTES")
            .unwrap_or_else(|_| "30".to_string())
            .parse::<u64>()
            .map_err(|e| format!("Invalid CLEANUP_INTERVAL_MINUTES: {}", e))?;

        let juicehost_api_key = std::env::var("JUICEHOST_API_KEY").unwrap_or_default();

        let juicehost_url = std::env::var("JUICEHOST_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:6402".to_string())
            .trim_end_matches('/')
            .to_string();

        let public_juicehost_url = std::env::var("PUBLIC_JUICEHOST_URL")
            .unwrap_or_else(|_| public_base_url.clone())
            .trim_end_matches('/')
            .to_string();

        let juiceback_origin = std::env::var("JUICEBACK_ORIGIN")
            .unwrap_or_else(|_| format!("http://{}:{}", host, port))
            .trim_end_matches('/')
            .to_string();

        let jwt_secret = std::env::var("JWT_SECRET").unwrap_or_else(|_| {
            let generated = uuid::Uuid::new_v4().to_string();
            tracing::warn!(
                "JWT_SECRET not set, using generated random secret (admins will be invalidated on restart)"
            );
            generated
        });

        let ip_encryption_key = std::env::var("IP_ENCRYPTION_KEY").unwrap_or_else(|_| {
            use rand::RngCore;
            let mut key = [0_u8; 32];
            rand::rngs::OsRng.fill_bytes(&mut key);
            let generated = hex::encode(key);
            tracing::warn!(
                "IP_ENCRYPTION_KEY not set, using generated key (existing encrypted IPs will be invalid on restart)"
            );
            generated
        });

        let encryption_key_bytes = hex::decode(&ip_encryption_key)
            .map_err(|_| "IP_ENCRYPTION_KEY must be a hexadecimal string".to_string())?;
        if encryption_key_bytes.len() != 32 {
            return Err("IP_ENCRYPTION_KEY must encode exactly 32 bytes".to_string());
        }

        let ip_pepper = std::env::var("IP_PEPPER").unwrap_or_else(|_| {
            let generated = uuid::Uuid::new_v4().to_string();
            tracing::warn!(
                "IP_PEPPER not set, using generated pepper (bans will be invalidated on restart)"
            );
            generated
        });

        let cors_origins = std::env::var("CORS_ORIGINS")
            .unwrap_or_else(|_| "http://localhost:6400".to_string())
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let report_webhook_url = std::env::var("REPORT_WEBHOOK_URL")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let smtp_host = std::env::var("SMTP_HOST")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let smtp_port = std::env::var("SMTP_PORT")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.parse::<u16>().unwrap_or(465));
        let smtp_username = std::env::var("SMTP_USERNAME")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let smtp_password = std::env::var("SMTP_PASSWORD")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let report_email_recipient = std::env::var("REPORT_EMAIL_RECIPIENT")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let report_email_sender = std::env::var("REPORT_EMAIL_SENDER")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let quic_cert_path = Some(
            std::env::var("QUIC_CERT_PATH")
                .ok()
                .filter(|s| !s.trim().is_empty())
                .map(std::path::PathBuf::from)
                .unwrap_or_else(|| std::path::PathBuf::from("./quic-cert.der")),
        );

        let trusted_proxy_cidrs = juiceutils::proxy::parse_trusted_proxy_cidrs(
            &std::env::var("TRUSTED_PROXY_CIDRS").unwrap_or_default(),
        )?;
        let report_retention_days = std::env::var("REPORT_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);
        let feedback_retention_days = std::env::var("FEEDBACK_RETENTION_DAYS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);

        let cf_api_token = std::env::var("CF_API_TOKEN")
            .ok()
            .filter(|s| !s.trim().is_empty());
        let cf_zone_id = std::env::var("CF_ZONE_ID")
            .ok()
            .filter(|s| !s.trim().is_empty());

        let ticket_jwt_secret =
            std::env::var("TICKET_JWT_SECRET").unwrap_or_else(|_| jwt_secret.clone());

        let secure_cookies = std::env::var("SECURE_COOKIES")
            .ok()
            .map(|value| value == "true" || value == "1")
            .unwrap_or(true);

        let direct_upload_enabled = std::env::var("DIRECT_UPLOAD_ENABLED")
            .ok()
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);

        let cobalt_enabled = std::env::var("COBALT_ENABLED")
            .ok()
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);

        let cobalt_api_url = std::env::var("COBALT_API_URL")
            .unwrap_or_else(|_| "http://localhost:7272".to_string())
            .trim_end_matches('/')
            .to_string();

        let cobalt_api_key = std::env::var("COBALT_API_KEY").unwrap_or_default();

        let dte_enabled = std::env::var("DTE")
            .ok()
            .map(|value| value == "true" || value == "1")
            .unwrap_or(false);

        let dte_assumed_bps = std::env::var("DTE_ASSUMED_BPS")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(crate::constants::DTE_ASSUMED_BPS);
        let dte_safety_mult = std::env::var("DTE_SAFETY_MULT")
            .ok()
            .and_then(|v| v.parse::<f64>().ok())
            .unwrap_or(crate::constants::DTE_SAFETY_MULT);
        let dte_base_overhead_secs = std::env::var("DTE_BASE_OVERHEAD_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(crate::constants::DTE_BASE_OVERHEAD_SECS);
        let dte_min_ttl_secs = std::env::var("DTE_MIN_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(crate::constants::DTE_MIN_TTL_SECS);
        let dte_max_ttl_secs = std::env::var("DTE_MAX_TTL_SECS")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .unwrap_or(crate::constants::DTE_MAX_TTL_SECS);
        let dte_mint_limit = std::env::var("DTE_MINT_LIMIT")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(crate::constants::DTE_MINT_LIMIT);
        let dte_mint_window_secs = std::env::var("DTE_MINT_WINDOW_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(crate::constants::DTE_MINT_WINDOW_SECS);
        let dte_mint_burst = std::env::var("DTE_MINT_BURST")
            .ok()
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(crate::constants::DTE_MINT_BURST);

        let region_public_juicehosts: HashMap<String, String> =
            std::env::var("REGION_PUBLIC_JUICEHOSTS")
                .unwrap_or_default()
                .split(',')
                .filter_map(|pair| {
                    let pair = pair.trim();
                    if pair.is_empty() {
                        return None;
                    }
                    let (region, url) = pair.split_once(':')?;
                    let region = region.trim().to_ascii_lowercase();
                    let url = url.trim().trim_end_matches('/').to_string();
                    if region.is_empty() || url.is_empty() {
                        None
                    } else {
                        Some((region, url))
                    }
                })
                .collect();

        let cobalt_session_api_url = std::env::var("COBALT_SESSION_API_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty());
        let cobalt_session_api_key = std::env::var("COBALT_SESSION_API_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let fetch_empty_retry_delay_secs = std::env::var("FETCH_EMPTY_RETRY_DELAY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(crate::constants::FETCH_EMPTY_RETRY_DELAY_SECS);

        if cobalt_session_api_url.is_some() != cobalt_session_api_key.is_some() {
            return Err(
                "COBALT_SESSION_API_URL and COBALT_SESSION_API_KEY must be set together \
                 (or both left unset)."
                    .to_string(),
            );
        }

        let bun_fetch_url = std::env::var("BUN_FETCH_URL")
            .ok()
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty());
        let bun_fetch_key = std::env::var("BUN_FETCH_KEY")
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        if bun_fetch_url.is_some() != bun_fetch_key.is_some() {
            return Err(
                "BUN_FETCH_URL and BUN_FETCH_KEY must be set together \
                 (or both left unset)."
                    .to_string(),
            );
        }

        // Refuse to start with placeholder secrets
        let placeholder_values = ["change_this_to_a_random_value", "change_this_in_production"];
        if placeholder_values.contains(&jwt_secret.as_str()) || jwt_secret.is_empty() {
            return Err(
                "JWT_SECRET is set to a placeholder value. Generate a real secret before starting."
                    .to_string(),
            );
        }

        if cobalt_enabled && cobalt_api_key.is_empty() {
            return Err(
                "COBALT_ENABLED is true but COBALT_API_KEY is not set. Cobalt instances with \
                 auth enabled require an API key; set COBALT_API_KEY or disable COBALT_ENABLED."
                    .to_string(),
            );
        }

        Ok(Self {
            host,
            port,
            quic_port,
            database_path,
            rate_limit_per_minute,
            db_pool_size,
            public_base_url,
            log_level,
            cleanup_interval_minutes,
            juicehost_api_key,
            juicehost_url,
            public_juicehost_url,
            juiceback_origin,
            jwt_secret,
            ip_encryption_key,
            ip_pepper,
            cors_origins,
            report_webhook_url,
            smtp_host,
            smtp_port,
            smtp_username,
            smtp_password,
            report_email_recipient,
            report_email_sender,
            quic_cert_path,
            trusted_proxy_cidrs,
            report_retention_days,
            feedback_retention_days,
            cf_api_token,
            cf_zone_id,
            ticket_jwt_secret,
            secure_cookies,
            direct_upload_enabled,
            cobalt_enabled,
            cobalt_api_url,
            cobalt_api_key,
            cobalt_session_api_url,
            cobalt_session_api_key,
            bun_fetch_url,
            bun_fetch_key,
            fetch_empty_retry_delay_secs,
            dte_enabled,
            dte_assumed_bps,
            dte_safety_mult,
            dte_base_overhead_secs,
            dte_min_ttl_secs,
            dte_max_ttl_secs,
            dte_mint_limit,
            dte_mint_window_secs,
            dte_mint_burst,
            region_public_juicehosts,
        })
    }
}
