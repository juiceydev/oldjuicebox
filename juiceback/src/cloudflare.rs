//! Purge the Cloudflare cache when files get deleted

use std::sync::Arc;

use crate::config::Config;

/// Purge Cloudflare URLs, it's fire-and-forget so failures just get logged
pub async fn purge_urls(config: Arc<Config>, urls: Vec<String>) {
    purge_cache(config, serde_json::json!({ "files": urls }), "urls").await;
}

pub async fn purge_tags(config: Arc<Config>, tags: Vec<String>) {
    purge_cache(config, serde_json::json!({ "purge_by_tags": tags }), "tags").await;
}

async fn purge_cache(config: Arc<Config>, body: serde_json::Value, kind: &'static str) {
    let (Some(token), Some(zone_id)) = (&config.cf_api_token, &config.cf_zone_id) else {
        return;
    };
    if (body.get("files").and_then(|v| v.as_array()).map(|a| a.is_empty()).unwrap_or(false))
        || (body
            .get("purge_by_tags")
            .and_then(|v| v.as_array())
            .map(|a| a.is_empty())
            .unwrap_or(false))
    {
        return;
    }

    let token = token.clone();
    let zone_id = zone_id.clone();

    tokio::spawn(async move {
        let url = format!(
            "https://api.cloudflare.com/client/v4/zones/{}/purge_cache",
            zone_id
        );

        let client = reqwest::Client::new();
        match client
            .delete(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    tracing::info!("cloudflare: cache purge {} OK (status={})", kind, status);
                } else {
                    let text = resp.text().await.unwrap_or_default();
                    tracing::warn!(
                        "cloudflare: cache purge {} failed (status={}): {}",
                        kind,
                        status,
                        text.chars().take(200).collect::<String>()
                    );
                }
            }
            Err(e) => {
                tracing::warn!("cloudflare: cache purge {} request failed: {}", kind, e);
            }
        }
    });
}

pub fn purge_file(config: &Arc<Config>, id: &str, filename: &str, storage_host: &Option<String>) {
    let url = crate::utils::public_url(&config.public_base_url, storage_host, id, filename);
    let config = Arc::clone(config);
    let id = id.to_string();
    tokio::spawn(async move {
        purge_urls(Arc::clone(&config), vec![url]).await;
        purge_tags(config, vec![format!("juicebox:{id}")]).await;
    });
}
