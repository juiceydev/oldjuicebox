//! JuiceFetch — autonomous updater for the Juicebox² server stack.
//!
//! Stages releases built by CI and signals the systemd services to restart:
//!
//!   1. Polls GitHub Releases (latest *stable* release only) on startup and
//!      every `JUICEFETCH_INTERVAL_SECS` seconds.
//!   2. When a newer version exists, downloads each per-component tarball for
//!      the current platform (plus the arch-independent UI tarball) together
//!      with their `.sha256` sidecars.
//!   3. Verifies every checksum, extracts into `versions/<ver>` and atomically
//!      flips the `current` symlink, so systemd `ExecStart` paths like
//!      `<root>/current/bin/juiceback` keep working across swaps.
//!   4. Signals the stack to restart (`systemctl try-restart <units>`) so the
//!      new binaries and UI build are picked up.
//!
//! Configuration is via environment variables:
//!   JUICEFETCH_REPO            GitHub repo `owner/name` (default juiceboxdev/juicebox)
//!   JUICEFETCH_ROOT            install root (default /var/lib/juicebox)
//!   JUICEFETCH_INTERVAL_SECS   poll interval (default 3600)
//!   JUICEFETCH_UNITS           space-separated systemd units to restart (default all)
//!   GITHUB_TOKEN               optional; avoids rate limits / enables private repos

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;

const DEFAULT_REPO: &str = "juiceboxdev/juicebox";
const UI_ASSET_PREFIX: &str = "juicefront-ui-";
const COMPONENTS: &[&str] = &["juicebox", "juiceback", "juicehost", "juicefront", "juicefetch"];
const DEFAULT_UNITS: &str = "juicebox juiceback juicehost juicefront juicefetch";

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug, Clone)]
struct Config {
    repo: String,
    root: PathBuf,
    interval: Duration,
    units: Vec<String>,
}

/// Resolve config from the environment.
fn config_from_env() -> anyhow::Result<Config> {
    let repo = std::env::var("JUICEFETCH_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let root = PathBuf::from(
        std::env::var("JUICEFETCH_ROOT").unwrap_or_else(|_| "/var/lib/juicebox".to_string()),
    );
    let interval = std::env::var("JUICEFETCH_INTERVAL_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .map(Duration::from_secs)
        .unwrap_or(Duration::from_secs(3600));
    let units = std::env::var("JUICEFETCH_UNITS")
        .unwrap_or_else(|_| DEFAULT_UNITS.to_string())
        .split_whitespace()
        .map(|u| u.to_string())
        .collect();
    Ok(Config {
        repo,
        root,
        interval,
        units,
    })
}

/// Platform suffix used in release asset names, e.g. `linux-x86_64`.
fn platform_suffix() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Asset name for a binary component at `version`.
fn binary_asset_name(component: &str, version: &str) -> String {
    format!("{component}-{version}-{}.tar.gz", platform_suffix())
}

/// Asset name for the arch-independent UI build.
fn ui_asset_name(version: &str) -> String {
    format!("{UI_ASSET_PREFIX}{version}.tar.gz")
}

/// The version currently staged at `root`, if any.
fn current_version(root: &Path) -> Option<String> {
    match std::fs::read_link(root.join("current")) {
        Ok(target) => target
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_string()),
        Err(_) => {
            let text = std::fs::read_to_string(root.join("current")).ok()?;
            let version = text.trim().to_string();
            if version.is_empty() {
                None
            } else {
                Some(version)
            }
        }
    }
}

/// SHA-256 hex digest of `data`.
fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(data);
    format!("{:x}", hasher.finalize())
}

/// First whitespace-separated token of a `sha256sum`-style file.
fn sha_from_sidecar(text: &str) -> Option<String> {
    let token = text.split_whitespace().next()?;
    if token.is_empty() {
        None
    } else {
        Some(token.to_lowercase())
    }
}

/// Download `url`, returning the body bytes.
async fn download(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let response = client.get(url).send().await?;
    let status = response.status();
    let bytes = response.bytes().await?;
    if !status.is_success() {
        anyhow::bail!("download of {url} failed: {status}");
    }
    Ok(bytes.to_vec())
}

/// Download `name` (and its `.sha256` sidecar) from `release`, verify the
/// checksum and return the bytes.
async fn fetch_verified_asset(
    client: &reqwest::Client,
    release: &Release,
    name: &str,
) -> anyhow::Result<Vec<u8>> {
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == name)
        .ok_or_else(|| anyhow::anyhow!("release has no {name} asset"))?;

    let data = download(client, &asset.browser_download_url).await?;

    let sha_name = format!("{name}.sha256");
    match release.assets.iter().find(|asset| asset.name == sha_name) {
        Some(sidecar) => {
            let sha_text = download(client, &sidecar.browser_download_url).await?;
            let text = String::from_utf8_lossy(&sha_text);
            let Some(expected) = sha_from_sidecar(&text) else {
                anyhow::bail!("empty .sha256 sidecar for {name}");
            };
            let actual = sha256_hex(&data);
            if actual != expected {
                anyhow::bail!("checksum mismatch for {name}: expected {expected}, got {actual}");
            }
            tracing::debug!("verified {name} ({expected})");
        }
        None => {
            tracing::warn!("no .sha256 sidecar for {name}, skipping integrity check");
        }
    }

    Ok(data)
}

/// Extract a `.tar.gz` (no top-level directory) into `dest`.
fn extract_tar_gz(bytes: &[u8], dest: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(dest)?;
    let gz = flate2::read::GzDecoder::new(bytes);
    let mut archive = tar::Archive::new(gz);
    archive.unpack(dest)?;
    Ok(())
}

/// The latest non-draft, non-prerelease release, if the repo has any.
async fn latest_stable_release(client: &reqwest::Client, repo: &str) -> anyhow::Result<Option<Release>> {
    let api_base = std::env::var("JUICEFETCH_API_BASE")
        .unwrap_or_else(|_| "https://api.github.com".to_string());
    let url = format!("{api_base}/repos/{repo}/releases/latest");
    let mut request = client.get(&url);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        request = request.header("Authorization", format!("Bearer {token}"));
    }

    let response = request.send().await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let status = response.status();
    let body = response.text().await?;
    if !status.is_success() {
        anyhow::bail!("GitHub releases API returned {status}: {body}");
    }

    let release: Release = serde_json::from_str(&body)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    Ok(Some(release))
}

/// If a newer stable release exists, stage it and flip the `current` symlink.
/// Returns the newly staged version when an update was applied.
async fn fetch_and_apply(config: &Config, client: &reqwest::Client) -> anyhow::Result<Option<String>> {
    let Some(release) = latest_stable_release(client, &config.repo).await? else {
        return Ok(None);
    };

    let Some(version) = release.tag_name.strip_prefix('v') else {
        tracing::debug!("release tag {:?} is not versioned, skipping", release.tag_name);
        return Ok(None);
    };
    let Ok(new_version) = semver::Version::parse(version) else {
        tracing::debug!("release tag {:?} is not semver, skipping", release.tag_name);
        return Ok(None);
    };

    let Some(current) = current_version(&config.root) else {
        tracing::info!("no staged release at {}, bootstrapping to {}", config.root.display(), version);
        return stage_release(config, client, &release, version).await;
    };
    match semver::Version::parse(&current) {
        Ok(current_version) if new_version <= current_version => {
            tracing::debug!("already on latest release ({current})");
            return Ok(None);
        }
        _ => {}
    }

    tracing::info!("new release {version} available (current {current}), staging...");
    stage_release(config, client, &release, version).await
}

/// Download every component for `version`, verify checksums, extract and swap
/// the `current` symlink.
async fn stage_release(
    config: &Config,
    client: &reqwest::Client,
    release: &Release,
    version: &str,
) -> anyhow::Result<Option<String>> {
    let version_dir = config.root.join("versions").join(version);
    let staging = config
        .root
        .join("versions")
        .join(format!(".tmp-{}-{}", version, std::process::id()));

    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    for component in COMPONENTS {
        let name = binary_asset_name(component, version);
        let bytes = fetch_verified_asset(client, release, &name).await?;
        extract_tar_gz(&bytes, &staging.join("bin"))?;
        if !staging.join("bin").join(component).is_file() {
            anyhow::bail!("{name} did not extract a {component} binary");
        }
    }

    let ui_name = ui_asset_name(version);
    let ui_bytes = fetch_verified_asset(client, release, &ui_name).await?;
    extract_tar_gz(&ui_bytes, &staging.join("ui"))?;
    if !staging.join("ui/server.mjs").is_file() || !staging.join("ui/dist/server/entry.mjs").is_file() {
        anyhow::bail!("{ui_name} is missing server.mjs or dist/server/entry.mjs");
    }

    std::fs::write(staging.join("VERSION"), format!("{version}\n"))?;

    // Atomic swap: promote staging into place, then flip the symlink.
    std::fs::create_dir_all(config.root.join("versions"))?;
    if version_dir.exists() {
        std::fs::remove_dir_all(&version_dir)?;
    }
    std::fs::rename(&staging, &version_dir)?;

    let current_tmp = config.root.join("current.tmp");
    let _ = std::fs::remove_file(&current_tmp);
    std::os::unix::fs::symlink(
        Path::new("versions").join(version),
        &current_tmp,
    )?;
    std::fs::rename(&current_tmp, config.root.join("current"))?;

    tracing::info!("staged release {version} at {}", version_dir.display());
    Ok(Some(version.to_string()))
}

/// Restart the configured systemd units so they pick up the new build.
/// Each unit is restarted individually so a missing/stopped unit doesn't
/// block the others.
async fn signal_restart(config: &Config) {
    for unit in &config.units {
        let mut command = tokio::process::Command::new("systemctl");
        command.arg("try-restart").arg(unit);
        tracing::info!("signalling restart of {unit}");
        match command.status().await {
            Ok(status) if status.success() => tracing::info!("{unit} restart signalled"),
            Ok(status) => tracing::warn!("systemctl {unit} exited with {}", status),
            Err(error) => tracing::warn!("failed to run systemctl for {unit}: {error}"),
        }
    }
}

/// Poll loop: check immediately, then every interval.
async fn run_loop(config: Config) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("juicefetch/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(300))
        .build()?;

    loop {
        match fetch_and_apply(&config, &client).await {
            Ok(Some(version)) => {
                signal_restart(&config).await;
                tracing::info!("applied {version}, waiting for next check");
            }
            Ok(None) => {}
            Err(error) => tracing::warn!("update check failed: {error:#}"),
        }
        tokio::time::sleep(config.interval).await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "juicefetch=info".into()),
        )
        .init();

    let config = config_from_env()?;
    tracing::info!(
        "juicefetch starting: repo={} root={} interval={:?}",
        config.repo,
        config.root.display(),
        config.interval
    );

    run_loop(config).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_asset_name_is_arch_specific() {
        assert_eq!(
            binary_asset_name("juiceback", "1.2.3"),
            format!("juiceback-1.2.3-{}-{}.tar.gz", std::env::consts::OS, std::env::consts::ARCH)
        );
    }

    #[test]
    fn ui_asset_name_is_arch_independent() {
        assert_eq!(ui_asset_name("1.2.3"), "juicefront-ui-1.2.3.tar.gz");
    }

    #[test]
    fn sha_sidecar_parsing() {
        let text = "d2d2d2...abc  juiceback-1.2.3-linux-x86_64.tar.gz\n";
        assert_eq!(
            sha_from_sidecar(text).as_deref(),
            Some("d2d2d2...abc")
        );
        assert_eq!(sha_from_sidecar("   \n"), None);
    }
}
