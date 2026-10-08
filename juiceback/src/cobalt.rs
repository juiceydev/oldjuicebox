//! Client for a self-hosted cobalt API instance (JuiceBox x Cobalt.Tools).
//!
//! cobalt does all media extraction/remuxing; this module just talks to its
//! JSON API and maps responses into something juiceback can act on.
//! Docs: https://github.com/imputnet/cobalt/blob/main/docs/api.md

use serde::Deserialize;
use std::time::Duration;

/// User-selectable options for a fetch job.
#[derive(Debug, Clone, PartialEq)]
pub struct FetchOptions {
    /// Audio only (downloadMode: "audio") vs video+audio (auto).
    pub audio_only: bool,
    /// Video quality cap: max/2160/1440/1080/720/480/360/240/144.
    pub video_quality: String,
    /// Video container preference (YouTube): auto/mp4/webm/mkv.
    pub youtube_video_container: String,
    /// Audio format for audio-only fetches: best/mp3/ogg/wav/opus.
    pub audio_format: String,
    /// Ask cobalt to hunt for the highest available audio quality (YouTube).
    pub youtube_better_audio: bool,
    /// YouTube video codec: h264/av1/vp9.
    pub youtube_video_codec: String,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            audio_only: false,
            video_quality: "1080".to_string(),
            youtube_video_container: "auto".to_string(),
            audio_format: "mp3".to_string(),
            youtube_better_audio: false,
            youtube_video_codec: "h264".to_string(),
        }
    }
}

impl FetchOptions {
    /// Sanitize untrusted option strings into values cobalt accepts.
    #[allow(clippy::too_many_arguments)]
    pub fn sanitized(
        audio_only: bool,
        video_quality: Option<&str>,
        video_container: Option<&str>,
        audio_format: Option<&str>,
        youtube_better_audio: bool,
        youtube_video_codec: Option<&str>,
    ) -> Self {
        let video_quality = match video_quality.unwrap_or("") {
            "max" | "4320" | "2160" | "1440" | "1080" | "720" | "480" | "360" | "240" | "144" => {
                video_quality.unwrap_or("1080").to_string()
            }
            _ => "1080".to_string(),
        };
        let youtube_video_container = match video_container.unwrap_or("") {
            "auto" | "mp4" | "webm" | "mkv" => video_container.unwrap_or("auto").to_string(),
            _ => "auto".to_string(),
        };
        let audio_format = match audio_format.unwrap_or("") {
            "best" | "mp3" | "ogg" | "wav" | "opus" => audio_format.unwrap_or("mp3").to_string(),
            _ => "mp3".to_string(),
        };
        let youtube_video_codec = match youtube_video_codec.unwrap_or("") {
            "h264" | "av1" | "vp9" => youtube_video_codec.unwrap_or("h264").to_string(),
            _ => "h264".to_string(),
        };
        Self {
            audio_only,
            video_quality,
            youtube_video_container,
            audio_format,
            youtube_better_audio,
            youtube_video_codec,
        }
    }
}

/// Build the cobalt `POST /` request body. Pure function so it is testable.
pub fn build_request_body(url: &str, opts: &FetchOptions) -> serde_json::Value {
    serde_json::json!({
        "url": url,
        "downloadMode": if opts.audio_only { "audio" } else { "auto" },
        "videoQuality": opts.video_quality,
        "audioFormat": opts.audio_format,
        "youtubeVideoCodec": opts.youtube_video_codec,
        "youtubeVideoContainer": opts.youtube_video_container,
        "youtubeBetterAudio": opts.youtube_better_audio,
        "filenameStyle": "basic",
        "localProcessing": "disabled",
    })
}

/// A single item in a picker response (e.g. tiktok slideshow entries).
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct CobaltPickerItem {
    #[serde(default)]
    pub r#type: String,
    pub url: String,
    #[serde(default)]
    pub thumb: Option<String>,
}

/// Parsed cobalt response. Only the statuses we act on are structured;
/// anything unexpected becomes `Unsupported`.
#[derive(Debug, Clone)]
pub enum CobaltResponse {
    /// cobalt is proxying/remuxing; fetch `tunnel_url` to get the file bytes.
    Tunnel {
        tunnel_url: String,
        filename: Option<String>,
    },
    /// cobalt redirects straight to the source service URL.
    Redirect {
        url: String,
        filename: Option<String>,
    },
    /// Multiple media items were found; user should pick one (not yet supported).
    Picker { items: Vec<CobaltPickerItem> },
    /// cobalt wants us to merge streams locally (needs ffmpeg); not supported.
    LocalProcessing { service: String },
    /// cobalt refused the request; `code` is its machine-readable error code.
    Error { code: String },
}

impl CobaltResponse {
    /// True when cobalt refused the link at client level (private,
    /// age-restricted or region-locked) — the class of refusals a
    /// session-enabled instance may still be able to serve.
    pub fn is_client_refused(&self) -> bool {
        matches!(
            self,
            CobaltResponse::Error { code }
                if code == "error.api.content.video.unavailable"
        )
    }

    pub fn from_json(value: serde_json::Value) -> Self {
        let status = value
            .get("status")
            .and_then(|s| s.as_str())
            .unwrap_or("")
            .to_string();
        let get_str = |key: &str| {
            value
                .get(key)
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        };
        match status.as_str() {
            "tunnel" => match get_str("url") {
                Some(tunnel_url) => CobaltResponse::Tunnel {
                    tunnel_url,
                    filename: get_str("filename"),
                },
                None => CobaltResponse::Error {
                    code: "error.api.malformed_response".into(),
                },
            },
            "redirect" => match get_str("url") {
                Some(url) => CobaltResponse::Redirect {
                    url,
                    filename: get_str("filename"),
                },
                None => CobaltResponse::Error {
                    code: "error.api.malformed_response".into(),
                },
            },
            "picker" => CobaltResponse::Picker {
                items: value
                    .get("picker")
                    .and_then(|p| p.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|item| {
                                item.get("url")
                                    .and_then(|u| u.as_str())
                                    .map(|u| CobaltPickerItem {
                                        r#type: item
                                            .get("type")
                                            .and_then(|t| t.as_str())
                                            .unwrap_or("")
                                            .to_string(),
                                        url: u.to_string(),
                                        thumb: item
                                            .get("thumb")
                                            .and_then(|t| t.as_str())
                                            .map(|s| s.to_string()),
                                    })
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
            },
            "local-processing" => CobaltResponse::LocalProcessing {
                service: get_str("service").unwrap_or_else(|| "unknown".into()),
            },
            "error" => CobaltResponse::Error {
                code: value
                    .get("error")
                    .and_then(|e| e.get("code"))
                    .and_then(|c| c.as_str())
                    .unwrap_or("error.api.unknown")
                    .to_string(),
            },
            "" => CobaltResponse::Error {
                code: "error.api.empty_response".into(),
            },
            other => {
                tracing::warn!("cobalt returned unknown status {:?}", other);
                CobaltResponse::Error {
                    code: format!("error.api.unknown_status:{}", other),
                }
            }
        }
    }
}

/// Ask cobalt to process a URL. Returns the parsed response, or an error
/// string describing transport-level failures (unreachable, bad status...).
pub async fn process(
    client: &reqwest::Client,
    api_url: &str,
    api_key: &str,
    source_url: &str,
    opts: &FetchOptions,
) -> Result<CobaltResponse, String> {
    let endpoint = format!("{}/", api_url.trim_end_matches('/'));
    let mut request = client
        .post(&endpoint)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .timeout(Duration::from_secs(30))
        .json(&build_request_body(source_url, opts));
    if !api_key.is_empty() {
        request = request.header("Authorization", format!("Api-Key {}", api_key));
    }

    let resp = request
        .send()
        .await
        .map_err(|e| format!("fetch service unreachable: {}", e))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        // Try to surface cobalt's structured error even on non-2xx.
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&body) {
            return Ok(CobaltResponse::from_json(v));
        }
        return Err(format!("fetch service returned status {}", status));
    }

    let value: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("fetch service returned invalid json: {}", e))?;
    Ok(CobaltResponse::from_json(value))
}

/// Build a dedicated download client for cobalt tunnel/redirect URLs.
/// Unlike juiceback's shared client this follows redirects (source-service
/// redirect URLs commonly bounce), but never more than a sane limit.
/// Extract the youtube video id from common link shapes (watch?v=, youtu.be/).
pub fn parse_youtube_video_id(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    if let Some((_, q)) = rest.split_once("v=") {
        let id: String = q.chars().take_while(|c| *c != '&' && *c != '#').collect();
        if !id.is_empty() {
            return Some(id);
        }
    }
    let parts: Vec<&str> = rest.split('/').collect();
    if parts.len() >= 2 && parts[0].eq_ignore_ascii_case("youtu.be") {
        let seg = parts[1].split('?').next().unwrap_or(parts[1]);
        if !seg.is_empty() {
            return Some(seg.to_string());
        }
    }
    None}

/// Best-effort YouTube link detection for session-instance fallback.
pub fn is_youtube_link(url: &str) -> bool {
    let rest = match url.split_once("://") {
        Some((_, r)) => r,
        None => return false,
    };
    let mut host = rest.split('/').next().unwrap_or("");
    if let Some((_user, h)) = host.rsplit_once('@') {
        host = h; // strip userinfo
    }
    let host = host.split(':').next().unwrap_or(host); // strip port
    let host = host.to_ascii_lowercase();
    host == "youtube.com"
        || host == "youtu.be"
        || host.ends_with(".youtube.com")
}

pub fn tunnel_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
}

/// Map a cobalt error code to a user-friendly message.
pub fn friendly_error(code: &str) -> String {
    match code {
        "error.api.auth.jwt.missing"
        | "error.api.auth.jwt.invalid"
        | "error.api.auth.key.missing"
        | "error.api.auth.key.invalid"
        | "error.api.auth.turnstile.missing"
        | "error.api.auth.turnstile.invalid" => {
            "fetch service rejected our credentials (check COBALT_API_KEY)".into()
        }
        "error.api.rate_limit" => {
            "the fetch service is rate limiting requests, try again later".into()
        }
        "error.api.link.invalid" | "error.api.link.unsupported" => {
            "that link isn't supported by cobalt".into()
        }
        "error.api.service.unsupported" => {
            "this service is not supported by the fetch service".into()
        }
        "error.api.service.disabled" => {
            "this service is disabled on the fetch service instance".into()
        }
        "error.api.fetch.empty" | "error.api.fetch.fail" | "error.api.fetch.critical" => {
            "couldn't find any downloadable media at that link".into()
        }
        "error.api.content.too_long" => "that video is too long for the fetch service".into(),
        "error.api.content.video.unavailable"
        | "error.api.content.video.region"
        | "error.api.content.video.age"
        | "error.api.content.video.private"
        | "error.api.content.video.unplayable" => {
            "the video is unavailable (private, age-restricted or region-locked)".into()
        }
        "error.api.content.post.private" | "error.api.content.post.account" => {
            "that post is private or requires an account".into()
        }
        "error.api.youtube.login"
        | "error.api.youtube.token_expired"
        | "error.api.youtube.token_mismatch" => "youtube wants a login for this video".into(),
        "error.api.malformed_response" | "error.api.empty_response" => {
            "the fetch service sent back a malformed response".into()
        }
        other => {
            let clean = other.split(':').next().unwrap_or(other);
            match clean.strip_prefix("error.api.") {
                Some(rest) => format!("fetch failed ({})", rest.replace('.', " ")),
                None => format!("fetch failed ({})", clean),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_body_auto_mode() {
        let body = build_request_body(
            "https://youtu.be/x",
            &FetchOptions::sanitized(false, Some("720"), None, None, false, None),
        );
        assert_eq!(body["downloadMode"], "auto");
        assert_eq!(body["videoQuality"], "720");
        assert_eq!(body["audioFormat"], "mp3");
        assert_eq!(body["youtubeVideoContainer"], "auto");
        assert_eq!(body["youtubeBetterAudio"], false);
        assert_eq!(body["filenameStyle"], "basic");
        assert_eq!(body["localProcessing"], "disabled");
    }

    #[test]
    fn request_body_audio_mode() {
        let body = build_request_body(
            "https://youtu.be/x",
            &FetchOptions::sanitized(true, Some("bogus"), Some("webm"), Some("opus"), true, Some("av1")),
        );
        assert_eq!(body["downloadMode"], "audio");
        assert_eq!(body["videoQuality"], "1080"); // bogus quality sanitized
        assert_eq!(body["youtubeVideoContainer"], "webm"); // kept; cobalt ignores it in audio mode
        assert_eq!(body["audioFormat"], "opus");
        assert_eq!(body["youtubeBetterAudio"], true);
        assert_eq!(body["youtubeVideoCodec"], "av1");
    }

    #[test]
    fn sanitize_rejects_bad_values() {
        let o = FetchOptions::sanitized(
            false,
            Some("<script>"),
            Some("avi"),
            Some("flac"),
            true,
            Some("mpeg2"),
        );
        assert_eq!(o.video_quality, "1080");
        assert_eq!(o.youtube_video_container, "auto");
        assert_eq!(o.audio_format, "mp3");
        assert_eq!(o.youtube_video_codec, "h264");
        assert!(o.youtube_better_audio);
    }

    #[test]
    fn parse_tunnel_response() {
        let v = serde_json::json!({
            "status": "tunnel",
            "url": "http://localhost:7272/tunnel?id=x",
            "filename": "video.mp4"
        });
        match CobaltResponse::from_json(v) {
            CobaltResponse::Tunnel {
                tunnel_url,
                filename,
            } => {
                assert_eq!(tunnel_url, "http://localhost:7272/tunnel?id=x");
                assert_eq!(filename.as_deref(), Some("video.mp4"));
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parse_error_response() {
        let v = serde_json::json!({
            "status": "error",
            "error": { "code": "error.api.link.invalid" }
        });
        match CobaltResponse::from_json(v) {
            CobaltResponse::Error { code } => assert_eq!(code, "error.api.link.invalid"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parse_picker_response() {
        let v = serde_json::json!({
            "status": "picker",
            "picker": [
                { "type": "photo", "url": "https://example.com/1.jpg", "thumb": "https://example.com/1_thumb.jpg" },
                { "type": "video", "url": "https://example.com/2.mp4" }
            ]
        });
        match CobaltResponse::from_json(v) {
            CobaltResponse::Picker { items } => {
                assert_eq!(items.len(), 2);
                assert_eq!(items[0].r#type, "photo");
                assert_eq!(items[1].thumb, None);
            }
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn parse_local_processing_response() {
        let v = serde_json::json!({
            "status": "local-processing",
            "type": "merge",
            "service": "youtube",
            "tunnel": [],
            "output": {}
        });
        match CobaltResponse::from_json(v) {
            CobaltResponse::LocalProcessing { service } => assert_eq!(service, "youtube"),
            other => panic!("unexpected: {:?}", other),
        }
    }

    #[test]
    fn friendly_errors_are_helpful() {
        assert!(friendly_error("error.api.auth.key.invalid").contains("credentials"));
        assert!(friendly_error("error.api.link.unsupported").contains("isn't supported"));
        assert!(friendly_error("error.api.content.too_long").contains("too long"));
        // unknown codes degrade gracefully
        let msg = friendly_error("error.api.something.new");
        assert!(msg.starts_with("fetch failed"));
        assert!(!msg.contains("error.api"));
    }

    #[test]
    fn client_refused_matches_only_unavailable() {
        let refused = CobaltResponse::from_json(serde_json::json!({
            "status": "error",
            "error": {"code": "error.api.content.video.unavailable"}
        }));
        assert!(refused.is_client_refused());

        for code in [
            "error.api.content.too_long",
            "error.api.auth.key.invalid",
            "error.api.fetch.critical",
        ] {
            let r = CobaltResponse::from_json(serde_json::json!({
                "status": "error",
                "error": {"code": code}
            }));
            assert!(!r.is_client_refused(), "{code} must not count as client-refused");
        }

        assert!(!CobaltResponse::LocalProcessing {
            service: "youtube".into()
        }
        .is_client_refused());
    }

    #[test]
    fn youtube_video_id_extraction() {
        assert_eq!(
            parse_youtube_video_id("https://www.youtube.com/watch?v=sPyAQQklc1s&t=1s").as_deref(),
            Some("sPyAQQklc1s")
        );
        assert_eq!(
            parse_youtube_video_id("https://youtu.be/Wfv4Uj-xBT4").as_deref(),
            Some("Wfv4Uj-xBT4")
        );
        assert_eq!(parse_youtube_video_id("https://vimeo.com/123"), None);
    }

    #[test]
    fn youtube_link_detection() {
        assert!(is_youtube_link(
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
        ));
        assert!(is_youtube_link("https://YOUTUBE.COM/shorts/abc"));
        assert!(is_youtube_link("https://youtu.be/jNQXAC9IVRw"));
        assert!(!is_youtube_link("https://vimeo.com/76979871"));
        assert!(!is_youtube_link("https://example.com/not-youtube.com/x"));
    }
}
