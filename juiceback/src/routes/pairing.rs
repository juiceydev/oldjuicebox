//! juicebox-plus pairing, where the website generates codes that devices exchange for JWTs.

use axum::{
    extract::{Path, State},
    Json,
};

use crate::routes::UserId;
use chrono::Utc;
use jsonwebtoken::{encode, EncodingKey, Header};
use rand::Rng;
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use crate::error::AppError;
use crate::state::AppState;
use crate::utils::ClientIp;

#[derive(Deserialize, ToSchema)]
pub struct GenerateCodeRequest {
    /// Optional name for the device being paired
    pub device_name: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct GenerateCodeResponse {
    /// One-time pairing code in XXXX-XXXX format (expires in 300 seconds)
    pub code: String,
    /// Seconds until the code expires
    pub expires_in: u64,
}

#[derive(Deserialize, ToSchema)]
pub struct VerifyCodeRequest {
    /// The 9-character pairing code obtained from the website
    pub code: String,
    /// A human-readable name for this device
    pub device_name: String,
}

#[derive(Serialize, ToSchema)]
pub struct VerifyCodeResponse {
    /// UUID of the paired device
    pub device_id: String,
    /// 30-day JWT used to authenticate this device
    pub token: String,
}

#[derive(Serialize, ToSchema)]
pub struct DeviceInfo {
    /// UUID of the paired device
    pub device_id: String,
    /// Human-readable device name
    pub device_name: String,
    /// Unix timestamp when the device was paired
    pub paired_at: i64,
    /// Unix timestamp of the last heartbeat from the device
    pub last_seen_at: i64,
}

// Handlers

#[utoipa::path(
    post,
    path = "/api/pair/generate",
    request_body = GenerateCodeRequest,
    responses(
        (status = 200, description = "One-time pairing code generated", body = GenerateCodeResponse),
    ),
    tag = "Pairing",
)]
/// POST /api/pair/generate returns a blake3-hashed code once and then it's gone
pub async fn generate_code(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    axum::extract::Extension(client_ip): axum::extract::Extension<ClientIp>,
) -> Result<Json<GenerateCodeResponse>, AppError> {
    let code = generate_pairing_code();
    let code_hash = blake3::hash(code.as_bytes()).to_hex().to_string();

    let now = Utc::now().timestamp();
    let expires_at = now + 300;

    let user_id_for_insert = user_id.clone();
    let ip_hash = crate::utils::hash_ip_for_ban(&client_ip.0.to_string(), &state.config.ip_pepper);
    let inserted = state
        .db_call("insert_pairing_code", move |db| {
            let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
            let outstanding_for_user: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pairing_codes WHERE user_id = ?1 AND used = 0 AND expires_at > ?2",
                rusqlite::params![user_id_for_insert, now],
                |row| row.get(0),
            )?;
            let outstanding_for_ip: i64 = tx.query_row(
                "SELECT COUNT(*) FROM pairing_codes WHERE ip_hash = ?1 AND used = 0 AND expires_at > ?2",
                rusqlite::params![ip_hash, now],
                |row| row.get(0),
            )?;
            if outstanding_for_user >= crate::constants::MAX_PAIRING_CODES_PER_USER
                || outstanding_for_ip >= crate::constants::MAX_PAIRING_CODES_PER_IP
            {
                return Ok(false);
            }
            let inserted = tx.execute(
                "INSERT INTO pairing_codes (code_hash, user_id, created_at, expires_at, used, ip_hash)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5)",
                rusqlite::params![code_hash, user_id, now, expires_at, ip_hash],
            )? > 0;
            tx.commit()?;
            Ok(inserted)
        })
        .await?;
    if !inserted {
        return Err(AppError::TooManyRequests(
            "too many outstanding pairing codes".into(),
        ));
    }

    Ok(Json(GenerateCodeResponse {
        code,
        expires_in: 300,
    }))
}

#[utoipa::path(
    post,
    path = "/api/pair/verify",
    request_body = VerifyCodeRequest,
    responses(
        (status = 200, description = "Device paired successfully", body = VerifyCodeResponse),
        (status = 400, description = "Invalid code format"),
        (status = 404, description = "Code not found"),
        (status = 409, description = "Code already used"),
    ),
    tag = "Pairing",
)]
/// POST /api/pair/verify exchanges a pairing code for a device JWT
pub async fn verify_code(
    State(state): State<Arc<AppState>>,
    Json(body): Json<VerifyCodeRequest>,
) -> Result<Json<VerifyCodeResponse>, AppError> {
    let code = body.code.trim().to_uppercase();
    if !is_valid_pairing_code(&code) {
        return Err(AppError::BadRequest(
            "Invalid code format. Expected XXXX-XXXX.".into(),
        ));
    }

    let code_hash = blake3::hash(code.as_bytes()).to_hex().to_string();
    let now = Utc::now().timestamp();
    let device_id = uuid::Uuid::new_v4().to_string();
    let device_name = body.device_name.trim().to_string();
    if device_name.is_empty() || device_name.len() > 128 {
        return Err(AppError::BadRequest("invalid device name".into()));
    }
    let device_id_clone = device_id.clone();
    let claim = state
        .db_call("claim_pairing_code", move |db| {
            claim_pairing_code(db, &code_hash, &device_id_clone, &device_name, now)
        })
        .await?;

    let user_id = match claim {
        PairingClaim::Claimed(user_id) => user_id,
        PairingClaim::NotFound => return Err(AppError::NotFound),
        PairingClaim::Expired => return Err(AppError::Gone),
        PairingClaim::AlreadyUsed => return Err(AppError::Conflict("Code already used".into())),
    };

    let token = create_device_jwt(&device_id, &user_id, &state.config.ticket_jwt_secret)
        .map_err(|e| AppError::Internal(format!("JWT encoding failed: {e}")))?;

    Ok(Json(VerifyCodeResponse { device_id, token }))
}

#[utoipa::path(
    get,
    path = "/api/device",
    responses(
        (status = 200, description = "List of paired devices", body = Vec<DeviceInfo>),
    ),
    tag = "Pairing",
)]
/// `GET /api/device` - list paired devices for the authenticated user.
pub async fn list_devices(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
) -> Result<Json<Vec<DeviceInfo>>, AppError> {
    let devices = state
        .db_call("list_devices", move |db| {
            let mut stmt = db.prepare(
                "SELECT id, device_name, paired_at, last_seen_at FROM devices WHERE user_id = ?1",
            )?;
            let rows = stmt.query_map(rusqlite::params![user_id], |row| {
                Ok(DeviceInfo {
                    device_id: row.get(0)?,
                    device_name: row.get(1)?,
                    paired_at: row.get(2)?,
                    last_seen_at: row.get(3)?,
                })
            })?;
            rows.collect::<Result<Vec<_>, _>>()
        })
        .await?;

    Ok(Json(devices))
}

#[utoipa::path(
    delete,
    path = "/api/device/{id}",
    params(
        ("id" = String, Path, description = "The device ID to unpair"),
    ),
    responses(
        (status = 200, description = "Device unpaired"),
        (status = 404, description = "Device not found"),
    ),
    tag = "Pairing",
)]
/// DELETE /api/device/:id removes it from the db and broadcasts a disconnect
pub async fn unpair_device(
    State(state): State<Arc<AppState>>,
    UserId(user_id): UserId,
    Path(device_id): Path<String>,
) -> Result<(), AppError> {
    let did_del = device_id.clone();
    let uid_del = user_id.clone();
    let deleted = state
        .db_call("unpair_device", move |db| {
            let mut stmt = db.prepare("DELETE FROM devices WHERE id = ?1 AND user_id = ?2")?;
            let rows = stmt.execute(rusqlite::params![did_del, uid_del])?;
            Ok(rows > 0)
        })
        .await?;

    if deleted {
        // Remove from in-memory connected devices and close WS if connected.
        if let Some(mut devices) = state.connected_devices.get_mut(&user_id) {
            devices.retain(|d| {
                if let Ok(inner) = d.try_lock() {
                    if inner.device_id == device_id {
                        // Dropping the sender closes the WS from the device side.
                        false
                    } else {
                        true
                    }
                } else {
                    true
                }
            });
            if devices.is_empty() {
                drop(devices);
                state.connected_devices.remove(&user_id);
            }
        }

        // Broadcast disconnect so SSE listeners update.
        if let Some(tx) = state.presence_listeners.get(&user_id) {
            let _ = tx.send(crate::state::PresenceEvent::DeviceDisconnected {
                device_id: device_id.clone(),
            });
        }

        Ok(())
    } else {
        Err(AppError::NotFound)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PairingClaim {
    Claimed(String),
    NotFound,
    Expired,
    AlreadyUsed,
}

fn claim_pairing_code(
    db: &rusqlite::Connection,
    code_hash: &str,
    device_id: &str,
    device_name: &str,
    now: i64,
) -> rusqlite::Result<PairingClaim> {
    let tx = Transaction::new_unchecked(db, TransactionBehavior::Immediate)?;
    let record: Option<(String, i64, i64)> = tx
        .query_row(
            "SELECT user_id, expires_at, used FROM pairing_codes WHERE code_hash = ?1",
            rusqlite::params![code_hash],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;

    let (user_id, expires_at, used) = match record {
        Some(record) => record,
        None => return Ok(PairingClaim::NotFound),
    };
    if now > expires_at {
        return Ok(PairingClaim::Expired);
    }
    if used != 0 {
        return Ok(PairingClaim::AlreadyUsed);
    }

    tx.execute(
        "UPDATE pairing_codes SET used = 1 WHERE code_hash = ?1",
        rusqlite::params![code_hash],
    )?;
    tx.execute(
        "INSERT INTO devices (id, user_id, device_name, paired_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?4)",
        rusqlite::params![device_id, user_id, device_name, now],
    )?;
    tx.commit()?;

    Ok(PairingClaim::Claimed(user_id))
}

/// Generate a 9-character pairing code without visually ambiguous characters.
fn generate_pairing_code() -> String {
    let mut rng = rand::thread_rng();
    let chars: Vec<char> = "ABCDEFGHJKLMNPQRSTUVWXYZ23456789".chars().collect();
    (0..9)
        .map(|i| {
            if i == 4 {
                '-'
            } else {
                chars[rng.gen_range(0..chars.len())]
            }
        })
        .collect()
}

/// Validate that a code matches the `XXXX-XXXX` alphanumeric format (9 chars).
pub fn is_valid_pairing_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    bytes.len() == 9
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1].is_ascii_alphanumeric()
        && bytes[2].is_ascii_alphanumeric()
        && bytes[3].is_ascii_alphanumeric()
        && bytes[4] == b'-'
        && bytes[5].is_ascii_alphanumeric()
        && bytes[6].is_ascii_alphanumeric()
        && bytes[7].is_ascii_alphanumeric()
        && bytes[8].is_ascii_alphanumeric()
}

/// Create a JWT for a paired device (30-day expiry).
fn create_device_jwt(device_id: &str, user_id: &str, secret: &str) -> Result<String, String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as usize;

    let claims = DeviceClaims {
        sub: device_id.to_string(),
        user_id: user_id.to_string(),
        iss: crate::auth::ISS_DEVICE.to_string(),
        iat: now,
        exp: now + 30 * 24 * 3600,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| format!("JWT encoding failed: {e}"))
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceClaims {
    pub sub: String,
    pub user_id: String,
    pub iss: String,
    pub iat: usize,
    pub exp: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_format_valid() {
        let code = generate_pairing_code();
        // 4 chars + dash + 4 chars = 9 total (XXXX-XXXX)
        assert_eq!(code.len(), 9);
        assert_eq!(code.as_bytes()[4], b'-');
        assert!(is_valid_pairing_code(&code));
    }

    #[test]
    fn code_validation_rejects_bad_lengths() {
        assert!(!is_valid_pairing_code("ABC"));
        assert!(!is_valid_pairing_code("ABCDEFGHJKLMNP23456789"));
        assert!(!is_valid_pairing_code("ABCDEFGH")); // 8 chars but no dash
    }

    #[test]
    fn code_validation_rejects_dash_in_wrong_place() {
        assert!(!is_valid_pairing_code("ABC-EFGHJ"));
    }

    #[test]
    fn code_is_uppercase_alphanumeric() {
        let code = generate_pairing_code();
        let chars: Vec<char> = code.chars().collect();
        // No lowercase, no I/1/O/0
        for (i, c) in chars.iter().enumerate() {
            if i == 4 {
                assert_eq!(*c, '-');
            } else {
                assert!(c.is_ascii_uppercase() || c.is_ascii_digit());
                assert!(!matches!(*c, 'I' | '1' | 'O' | '0'));
            }
        }
    }

    fn pairing_db() -> rusqlite::Connection {
        let db = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::init_db(&db).unwrap();
        db
    }

    fn insert_pairing_code(db: &rusqlite::Connection, hash: &str, expires_at: i64, used: i64) {
        db.execute(
            "INSERT INTO pairing_codes (code_hash, user_id, created_at, expires_at, used)
             VALUES (?1, 'user-1', 1, ?2, ?3)",
            rusqlite::params![hash, expires_at, used],
        )
        .unwrap();
    }

    #[test]
    fn claim_pairing_code_inserts_device_and_prevents_reuse() {
        let db = pairing_db();
        insert_pairing_code(&db, "hash", 200, 0);

        assert_eq!(
            claim_pairing_code(&db, "hash", "device-1", "Laptop", 100).unwrap(),
            PairingClaim::Claimed("user-1".into())
        );
        assert_eq!(
            claim_pairing_code(&db, "hash", "device-2", "Phone", 100).unwrap(),
            PairingClaim::AlreadyUsed
        );
        let count: i64 = db
            .query_row("SELECT COUNT(*) FROM devices", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    #[test]
    fn claim_pairing_code_distinguishes_missing_and_expired() {
        let db = pairing_db();
        insert_pairing_code(&db, "expired", 99, 0);

        assert_eq!(
            claim_pairing_code(&db, "missing", "device-1", "Laptop", 100).unwrap(),
            PairingClaim::NotFound
        );
        assert_eq!(
            claim_pairing_code(&db, "expired", "device-1", "Laptop", 100).unwrap(),
            PairingClaim::Expired
        );
    }

    #[test]
    fn failed_device_insert_does_not_consume_code() {
        let db = pairing_db();
        insert_pairing_code(&db, "hash", 200, 0);
        db.execute(
            "INSERT INTO devices (id, user_id, device_name, paired_at, last_seen_at)
             VALUES ('duplicate', 'user-1', 'Existing', 1, 1)",
            [],
        )
        .unwrap();

        assert!(claim_pairing_code(&db, "hash", "duplicate", "Laptop", 100).is_err());
        let used: i64 = db
            .query_row(
                "SELECT used FROM pairing_codes WHERE code_hash = 'hash'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(used, 0);
    }
}
