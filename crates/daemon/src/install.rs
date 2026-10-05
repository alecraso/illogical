//! `illogicald install`: run as a systemd user service, at boot (with
//! lingering) and after crashes; on macOS, a launchd agent that starts at
//! login and after crashes.

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

#[cfg_attr(target_os = "macos", allow(dead_code))]
const UNIT: &str = "illogicald.service";

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn unit_text(args: &[String]) -> String {
    let args: String = args.iter().map(|a| format!(" {a}")).collect();
    format!(
        "\
[Unit]
Description=illogical: terminals that outlive their windows
# VM panes reattach to wispd's machines; start after it when it's here.
After=wisp.service

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
# Pane terminals are kept here while the daemon restarts, so the programs
# in them carry on (each pane runs in its own scope, outside this service).
FileDescriptorStoreMax=4096

[Install]
WantedBy=default.target
"
    )
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
fn systemctl(args: &[&str]) -> anyhow::Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("running systemctl (no systemd here? run `illogicald` directly, or `illogicald install --tailnet` in a sandbox)")?;
    if !status.success() {
        bail!("systemctl --user {} failed", args.join(" "));
    }
    Ok(())
}

/// The daemon arguments to install: those given, or with none, the ones an
/// earlier install wrote (read back by `earlier`), so an upgrade keeps them.
fn args_to_install(given: &[String], reset: bool, earlier: impl FnOnce() -> Option<Vec<String>>) -> Vec<String> {
    if !given.is_empty() || reset {
        return given.to_vec();
    }
    let kept = earlier().unwrap_or_default();
    if !kept.is_empty() {
        println!("keeping the daemon arguments from the last install: {} (--reset-args drops them)", kept.join(" "));
    }
    kept
}

/// Where the installed daemon listens: its `--listen`, else the default.
fn listen_of(args: &[String]) -> String {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if let Some(v) = a.strip_prefix("--listen=") {
            return v.to_owned();
        }
        if a == "--listen"
            && let Some(v) = it.next()
        {
            return v.clone();
        }
    }
    "127.0.0.1:7681".into()
}

/// What to do after it starts (#107): open it, read its logs, reach it
/// from elsewhere.
fn next_steps(args: &[String], logs: &str) -> String {
    format!(
        "Open http://{} with `illogical web` (it signs your browser in)\nLogs: {logs}\nFrom other devices: `tailscale serve`, or `illogicald join https://control.illogical.widgets.wtf`\n",
        listen_of(args)
    )
}

/// Arguments in a unit `unit_text` wrote.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn unit_args(unit: &str) -> Option<Vec<String>> {
    let line = unit.lines().find_map(|l| l.strip_prefix("ExecStart=%h/.local/bin/illogicald"))?;
    Some(line.split_whitespace().map(String::from).collect())
}

#[cfg(target_os = "macos")]
pub fn install(start: bool, daemon_args: &[String], reset: bool) -> anyhow::Result<()> {
    launchd::install(start, daemon_args, reset)
}

#[cfg(not(target_os = "macos"))]
pub fn install(start: bool, daemon_args: &[String], reset: bool) -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    copy_binaries(&home)?;

    let unit_dir = home.join(".config/systemd/user");
    fs::create_dir_all(&unit_dir)?;
    let unit = unit_dir.join(UNIT);
    let args = args_to_install(daemon_args, reset, || unit_args(&fs::read_to_string(&unit).ok()?));
    fs::write(&unit, unit_text(&args))?;
    println!("wrote {}", unit.display());

    systemctl(&["daemon-reload"])?;
    systemctl(&["enable", UNIT])?;
    if start {
        // Restart picks up a new binary; running panes are adopted by the
        // new daemon and keep running.
        systemctl(&["restart", UNIT])?;
        println!("started {UNIT}");
        print!("{}", next_steps(&args, "journalctl --user -u illogicald -e"));
    } else {
        println!("start it with `systemctl --user start {UNIT}`");
    }
    let linger = Command::new("loginctl")
        .args(["show-user", &std::env::var("USER").unwrap_or_default(), "-p", "Linger", "--value"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim() == "yes")
        .unwrap_or(false);
    if !linger {
        println!("note: lingering is off, so it starts at login, not boot: `loginctl enable-linger $USER`");
    }
    Ok(())
}

/// This binary (and the CLI beside it) into `~/.local/bin`; where the
/// daemon now is.
pub fn copy_binaries(home: &Path) -> anyhow::Result<PathBuf> {
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

    // The CLI, built next to the daemon, goes next to it too (panes find it
    // on PATH there).
    if let Some(cli) = exe.parent().map(|d| d.join("illogical")).filter(|p| p.exists()) {
        let tmp = bin_dir.join(".illogical.new");
        fs::copy(&cli, &tmp).with_context(|| format!("copying {}", cli.display()))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, bin_dir.join("illogical"))?;
        println!("installed {}", bin_dir.join("illogical").display());
    } else {
        println!("note: no `illogical` CLI next to {}; build it with `cargo build -p illogical`", exe.display());
    }
    Ok(dest)
}

/// macOS: a LaunchAgent in the user's GUI domain. There's no FD store, so
/// restarting the daemon ends its panes' programs (as before M2b on Linux).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod launchd {
    use std::{fs, path::PathBuf, process::Command};

    use anyhow::{Context, bail};

    pub const LABEL: &str = "illogicald";

    fn xml(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
    }

    /// The daemon arguments in a plist `plist_text` wrote (after the
    /// program itself).
    pub fn plist_args(plist: &str) -> Option<Vec<String>> {
        let rest = &plist[plist.find("<key>ProgramArguments</key>")?..];
        let array = &rest[rest.find("<array>")? + "<array>".len()..rest.find("</array>")?];
        let strings = array.split("<string>").skip(1).filter_map(|s| s.split_once("</string>").map(|(v, _)| v));
        let unxml = |s: &str| s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
        Some(strings.skip(1).map(unxml).collect())
    }

    pub fn plist_text(exe: &str, args: &[String], log: &str) -> String {
        let args: String = std::iter::once(exe)
            .chain(args.iter().map(String::as_str))
            .map(|a| format!("\n    <string>{}</string>", xml(a)))
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{LABEL}</string>
  <key>ProgramArguments</key>
  <array>{args}
  </array>
  <key>RunAtLoad</key>
  <true/>
  <!-- Restart after a crash, not after a clean stop. -->
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <!-- Terminals are interactive: don't throttle them like a background job. -->
  <key>ProcessType</key>
  <string>Interactive</string>
  <!-- No FD store here: pane shims keep the terminals while it restarts. -->
  <key>EnvironmentVariables</key>
  <dict>
    <key>ILLOGICAL_KEEP_PANES</key>
    <string>true</string>
  </dict>
  <!-- Stop the daemon first, so it saves every pane. -->
  <key>ExitTimeOut</key>
  <integer>15</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
"#,
            log = xml(log)
        )
    }

    fn launchctl(args: &[&str]) -> anyhow::Result<bool> {
        let status = Command::new("launchctl").args(args).status().context("running launchctl")?;
        Ok(status.success())
    }

    pub fn install(start: bool, daemon_args: &[String], reset: bool) -> anyhow::Result<()> {
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let exe = super::copy_binaries(&home)?;
        let agents = home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents)?;
        let logs = home.join("Library/Logs");
        fs::create_dir_all(&logs)?;
        let log = logs.join("illogicald.log");
        let plist = agents.join(format!("{LABEL}.plist"));
        let args = super::args_to_install(daemon_args, reset, || plist_args(&fs::read_to_string(&plist).ok()?));
        fs::write(&plist, plist_text(&exe.display().to_string(), &args, &log.display().to_string()))?;
        println!("wrote {}", plist.display());

        let domain = format!("gui/{}", nix::unistd::getuid());
        let service = format!("{domain}/{LABEL}");
        if start {
            // In case it was disabled (`launchctl disable`) before.
            let _ = launchctl(&["enable", &service]);
            // Unload the old one (if any) so the new binary and plist are
            // used; its panes end with it.
            let _ = Command::new("launchctl").args(["bootout", &service]).stderr(std::process::Stdio::null()).status();
            let plist = plist.display().to_string();
            // bootout returns before the old one is gone; retry briefly.
            let mut ok = false;
            // Quietly until the last try: the early ones fail while the old
            // one is still going, and launchctl says so on stderr.
            for attempt in 0..20 {
                let mut c = Command::new("launchctl");
                c.args(["bootstrap", &domain, &plist]);
                if attempt < 19 {
                    c.stderr(std::process::Stdio::null());
                }
                if c.status().context("running launchctl")?.success() {
                    ok = true;
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(250));
            }
            if !ok {
                bail!(
                    "launchctl bootstrap {domain} {plist} failed; see {}, or run `{}` in a terminal to see why it stops",
                    log.display(),
                    exe.display()
                );
            }
            println!("started {LABEL}");
            print!("{}", super::next_steps(&args, &log.display().to_string()));
        } else {
            println!("it starts at your next login (or: launchctl bootstrap {domain} {})", plist.display());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn plist_carries_daemon_args() {
        let t = super::launchd::plist_text(
            "/Users/me/.local/bin/illogicald",
            &["--listen".into(), "127.0.0.1:9000".into(), "a<b".into()],
            "/Users/me/Library/Logs/illogicald.log",
        );
        assert!(t.contains(
            "<string>/Users/me/.local/bin/illogicald</string>\n    <string>--listen</string>\n    <string>127.0.0.1:9000</string>\n    <string>a&lt;b</string>\n  </array>"
        ));
        assert!(t.contains("<key>RunAtLoad</key>"));
        assert!(t.contains("<key>ILLOGICAL_KEEP_PANES</key>\n    <string>true</string>"));
        assert_eq!(super::launchd::plist_args(&t).unwrap(), ["--listen", "127.0.0.1:9000", "a<b"]);
        let bare = super::launchd::plist_text("/x/illogicald", &[], "/x/log");
        assert_eq!(super::launchd::plist_args(&bare).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unit_carries_daemon_args() {
        let t = super::unit_text(&["--listen".into(), "127.0.0.1:9000".into()]);
        assert!(t.contains("ExecStart=%h/.local/bin/illogicald --listen 127.0.0.1:9000\n"));
        assert!(t.contains("KillMode=mixed"));
        assert!(t.contains("Type=notify"));
        assert!(t.contains("FileDescriptorStoreMax="));
        assert_eq!(super::unit_args(&t).unwrap(), ["--listen", "127.0.0.1:9000"]);
        assert_eq!(super::unit_args(&super::unit_text(&[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn next_steps_name_the_listen_address() {
        assert!(
            super::next_steps(&[], "logs").starts_with(
                "Open http://127.0.0.1:7681 with `illogical web` (it signs your browser in)\nLogs: logs\n"
            )
        );
        assert!(
            super::next_steps(&["--listen".into(), "127.0.0.1:9000".into()], "l")
                .starts_with("Open http://127.0.0.1:9000 with")
        );
        assert!(super::next_steps(&["--listen=0.0.0.0:1".into()], "l").starts_with("Open http://0.0.0.0:1 with"));
    }

    #[test]
    fn install_keeps_earlier_args_unless_given_or_reset() {
        let earlier = || Some(vec!["--block-listen".to_string(), "1.2.3.4:7443".to_string()]);
        assert_eq!(super::args_to_install(&[], false, earlier), ["--block-listen", "1.2.3.4:7443"]);
        assert_eq!(super::args_to_install(&[], true, earlier), Vec::<String>::new());
        assert_eq!(super::args_to_install(&["--owner".into(), "a@b".into()], false, earlier), ["--owner", "a@b"]);
        assert_eq!(super::args_to_install(&[], false, || None), Vec::<String>::new());
    }
}
