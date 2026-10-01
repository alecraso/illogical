//! `illogicald install`: run as a systemd user service, at boot (with
//! lingering) and after crashes.

use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

use anyhow::{Context, bail};

const UNIT: &str = "illogicald.service";

fn unit_text(args: &[String]) -> String {
    let args: String = args.iter().map(|a| format!(" {a}")).collect();
    format!(
        "\
[Unit]
Description=illogical: terminals that outlive their windows

[Service]
Type=notify
NotifyAccess=main
ExecStart=%h/.local/bin/illogicald{args}
Restart=on-failure
RestartSec=1
# Stop the daemon first: it saves every pane, then exits. Only then are the
# shells killed, so a shutdown can't race the save.
KillMode=mixed
TimeoutStopSec=15

[Install]
WantedBy=default.target
"
    )
}

fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

pub fn install(start: bool, daemon_args: &[String]) -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let bin_dir = home.join(".local/bin");
    let dest = bin_dir.join("illogicald");
    let exe = std::env::current_exe()?.canonicalize()?;
    fs::create_dir_all(&bin_dir)?;
    if exe != dest.canonicalize().unwrap_or_default() {
        // Copy then rename, so a running daemon's binary is replaced whole.
        let tmp = bin_dir.join(".illogicald.new");
        fs::copy(&exe, &tmp).with_context(|| format!("copying {}", exe.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, &dest)?;
        println!("installed {}", dest.display());
    }

    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir)?;
    let unit = unit_dir.join(UNIT);
    fs::write(&unit, unit_text(daemon_args))?;
    println!("wrote {}", unit.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    if start {
        // Restart picks up a new binary; panes come back from their
        // checkpoints (in-place upgrades without that are M2b).
        systemctl(&["restart", UNIT])?;
        println!("started {UNIT}");
    }
    let linger = Command::new("loginctl")
        .args([
            "show-user",
            &std::env::var("USER").unwrap_or_default(),
            "-p",
            "Linger",
            "--value",
        ])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
        .unwrap_or(false);
    if !linger {
        println!(
            "note: lingering is off, so it starts at login, not boot: `loginctl enable-linger $USER`"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_carries_daemon_args() {
        let t = super::unit_text(&["--listen".into(), "127.0.0.1:9000".into()]);
        assert!(t.contains("ExecStart=%h/.local/bin/illogicald --listen 127.0.0.1:9000\n"));
        assert!(t.contains("KillMode=mixed"));
        assert!(t.contains("Type=notify"));
    }
}
