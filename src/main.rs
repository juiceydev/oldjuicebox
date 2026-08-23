use std::num::NonZeroI32;
use std::process::Stdio;
use tokio::process::Command;
use tokio::signal;
use tracing::{info, warn};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ProcessGroupId(NonZeroI32);

impl ProcessGroupId {
    fn from_child_id(id: Option<u32>) -> Option<Self> {
        id.and_then(|id| i32::try_from(id).ok())
            .and_then(NonZeroI32::new)
            .map(Self)
    }

    fn kill_target(self) -> String {
        format!("-{}", self.0)
    }
}

fn release_flag() -> &'static str {
    if cfg!(debug_assertions) {
        ""
    } else {
        "--release"
    }
}

/// Check whether the `nice` command is available on this system.
fn has_nice() -> bool {
    std::process::Command::new("nice")
        .arg("--help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Build the full argument list for spawning a package: [nice..., run, --package, name, release]
fn spawn_args(nice_val: Option<i32>, package: &str, release: &str) -> (String, Vec<String>) {
    if let Some(n) = nice_val {
        let args = vec![
            "-n".into(),
            n.to_string(),
            "cargo".into(),
            "run".into(),
            "--package".into(),
            package.into(),
            release.into(),
        ];
        ("nice".into(), args)
    } else {
        let args = vec![
            "run".into(),
            "--package".into(),
            package.into(),
            release.into(),
        ];
        ("cargo".into(), args)
    }
}

/// Kill a process group by PGID: SIGTERM first, then SIGKILL after timeout.
async fn kill_process_group(pgid: Option<ProcessGroupId>, name: &str) {
    let Some(pgid) = pgid else {
        warn!(
            "cannot shut down {} process group: child ID unavailable",
            name
        );
        return;
    };
    let target = pgid.kill_target();

    let _ = Command::new("kill")
        .args(["-TERM", "--", &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
    info!("sent SIGTERM to {} process group (pgid {})", name, pgid.0);

    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

    let alive = Command::new("kill")
        .args(["-0", "--", &target])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false);

    if alive {
        info!("{} process group did not exit, sending SIGKILL", name);
        let _ = Command::new("kill")
            .args(["-KILL", "--", &target])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let release = release_flag();
    let use_nice = has_nice();

    // lower nice values = higher priority, back handles api so it gets the most, host handles i/o, front is least critical
    let (back_cmd, back_args) = spawn_args(use_nice.then_some(-5), "juiceback", release);
    let (host_cmd, host_args) = spawn_args(use_nice.then_some(-2), "juicehost", release);
    let (front_cmd, front_args) = spawn_args(use_nice.then_some(5), "juicefront", release);

    info!(
        "starting juiceback (cmd={}, release={})",
        back_cmd,
        release == "--release"
    );
    let mut delta = Command::new(&back_cmd)
        .args(&back_args)
        .process_group(0)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let delta_pgid = ProcessGroupId::from_child_id(delta.id());

    info!(
        "starting juicehost (cmd={}, release={})",
        host_cmd,
        release == "--release"
    );
    let mut omega = Command::new(&host_cmd)
        .args(&host_args)
        .process_group(0)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let omega_pgid = ProcessGroupId::from_child_id(omega.id());

    info!(
        "starting juicefront (cmd={}, release={})",
        front_cmd,
        release == "--release"
    );
    let mut epsilon = Command::new(&front_cmd)
        .args(&front_args)
        .process_group(0)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()?;
    let epsilon_pgid = ProcessGroupId::from_child_id(epsilon.id());

    let result = tokio::select! {
        _ = signal::ctrl_c() => {
            info!("Shutting down Juicebox²...");
            Ok(())
        }
        status = delta.wait() => {
            let code = status?.code().unwrap_or(-1);
            info!("juiceback exited with code {}", code);
            Err(anyhow::anyhow!("juiceback exited with code {}", code))
        }
        status = omega.wait() => {
            let code = status?.code().unwrap_or(-1);
            info!("juicehost exited with code {}", code);
            Err(anyhow::anyhow!("juicehost exited with code {}", code))
        }
        status = epsilon.wait() => {
            let code = status?.code().unwrap_or(-1);
            info!("juicefront exited with code {}", code);
            Err(anyhow::anyhow!("juicefront exited with code {}", code))
        }
    };

    kill_process_group(delta_pgid, "juiceback").await;
    kill_process_group(omega_pgid, "juicehost").await;
    kill_process_group(epsilon_pgid, "juicefront").await;

    info!("shutdown complete");
    result
}

#[cfg(test)]
mod tests {
    use super::ProcessGroupId;

    #[test]
    fn process_group_id_rejects_missing_zero_and_out_of_range_ids() {
        assert_eq!(ProcessGroupId::from_child_id(None), None);
        assert_eq!(ProcessGroupId::from_child_id(Some(0)), None);
        assert_eq!(
            ProcessGroupId::from_child_id(Some(i32::MAX as u32 + 1)),
            None
        );
    }

    #[test]
    fn process_group_id_uses_negative_kill_operand() {
        let pgid = ProcessGroupId::from_child_id(Some(1234)).unwrap();
        assert_eq!(pgid.kill_target(), "-1234");
    }
}
