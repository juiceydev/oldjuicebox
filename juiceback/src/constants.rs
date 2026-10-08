/// Hours-to-seconds conversion factor.
pub const SECONDS_PER_HOUR: i64 = 3600;
/// Hours-to-seconds conversion factor (f64).
pub const SECONDS_PER_HOUR_F64: f64 = 3600.0;
/// Additional buffer added to upload size for overhead (1 MiB).
pub const UPLOAD_SIZE_OVERHEAD: usize = 1024 * 1024;

/// Capacity of the streaming upload channel.
pub const STREAM_CHANNEL_CAPACITY: usize = 256;
/// Maximum allowed filename length.
pub const MAX_FILENAME_LEN: usize = 255;
/// Maximum length for a custom file ID slug.
pub const MAX_CUSTOM_ID_LEN: usize = 32;
/// Minimum length for a custom file ID slug.
pub const MIN_CUSTOM_ID_LEN: usize = 3;
/// Buffer size for reading gzip-compressed data (128 KiB).
pub const GZIP_READ_BUFFER_SIZE: usize = 128 * 1024;
/// Maximum concurrent TUS upload sessions.
pub const MAX_TUS_SESSIONS: usize = 1000;
/// Maximum number of parts in one parallel TUS upload.
pub const MAX_TUS_PARALLEL_PARTS: usize = 64;
/// Bytes per MiB (1024 * 1024).
pub const BYTES_PER_MIB: f64 = 1024.0 * 1024.0;

/// How long incomplete uploads may remain before cleanup purges them.
pub const STALE_UPLOAD_TIMEOUT_SECS: i64 = 3600;

/// Admin rate limit window in seconds.
pub const ADMIN_RATE_LIMIT_WINDOW_SECS: u64 = 60;
/// Admin rate limit burst size (requests per window).
pub const ADMIN_RATE_LIMIT_BURST: u32 = 5;
/// Admin JWT cookie max age in seconds (24 hours).
pub const ADMIN_JWT_COOKIE_MAX_AGE_SECS: i64 = 86400;

/// HTTP client connect timeout in seconds.
pub const HTTP_CONNECT_TIMEOUT_SECS: u64 = 10;
/// HTTP client idle timeout in seconds.
pub const HTTP_IDLE_TIMEOUT_SECS: u64 = 30;
/// Maximum concurrent TUS sessions per IP address.
/// Sized so two max-part uploads (32 parts each) fit alongside smaller ones.
pub const MAX_TUS_PER_IP: usize = 128;
pub const MAX_REPORT_FILE_URL_LEN: usize = 2048;
pub const MAX_REPORT_REASON_LEN: usize = 256;
pub const MAX_REPORT_DETAILS_LEN: usize = 4096;
pub const MAX_FEEDBACK_MESSAGE_LEN: usize = 4096;
pub const MAX_FEEDBACK_EMAIL_LEN: usize = 320;
pub const MAX_PAIRING_CODES_PER_USER: i64 = 5;
pub const MAX_PAIRING_CODES_PER_IP: i64 = 10;
pub const MAX_SSE_CONNECTIONS: usize = 256;
pub const MAX_SSE_CONNECTIONS_PER_IP: u32 = 8;

/// Interval (seconds) between background retries of juicehost config fetch when in degraded mode.
pub const DEGRADED_RETRY_INTERVAL_SECS: u64 = 30;
/// Maximum number of startup retries for juicehost config fetch.
pub const STARTUP_CONFIG_RETRIES: u32 = 3;
/// Backoff base delay (seconds) for startup retries.
pub const STARTUP_BACKOFF_BASE_SECS: u64 = 2;

// == JuiceBox x Cobalt.Tools URL fetching ==

/// Cobalt fetch requests allowed per minute per IP.
pub const COBALT_FETCH_RATE_LIMIT_PER_MINUTE: u32 = 3;
/// Hard wall-clock limit for one background fetch job (cobalt processing + transfer).
pub const FETCH_JOB_TIMEOUT_SECS: u64 = 30 * 60;
/// Wait before a second primary attempt when googlevideo serves an empty
/// stream — transient per-IP blocks often clear within minutes.
pub const FETCH_EMPTY_RETRY_DELAY_SECS: u64 = 10;
/// Maximum rescue-ladder passes per fetch job.
pub const FETCH_RESCUE_MAX_PASSES: u32 = 30;
/// Done/failed fetch jobs are purged this many seconds after their last update.
pub const FETCH_JOB_RETENTION_SECS: i64 = 86_400;
/// Pending fetch jobs with no update for this long are considered dead.
pub const STALE_FETCH_JOB_TIMEOUT_SECS: i64 = 3600;
/// Maximum accepted length for a user-supplied source URL.
pub const FETCH_SOURCE_URL_MAX_LEN: usize = 2048;

pub fn clamp_to_nearest(value: f64, allowed: &[f64]) -> f64 {
    assert!(!allowed.is_empty(), "allowed_ttl_hours must not be empty");
    *allowed
        .iter()
        .min_by_key(|&&a| ((a - value).abs() * 1000.0) as u64)
        .unwrap()
}

pub const DTE_ASSUMED_BPS: f64 = 2.0 * 1024.0 * 1024.0;
pub const DTE_SAFETY_MULT: f64 = 2.0;
pub const DTE_BASE_OVERHEAD_SECS: i64 = 120;
pub const DTE_MIN_TTL_SECS: i64 = 600;
pub const DTE_MAX_TTL_SECS: i64 = 21600;

pub const DTE_MINT_LIMIT: u32 = 60;
pub const DTE_MINT_WINDOW_SECS: u64 = 600;
pub const DTE_MINT_BURST: u32 = 15;
