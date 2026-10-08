//! Shared IP-ban helpers for juiceback and juicehost.

use std::collections::HashSet;
use std::net::IpAddr;
use std::path::Path;
use std::sync::RwLock;

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Compute HMAC-SHA256(pepper, canonical IP address) for ban lookups.
pub fn hash_ip_for_ban(ip: &str, pepper: &str) -> String {
    let ip = ip
        .parse::<IpAddr>()
        .map(|ip| ip.to_string())
        .unwrap_or_else(|_| ip.to_string());
    let mut mac = <HmacSha256 as Mac>::new_from_slice(pepper.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(ip.as_bytes());
    hex::encode(mac.finalize().into_bytes())
}

/// Truncate a hex string to 12 characters for display in logs/notifications.
pub fn truncate_hash(hex_str: &str) -> &str {
    if hex_str.len() <= 12 {
        hex_str
    } else {
        &hex_str[..12]
    }
}

#[derive(Default)]
struct BanListInner {
    pepper: String,
    hashes: HashSet<String>,
}

/// In-memory set of banned hashes plus the pepper used to compute them.
pub struct BanList {
    inner: RwLock<BanListInner>,
}

impl Default for BanList {
    fn default() -> Self {
        Self::new("")
    }
}

impl BanList {
    /// Create an empty ban list with an optional pepper for hashing incoming IPs.
    pub fn new(pepper: impl Into<String>) -> Self {
        Self {
            inner: RwLock::new(BanListInner {
                pepper: pepper.into(),
                hashes: HashSet::new(),
            }),
        }
    }

    /// Number of banned hashes currently loaded.
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .hashes
            .len()
    }

    /// True when no hashes are loaded.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether banning is enabled: requires a pepper so incoming IPs can be hashed.
    pub fn enabled(&self) -> bool {
        !self
            .inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .pepper
            .is_empty()
    }

    /// Check an IP against the ban list. Always false while banning is disabled.
    pub fn is_banned(&self, ip: &str) -> bool {
        let inner = self.inner.read().unwrap_or_else(|error| error.into_inner());
        if inner.pepper.is_empty() {
            return false;
        }
        let hashed = hash_ip_for_ban(ip, &inner.pepper);
        inner.hashes.contains(&hashed)
    }

    /// The current pepper, used to hash incoming IPs.
    pub fn pepper(&self) -> String {
        self.inner
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .pepper
            .clone()
    }

    /// Set the pepper used to hash incoming IPs.
    pub fn set_pepper(&self, pepper: &str) {
        self.inner
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .pepper = pepper.to_string();
    }

    /// Replace the whole snapshot (authoritative sync from a backend).
    pub fn set_snapshot(&self, pepper: &str, hashes: impl IntoIterator<Item = String>) {
        let hashes: HashSet<String> = hashes.into_iter().collect();
        let mut inner = self
            .inner
            .write()
            .unwrap_or_else(|error| error.into_inner());
        inner.pepper = pepper.to_string();
        inner.hashes = hashes;
    }

    /// Merge extra hashes into the current set (local file additions).
    pub fn merge_hashes(&self, hashes: impl IntoIterator<Item = String>) {
        self.inner
            .write()
            .unwrap_or_else(|error| error.into_inner())
            .hashes
            .extend(hashes);
    }

    /// Load a ban list file and merge its entries into the set.
    pub fn load_file(&self, path: &Path, config_pepper: &str) -> (usize, usize) {
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!("ban list file {} unreadable: {e}", path.display());
                return (0, 0);
            }
        };
        let json: serde_json::Value = match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("ban list file {} is not valid JSON: {e}", path.display());
                return (0, 0);
            }
        };

        let mut hashes: Vec<String> = Vec::new();
        let mut raw_ips: Vec<String> = Vec::new();
        collect_ban_entries(&json, &mut hashes, &mut raw_ips);

        let pepper = self.pepper();
        let pepper = if pepper.is_empty() {
            config_pepper.to_string()
        } else {
            pepper
        };

        let mut added = hashes.len();
        let mut skipped = 0usize;
        for ip in &raw_ips {
            if pepper.is_empty() {
                skipped += 1;
                tracing::warn!(
                    "ban list file {}: cannot hash raw IP {ip} without IP_PEPPER",
                    path.display()
                );
            } else {
                match ip.parse::<IpAddr>() {
                    Ok(ip) => {
                        hashes.push(hash_ip_for_ban(&ip.to_string(), &pepper));
                        added += 1;
                    }
                    Err(_) => {
                        skipped += 1;
                        tracing::warn!("ban list file {}: invalid IP {ip}", path.display());
                    }
                }
            }
        }

        self.merge_hashes(hashes);
        if added > 0 {
            tracing::info!(
                "loaded {} bans from {} (skipped {skipped})",
                added,
                path.display()
            );
        }
        (added, skipped)
    }
}

/// Recursively collect hashes/raw IPs from a parsed JSON document.
fn collect_ban_entries(
    value: &serde_json::Value,
    hashes: &mut Vec<String>,
    raw_ips: &mut Vec<String>,
) {
    match value {
        serde_json::Value::String(s) => hashes.push(s.clone()),
        serde_json::Value::Array(items) => {
            for item in items {
                collect_ban_entries(item, hashes, raw_ips);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(entries) = map.get("entries") {
                collect_ban_entries(entries, hashes, raw_ips);
            }
            if let Some(hashes_arr) = map.get("hashes").and_then(|v| v.as_array()) {
                for h in hashes_arr {
                    if let Some(s) = h.as_str() {
                        hashes.push(s.to_string());
                    }
                }
            }
            if let Some(h) = map.get("hash").and_then(|v| v.as_str()) {
                hashes.push(h.to_string());
            }
            if let Some(ip) = map.get("ip").and_then(|v| v.as_str()) {
                raw_ips.push(ip.to_string());
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic() {
        let a = hash_ip_for_ban("1.2.3.4", "pepper");
        let b = hash_ip_for_ban("1.2.3.4", "pepper");
        assert_eq!(a, b);
        assert_ne!(a, hash_ip_for_ban("1.2.3.5", "pepper"));
        assert_ne!(a, hash_ip_for_ban("1.2.3.4", "other"));
    }

    #[test]
    fn hash_normalizes_ip_addresses() {
        assert_eq!(
            hash_ip_for_ban("2001:0db8:0:0:0:0:0:1", "pepper"),
            hash_ip_for_ban("2001:db8::1", "pepper")
        );
    }

    #[test]
    fn truncate_short_and_long() {
        assert_eq!(truncate_hash("abc"), "abc");
        assert_eq!(truncate_hash(&"a".repeat(40)).len(), 12);
    }

    #[test]
    fn ban_list_basic() {
        let list = BanList::new("pepper");
        let ip = "10.0.0.1";
        let hash = hash_ip_for_ban(ip, "pepper");
        assert!(!list.is_banned(ip));
        list.merge_hashes([hash.clone()]);
        assert!(list.is_banned(ip));
        assert!(!list.is_banned("10.0.0.2"));
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn ban_list_disabled_without_pepper() {
        let list = BanList::new("");
        list.merge_hashes([hash_ip_for_ban("10.0.0.1", "pepper")]);
        assert!(!list.enabled());
        assert!(!list.is_banned("10.0.0.1"));
    }

    #[test]
    fn ban_list_snapshot_replaces() {
        let list = BanList::new("pepper");
        list.set_snapshot("pepper", vec!["a".to_string(), "b".to_string()]);
        assert_eq!(list.len(), 2);
        list.set_snapshot("pepper", vec!["c".to_string()]);
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn load_file_parses_export_format() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        let doc = r#"{
            "entries": [
                {"hash": "h1", "reason": "spam", "banned_by": "admin", "banned_at": 1},
                {"ip": "10.0.0.1", "reason": "raw"}
            ],
            "count": 2
        }"#;
        std::fs::write(&path, doc).unwrap();
        let list = BanList::new("pepper");
        let (added, _) = list.load_file(&path, "pepper");
        assert_eq!(added, 2);
        assert!(list.is_banned("10.0.0.1"));
    }

    #[test]
    fn load_file_parses_bare_array() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bans.json");
        std::fs::write(&path, r#"["h1", "h2"]"#).unwrap();
        let list = BanList::new("pepper");
        let (added, _) = list.load_file(&path, "pepper");
        assert_eq!(added, 2);
        assert_eq!(list.len(), 2);
    }
}
