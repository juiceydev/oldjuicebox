//! HTML endpoints that work without javascript like upload confirmation and file listing and reporting so the site still works when js is disabled, very inclusive
// might get rid of it in the future
use axum::{
    extract::{Extension, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect, Response},
    Form,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::ToSchema;

use crate::db;
use crate::error::AppError;
use crate::notify;
use crate::state::AppState;
use crate::utils::ClientIp;

const COOKIE_NAME: &str = "jb_files";
const COOKIE_MAX_AGE: &str = "2592000";

#[derive(Deserialize, ToSchema)]
pub struct ReportForm {
    #[serde(default)]
    pub file_url: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub details: String,
    #[serde(default)]
    pub email: String,
}

/// POST /api/report accepts a form submission, stores it, notifies admins, and redirects back
#[utoipa::path(
    post,
    path = "/api/report",
    request_body(content_type = "application/x-www-form-urlencoded", content = inline(ReportForm), description = "Report form fields"),
    responses(
        (status = 303, description = "Redirects to /report?submitted=1"),
    ),
    tag = "General",
)]
pub async fn report_submit_handler(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    Form(form): Form<ReportForm>,
) -> Result<Redirect, AppError> {
    let file_url = form.file_url.trim();
    if file_url.is_empty()
        || file_url.len() > crate::constants::MAX_REPORT_FILE_URL_LEN
        || form.reason.trim().is_empty()
        || form.reason.trim().len() > crate::constants::MAX_REPORT_REASON_LEN
        || form.details.len() > crate::constants::MAX_REPORT_DETAILS_LEN
    {
        return Err(AppError::InvalidMultipart(
            "file_url and reason are required".into(),
        ));
    }

    let reporter_ip = Some(client_ip.0.to_string());
    let encrypted_ip = reporter_ip
        .as_ref()
        .and_then(|ip| crate::utils::encrypt_ip(ip, &state.config.ip_encryption_key));
    let hashed_ip = reporter_ip
        .as_ref()
        .map(|ip| crate::utils::hash_ip_for_ban(ip, &state.config.ip_pepper));

    let file_url_owned = file_url.to_string();
    let reason_owned = form.reason.trim().to_string();
    let details_owned = form.details.clone();
    let ip_clone = encrypted_ip.clone();
    let email_val = if form.email.trim().is_empty() {
        None
    } else {
        if form.email.trim().len() > crate::constants::MAX_FEEDBACK_EMAIL_LEN {
            return Err(AppError::BadRequest("email is too long".into()));
        }
        Some(form.email.trim().to_string())
    };
    let email_clone = email_val.clone();

    let report_id = state
        .db_call("insert_report", move |conn| {
            db::insert_report(
                conn,
                &file_url_owned,
                &reason_owned,
                &details_owned,
                ip_clone.as_deref(),
                email_clone.as_deref(),
            )
        })
        .await?;

    tracing::info!(
        "report #{}: url={} reason={} ip={}",
        report_id,
        file_url,
        form.reason,
        hashed_ip
            .as_ref()
            .map(|h| crate::utils::truncate_hash(h))
            .unwrap_or("unknown"),
    );

    notify::dispatch_report_notifications(
        &state,
        &state.config,
        file_url,
        form.reason.trim(),
        &form.details,
        hashed_ip.as_deref(),
    );

    if let Some(ref email) = email_val {
        notify::send_reporter_confirmation(
            &state,
            &state.config,
            email,
            file_url,
            form.reason.trim(),
        );
    }

    Ok(Redirect::to("/report?submitted=1"))
}

#[derive(Deserialize, ToSchema)]
pub struct FeedbackForm {
    #[serde(default)]
    pub message: String,
    #[serde(default)]
    pub email: String,
}

/// POST /api/feedback - accept a feedback form submission and redirect to confirmation.
#[utoipa::path(
    post,
    path = "/api/feedback",
    request_body(content_type = "application/x-www-form-urlencoded", content = inline(FeedbackForm), description = "Feedback form fields"),
    responses(
        (status = 303, description = "Redirect to feedback page with submitted flag"),
    ),
    tag = "General",
)]
pub async fn feedback_submit_handler(
    State(state): State<Arc<AppState>>,
    Extension(client_ip): Extension<ClientIp>,
    Form(form): Form<FeedbackForm>,
) -> Result<Redirect, AppError> {
    let message = form.message.trim();
    if message.is_empty() || message.len() > crate::constants::MAX_FEEDBACK_MESSAGE_LEN {
        return Err(AppError::InvalidMultipart("message is required".into()));
    }

    let reporter_ip = Some(client_ip.0.to_string());
    let encrypted_ip = reporter_ip
        .as_ref()
        .and_then(|ip| crate::utils::encrypt_ip(ip, &state.config.ip_encryption_key));
    let hashed_ip = reporter_ip
        .as_ref()
        .map(|ip| crate::utils::hash_ip_for_ban(ip, &state.config.ip_pepper));
    let message_owned = message.to_string();
    let email_val = if form.email.trim().is_empty() {
        None
    } else {
        if form.email.trim().len() > crate::constants::MAX_FEEDBACK_EMAIL_LEN {
            return Err(AppError::BadRequest("email is too long".into()));
        }
        Some(form.email.trim().to_string())
    };
    let ip_clone = encrypted_ip.clone();
    let email_clone = email_val.clone();

    let feedback_id = state
        .db_call("insert_feedback", move |conn| {
            db::insert_feedback(
                conn,
                &message_owned,
                email_clone.as_deref(),
                ip_clone.as_deref(),
            )
        })
        .await?;

    tracing::info!(
        "feedback #{}: len={} email={} ip={}",
        feedback_id,
        message.len(),
        form.email,
        hashed_ip
            .as_ref()
            .map(|h| crate::utils::truncate_hash(h))
            .unwrap_or("unknown"),
    );

    Ok(Redirect::to("/feedback?submitted=1"))
}

#[derive(Deserialize)]
pub struct DeleteForm {
    #[serde(default)]
    pub token: String,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FileCookieEntry {
    pub id: String,
    pub token: String,
}

fn parse_file_cookie(header: &HeaderValue) -> Vec<FileCookieEntry> {
    let cookie_str = header.to_str().unwrap_or("");
    for part in cookie_str.split(';') {
        let part = part.trim();
        if let Some(val) = part.strip_prefix("jb_files=") {
            if let Ok(bytes) = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(val) {
                if let Ok(arr) = serde_json::from_slice::<Vec<FileCookieEntry>>(&bytes) {
                    return arr;
                }
            }
        }
    }
    Vec::new()
}

pub(crate) fn build_file_cookie_value(existing: &HeaderMap, new_entry: &FileCookieEntry) -> String {
    let mut entries = existing
        .get(header::COOKIE)
        .map(parse_file_cookie)
        .unwrap_or_default();

    entries.retain(|e| e.id != new_entry.id);
    entries.push(new_entry.clone());

    let json = serde_json::to_string(&entries).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes())
}

pub fn file_cookie_header(existing: &HeaderMap, new_entry: &FileCookieEntry) -> HeaderValue {
    let val = build_file_cookie_value(existing, new_entry);
    cookie_header_value(&val, existing)
}

/// Build the Set-Cookie header for a `jb_files` value, mirroring the request's
/// security context (Secure only over HTTPS / non-localhost).
fn cookie_header_value(val: &str, existing: &HeaderMap) -> HeaderValue {
    // Only set Secure when the request is over HTTPS (or from localhost over plain HTTP)
    let is_localhost = existing
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|h| h.starts_with("localhost") || h.starts_with("127.0.0.1") || h.starts_with("[::1]"))
        .unwrap_or(false);
    let is_https = existing
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .map(|p| p == "https")
        .unwrap_or(false);
    let secure = !(is_localhost && !is_https);

    HeaderValue::from_str(&format!(
        "{}={}; Path=/; SameSite=Strict{}; Max-Age={}",
        COOKIE_NAME,
        val,
        if secure { "; Secure" } else { "" },
        COOKIE_MAX_AGE
    ))
    .unwrap_or_else(|_| HeaderValue::from_static("jb_files=; Path=/; Max-Age=0"))
}

/// Rewrite the `jb_files` cookie so a renamed file's old ID becomes its new one.
///
/// A rename keeps the delete token, so the entry just swaps its id in place
/// (deduping against any entry already under the new id). Returns the new
/// Set-Cookie header value to attach to the redirect response.
pub fn rename_file_cookie_header(
    existing: &HeaderMap,
    old_id: &str,
    new_id: &str,
    token: &str,
) -> HeaderValue {
    let mut entries = existing
        .get(header::COOKIE)
        .map(parse_file_cookie)
        .unwrap_or_default();

    entries.retain(|e| e.id != old_id && e.id != new_id);
    entries.push(FileCookieEntry {
        id: new_id.to_string(),
        token: token.to_string(),
    });

    let json = serde_json::to_string(&entries).unwrap_or_default();
    let val = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(json.as_bytes());
    cookie_header_value(&val, existing)
}

/// Redirect to the index page with file metadata encoded as base64url so it shows up in the file list for no-JS uploads
#[allow(clippy::too_many_arguments)]
pub fn upload_redirect(
    _headers: &HeaderMap,
    public_url: &str,
    filename: &str,
    _delete_token: &str,
    file_id: &str,
    mime_type: &str,
    size_bytes: i64,
    uploaded_at: i64,
    expires_at: i64,
) -> Response {
    let payload = serde_json::json!({
        "id": file_id,
        "url": public_url,
        "filename": filename,
        "mime_type": mime_type,
        "size_bytes": size_bytes,
        "uploaded_at": uploaded_at,
        "expires_at": expires_at,
    });
    let encoded =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes());

    let location = format!("/index.html?uploaded={}", encoded);
    Redirect::to(&location).into_response()
}

/// Returns true when the request prefers an HTML response which means it's a no-JS form submission
pub fn wants_html(headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|v| v.to_str().ok())
        .map(|a| a.contains("text/html"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wants_html_with_text_html() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("text/html"));
        assert!(wants_html(&headers));
    }

    #[test]
    fn wants_html_with_json() {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static("application/json"));
        assert!(!wants_html(&headers));
    }

    #[test]
    fn wants_html_mixed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::ACCEPT,
            HeaderValue::from_static("application/json, text/html"),
        );
        assert!(wants_html(&headers));
    }

    #[test]
    fn wants_html_missing() {
        let headers = HeaderMap::new();
        assert!(!wants_html(&headers));
    }

    #[test]
    fn cookie_roundtrip_single_entry() {
        let entry = FileCookieEntry {
            id: "abc123".into(),
            token: "tok456".into(),
        };
        let headers = HeaderMap::new();
        let value = build_file_cookie_value(&headers, &entry);
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&value)
            .unwrap();
        let entries: Vec<FileCookieEntry> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].id, "abc123");
        assert_eq!(entries[0].token, "tok456");
    }

    #[test]
    fn cookie_dedup_by_id() {
        let mut headers = HeaderMap::new();
        let entry1 = FileCookieEntry {
            id: "abc".into(),
            token: "old".into(),
        };
        let val1 = build_file_cookie_value(&headers, &entry1);
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("jb_files={}", val1)).unwrap(),
        );

        let entry2 = FileCookieEntry {
            id: "abc".into(),
            token: "new".into(),
        };
        let val2 = build_file_cookie_value(&headers, &entry2);
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&val2)
            .unwrap();
        let entries: Vec<FileCookieEntry> = serde_json::from_slice(&decoded).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].token, "new");
    }

    #[test]
    fn file_cookie_header_format() {
        let entry = FileCookieEntry {
            id: "test".into(),
            token: "tok".into(),
        };
        let headers = HeaderMap::new();
        let hdr = file_cookie_header(&headers, &entry);
        let s = hdr.to_str().unwrap();
        assert!(s.starts_with("jb_files="));
        assert!(s.contains("Path=/"));
        assert!(s.contains("SameSite=Strict"));
        // No Secure flag when no host header and no x-forwarded-proto (plain HTTP dev)
    }

    #[test]
    fn file_cookie_header_secure_when_x_forwarded_proto_https() {
        let entry = FileCookieEntry {
            id: "test".into(),
            token: "tok".into(),
        };
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-proto", HeaderValue::from_static("https"));
        let hdr = file_cookie_header(&headers, &entry);
        let s = hdr.to_str().unwrap();
        assert!(s.contains("Secure"));
    }

    #[test]
    fn parse_file_cookie_empty() {
        let hdr = HeaderValue::from_static("other=value");
        let entries = parse_file_cookie(&hdr);
        assert!(entries.is_empty());
    }
}
