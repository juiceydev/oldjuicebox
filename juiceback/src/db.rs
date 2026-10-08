//! SQLite schema, migrations, and CRUD operations for application data.

use rusqlite::{params, Connection, OptionalExtension, Result};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Result of attempting to complete an existing reservation.
#[derive(Debug, Clone)]
pub enum CompleteReservationResult {
    Completed(Box<FileRecord>),
    NotFound,
    NotUploading,
    InvalidToken,
}

/// Atomically mark an owned, pending reservation ready with its final metadata.
pub fn complete_reservation(
    db: &Connection,
    filename: &str,
    mime_type: &str,
    size_bytes: i64,
    id: &str,
    delete_token: &str,
    uploader_ip: Option<&str>,
) -> Result<CompleteReservationResult> {
    let affected = if let Some(ip) = uploader_ip {
        db.execute(
            "UPDATE files SET filename = ?1, mime_type = ?2, size_bytes = ?3, uploader_ip = ?6, status = 'ready' \
             WHERE id = ?4 AND delete_token = ?5 AND status = 'uploading'",
            params![filename, mime_type, size_bytes, id, delete_token, ip],
        )?
    } else {
        db.execute(
            "UPDATE files SET filename = ?1, mime_type = ?2, size_bytes = ?3, status = 'ready' \
             WHERE id = ?4 AND delete_token = ?5 AND status = 'uploading'",
            params![filename, mime_type, size_bytes, id, delete_token],
        )?
    };
    if affected == 1 {
        return Ok(CompleteReservationResult::Completed(Box::new(
            get_file(db, id)?.ok_or(rusqlite::Error::QueryReturnedNoRows)?,
        )));
    }

    let Some(record) = get_file(db, id)? else {
        return Ok(CompleteReservationResult::NotFound);
    };
    if record.status != "uploading" {
        return Ok(CompleteReservationResult::NotUploading);
    }
    Ok(CompleteReservationResult::InvalidToken)
}

/// An admin user from the admins table.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

/// A report record from the reports table.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReportRecord {
    pub id: i64,
    pub file_url: String,
    pub reason: String,
    pub details: String,
    pub reporter_ip: Option<String>,
    pub email: Option<String>,
    pub created_at: i64,
}

/// A feedback record from the feedback table.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeedbackRecord {
    pub id: i64,
    pub message: String,
    pub email: Option<String>,
    pub reporter_ip: Option<String>,
    pub created_at: i64,
}

/// A banned IP record.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct BanRecord {
    pub ip: String,
    pub reason: String,
    pub banned_by: String,
    pub banned_at: i64,
}

/// A known custom host (juicehost instance) that users have pointed the UI at.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct HosterRecord {
    pub host: String,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub last_status: String,
    pub last_error: String,
    pub banned: bool,
    pub banned_reason: String,
    pub banned_by: String,
    pub banned_at: Option<i64>,
}

/// A site-wide announcement.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Announcement {
    pub id: i64,
    pub message: String,
    pub link_url: String,
    pub mode: String,
    pub is_active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// A file record from the files table.
///
/// storage_path points to where the file lives on juicehost's disk.
/// The actual bytes never touch juiceback's filesystem during a streaming upload.
#[derive(Debug, Clone)]
pub struct FileRecord {
    pub id: String,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i64,
    pub storage_path: String,
    pub delete_token: String,
    pub uploaded_at: i64,
    pub expires_at: i64,
    pub uploader_ip: Option<String>,
    pub storage_host: Option<String>,
    pub status: String,
}

const FILE_COLUMNS: &str = "id, filename, mime_type, size_bytes, storage_path, \
    delete_token, uploaded_at, expires_at, uploader_ip, storage_host, status";

impl FileRecord {
    /// Create a new FileRecord with the standard remote storage path.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: String,
        filename: String,
        mime_type: String,
        size_bytes: i64,
        delete_token: String,
        uploaded_at: i64,
        expires_at: i64,
        uploader_ip: Option<String>,
        storage_host: Option<String>,
    ) -> Self {
        Self {
            storage_path: format!("remote-{}", id),
            id,
            filename,
            mime_type,
            size_bytes,
            delete_token,
            uploaded_at,
            expires_at,
            uploader_ip,
            storage_host,
            status: "ready".to_string(),
        }
    }

    /// Build a FileRecord from upload parameters with an existing delete token.
    #[allow(clippy::too_many_arguments)]
    pub fn from_upload_with_token(
        id: String,
        filename: String,
        mime_type: String,
        size_bytes: i64,
        ttl_hours: f64,
        delete_token: String,
        uploader_ip: Option<String>,
        storage_host: Option<String>,
    ) -> Self {
        let now = chrono::Utc::now().timestamp();
        let ttl_secs = (ttl_hours * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
        Self::new(
            id,
            filename,
            mime_type,
            size_bytes,
            delete_token,
            now,
            now + ttl_secs,
            uploader_ip,
            storage_host,
        )
    }
}

fn row_to_file_record(row: &rusqlite::Row) -> rusqlite::Result<FileRecord> {
    let raw_host: String = row.get(9)?;
    let status: String = row.get(10).unwrap_or_else(|_| "ready".to_string());
    Ok(FileRecord {
        id: row.get(0)?,
        filename: row.get(1)?,
        mime_type: row.get(2)?,
        size_bytes: row.get(3)?,
        storage_path: row.get(4)?,
        delete_token: row.get(5)?,
        uploaded_at: row.get(6)?,
        expires_at: row.get(7)?,
        uploader_ip: row.get(8)?,
        storage_host: if raw_host.is_empty() {
            None
        } else {
            Some(raw_host)
        },
        status,
    })
}

/// A file owned by a client (server-side mirror of the browser's upload list).
///
/// juiceback's `files` table is anonymous (no owner), so the browser holds the
/// only copy of "what did I upload". This registry persists that list per client
/// (keyed by the `jb_uid` cookie user id), so a no-JS SSR render can show the
/// full list without the ~4KB cookie cap. Field names mirror the compact cookie
/// payload (`n`/`m`/`s`/`u`/`e`) plus the owning `id`/`token` pair.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ClientFileRecord {
    pub id: String,
    #[serde(default, skip_serializing)]
    pub token: String,
    #[serde(default)]
    pub n: String, // filename
    #[serde(default)]
    pub m: String, // mime_type
    #[serde(default)]
    pub s: i64, // size_bytes
    #[serde(default)]
    pub u: i64, // uploaded_at (sec)
    #[serde(default)]
    pub e: i64, // expires_at (sec)
}

/// Set up the database tables and run any needed migrations.
///
/// Creates the files and admins tables plus an index on expires_at.
/// Also handles the storage_host column migration for older databases.
pub fn init_db(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            token_hash   TEXT PRIMARY KEY,
            user_id      TEXT NOT NULL,
            created_at   INTEGER NOT NULL,
            expires_at   INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_sessions_expires ON sessions(expires_at);",
    )?;
    conn.execute(
        "CREATE TABLE IF NOT EXISTS files (
            id            TEXT PRIMARY KEY,
            filename      TEXT NOT NULL,
            mime_type     TEXT NOT NULL,
            size_bytes    INTEGER NOT NULL,
            storage_path  TEXT NOT NULL UNIQUE,
            delete_token  TEXT NOT NULL,
            uploaded_at   INTEGER NOT NULL,
            expires_at    INTEGER NOT NULL,
            uploader_ip   TEXT
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS admins (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            username      TEXT NOT NULL UNIQUE,
            password_hash TEXT NOT NULL,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS reports (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            file_url      TEXT NOT NULL,
            reason        TEXT NOT NULL,
            details       TEXT NOT NULL DEFAULT '',
            reporter_ip   TEXT,
            email         TEXT,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS banned_ips (
            ip          TEXT PRIMARY KEY,
            reason      TEXT NOT NULL DEFAULT '',
            banned_by   TEXT NOT NULL DEFAULT 'admin',
            banned_at   INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS announcements (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            message     TEXT NOT NULL DEFAULT '',
            link_url    TEXT NOT NULL DEFAULT '',
            mode        TEXT NOT NULL DEFAULT 'warning',
            is_active   INTEGER NOT NULL DEFAULT 0,
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    conn.execute(
        "CREATE TABLE IF NOT EXISTS feedback (
            id            INTEGER PRIMARY KEY AUTOINCREMENT,
            message       TEXT NOT NULL,
            email         TEXT,
            reporter_ip   TEXT,
            created_at    INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    // Known custom hosts users have pointed the UI at. First use is tracked so
    // admins can see who's been used and ban offenders.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS hosters (
            host          TEXT PRIMARY KEY,
            first_seen_at INTEGER NOT NULL DEFAULT (unixepoch()),
            last_seen_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            last_status   TEXT NOT NULL DEFAULT 'unknown',
            last_error    TEXT NOT NULL DEFAULT '',
            banned        INTEGER NOT NULL DEFAULT 0,
            banned_reason TEXT NOT NULL DEFAULT '',
            banned_by     TEXT NOT NULL DEFAULT '',
            banned_at     INTEGER
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_hosters_last_seen ON hosters(last_seen_at)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_expires_at ON files(expires_at)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_reports_created_at ON reports(created_at)",
        [],
    )?;

    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_feedback_created_at ON feedback(created_at)",
        [],
    )?;

    // Table for persisting admin failed login attempts (survives restarts)
    conn.execute(
        "CREATE TABLE IF NOT EXISTS failed_logins (
            username   TEXT NOT NULL,
            ip_hash    TEXT NOT NULL DEFAULT '',
            attempt_at INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;

    // Maps previous file IDs to their current ID so old URLs can redirect.
    conn.execute(
        "CREATE TABLE IF NOT EXISTS aliases (
            old_id    TEXT PRIMARY KEY,
            new_id    TEXT NOT NULL,
            created_at INTEGER NOT NULL DEFAULT (unixepoch())
        )",
        [],
    )?;
    conn.execute(
        "CREATE INDEX IF NOT EXISTS idx_failed_logins_username ON failed_logins(username)",
        [],
    )?;
    let has_failed_login_ip: bool = conn
        .prepare("PRAGMA table_info(failed_logins)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "ip_hash"));
    if !has_failed_login_ip {
        conn.execute(
            "ALTER TABLE failed_logins ADD COLUMN ip_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    // Migration: add storage_host column if it doesn't exist
    let has_storage_host: bool = conn
        .prepare("PRAGMA table_info(files)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "storage_host"));
    if !has_storage_host {
        conn.execute(
            "ALTER TABLE files ADD COLUMN storage_host TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    // Migration: add email column to reports if it doesn't exist
    let has_report_email: bool = conn
        .prepare("PRAGMA table_info(reports)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "email"));
    if !has_report_email {
        conn.execute("ALTER TABLE reports ADD COLUMN email TEXT", [])?;
    }

    // Migration: add mode column to announcements if it doesn't exist
    let has_announcement_mode: bool = conn
        .prepare("PRAGMA table_info(announcements)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "mode"));
    if !has_announcement_mode {
        conn.execute(
            "ALTER TABLE announcements ADD COLUMN mode TEXT NOT NULL DEFAULT 'warning'",
            [],
        )?;
    }

    // Migration: add status column to files if it doesn't exist
    let has_file_status: bool = conn
        .prepare("PRAGMA table_info(files)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "status"));
    if !has_file_status {
        conn.execute(
            "ALTER TABLE files ADD COLUMN status TEXT NOT NULL DEFAULT 'ready'",
            [],
        )?;
    }

    // Pairing codes: short-lived codes for juicebox-plus device registration
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS pairing_codes (
            code_hash TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            used INTEGER NOT NULL DEFAULT 0,
            ip_hash TEXT NOT NULL DEFAULT ''
        );
        CREATE INDEX IF NOT EXISTS idx_pairing_codes_expires ON pairing_codes(expires_at);",
    )?;
    let has_pairing_ip: bool = conn
        .prepare("PRAGMA table_info(pairing_codes)")?
        .query_map([], |row| row.get::<_, String>(1))?
        .any(|name| name.is_ok_and(|n| n == "ip_hash"));
    if !has_pairing_ip {
        conn.execute(
            "ALTER TABLE pairing_codes ADD COLUMN ip_hash TEXT NOT NULL DEFAULT ''",
            [],
        )?;
    }

    // Registered devices (juicebox-plus instances paired to user accounts)
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS devices (
            id TEXT PRIMARY KEY,
            user_id TEXT NOT NULL,
            device_name TEXT NOT NULL,
            paired_at INTEGER NOT NULL,
            last_seen_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_devices_user ON devices(user_id);",
    )?;

    // Per-client upload registry: the browser's upload list mirrored server-side
    // (keyed by the `jb_uid` user id), so no-JS /files can render the full list
    // without the ~4KB cookie cap and without the files table knowing the owner.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS client_files (
            client_key   TEXT NOT NULL,
            file_id      TEXT NOT NULL,
            filename     TEXT NOT NULL DEFAULT '',
            mime_type    TEXT NOT NULL DEFAULT '',
            size_bytes   INTEGER NOT NULL DEFAULT 0,
            uploaded_at  INTEGER NOT NULL DEFAULT 0,
            expires_at   INTEGER NOT NULL DEFAULT 0,
            delete_token TEXT NOT NULL,
            PRIMARY KEY (client_key, file_id)
        );
        CREATE INDEX IF NOT EXISTS idx_client_files_client ON client_files(client_key);",
    )?;

    // JuiceBox x Cobalt.Tools URL fetch jobs. Tracks async cobalt processing
    // so the browser can poll for completion; file_id links to a normal
    // record in the files table once the transfer succeeds.
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS fetch_jobs (
            id          TEXT PRIMARY KEY,
            user_id     TEXT NOT NULL,
            source_url  TEXT NOT NULL,
            status      TEXT NOT NULL DEFAULT 'pending',
            error       TEXT NOT NULL DEFAULT '',
            file_id     TEXT NOT NULL DEFAULT '',
            created_at  INTEGER NOT NULL DEFAULT (unixepoch()),
            updated_at  INTEGER NOT NULL DEFAULT (unixepoch())
        );
        CREATE INDEX IF NOT EXISTS idx_fetch_jobs_user ON fetch_jobs(user_id);
        CREATE INDEX IF NOT EXISTS idx_fetch_jobs_updated ON fetch_jobs(updated_at);",
    )?;

    // Progress columns (added after the table shipped); ignore errors when
    // they already exist on upgraded databases.
    let _ = conn.execute(
        "ALTER TABLE fetch_jobs ADD COLUMN stage TEXT NOT NULL DEFAULT ''",
        [],
    );
    let _ = conn.execute(
        "ALTER TABLE fetch_jobs ADD COLUMN bytes_received INTEGER NOT NULL DEFAULT 0",
        [],
    );

    Ok(())
}

pub fn resolve_session(conn: &Connection, token_hash: &str, now: i64) -> Result<Option<String>> {
    let user_id = conn
        .query_row(
            "SELECT user_id FROM sessions WHERE token_hash = ?1 AND expires_at > ?2",
            params![token_hash, now],
            |row| row.get(0),
        )
        .optional()?;
    if user_id.is_some() {
        conn.execute(
            "UPDATE sessions SET last_seen_at = ?2 WHERE token_hash = ?1",
            params![token_hash, now],
        )?;
    }
    Ok(user_id)
}

pub fn create_session(
    conn: &Connection,
    token_hash: &str,
    user_id: &str,
    now: i64,
    expires_at: i64,
) -> Result<()> {
    conn.execute(
        "INSERT INTO sessions (token_hash, user_id, created_at, expires_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?3)",
        params![token_hash, user_id, now, expires_at],
    )?;
    Ok(())
}

pub fn cleanup_ephemeral_data(
    conn: &Connection,
    now: i64,
    report_cutoff: i64,
    feedback_cutoff: i64,
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut deleted = tx.execute("DELETE FROM sessions WHERE expires_at <= ?1", params![now])?;
    deleted += tx.execute(
        "DELETE FROM pairing_codes WHERE used != 0 OR expires_at <= ?1",
        params![now],
    )?;
    deleted += tx.execute(
        "DELETE FROM reports WHERE created_at < ?1",
        params![report_cutoff],
    )?;
    deleted += tx.execute(
        "DELETE FROM feedback WHERE created_at < ?1",
        params![feedback_cutoff],
    )?;
    deleted += tx.execute(
        "DELETE FROM failed_logins WHERE attempt_at < ?1",
        params![now - 86_400],
    )?;
    match cleanup_fetch_jobs(&tx, now) {
        Ok(n) => deleted += n,
        Err(e) => tracing::warn!("fetch_jobs cleanup failed: {}", e),
    }
    tx.commit()?;
    Ok(deleted)
}

pub fn device_belongs_to_user(conn: &Connection, device_id: &str, user_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM devices WHERE id = ?1 AND user_id = ?2",
            params![device_id, user_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn client_owns_file(conn: &Connection, user_id: &str, file_id: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT 1 FROM client_files WHERE client_key = ?1 AND file_id = ?2",
            params![user_id, file_id],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

pub fn add_client_file(conn: &Connection, user_id: &str, record: &FileRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO client_files
         (client_key, file_id, filename, mime_type, size_bytes, uploaded_at, expires_at, delete_token)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(client_key, file_id) DO UPDATE SET
           filename = excluded.filename,
           mime_type = excluded.mime_type,
           size_bytes = excluded.size_bytes,
           uploaded_at = excluded.uploaded_at,
           expires_at = excluded.expires_at",
        params![
            user_id,
            record.id,
            record.filename,
            record.mime_type,
            record.size_bytes,
            record.uploaded_at,
            record.expires_at,
            record.delete_token
        ],
    )?;
    Ok(())
}

pub fn remove_client_file(conn: &Connection, user_id: &str, file_id: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM client_files WHERE client_key = ?1 AND file_id = ?2",
        params![user_id, file_id],
    )?;
    Ok(())
}

pub fn import_verified_client_files(
    conn: &Connection,
    user_id: &str,
    records: &[ClientFileRecord],
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    let mut imported = 0;
    for record in records {
        let verified: bool = tx
            .query_row(
                "SELECT 1 FROM files WHERE id = ?1 AND storage_host = '' AND delete_token = ?2",
                params![record.id, record.token],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if verified {
            imported += tx.execute(
                "INSERT OR IGNORE INTO client_files
                 (client_key, file_id, filename, mime_type, size_bytes, uploaded_at, expires_at, delete_token)
                 SELECT ?1, id, filename, mime_type, size_bytes, uploaded_at, expires_at, delete_token
                 FROM files WHERE id = ?2",
                params![user_id, record.id],
            )?;
        }
    }
    tx.commit()?;
    Ok(imported)
}

// == cobalt fetch jobs ==

/// A JuiceBox x Cobalt.Tools URL fetch job.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FetchJob {
    pub id: String,
    pub user_id: String,
    pub source_url: String,
    /// `pending`, `processing`, `downloading`, `done`, or `failed`.
    pub status: String,
    pub error: String,
    /// Set once the fetched media has been stored as a normal file.
    #[serde(default)]
    pub file_id: String,
    pub created_at: i64,
    pub updated_at: i64,
    /// Fine-grained progress hint (e.g. "cobalt", "session-fallback",
    /// "transfer"); empty for terminal states.
    #[serde(default)]
    pub stage: String,
    /// Bytes received from the media source so far (best-effort).
    #[serde(default)]
    pub bytes_received: i64,
}

fn row_to_fetch_job(row: &rusqlite::Row) -> rusqlite::Result<FetchJob> {
    Ok(FetchJob {
        id: row.get(0)?,
        user_id: row.get(1)?,
        source_url: row.get(2)?,
        status: row.get(3)?,
        error: row.get(4)?,
        file_id: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        stage: row.get(8)?,
        bytes_received: row.get(9)?,
    })
}

const FETCH_JOB_COLUMNS: &str =
    "id, user_id, source_url, status, error, file_id, created_at, updated_at, stage, bytes_received";

pub fn insert_fetch_job(
    conn: &Connection,
    id: &str,
    user_id: &str,
    source_url: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO fetch_jobs (id, user_id, source_url) VALUES (?1, ?2, ?3)",
        params![id, user_id, source_url],
    )?;
    Ok(())
}

pub fn get_fetch_job(conn: &Connection, id: &str) -> Result<Option<FetchJob>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM fetch_jobs WHERE id = ?1",
        FETCH_JOB_COLUMNS
    ))?;
    let mut rows = stmt.query(params![id])?;
    match rows.next()? {
        Some(row) => Ok(Some(row_to_fetch_job(row)?)),
        None => Ok(None),
    }
}

/// Transition a job's status. Accepts transitions from any non-terminal
/// state (`pending`, `processing`, `downloading`) so intermediate progress
/// writes can't block completion; prevents a late task from overwriting a
/// finished job.
pub fn finish_fetch_job(
    conn: &Connection,
    id: &str,
    status: &str,
    error: &str,
    file_id: &str,
) -> Result<bool> {
    let affected = conn.execute(
        "UPDATE fetch_jobs SET status = ?2, error = ?3, file_id = ?4, updated_at = unixepoch() \
         WHERE id = ?1 AND status IN ('pending', 'processing', 'downloading')",
        params![id, status, error, file_id],
    )?;
    Ok(affected == 1)
}

/// Record live progress for a running fetch job. Best-effort by design:
/// callers ignore the result, since losing a progress tick must never fail
/// the transfer itself.
pub fn update_fetch_job_progress(
    conn: &Connection,
    id: &str,
    stage: &str,
    status: &str,
    bytes_received: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE fetch_jobs SET stage = ?2, status = ?3, bytes_received = ?4, \
         updated_at = unixepoch() \
         WHERE id = ?1 AND status IN ('pending', 'processing', 'downloading')",
        params![id, stage, status, bytes_received],
    )?;
    Ok(())
}

/// Purge finished jobs past their retention window and fail stale pending ones.
/// Runs inside the caller's transaction (see `cleanup_ephemeral_data`).
pub fn cleanup_fetch_jobs(conn: &Connection, now: i64) -> Result<usize> {
    let mut deleted = conn.execute(
        "DELETE FROM fetch_jobs WHERE status != 'pending' AND updated_at <= ?1",
        params![now - crate::constants::FETCH_JOB_RETENTION_SECS],
    )?;
    deleted += conn.execute(
        "UPDATE fetch_jobs SET status = 'failed', error = 'fetch job timed out', \
         updated_at = unixepoch() WHERE status = 'pending' AND updated_at <= ?1",
        params![now - crate::constants::STALE_FETCH_JOB_TIMEOUT_SECS],
    )?;
    Ok(deleted)
}

// == failed login tracking ==

/// Record a failed login attempt for the given username.
pub fn record_failed_login(conn: &Connection, username: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO failed_logins (username, attempt_at) VALUES (?1, unixepoch())",
        params![username],
    )?;
    Ok(())
}

pub fn record_failed_login_for_ip(conn: &Connection, username: &str, ip_hash: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO failed_logins (username, ip_hash, attempt_at) VALUES (?1, ?2, unixepoch())",
        params![username, ip_hash],
    )?;
    Ok(())
}

/// Count failed login attempts within the last `window_secs` seconds.
pub fn count_failed_logins(conn: &Connection, username: &str, window_secs: i64) -> Result<u32> {
    let count: u32 = conn.query_row(
        "SELECT COUNT(*) FROM failed_logins WHERE username = ?1 AND attempt_at > unixepoch() - ?2",
        params![username, window_secs],
        |row| row.get(0),
    )?;
    Ok(count)
}

pub fn count_failed_logins_for_ip(
    conn: &Connection,
    username: &str,
    ip_hash: &str,
    window_secs: i64,
) -> Result<u32> {
    conn.query_row(
        "SELECT COUNT(*) FROM failed_logins WHERE username = ?1 AND ip_hash = ?2 AND attempt_at > unixepoch() - ?3",
        params![username, ip_hash, window_secs],
        |row| row.get(0),
    )
}

/// Clear failed login attempts for the given username.
pub fn clear_failed_logins(conn: &Connection, username: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM failed_logins WHERE username = ?1",
        params![username],
    )?;
    Ok(())
}

pub fn clear_failed_logins_for_ip(conn: &Connection, username: &str, ip_hash: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM failed_logins WHERE username = ?1 AND ip_hash = ?2",
        params![username, ip_hash],
    )?;
    Ok(())
}

/// Look up an admin by username.
pub fn get_admin_by_username(conn: &Connection, username: &str) -> Result<Option<AdminUser>> {
    let mut stmt = conn.prepare(
        "SELECT id, username, password_hash, created_at FROM admins WHERE username = ?1",
    )?;
    let mut rows = stmt.query(params![username])?;
    if let Some(row) = rows.next()? {
        Ok(Some(AdminUser {
            id: row.get(0)?,
            username: row.get(1)?,
            password_hash: row.get(2)?,
            created_at: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}

/// Insert a new file record into the database.
pub fn insert_file(conn: &Connection, record: &FileRecord) -> Result<()> {
    conn.execute(
        &format!(
            "INSERT INTO files ({}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            FILE_COLUMNS
        ),
        params![
            record.id,
            record.filename,
            record.mime_type,
            record.size_bytes,
            record.storage_path,
            record.delete_token,
            record.uploaded_at,
            record.expires_at,
            record.uploader_ip,
            record.storage_host.as_deref().unwrap_or(""),
            record.status,
        ],
    )?;
    Ok(())
}

pub async fn insert_new_file(
    state: &std::sync::Arc<crate::state::AppState>,
    record: FileRecord,
) -> Result<FileRecord, crate::error::AppError> {
    let r2 = record.clone();
    state
        .db_call("insert_file", move |db| insert_file(db, &r2))
        .await?;
    Ok(record)
}

/// Insert a pending file record (status = 'uploading') via the AppState db_call abstraction.
pub async fn insert_pending_file(
    state: &std::sync::Arc<crate::state::AppState>,
    mut record: FileRecord,
) -> Result<FileRecord, crate::error::AppError> {
    record.status = "uploading".to_string();
    let r2 = record.clone();
    state
        .db_call("insert_pending_file", move |db| insert_file(db, &r2))
        .await?;
    Ok(record)
}

/// Mark a file as ready (status = 'uploading' -> 'ready').
pub async fn complete_file(
    state: &std::sync::Arc<crate::state::AppState>,
    id: String,
) -> Result<(), crate::error::AppError> {
    state
        .db_call("complete_file", move |db| {
            let affected = db.execute(
                "UPDATE files SET status = 'ready' WHERE id = ?1 AND status = 'uploading'",
                params![id],
            )?;
            if affected == 0 {
                Err(rusqlite::Error::QueryReturnedNoRows)
            } else {
                Ok(())
            }
        })
        .await
}

/// Delete a pending file record on upload failure.
pub async fn delete_pending_file(
    state: &std::sync::Arc<crate::state::AppState>,
    id: String,
) -> Result<(), crate::error::AppError> {
    state
        .db_call("delete_pending_file", move |db| {
            db.execute(
                "DELETE FROM files WHERE id = ?1 AND status = 'uploading'",
                params![id],
            )?;
            Ok(())
        })
        .await
}

/// Check if a file is in 'uploading' status (reserved but not yet complete).
pub async fn is_file_uploading(
    state: &std::sync::Arc<crate::state::AppState>,
    id: String,
) -> Result<bool, crate::error::AppError> {
    state
        .db_call("is_file_uploading", move |db| {
            let mut stmt =
                db.prepare("SELECT 1 FROM files WHERE id = ?1 AND status = 'uploading'")?;
            let mut rows = stmt.query(params![id])?;
            Ok(rows.next()?.is_some())
        })
        .await
}

/// Look up a single file record by its ID.
pub fn get_file(conn: &Connection, id: &str) -> Result<Option<FileRecord>> {
    let mut stmt = conn.prepare(&format!("SELECT {} FROM files WHERE id = ?1", FILE_COLUMNS))?;

    let mut rows = stmt.query(params![id])?;

    if let Some(row) = rows.next()? {
        Ok(Some(row_to_file_record(row)?))
    } else {
        Ok(None)
    }
}

/// Look up multiple file records by their IDs.
pub fn get_files_by_ids(conn: &Connection, ids: &[String]) -> Result<Vec<FileRecord>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders: String = ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM files WHERE id IN ({})",
        FILE_COLUMNS, placeholders
    ))?;
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let rows = stmt.query_map(params.as_slice(), row_to_file_record)?;
    rows.collect()
}

/// Resolve a batch of possibly-stale IDs to their current records.
///
/// A rename/renew moves a file to a new ID and records an alias, so client
/// lists that still hold the old ID must resolve through the alias table to
/// find the live record. Returns `(submitted_id, current_record)` pairs so the
/// caller can match the client's own ID/token bookkeeping.
pub fn get_files_by_ids_resolving_aliases(
    conn: &Connection,
    ids: &[String],
) -> Result<Vec<(String, FileRecord)>> {
    let mut out = Vec::new();
    for id in ids {
        if let Some(current) = resolve_alias(conn, id)? {
            if let Some(record) = get_file(conn, &current)? {
                out.push((id.clone(), record));
            }
        }
    }
    Ok(out)
}

/// Delete a file record by ID and return true if anything was actually removed
pub fn delete_file(conn: &Connection, id: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM files WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

/// List every file record, newest first.
pub fn list_all_files(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM files ORDER BY uploaded_at DESC",
        FILE_COLUMNS
    ))?;
    let rows = stmt.query_map([], row_to_file_record)?;
    rows.collect()
}

/// Give a file a new ID and URL while keeping the delete_token filename and expiry the same
pub fn renew_file_id(
    conn: &Connection,
    old_id: &str,
    new_id: &str,
    new_storage_path: &str,
) -> Result<bool> {
    let affected = conn.execute(
        "UPDATE files SET id = ?1, storage_path = ?2 WHERE id = ?3",
        params![new_id, new_storage_path, old_id],
    )?;
    Ok(affected > 0)
}

/// Record that `old_id` now redirects to `new_id`.
pub fn add_alias(conn: &Connection, old_id: &str, new_id: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO aliases (old_id, new_id) VALUES (?1, ?2)",
        params![old_id, new_id],
    )?;
    Ok(())
}

/// Remove a file alias by its old_id.
pub fn delete_alias(conn: &Connection, old_id: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM aliases WHERE old_id = ?1", params![old_id])?;
    Ok(affected > 0)
}

/// Resolve a previous file ID to its current ID.
///
/// Follows alias chains up to 10 hops to avoid infinite loops.
pub fn resolve_alias(conn: &Connection, old_id: &str) -> Result<Option<String>> {
    let mut current = old_id.to_string();
    for _ in 0..10 {
        let result: Option<String> = conn
            .query_row(
                "SELECT new_id FROM aliases WHERE old_id = ?1",
                params![current],
                |row| row.get(0),
            )
            .optional()?;
        match result {
            Some(next) if next != current => current = next,
            Some(final_id) => return Ok(Some(final_id)),
            None => return Ok(Some(current)),
        }
    }
    Ok(Some(current))
}

/// Replace all stored records for a client (the browser is the source of truth:
/// each POST mirrors the client's full upload list, so stale/removed entries
/// naturally fall away). Runs in a transaction.
pub fn replace_client_files(
    conn: &Connection,
    client_key: &str,
    records: &[ClientFileRecord],
) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM client_files WHERE client_key = ?1",
        params![client_key],
    )?;
    for r in records {
        tx.execute(
            "INSERT OR REPLACE INTO client_files
             (client_key, file_id, filename, mime_type, size_bytes, uploaded_at, expires_at, delete_token)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![client_key, r.id, r.n, r.m, r.s, r.u, r.e, r.token],
        )?;
    }
    tx.commit()?;
    Ok(records.len())
}

/// Fetch a client's stored upload records, newest first.
pub fn get_client_files(conn: &Connection, client_key: &str) -> Result<Vec<ClientFileRecord>> {
    let mut stmt = conn.prepare(
        "SELECT file_id, delete_token, filename, mime_type, size_bytes, uploaded_at, expires_at
         FROM client_files WHERE client_key = ?1 ORDER BY uploaded_at DESC",
    )?;
    let rows = stmt.query_map(params![client_key], |row| {
        Ok(ClientFileRecord {
            id: row.get(0)?,
            token: row.get(1)?,
            n: row.get(2)?,
            m: row.get(3)?,
            s: row.get(4)?,
            u: row.get(5)?,
            e: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Swap a client's registered file ID after a server-side rename.
///
/// Renames keep the delete token and metadata, so only `file_id` changes. Any
/// row already under `new_id` is removed first so the primary key can't clash.
/// Returns true when a row was actually renamed.
pub fn update_client_file_id(
    conn: &Connection,
    client_key: &str,
    old_id: &str,
    new_id: &str,
) -> Result<bool> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "DELETE FROM client_files WHERE client_key = ?1 AND file_id = ?2",
        params![client_key, new_id],
    )?;
    let affected = tx.execute(
        "UPDATE client_files SET file_id = ?1 WHERE client_key = ?2 AND file_id = ?3",
        params![new_id, client_key, old_id],
    )?;
    tx.commit()?;
    Ok(affected > 0)
}

/// List expired file records only.
pub fn list_expired(conn: &Connection) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM files WHERE expires_at < unixepoch()",
        FILE_COLUMNS
    ))?;
    let rows = stmt.query_map([], row_to_file_record)?;
    rows.collect()
}

/// Find quick link uploads that were reserved but never completed so we know they're safe to delete
pub fn list_abandoned_uploads(conn: &Connection, older_than_secs: i64) -> Result<Vec<FileRecord>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {} FROM files WHERE status = 'uploading' AND uploaded_at < unixepoch() - ?1",
        FILE_COLUMNS
    ))?;
    let rows = stmt.query_map(params![older_than_secs], row_to_file_record)?;
    rows.collect()
}

/// Bulk-delete file records by their IDs and return the number of removed rows.
pub fn delete_files_by_ids(conn: &Connection, ids: &[String]) -> Result<usize> {
    if ids.is_empty() {
        return Ok(0);
    }
    let placeholders: String = ids
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 1))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!("DELETE FROM files WHERE id IN ({})", placeholders);
    let params: Vec<&dyn rusqlite::ToSql> = ids.iter().map(|s| s as &dyn rusqlite::ToSql).collect();
    let affected = conn.execute(&sql, params.as_slice())?;
    Ok(affected)
}

// == paginated admin queries ==

/// Whitelist of allowed sort columns for files.
fn file_sort_column(sort: &str) -> &str {
    match sort {
        "filename" => "filename",
        "size_bytes" => "size_bytes",
        "mime_type" => "mime_type",
        "uploaded_at" => "uploaded_at",
        "expires_at" => "expires_at",
        _ => "uploaded_at",
    }
}

/// Whitelist of allowed sort columns for reports.
fn report_sort_column(sort: &str) -> &str {
    match sort {
        "reason" => "reason",
        "id" => "id",
        _ => "created_at",
    }
}

/// Whitelist of allowed sort columns for feedback.
fn feedback_sort_column(sort: &str) -> &str {
    match sort {
        "id" => "id",
        _ => "created_at",
    }
}

/// Whitelist of allowed sort columns for bans.
fn ban_sort_column(sort: &str) -> &str {
    match sort {
        "reason" => "reason",
        "banned_by" => "banned_by",
        _ => "banned_at",
    }
}

fn sort_direction(dir: &str) -> &str {
    if dir.eq_ignore_ascii_case("asc") {
        "ASC"
    } else {
        "DESC"
    }
}

/// Search pattern for files: matches id, filename, mime_type, uploader_ip.
pub fn list_files_paginated(
    conn: &Connection,
    search: &str,
    sort: &str,
    dir: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<FileRecord>, i64)> {
    let col = file_sort_column(sort);
    let d = sort_direction(dir);

    let (where_clause, search_param): (&str, String) = if search.is_empty() {
        ("", String::new())
    } else {
        let pattern = format!("%{}%", search);
        (
            "WHERE (id LIKE ?1 OR filename LIKE ?1 OR mime_type LIKE ?1 OR uploader_ip LIKE ?1)",
            pattern,
        )
    };

    let count_sql = format!("SELECT COUNT(*) FROM files {}", where_clause);
    let total: i64 = if search.is_empty() {
        conn.query_row(&count_sql, [], |row| row.get(0))?
    } else {
        conn.query_row(&count_sql, params![search_param], |row| row.get(0))?
    };

    let sql = format!(
        "SELECT {} FROM files {} ORDER BY {} {} LIMIT ?{} OFFSET ?{}",
        FILE_COLUMNS,
        where_clause,
        col,
        d,
        if search.is_empty() { "1" } else { "2" },
        if search.is_empty() { "2" } else { "3" },
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = if search.is_empty() {
        let r = stmt.query_map(params![limit, offset], row_to_file_record)?;
        r.collect::<Result<Vec<_>>>()?
    } else {
        let r = stmt.query_map(params![search_param, limit, offset], row_to_file_record)?;
        r.collect::<Result<Vec<_>>>()?
    };

    Ok((rows, total))
}

/// Paginated reports with search, sort, offset, limit.
pub fn list_reports_paginated(
    conn: &Connection,
    search: &str,
    sort: &str,
    dir: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<ReportRecord>, i64)> {
    let col = report_sort_column(sort);
    let d = sort_direction(dir);

    let (where_clause, search_param): (&str, String) = if search.is_empty() {
        ("", String::new())
    } else {
        let pattern = format!("%{}%", search);
        (
            "WHERE (file_url LIKE ?1 OR reason LIKE ?1 OR details LIKE ?1 OR email LIKE ?1)",
            pattern,
        )
    };

    let count_sql = format!("SELECT COUNT(*) FROM reports {}", where_clause);
    let total: i64 = if search.is_empty() {
        conn.query_row(&count_sql, [], |row| row.get(0))?
    } else {
        conn.query_row(&count_sql, params![search_param], |row| row.get(0))?
    };

    let sql = format!(
        "SELECT id, file_url, reason, details, reporter_ip, email, created_at FROM reports {} ORDER BY {} {} LIMIT ?{} OFFSET ?{}",
        where_clause,
        col,
        d,
        if search.is_empty() { "1" } else { "2" },
        if search.is_empty() { "2" } else { "3" },
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = if search.is_empty() {
        let r = stmt.query_map(params![limit, offset], |row| {
            Ok(ReportRecord {
                id: row.get(0)?,
                file_url: row.get(1)?,
                reason: row.get(2)?,
                details: row.get(3)?,
                reporter_ip: row.get(4)?,
                email: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    } else {
        let r = stmt.query_map(params![search_param, limit, offset], |row| {
            Ok(ReportRecord {
                id: row.get(0)?,
                file_url: row.get(1)?,
                reason: row.get(2)?,
                details: row.get(3)?,
                reporter_ip: row.get(4)?,
                email: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    };

    Ok((rows, total))
}

/// Paginated feedback with search, sort, offset, limit.
pub fn list_feedback_paginated(
    conn: &Connection,
    search: &str,
    sort: &str,
    dir: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<FeedbackRecord>, i64)> {
    let col = feedback_sort_column(sort);
    let d = sort_direction(dir);

    let (where_clause, search_param): (&str, String) = if search.is_empty() {
        ("", String::new())
    } else {
        let pattern = format!("%{}%", search);
        ("WHERE (message LIKE ?1 OR email LIKE ?1)", pattern)
    };

    let count_sql = format!("SELECT COUNT(*) FROM feedback {}", where_clause);
    let total: i64 = if search.is_empty() {
        conn.query_row(&count_sql, [], |row| row.get(0))?
    } else {
        conn.query_row(&count_sql, params![search_param], |row| row.get(0))?
    };

    let sql = format!(
        "SELECT id, message, email, reporter_ip, created_at FROM feedback {} ORDER BY {} {} LIMIT ?{} OFFSET ?{}",
        where_clause,
        col,
        d,
        if search.is_empty() { "1" } else { "2" },
        if search.is_empty() { "2" } else { "3" },
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = if search.is_empty() {
        let r = stmt.query_map(params![limit, offset], |row| {
            Ok(FeedbackRecord {
                id: row.get(0)?,
                message: row.get(1)?,
                email: row.get(2)?,
                reporter_ip: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    } else {
        let r = stmt.query_map(params![search_param, limit, offset], |row| {
            Ok(FeedbackRecord {
                id: row.get(0)?,
                message: row.get(1)?,
                email: row.get(2)?,
                reporter_ip: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    };

    Ok((rows, total))
}

/// Paginated bans with search, sort, offset, limit.
pub fn list_bans_paginated(
    conn: &Connection,
    search: &str,
    sort: &str,
    dir: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<BanRecord>, i64)> {
    let col = ban_sort_column(sort);
    let d = sort_direction(dir);

    let (where_clause, search_param): (&str, String) = if search.is_empty() {
        ("", String::new())
    } else {
        let pattern = format!("%{}%", search);
        (
            "WHERE (ip LIKE ?1 OR reason LIKE ?1 OR banned_by LIKE ?1)",
            pattern,
        )
    };

    let count_sql = format!("SELECT COUNT(*) FROM banned_ips {}", where_clause);
    let total: i64 = if search.is_empty() {
        conn.query_row(&count_sql, [], |row| row.get(0))?
    } else {
        conn.query_row(&count_sql, params![search_param], |row| row.get(0))?
    };

    let sql = format!(
        "SELECT ip, reason, banned_by, banned_at FROM banned_ips {} ORDER BY {} {} LIMIT ?{} OFFSET ?{}",
        where_clause,
        col,
        d,
        if search.is_empty() { "1" } else { "2" },
        if search.is_empty() { "2" } else { "3" },
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = if search.is_empty() {
        let r = stmt.query_map(params![limit, offset], |row| {
            Ok(BanRecord {
                ip: row.get(0)?,
                reason: row.get(1)?,
                banned_by: row.get(2)?,
                banned_at: row.get(3)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    } else {
        let r = stmt.query_map(params![search_param, limit, offset], |row| {
            Ok(BanRecord {
                ip: row.get(0)?,
                reason: row.get(1)?,
                banned_by: row.get(2)?,
                banned_at: row.get(3)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    };

    Ok((rows, total))
}

// == hosters (known custom hosts) ==

/// Record that a custom host was used, updating last-seen while keeping the
/// original first-seen timestamp.
pub fn upsert_hoster(conn: &Connection, host: &str, status: &str, error: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO hosters (host, first_seen_at, last_seen_at, last_status, last_error)
         VALUES (?1, unixepoch(), unixepoch(), ?2, ?3)
         ON CONFLICT(host) DO UPDATE SET
            last_seen_at = unixepoch(),
            last_status = excluded.last_status,
            last_error = excluded.last_error",
        params![host, status, error],
    )?;
    Ok(())
}

/// Fetch a single hoster record by host.
pub fn get_hoster(conn: &Connection, host: &str) -> Result<Option<HosterRecord>> {
    conn.query_row(
        "SELECT host, first_seen_at, last_seen_at, last_status, last_error, \
         banned, banned_reason, banned_by, banned_at FROM hosters WHERE host = ?1",
        params![host],
        |row| {
            Ok(HosterRecord {
                host: row.get(0)?,
                first_seen_at: row.get(1)?,
                last_seen_at: row.get(2)?,
                last_status: row.get(3)?,
                last_error: row.get(4)?,
                banned: row.get::<_, i64>(5)? != 0,
                banned_reason: row.get(6)?,
                banned_by: row.get(7)?,
                banned_at: row.get(8)?,
            })
        },
    )
    .optional()
}

/// Ban or unban a hoster. Returns true if a row was updated.
pub fn set_hoster_banned(
    conn: &Connection,
    host: &str,
    banned: bool,
    reason: &str,
    banned_by: &str,
) -> Result<bool> {
    let flag: i64 = if banned { 1 } else { 0 };
    let affected = conn.execute(
        "UPDATE hosters SET banned = ?1, banned_reason = ?2, banned_by = ?3, \
         banned_at = CASE WHEN ?1 = 1 THEN unixepoch() ELSE NULL END WHERE host = ?4",
        params![flag, reason, banned_by, host],
    )?;
    Ok(affected > 0)
}

/// Whitelist of allowed sort columns for hosters.
fn hoster_sort_column(sort: &str) -> &str {
    match sort {
        "host" => "host",
        "first_seen" => "first_seen_at",
        "last_status" => "last_status",
        "banned" => "banned",
        _ => "last_seen_at",
    }
}

/// Paginated hosters with search, sort, offset, limit.
pub fn list_hosters_paginated(
    conn: &Connection,
    search: &str,
    sort: &str,
    dir: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<HosterRecord>, i64)> {
    let col = hoster_sort_column(sort);
    let d = sort_direction(dir);

    let (where_clause, search_param): (&str, String) = if search.is_empty() {
        ("", String::new())
    } else {
        let pattern = format!("%{}%", search);
        (
            "WHERE (host LIKE ?1 OR last_status LIKE ?1 OR last_error LIKE ?1 OR banned_reason LIKE ?1)",
            pattern,
        )
    };

    let count_sql = format!("SELECT COUNT(*) FROM hosters {}", where_clause);
    let total: i64 = if search.is_empty() {
        conn.query_row(&count_sql, [], |row| row.get(0))?
    } else {
        conn.query_row(&count_sql, params![search_param], |row| row.get(0))?
    };

    let sql = format!(
        "SELECT host, first_seen_at, last_seen_at, last_status, last_error, \
         banned, banned_reason, banned_by, banned_at FROM hosters {} \
         ORDER BY {} {} LIMIT ?{} OFFSET ?{}",
        where_clause,
        col,
        d,
        if search.is_empty() { "1" } else { "2" },
        if search.is_empty() { "2" } else { "3" },
    );

    let mut stmt = conn.prepare(&sql)?;
    let rows = if search.is_empty() {
        let r = stmt.query_map(params![limit, offset], |row| {
            Ok(HosterRecord {
                host: row.get(0)?,
                first_seen_at: row.get(1)?,
                last_seen_at: row.get(2)?,
                last_status: row.get(3)?,
                last_error: row.get(4)?,
                banned: row.get::<_, i64>(5)? != 0,
                banned_reason: row.get(6)?,
                banned_by: row.get(7)?,
                banned_at: row.get(8)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    } else {
        let r = stmt.query_map(params![search_param, limit, offset], |row| {
            Ok(HosterRecord {
                host: row.get(0)?,
                first_seen_at: row.get(1)?,
                last_seen_at: row.get(2)?,
                last_status: row.get(3)?,
                last_error: row.get(4)?,
                banned: row.get::<_, i64>(5)? != 0,
                banned_reason: row.get(6)?,
                banned_by: row.get(7)?,
                banned_at: row.get(8)?,
            })
        })?;
        r.collect::<Result<Vec<_>>>()?
    };

    Ok((rows, total))
}

// == reports ==

/// Insert a new report and return the new row ID
pub fn insert_report(
    conn: &Connection,
    file_url: &str,
    reason: &str,
    details: &str,
    reporter_ip: Option<&str>,
    email: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO reports (file_url, reason, details, reporter_ip, email) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![file_url, reason, details, reporter_ip, email],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List all reports, newest first.
pub fn list_reports(conn: &Connection) -> Result<Vec<ReportRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, file_url, reason, details, reporter_ip, email, created_at FROM reports ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(ReportRecord {
            id: row.get(0)?,
            file_url: row.get(1)?,
            reason: row.get(2)?,
            details: row.get(3)?,
            reporter_ip: row.get(4)?,
            email: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    rows.collect()
}

/// Delete a report by ID and return true if anything was actually removed
pub fn delete_report(conn: &Connection, id: i64) -> Result<bool> {
    let affected = conn.execute("DELETE FROM reports WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

// == feedback ==

/// Insert a new feedback entry and return the new row ID
pub fn insert_feedback(
    conn: &Connection,
    message: &str,
    email: Option<&str>,
    reporter_ip: Option<&str>,
) -> Result<i64> {
    conn.execute(
        "INSERT INTO feedback (message, email, reporter_ip) VALUES (?1, ?2, ?3)",
        params![message, email, reporter_ip],
    )?;
    Ok(conn.last_insert_rowid())
}

/// List all feedback, newest first.
pub fn list_feedback(conn: &Connection) -> Result<Vec<FeedbackRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, message, email, reporter_ip, created_at FROM feedback ORDER BY created_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(FeedbackRecord {
            id: row.get(0)?,
            message: row.get(1)?,
            email: row.get(2)?,
            reporter_ip: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect()
}

/// Delete a feedback entry by ID and return true if anything was actually removed
pub fn delete_feedback(conn: &Connection, id: i64) -> Result<bool> {
    let affected = conn.execute("DELETE FROM feedback WHERE id = ?1", params![id])?;
    Ok(affected > 0)
}

// == banned ips ==

/// Get the ban record for an IP, if banned.
pub fn get_ban_record(conn: &Connection, ip: &str) -> Result<Option<BanRecord>> {
    let mut stmt =
        conn.prepare("SELECT ip, reason, banned_by, banned_at FROM banned_ips WHERE ip = ?1")?;
    let mut rows = stmt.query(params![ip])?;
    if let Some(row) = rows.next()? {
        Ok(Some(BanRecord {
            ip: row.get(0)?,
            reason: row.get(1)?,
            banned_by: row.get(2)?,
            banned_at: row.get(3)?,
        }))
    } else {
        Ok(None)
    }
}

/// Ban an IP address.
pub fn ban_ip(conn: &Connection, ip: &str, reason: &str, banned_by: &str) -> Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO banned_ips (ip, reason, banned_by) VALUES (?1, ?2, ?3)",
        params![ip, reason, banned_by],
    )?;
    Ok(())
}

/// Unban an IP address and return true if anything was actually removed
pub fn unban_ip(conn: &Connection, ip: &str) -> Result<bool> {
    let affected = conn.execute("DELETE FROM banned_ips WHERE ip = ?1", params![ip])?;
    Ok(affected > 0)
}

/// A ban entry ready to be inserted, with the IP already hashed.
#[derive(Debug, Clone)]
pub struct ImportBan {
    pub ip: String,
    pub reason: String,
    pub banned_by: String,
}

/// Insert many bans in a single transaction.
pub fn bulk_ban_ips(conn: &Connection, bans: &[ImportBan]) -> Result<usize> {
    let tx = conn.unchecked_transaction()?;
    for b in bans {
        tx.execute(
            "INSERT OR REPLACE INTO banned_ips (ip, reason, banned_by) VALUES (?1, ?2, ?3)",
            params![b.ip, b.reason, b.banned_by],
        )?;
    }
    tx.commit()?;
    Ok(bans.len())
}

/// List all banned IPs, newest first.
pub fn list_banned_ips(conn: &Connection) -> Result<Vec<BanRecord>> {
    let mut stmt = conn.prepare(
        "SELECT ip, reason, banned_by, banned_at FROM banned_ips ORDER BY banned_at DESC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(BanRecord {
            ip: row.get(0)?,
            reason: row.get(1)?,
            banned_by: row.get(2)?,
            banned_at: row.get(3)?,
        })
    })?;
    rows.collect()
}

/// Load all banned IPs into a HashSet for fast lookup.
pub fn load_banned_ips_set(conn: &Connection) -> Result<std::collections::HashSet<String>> {
    let mut stmt = conn.prepare("SELECT ip FROM banned_ips")?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
    let mut set = std::collections::HashSet::new();
    for row in rows {
        set.insert(row?);
    }
    Ok(set)
}

// == announcements ==

fn row_to_announcement(row: &rusqlite::Row) -> rusqlite::Result<Announcement> {
    Ok(Announcement {
        id: row.get(0)?,
        message: row.get(1)?,
        link_url: row.get(2)?,
        mode: row.get(3)?,
        is_active: row.get::<_, i64>(4)? != 0,
        created_at: row.get(5)?,
        updated_at: row.get(6)?,
    })
}

/// Get the active announcement, if any.
pub fn get_active_announcement(conn: &Connection) -> Result<Option<Announcement>> {
    let mut stmt = conn.prepare(
        "SELECT id, message, link_url, mode, is_active, created_at, updated_at \
         FROM announcements WHERE is_active = 1 LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_announcement(row)?))
    } else {
        Ok(None)
    }
}

/// Get any announcement (admin use - returns active or most recent).
pub fn get_announcement(conn: &Connection) -> Result<Option<Announcement>> {
    let mut stmt = conn.prepare(
        "SELECT id, message, link_url, mode, is_active, created_at, updated_at \
         FROM announcements ORDER BY is_active DESC, updated_at DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    if let Some(row) = rows.next()? {
        Ok(Some(row_to_announcement(row)?))
    } else {
        Ok(None)
    }
}

/// Upsert the site announcement by deactivating all others then inserting or updating
pub fn upsert_announcement(
    conn: &Connection,
    message: &str,
    link_url: &str,
    mode: &str,
    is_active: bool,
) -> Result<Announcement> {
    conn.execute("UPDATE announcements SET is_active = 0", [])?;

    let existing: Option<i64> = {
        let mut stmt = conn.prepare("SELECT id FROM announcements LIMIT 1")?;
        let mut rows = stmt.query([])?;
        rows.next()?.map(|r| r.get(0)).transpose()?
    };

    if let Some(id) = existing {
        conn.execute(
            "UPDATE announcements SET message = ?1, link_url = ?2, mode = ?3, is_active = ?4, updated_at = unixepoch() WHERE id = ?5",
            params![message, link_url, mode, is_active as i64, id],
        )?;
        let row = conn.query_row(
            "SELECT id, message, link_url, mode, is_active, created_at, updated_at FROM announcements WHERE id = ?1",
            params![id],
            row_to_announcement,
        )?;
        Ok(row)
    } else {
        conn.execute(
            "INSERT INTO announcements (message, link_url, mode, is_active) VALUES (?1, ?2, ?3, ?4)",
            params![message, link_url, mode, is_active as i64],
        )?;
        let id = conn.last_insert_rowid();
        let row = conn.query_row(
            "SELECT id, message, link_url, mode, is_active, created_at, updated_at FROM announcements WHERE id = ?1",
            params![id],
            row_to_announcement,
        )?;
        Ok(row)
    }
}

/// Migrate raw IPs in the DB to encrypted or hashed form, it's a one-time startup thing
pub fn migrate_raw_ips_to_encrypted(
    conn: &Connection,
    encryption_key: &str,
    pepper: &str,
) -> rusqlite::Result<()> {
    fn looks_like_raw_ip(val: &str) -> bool {
        // Simple heuristic: IPv4 (digits and dots) or IPv6 (contains colons, short)
        let trimmed = val.trim();
        if trimmed.is_empty() {
            return false;
        }
        // Already encrypted values contain ':' separator between nonce and ciphertext
        // and are long hex strings
        if trimmed.len() > 40 && trimmed.contains(':') {
            return false;
        }
        // Already HMAC hashes are 64-char hex
        if trimmed.len() == 64 && trimmed.chars().all(|c| c.is_ascii_hexdigit()) {
            return false;
        }
        // IPv4: digits and dots
        if trimmed.split('.').all(|part| part.parse::<u8>().is_ok()) {
            return true;
        }
        // IPv6: contains colons, relatively short
        if trimmed.contains(':') && trimmed.len() < 40 {
            return true;
        }
        false
    }

    let mut encrypted_count = 0u64;
    let mut hashed_count = 0u64;

    // Migrate files.uploader_ip -> encrypted
    {
        let mut stmt =
            conn.prepare("SELECT id, uploader_ip FROM files WHERE uploader_ip IS NOT NULL")?;
        let rows: Vec<(String, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        for (id, ip) in rows {
            if looks_like_raw_ip(&ip) {
                let encrypted = crate::utils::encrypt_ip(&ip, encryption_key);
                conn.execute(
                    "UPDATE files SET uploader_ip = ?1 WHERE id = ?2",
                    rusqlite::params![encrypted, id],
                )?;
                encrypted_count += 1;
            }
        }
    }

    // Migrate reports.reporter_ip -> encrypted
    {
        let mut stmt =
            conn.prepare("SELECT id, reporter_ip FROM reports WHERE reporter_ip IS NOT NULL")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        for (id, ip) in rows {
            if looks_like_raw_ip(&ip) {
                let encrypted = crate::utils::encrypt_ip(&ip, encryption_key);
                conn.execute(
                    "UPDATE reports SET reporter_ip = ?1 WHERE id = ?2",
                    rusqlite::params![encrypted, id],
                )?;
                encrypted_count += 1;
            }
        }
    }

    // Migrate feedback.reporter_ip -> encrypted
    {
        let mut stmt =
            conn.prepare("SELECT id, reporter_ip FROM feedback WHERE reporter_ip IS NOT NULL")?;
        let rows: Vec<(i64, String)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;

        for (id, ip) in rows {
            if looks_like_raw_ip(&ip) {
                let encrypted = crate::utils::encrypt_ip(&ip, encryption_key);
                conn.execute(
                    "UPDATE feedback SET reporter_ip = ?1 WHERE id = ?2",
                    rusqlite::params![encrypted, id],
                )?;
                encrypted_count += 1;
            }
        }
    }

    // Migrate banned_ips.ip -> HMAC hash
    {
        let mut stmt = conn.prepare("SELECT ip FROM banned_ips")?;
        let rows: Vec<String> = stmt
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;

        for ip in rows {
            if looks_like_raw_ip(&ip) {
                let hashed = crate::utils::hash_ip_for_ban(&ip, pepper);
                conn.execute(
                    "UPDATE banned_ips SET ip = ?1 WHERE ip = ?2",
                    rusqlite::params![hashed, ip],
                )?;
                hashed_count += 1;
            }
        }
    }

    if encrypted_count > 0 || hashed_count > 0 {
        tracing::info!(
            "ip migration: {} IPs encrypted, {} ban entries hashed",
            encrypted_count,
            hashed_count
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_storage_uses_only_token_hash() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let token = crate::auth::new_session_token();
        let hash = crate::auth::session_token_hash(&token);
        create_session(&conn, &hash, "user-1", 10, 20).unwrap();

        let stored: String = conn
            .query_row("SELECT token_hash FROM sessions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(stored, hash);
        assert_ne!(stored, token);
        assert_eq!(
            resolve_session(&conn, &hash, 11).unwrap().as_deref(),
            Some("user-1")
        );
        assert!(resolve_session(&conn, &hash, 20).unwrap().is_none());
    }

    #[test]
    fn legacy_import_verifies_local_token_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        let local = FileRecord::new(
            "local".into(),
            "a.txt".into(),
            "text/plain".into(),
            1,
            "valid-token".into(),
            10,
            20,
            None,
            None,
        );
        let remote = FileRecord::new(
            "remote".into(),
            "b.txt".into(),
            "text/plain".into(),
            1,
            "remote-token".into(),
            10,
            20,
            None,
            Some("https://elsewhere.test".into()),
        );
        insert_file(&conn, &local).unwrap();
        insert_file(&conn, &remote).unwrap();
        let records = vec![
            ClientFileRecord {
                id: "local".into(),
                token: "valid-token".into(),
                n: "ignored".into(),
                m: String::new(),
                s: 0,
                u: 0,
                e: 0,
            },
            ClientFileRecord {
                id: "remote".into(),
                token: "remote-token".into(),
                n: String::new(),
                m: String::new(),
                s: 0,
                u: 0,
                e: 0,
            },
            ClientFileRecord {
                id: "local".into(),
                token: "wrong".into(),
                n: String::new(),
                m: String::new(),
                s: 0,
                u: 0,
                e: 0,
            },
        ];

        assert_eq!(
            import_verified_client_files(&conn, "user-1", &records).unwrap(),
            1
        );
        assert_eq!(
            import_verified_client_files(&conn, "user-1", &records).unwrap(),
            0
        );
        let imported = get_client_files(&conn, "user-1").unwrap();
        assert_eq!(imported.len(), 1);
        assert_eq!(imported[0].n, "a.txt");
        assert!(!serde_json::to_string(&imported)
            .unwrap()
            .contains("valid-token"));
    }

    #[test]
    fn removed_device_is_immediately_revoked() {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute(
            "INSERT INTO devices (id, user_id, device_name, paired_at, last_seen_at) VALUES ('d', 'u', 'test', 1, 1)",
            [],
        ).unwrap();
        assert!(device_belongs_to_user(&conn, "d", "u").unwrap());
        conn.execute("DELETE FROM devices WHERE id = 'd'", [])
            .unwrap();
        assert!(!device_belongs_to_user(&conn, "d", "u").unwrap());
    }

    #[test]
    fn file_record_new_sets_fields() {
        let record = FileRecord::new(
            "id1".into(),
            "test.txt".into(),
            "text/plain".into(),
            1024,
            "token1".into(),
            100,
            200,
            Some("1.2.3.4".into()),
            Some("https://files.example.com".into()),
        );
        assert_eq!(record.id, "id1");
        assert_eq!(record.filename, "test.txt");
        assert_eq!(record.mime_type, "text/plain");
        assert_eq!(record.size_bytes, 1024);
        assert_eq!(record.storage_path, "remote-id1");
        assert_eq!(record.delete_token, "token1");
        assert_eq!(record.uploaded_at, 100);
        assert_eq!(record.expires_at, 200);
        assert_eq!(record.uploader_ip.as_deref(), Some("1.2.3.4"));
        assert_eq!(
            record.storage_host.as_deref(),
            Some("https://files.example.com")
        );
    }

    #[test]
    fn file_record_from_upload_with_token_ttl() {
        let before = chrono::Utc::now().timestamp();
        let record = FileRecord::from_upload_with_token(
            "id2".into(),
            "file.bin".into(),
            "application/octet-stream".into(),
            2048,
            24.0,
            "tok2".into(),
            None,
            None,
        );
        let after = chrono::Utc::now().timestamp();
        assert_eq!(record.id, "id2");
        assert_eq!(record.size_bytes, 2048);
        assert_eq!(record.delete_token, "tok2");
        assert!(record.uploaded_at >= before);
        assert!(record.expires_at <= after + 24 * 3600 + 2);
        assert!(record.expires_at >= before + 24 * 3600 - 2);
        assert!(record.uploader_ip.is_none());
        assert!(record.storage_host.is_none());
    }

    #[test]
    fn file_record_from_upload_with_token_clamped_high() {
        let before = chrono::Utc::now().timestamp();
        let record = FileRecord::from_upload_with_token(
            "id3".into(),
            "file.bin".into(),
            "application/octet-stream".into(),
            100,
            99999.0,
            "tok3".into(),
            None,
            None,
        );
        let expected_ttl = (99999.0 * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
        assert!(record.expires_at >= before + expected_ttl - 2);
        assert!(record.expires_at <= before + expected_ttl + 3);
    }

    #[test]
    fn file_record_from_upload_with_token_clamped_low() {
        let before = chrono::Utc::now().timestamp();
        let record = FileRecord::from_upload_with_token(
            "id4".into(),
            "file.bin".into(),
            "application/octet-stream".into(),
            100,
            0.0001,
            "tok4".into(),
            None,
            None,
        );
        let expected_ttl = (0.0001 * crate::constants::SECONDS_PER_HOUR_F64).round() as i64;
        assert!(record.expires_at >= before + expected_ttl - 2);
        assert!(record.expires_at <= before + expected_ttl + 3);
    }

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn
    }

    #[test]
    fn test_init_db_idempotent() {
        let conn = setup_db();
        init_db(&conn).unwrap();
    }

    #[test]
    fn test_insert_get_file() {
        let conn = setup_db();
        let record = FileRecord::new(
            "test1".into(),
            "file.txt".into(),
            "text/plain".into(),
            100,
            "tok1".into(),
            1000,
            2000,
            Some("127.0.0.1".into()),
            None,
        );
        insert_file(&conn, &record).unwrap();
        let got = get_file(&conn, "test1").unwrap().unwrap();
        assert_eq!(got.id, "test1");
        assert_eq!(got.filename, "file.txt");
        assert_eq!(got.size_bytes, 100);
    }

    #[test]
    fn test_get_file_not_found() {
        let conn = setup_db();
        assert!(get_file(&conn, "nonexistent").unwrap().is_none());
    }

    #[test]
    fn complete_reservation_requires_token_and_uploading_status() {
        let conn = setup_db();
        let mut pending = FileRecord::new(
            "reserved".into(),
            "placeholder.txt".into(),
            "text/plain".into(),
            0,
            "secret".into(),
            100,
            200,
            None,
            None,
        );
        pending.status = "uploading".into();
        insert_file(&conn, &pending).unwrap();

        assert!(matches!(
            complete_reservation(
                &conn,
                "final.txt",
                "text/plain",
                4,
                "reserved",
                "wrong",
                None
            )
            .unwrap(),
            CompleteReservationResult::InvalidToken
        ));
        assert_eq!(
            get_file(&conn, "reserved").unwrap().unwrap().status,
            "uploading"
        );
        let completed = complete_reservation(
            &conn,
            "final.txt",
            "text/plain",
            4,
            "reserved",
            "secret",
            None,
        )
        .unwrap();
        assert!(matches!(completed, CompleteReservationResult::Completed(_)));

        assert!(matches!(
            complete_reservation(
                &conn,
                "again.txt",
                "text/plain",
                5,
                "reserved",
                "secret",
                None
            )
            .unwrap(),
            CompleteReservationResult::NotUploading
        ));
    }

    #[test]
    fn complete_reservation_propagates_database_errors() {
        let conn = setup_db();
        conn.execute("DROP TABLE files", []).unwrap();
        assert!(complete_reservation(
            &conn,
            "final.txt",
            "text/plain",
            4,
            "reserved",
            "secret",
            None,
        )
        .is_err());
    }

    #[test]
    fn test_get_files_by_ids() {
        let conn = setup_db();
        for i in 0..3 {
            let record = FileRecord::new(
                format!("id{}", i),
                "f.txt".into(),
                "text/plain".into(),
                10,
                format!("tok{}", i),
                100,
                200,
                None,
                None,
            );
            insert_file(&conn, &record).unwrap();
        }
        let files = get_files_by_ids(&conn, &["id0".into(), "id2".into()]).unwrap();
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.id == "id0"));
        assert!(files.iter().any(|f| f.id == "id2"));
    }

    #[test]
    fn test_delete_file() {
        let conn = setup_db();
        let record = FileRecord::new(
            "del1".into(),
            "f.txt".into(),
            "text/plain".into(),
            10,
            "tok".into(),
            100,
            200,
            None,
            None,
        );
        insert_file(&conn, &record).unwrap();
        assert!(delete_file(&conn, "del1").unwrap());
        assert!(get_file(&conn, "del1").unwrap().is_none());
    }

    #[test]
    fn test_list_all_files_ordered() {
        let conn = setup_db();
        for i in 0..3 {
            let record = FileRecord::new(
                format!("id{}", i),
                "f.txt".into(),
                "text/plain".into(),
                10,
                "tok".into(),
                1000 + i,
                2000,
                None,
                None,
            );
            insert_file(&conn, &record).unwrap();
        }
        let files = list_all_files(&conn).unwrap();
        assert_eq!(files.len(), 3);
        assert!(files[0].uploaded_at >= files[1].uploaded_at);
        assert!(files[1].uploaded_at >= files[2].uploaded_at);
    }

    #[test]
    fn test_renew_file_id() {
        let conn = setup_db();
        let record = FileRecord::new(
            "old_id".into(),
            "f.txt".into(),
            "text/plain".into(),
            10,
            "tok".into(),
            100,
            200,
            None,
            None,
        );
        insert_file(&conn, &record).unwrap();
        assert!(renew_file_id(&conn, "old_id", "new_id", "remote-new_id").unwrap());
        assert!(get_file(&conn, "old_id").unwrap().is_none());
        let renewed = get_file(&conn, "new_id").unwrap().unwrap();
        assert_eq!(renewed.delete_token, "tok");
        assert_eq!(renewed.storage_path, "remote-new_id");
    }

    #[test]
    fn test_list_expired() {
        let conn = setup_db();
        let past = FileRecord::new(
            "expired".into(),
            "f.txt".into(),
            "text/plain".into(),
            10,
            "tok".into(),
            100,
            1,
            None,
            None,
        );
        let future = FileRecord::new(
            "active".into(),
            "f.txt".into(),
            "text/plain".into(),
            10,
            "tok".into(),
            100,
            9999999999,
            None,
            None,
        );
        insert_file(&conn, &past).unwrap();
        insert_file(&conn, &future).unwrap();
        let expired = list_expired(&conn).unwrap();
        assert_eq!(expired.len(), 1);
        assert_eq!(expired[0].id, "expired");
    }

    #[test]
    fn test_delete_files_by_ids() {
        let conn = setup_db();
        for i in 0..3 {
            let record = FileRecord::new(
                format!("bulk{}", i),
                "f.txt".into(),
                "text/plain".into(),
                10,
                "tok".into(),
                100,
                200,
                None,
                None,
            );
            insert_file(&conn, &record).unwrap();
        }
        let deleted = delete_files_by_ids(&conn, &["bulk0".into(), "bulk2".into()]).unwrap();
        assert_eq!(deleted, 2);
        assert!(get_file(&conn, "bulk0").unwrap().is_none());
        assert!(get_file(&conn, "bulk1").unwrap().is_some());
        assert!(get_file(&conn, "bulk2").unwrap().is_none());
    }

    #[test]
    fn test_insert_list_delete_report() {
        let conn = setup_db();
        let id = insert_report(
            &conn,
            "http://example.com",
            "spam",
            "details",
            Some("1.2.3.4"),
            Some("a@b.com"),
        )
        .unwrap();
        assert!(id > 0);
        let reports = list_reports(&conn).unwrap();
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].file_url, "http://example.com");
        assert!(delete_report(&conn, id).unwrap());
        assert!(list_reports(&conn).unwrap().is_empty());
    }

    #[test]
    fn test_insert_list_delete_feedback() {
        let conn = setup_db();
        let id = insert_feedback(&conn, "great app", Some("a@b.com"), Some("1.2.3.4")).unwrap();
        assert!(id > 0);
        let feedback = list_feedback(&conn).unwrap();
        assert_eq!(feedback.len(), 1);
        assert_eq!(feedback[0].message, "great app");
        assert!(delete_feedback(&conn, id).unwrap());
        assert!(list_feedback(&conn).unwrap().is_empty());
    }

    #[test]
    fn cleanup_ephemeral_data_removes_only_expired_or_retained_rows() {
        let conn = setup_db();
        create_session(&conn, "expired", "u", 1, 10).unwrap();
        create_session(&conn, "active", "u", 1, 200).unwrap();
        conn.execute(
            "INSERT INTO pairing_codes (code_hash, user_id, created_at, expires_at, used) VALUES
             ('expired', 'u', 1, 10, 0), ('used', 'u', 1, 200, 1), ('active', 'u', 1, 200, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO reports (file_url, reason, details, created_at) VALUES ('old', 'r', '', 10), ('new', 'r', '', 90)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO feedback (message, created_at) VALUES ('old', 10), ('new', 90)",
            [],
        )
        .unwrap();

        assert_eq!(cleanup_ephemeral_data(&conn, 100, 50, 50).unwrap(), 5);
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM sessions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pairing_codes", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(list_reports(&conn).unwrap().len(), 1);
        assert_eq!(list_feedback(&conn).unwrap().len(), 1);
    }

    #[test]
    fn failed_login_counts_are_scoped_to_username_and_ip() {
        let conn = setup_db();
        record_failed_login_for_ip(&conn, "admin", "ip-a").unwrap();
        record_failed_login_for_ip(&conn, "admin", "ip-a").unwrap();
        record_failed_login_for_ip(&conn, "admin", "ip-b").unwrap();
        record_failed_login_for_ip(&conn, "other", "ip-a").unwrap();

        assert_eq!(
            count_failed_logins_for_ip(&conn, "admin", "ip-a", 60).unwrap(),
            2
        );
        assert_eq!(
            count_failed_logins_for_ip(&conn, "admin", "ip-b", 60).unwrap(),
            1
        );
        clear_failed_logins_for_ip(&conn, "admin", "ip-a").unwrap();
        assert_eq!(
            count_failed_logins_for_ip(&conn, "admin", "ip-a", 60).unwrap(),
            0
        );
        assert_eq!(
            count_failed_logins_for_ip(&conn, "other", "ip-a", 60).unwrap(),
            1
        );
    }

    #[test]
    fn test_ban_unban_ip() {
        let conn = setup_db();
        ban_ip(&conn, "10.0.0.1", "spam", "admin").unwrap();
        let record = get_ban_record(&conn, "10.0.0.1").unwrap().unwrap();
        assert_eq!(record.ip, "10.0.0.1");
        assert_eq!(record.reason, "spam");
        assert!(unban_ip(&conn, "10.0.0.1").unwrap());
        assert!(get_ban_record(&conn, "10.0.0.1").unwrap().is_none());
    }

    #[test]
    fn test_load_banned_ips_set() {
        let conn = setup_db();
        ban_ip(&conn, "1.1.1.1", "r1", "admin").unwrap();
        ban_ip(&conn, "2.2.2.2", "r2", "admin").unwrap();
        let set = load_banned_ips_set(&conn).unwrap();
        assert_eq!(set.len(), 2);
        assert!(set.contains("1.1.1.1"));
        assert!(set.contains("2.2.2.2"));
    }

    #[test]
    fn test_announcement_upsert() {
        let conn = setup_db();
        let a1 = upsert_announcement(&conn, "Hello", "https://example.com", "info", true).unwrap();
        assert!(a1.is_active);
        let a2 = upsert_announcement(&conn, "Updated", "https://new.com", "warning", true).unwrap();
        assert_eq!(a2.message, "Updated");
        let active = get_active_announcement(&conn).unwrap().unwrap();
        assert_eq!(active.id, a2.id);
    }

    #[test]
    fn test_get_admin_by_username() {
        let conn = setup_db();
        let result = get_admin_by_username(&conn, "admin").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_update_client_file_id_swaps_key() {
        let conn = setup_db();
        let rec = ClientFileRecord {
            id: "old_id".into(),
            token: "tok".into(),
            n: "f.txt".into(),
            m: "text/plain".into(),
            s: 10,
            u: 100,
            e: 200,
        };
        replace_client_files(&conn, "client-1", &[rec]).unwrap();
        assert!(update_client_file_id(&conn, "client-1", "old_id", "new_id").unwrap());
        let files = get_client_files(&conn, "client-1").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, "new_id");
        assert_eq!(files[0].token, "tok");
        // A rename that doesn't exist for this client is a no-op.
        assert!(!update_client_file_id(&conn, "client-1", "missing", "x").unwrap());
    }

    #[test]
    fn test_update_client_file_id_clears_new_id_clash() {
        let conn = setup_db();
        let recs = vec![
            ClientFileRecord {
                id: "old_id".into(),
                token: "tok".into(),
                n: "f.txt".into(),
                m: "".into(),
                s: 10,
                u: 100,
                e: 200,
            },
            ClientFileRecord {
                id: "new_id".into(),
                token: "tok2".into(),
                n: "g.txt".into(),
                m: "".into(),
                s: 20,
                u: 100,
                e: 200,
            },
        ];
        replace_client_files(&conn, "client-1", &recs).unwrap();
        assert!(update_client_file_id(&conn, "client-1", "old_id", "new_id").unwrap());
        let files = get_client_files(&conn, "client-1").unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].id, "new_id");
        assert_eq!(files[0].token, "tok");
    }
}
