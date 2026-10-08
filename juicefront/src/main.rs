//! juicefront runs the Astro frontend dev = astro dev, prod = build + node.
//! bun > npm. syncs version from cargo workspace.

use std::path::Path;
use std::process::Stdio;
use tokio::process::Command;

/// Set PR_SET_PDEATHSIG so we die with our parent.
unsafe fn watch_parent() {
    extern "C" {
        fn prctl(option: i32, ...) -> i32;
    }
    const PR_SET_PDEATHSIG: i32 = 1;
    const SIGTERM: i32 = 15;
    prctl(PR_SET_PDEATHSIG, SIGTERM);
}

/// Pull the version from [workspace.package] in Cargo.toml.
fn workspace_version() -> Option<String> {
    let candidates = ["Cargo.toml", "../Cargo.toml"];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut in_target_section = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('[') {
                in_target_section = trimmed == "[workspace.package]" || trimmed == "[package]";
                continue;
            }
            if in_target_section {
                if let Some(v) = trimmed.strip_prefix("version") {
                    let v = v.trim().strip_prefix('=')?.trim().trim_matches('"');
                    return Some(v.to_string());
                }
            }
        }
    }
    None
}

/// Find-replace the version string in a json file. first match only.
fn patch_json_version(path: &std::path::Path, new_version: &str) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let full_start = match text.find("\"version\"") {
        Some(i) => i,
        None => return false,
    };
    let after = &text[full_start..];
    let colon = match after.find(':') {
        Some(i) => i,
        None => return false,
    };
    let val_start = match after[colon..].find('"') {
        Some(i) => colon + i + 1,
        None => return false,
    };
    let val_end = match after[val_start..].find('"') {
        Some(i) => val_start + i,
        None => return false,
    };
    if val_end <= val_start {
        return false;
    }
    let old = &after[val_start..val_end];
    if old == new_version {
        return false; // already up to date
    }
    let before = &text[..full_start + val_start];
    let after_ver = &text[full_start + val_end..];
    let new_text = format!("{}{}{}", before, new_version, after_ver);
    std::fs::write(path, new_text).is_ok()
}

/// Bump package.json to match the cargo workspace version.
fn sync_version(ui_dir: &str) {
    let Some(version) = workspace_version() else {
        tracing::warn!("could not read workspace version from Cargo.toml");
        return;
    };

    let pkg_json = Path::new(ui_dir).join("package.json");
    match patch_json_version(&pkg_json, &version) {
        true => tracing::info!("synced package.json version to {}", version),
        false => tracing::debug!("package.json version already up to date ({})", version),
    }
}

/// Spawn the production node server for the given UI directory.
fn spawn_node_server(
    ui_dir: &str,
    ui_port: &str,
    ui_host: &str,
) -> Result<tokio::process::Child, Box<dyn std::error::Error>> {
    let server_entry = format!("{}/server.mjs", ui_dir);
    let mut server_builder = Command::new("node");
    server_builder
        .arg(&server_entry)
        .env("PORT", ui_port)
        .env("HOST", ui_host)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    unsafe {
        server_builder.pre_exec(|| {
            watch_parent();
            Ok(())
        })
    };
    Ok(server_builder
        .spawn()
        .map_err(|e| format!("Failed to spawn Node.js server: {}", e))?)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();

    tracing::info!("juicefront (Astro) starting");

    // Serve a prebuilt (staged) UI build directly. Used by JuiceFetch / systemd
    // deployments where the source tree isn't present: skip install & build.
    if let Some(staged) = std::env::var("JUICEFRONT_UI_DIR")
        .ok()
        .filter(|dir| Path::new(dir).join("server.mjs").is_file())
    {
        let ui_port = std::env::var("UI_PORT").unwrap_or_else(|_| "6400".to_string());
        let ui_host = std::env::var("UI_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        tracing::info!("serving staged UI from {}", staged);
        let mut server = spawn_node_server(&staged, &ui_port, &ui_host)?;
        let status = server.wait().await?;
        if !status.success() {
            tracing::error!("juicefront exited with error: {:?}", status.code());
        }
        return Ok(());
    }

    let ui_dir = if Path::new("juicefront/ui").exists() {
        "juicefront/ui"
    } else if Path::new("ui").exists() {
        "ui"
    } else {
        return Err("Could not find the 'ui' directory. Please ensure you are running from the workspace root or the juicefront directory.".into());
    };

    tracing::info!("Using UI directory: {}", ui_dir);

    sync_version(ui_dir);

    let cmd = match Command::new("bun")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let _ = child.wait().await;
            "bun"
        }
        Err(_) => {
            tracing::warn!("'bun' not found, falling back to 'npm'");
            "npm"
        }
    };

    let mut install = Command::new(cmd);
    install
        .arg("install")
        .current_dir(ui_dir)
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    let install_status = install.spawn().map_err(|e| {
        format!("Failed to spawn '{} install' in {}: {}", cmd, ui_dir, e)
    })?.wait().await?;
    if !install_status.success() {
        return Err("dependency install failed".into());
    }

    if cfg!(debug_assertions) {
        tracing::info!("Starting Astro dev server...");
        let mut cmd_builder = Command::new(cmd);
        cmd_builder
            .arg("run")
            .arg("dev")
            .current_dir(ui_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        // SAFETY: prctl is always safe to call; pre_exec requires unsafe.
        unsafe {
            cmd_builder.pre_exec(|| {
                watch_parent();
                Ok(())
            })
        };
        let mut child = cmd_builder
            .spawn()
            .map_err(|e| format!("Failed to spawn '{} run dev' in {}: {}", cmd, ui_dir, e))?;

        let status = child.wait().await?;

        if !status.success() {
            tracing::error!("juicefront exited with error: {:?}", status.code());
        }
    } else {
        tracing::info!("Building Astro for production...");
        let mut build_builder = Command::new(cmd);
        build_builder
            .arg("run")
            .arg("build")
            .current_dir(ui_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        unsafe {
            build_builder.pre_exec(|| {
                watch_parent();
                Ok(())
            })
        };
        let mut build = build_builder
            .spawn()
            .map_err(|e| format!("Failed to spawn '{} run build' in {}: {}", cmd, ui_dir, e))?;
        let build_status = build.wait().await?;
        if !build_status.success() {
            return Err("Astro build failed".into());
        }

        let ui_port = std::env::var("UI_PORT").unwrap_or_else(|_| "6400".to_string());
        let ui_host = std::env::var("UI_HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
        tracing::info!("Starting production server on {}:{}...", ui_host, ui_port);
        let mut server = spawn_node_server(ui_dir, &ui_port, &ui_host)?;

        let status = server.wait().await?;
        if !status.success() {
            tracing::error!("juicefront exited with error: {:?}", status.code());
        }
    }

    Ok(())
}
