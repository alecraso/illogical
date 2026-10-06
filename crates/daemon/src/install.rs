//! `illogicald install`: run as a systemd user service, at boot (with
//! lingering) and after crashes; on macOS, a launchd agent that starts at
//! login and after crashes (or, with `--system`, a LaunchDaemon that starts
//! at boot); on Windows, a scheduled task at logon (or at boot, with
//! `--system`). `illogicald uninstall` removes it.

use std::process::Command;
#[cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
const UNIT: &str = "illogicald.service";

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
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

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
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
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn unit_args(unit: &str) -> Option<Vec<String>> {
    let line = unit.lines().find_map(|l| l.strip_prefix("ExecStart=%h/.local/bin/illogicald"))?;
    Some(line.split_whitespace().map(String::from).collect())
}

#[cfg(target_os = "macos")]
pub fn install(start: bool, daemon_args: &[String], reset: bool, system: bool) -> anyhow::Result<()> {
    launchd::install(start, daemon_args, reset, system)
}

#[cfg(target_os = "macos")]
pub fn uninstall() -> anyhow::Result<()> {
    launchd::uninstall()
}

/// Windows: a logon task, in M59 (#222).
#[cfg(windows)]
pub fn install(start: bool, daemon_args: &[String], reset: bool, system: bool) -> anyhow::Result<()> {
    windows::install(start, daemon_args, reset, system)
}

#[cfg(windows)]
pub fn uninstall() -> anyhow::Result<()> {
    windows::uninstall()
}

/// Windows (M59, #222): the binaries in `%LOCALAPPDATA%\Programs\illogical`,
/// and a scheduled task, `illogicald`, that starts the daemon at logon as
/// this user (no admin), with no window (`conhost --headless`), logging to
/// `illogicald.log` in the state directory. Logging off ends it, as with
/// launchd and systemd without linger; its panes' hosts close after their
/// grace. `--system` starts it at boot instead (S4U: an admin prompt once,
/// and panes there have no DPAPI, so no Credential Manager; S29).
#[cfg(windows)]
mod windows {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::Command,
        time::{Duration, Instant},
    };

    use anyhow::{Context, bail};

    const TASK: &str = "illogicald";
    /// What goes in, from beside this exe.
    const FILES: [&str; 4] = ["illogicald.exe", "illogical.exe", "conpty.dll", "OpenConsole.exe"];

    pub fn programs() -> PathBuf {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Programs").join("illogical")
    }

    /// The files, into `programs()`. One in use (the running daemon's
    /// exe, its ConPTY) can't be overwritten but can be renamed: it moves to
    /// `old\` first, and goes once nothing runs it.
    pub fn copy_binaries() -> anyhow::Result<PathBuf> {
        let dir = programs();
        let old = dir.join("old");
        fs::create_dir_all(&old)?;
        let me = std::env::current_exe()?.canonicalize()?;
        let from = me.parent().context("this exe has no directory")?.to_owned();
        let stamp = crate::store::now_ms();
        for name in FILES {
            let src = from.join(name);
            let dest = dir.join(name);
            if !src.is_file() {
                if name == "illogical.exe" {
                    println!(
                        "note: no `illogical` CLI next to {}; build it with `cargo build -p illogical`",
                        me.display()
                    );
                }
                continue;
            }
            if dest.canonicalize().is_ok_and(|d| d == src.canonicalize().unwrap_or_default()) {
                continue;
            }
            let tmp = dir.join(format!("{name}.new"));
            fs::copy(&src, &tmp).with_context(|| format!("copying {}", src.display()))?;
            if dest.exists() && fs::remove_file(&dest).is_err() {
                fs::rename(&dest, old.join(format!("{name}.{stamp}")))
                    .with_context(|| format!("moving the running {} aside", dest.display()))?;
            }
            fs::rename(&tmp, &dest)?;
            println!("installed {}", dest.display());
        }
        if let Ok(entries) = fs::read_dir(&old) {
            for e in entries.flatten() {
                let _ = fs::remove_file(e.path());
            }
        }
        Ok(dir.join("illogicald.exe"))
    }

    fn xml(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
    }

    /// This user, by SID: a name with its domain isn't always one Task
    /// Scheduler can map (an ssh session's `USERDOMAIN` is `WORKGROUP`).
    fn user() -> anyhow::Result<String> {
        Ok(crate::pipe::my_sid()?)
    }

    /// The task, as Task Scheduler's XML.
    pub fn task_xml(exe: &Path, log: &Path, args: &[String], system: bool, user: &str) -> String {
        let mut words =
            vec!["--headless".to_owned(), exe.display().to_string(), "--log-file".into(), log.display().to_string()];
        words.extend(args.iter().cloned());
        let arguments = crate::conpty::command_line(&words[0], &words[1..]);
        let (trigger, logon) = if system {
            ("<BootTrigger><Enabled>true</Enabled></BootTrigger>".to_owned(), "S4U")
        } else {
            (
                format!("<LogonTrigger><Enabled>true</Enabled><UserId>{}</UserId></LogonTrigger>", xml(user)),
                "InteractiveToken",
            )
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>illogicald: illogical's daemon, which keeps your terminals running</Description></RegistrationInfo>
  <Triggers>{trigger}</Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>{logon}</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>
    <StartWhenAvailable>true</StartWhenAvailable>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>conhost.exe</Command><Arguments>{arguments}</Arguments></Exec></Actions>
</Task>
"#,
            user = xml(user),
            arguments = xml(&arguments),
        )
    }

    fn schtasks(args: &[&str]) -> anyhow::Result<()> {
        let out = Command::new("schtasks").args(args).output().context("running schtasks")?;
        if !out.status.success() {
            bail!("schtasks {} failed: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
        }
        Ok(())
    }

    pub fn install(start: bool, daemon_args: &[String], reset: bool, system: bool) -> anyhow::Result<()> {
        let exe = copy_binaries()?;
        let dir = programs();
        let args_file = dir.join("daemon-args.json");
        let args =
            super::args_to_install(daemon_args, reset, || serde_json::from_slice(&fs::read(&args_file).ok()?).ok());
        crate::store::write_atomic(&args_file, &serde_json::to_vec(&args)?)?;
        let state = crate::default_state_dir();
        fs::create_dir_all(&state)?;
        let log = state.join("illogicald.log");
        let task = task_xml(&exe, &log, &args, system, &user()?);
        // Task Scheduler reads it as UTF-16.
        let file = dir.join("illogicald-task.xml");
        let mut bytes = vec![0xff, 0xfe];
        bytes.extend(task.encode_utf16().flat_map(u16::to_le_bytes));
        fs::write(&file, bytes)?;
        let created = schtasks(&["/Create", "/TN", TASK, "/XML", &file.display().to_string(), "/F"]);
        let _ = fs::remove_file(&file);
        if let Err(e) = created {
            if system {
                bail!("{e:#}\n--system (start at boot) needs an administrator: run it from an elevated terminal");
            }
            return Err(e);
        }
        println!("registered the scheduled task {TASK} ({})", if system { "at boot" } else { "at logon" });
        on_path(&dir);
        if start {
            // The running one (the old binary) saves and goes; its panes'
            // hosts wait for the new one.
            let pid = ask_to_stop(&state);
            let asked = Instant::now();
            let mut ended = false;
            while crate::daemon_running(&state) {
                // One that doesn't stop when asked (from before it could
                // be) is ended; its panes' hosts carry on regardless.
                if !ended && asked.elapsed() > Duration::from_secs(10) {
                    if let Some(pid) = pid {
                        println!("the running illogicald (pid {pid}) didn't stop when asked; ending it");
                        crate::procinfo::kill(pid);
                    }
                    ended = true;
                }
                if asked.elapsed() > Duration::from_secs(20) {
                    bail!("the running illogicald didn't stop; stop it (or log off and on) and run this again");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = schtasks(&["/End", "/TN", TASK]);
            schtasks(&["/Run", "/TN", TASK])?;
            let deadline = Instant::now() + Duration::from_secs(20);
            while !crate::daemon_running(&state) {
                if Instant::now() > deadline {
                    bail!("started {TASK}, but it isn't answering; see {}", log.display());
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            println!("started {TASK}");
            print!("{}", super::next_steps(&args, &log.display().to_string()));
        } else {
            println!("start it with `schtasks /Run /TN {TASK}`");
        }
        if system {
            println!(
                "note: at boot it runs without your password: panes can't use Windows' stored credentials (Credential Manager, Git Credential Manager)"
            );
        }
        Ok(())
    }

    /// `dir` on the user's PATH (new terminals see it), if it isn't.
    /// Through .NET's own setter, which tells running programs too (setx
    /// would cut a long PATH at 1024 characters).
    fn on_path(dir: &Path) {
        let have = std::env::var_os("PATH")
            .is_some_and(|p| std::env::split_paths(&p).any(|d| d.as_os_str().eq_ignore_ascii_case(dir.as_os_str())));
        if have {
            return;
        }
        let d = dir.display().to_string().replace('\'', "''");
        let script = format!(
            "$p = [Environment]::GetEnvironmentVariable('Path', 'User'); \
             if (-not (($p -split ';') -contains '{d}')) {{ \
               [Environment]::SetEnvironmentVariable('Path', (@($p, '{d}') | Where-Object {{ $_ }}) -join ';', 'User') }}"
        );
        let ok = Command::new("powershell")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .status()
            .is_ok_and(|s| s.success());
        if ok {
            println!("added {} to your PATH (new terminals have `illogical`)", dir.display());
        } else {
            println!("note: add {} to your PATH for `illogical`", dir.display());
        }
    }

    /// `POST /api/daemon/stop` over its pipe: it saves every pane and goes.
    /// The daemon's pid (the pipe's server), if one answered.
    fn ask_to_stop(state: &Path) -> Option<u32> {
        use std::{
            io::{Read, Write},
            os::windows::io::AsRawHandle,
        };
        let pipe = fs::read_to_string(state.join("sock.path")).ok()?;
        let mut f = fs::OpenOptions::new().read(true).write(true).open(pipe.trim()).ok()?;
        let mut pid = 0u32;
        // SAFETY: a pipe handle we hold.
        let known =
            unsafe { windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId(f.as_raw_handle(), &mut pid) } != 0;
        let req = "POST /api/daemon/stop HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        if f.write_all(req.as_bytes()).is_ok() {
            let mut buf = [0u8; 512];
            let _ = f.read(&mut buf);
        }
        known.then_some(pid)
    }

    pub fn uninstall() -> anyhow::Result<()> {
        ask_to_stop(&crate::default_state_dir());
        let _ = schtasks(&["/End", "/TN", TASK]);
        if schtasks(&["/Delete", "/TN", TASK, "/F"]).is_err() {
            println!("illogicald isn't installed as a scheduled task here");
            return Ok(());
        }
        println!(
            "illogicald is no longer a scheduled task; {} and the panes' state ({}) are kept",
            programs().display(),
            crate::default_state_dir().display()
        );
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn the_task_runs_headless_with_the_log_and_args() {
            let t = task_xml(
                Path::new(r"C:\Users\a b\AppData\Local\Programs\illogical\illogicald.exe"),
                Path::new(r"C:\s\illogicald.log"),
                &["--listen".into(), "127.0.0.1:7681".into()],
                false,
                r"BOX\a b",
            );
            assert!(t.contains("<LogonTrigger>") && t.contains("<LogonType>InteractiveToken</LogonType>"));
            assert!(t.contains("<UserId>BOX\\a b</UserId>"));
            assert!(t.contains(
                "<Arguments>--headless &quot;C:\\Users\\a b\\AppData\\Local\\Programs\\illogical\\illogicald.exe&quot; --log-file C:\\s\\illogicald.log --listen 127.0.0.1:7681</Arguments>"
            ));
            let boot = task_xml(Path::new("x.exe"), Path::new("l"), &[], true, "u");
            assert!(boot.contains("<BootTrigger>") && boot.contains("<LogonType>S4U</LogonType>"));
        }
    }
}

/// Stop and remove the systemd user service; the binaries and state stay.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn uninstall() -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let unit = home.join(".config/systemd/user").join(UNIT);
    if !unit.is_file() {
        println!("illogicald isn't installed as a service here");
        return Ok(());
    }
    systemctl(&["disable", "--now", UNIT])?;
    fs::remove_file(&unit)?;
    println!("removed {}", unit.display());
    systemctl(&["daemon-reload"])?;
    println!(
        "illogicald is no longer a service here; {} and the panes' state (~/.local/state/illogical) are kept",
        home.join(".local/bin").display()
    );
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
pub fn install(start: bool, daemon_args: &[String], reset: bool, system: bool) -> anyhow::Result<()> {
    if system {
        bail!(
            "--system is for macOS (a LaunchDaemon). On Linux the user service starts at boot once lingering is on: \
             `loginctl enable-linger $USER`"
        );
    }
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

#[cfg(unix)]
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
        crate::perm::set(&tmp, 0o755)?;
        fs::rename(&tmp, &dest)?;
        println!("installed {}", dest.display());
    }

    // The CLI, built next to the daemon, goes next to it too (panes find it
    // on PATH there).
    if let Some(cli) = exe.parent().map(|d| d.join("illogical")).filter(|p| p.exists()) {
        let tmp = bin_dir.join(".illogical.new");
        fs::copy(&cli, &tmp).with_context(|| format!("copying {}", cli.display()))?;
        crate::perm::set(&tmp, 0o755)?;
        fs::rename(&tmp, bin_dir.join("illogical"))?;
        println!("installed {}", bin_dir.join("illogical").display());
    } else {
        println!("note: no `illogical` CLI next to {}; build it with `cargo build -p illogical`", exe.display());
    }
    Ok(dest)
}

/// macOS: a LaunchAgent in the user's GUI domain when they have a GUI
/// login; a background agent in `user/UID` when they don't (reached only
/// over ssh), which survives logging out but not a reboot; or, with
/// `--system`, a LaunchDaemon that runs as them and starts at boot (sudo
/// once). There's no FD store, so pane shims keep the terminals while the
/// daemon restarts.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
mod launchd {
    use std::{
        fs,
        path::{Path, PathBuf},
        process::{Command, Stdio},
    };

    use anyhow::{Context, bail};

    pub const LABEL: &str = "illogicald";

    /// How launchd runs it.
    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum Mode {
        /// A LaunchAgent in `gui/UID`: starts at the user's GUI login.
        Gui,
        /// The same plist with `LimitLoadToSessionType` Background, in
        /// `user/UID`: what works with no GUI login. Loaded again at the
        /// next login (GUI), or by installing again.
        Background,
        /// A LaunchDaemon in `system` with `UserName`: starts at boot.
        System { user: String, home: String },
    }

    impl Mode {
        pub fn label(&self) -> String {
            match self {
                Mode::System { user, .. } => format!("{LABEL}.{user}"),
                _ => LABEL.into(),
            }
        }
    }

    /// The LaunchDaemon `--system` installs for `user`.
    pub fn system_plist(user: &str) -> PathBuf {
        PathBuf::from(format!("/Library/LaunchDaemons/{LABEL}.{user}.plist"))
    }

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

    pub fn plist_text(exe: &str, args: &[String], log: &str, mode: &Mode) -> String {
        let args: String = std::iter::once(exe)
            .chain(args.iter().map(String::as_str))
            .map(|a| format!("\n    <string>{}</string>", xml(a)))
            .collect();
        let (session, user, env) = match mode {
            Mode::Gui => (String::new(), String::new(), String::new()),
            Mode::Background => (
                "\n  <!-- No GUI login here: the user's background session. -->\n  <key>LimitLoadToSessionType</key>\n  <string>Background</string>".into(),
                String::new(),
                String::new(),
            ),
            Mode::System { user, home } => (
                String::new(),
                format!("\n  <!-- Started at boot, as this user. -->\n  <key>UserName</key>\n  <string>{}</string>", xml(user)),
                format!(
                    "\n    <key>HOME</key>\n    <string>{}</string>\n    <key>USER</key>\n    <string>{u}</string>\n    <key>LOGNAME</key>\n    <string>{u}</string>",
                    xml(home),
                    u = xml(user)
                ),
            ),
        };
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>{user}{session}
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
    <string>true</string>{env}
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
            label = xml(&mode.label()),
            log = xml(log)
        )
    }

    /// The one-line warning a background install prints. `illogical
    /// --ssh` passes `note:` lines through.
    pub fn background_note(user: &str) -> String {
        format!(
            "note: {user} has no GUI login on this Mac (only ssh), so illogicald runs as a background agent: it keeps \
             running after you log out, but after a reboot it won't start until {user} logs in to the desktop or runs \
             `illogicald install` again. `illogicald install --system` starts it at boot instead (a LaunchDaemon; \
             needs sudo)."
        )
    }

    /// Whether launchd has `target` (a domain or a service), asked quietly.
    fn has(target: &str) -> bool {
        Command::new("launchctl")
            .args(["print", target])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    }

    /// `launchctl` (or `sudo launchctl`) bootout, ignoring "not loaded".
    fn bootout(sudo: bool, service: &str) {
        let mut c = if sudo { Command::new("sudo") } else { Command::new("launchctl") };
        if sudo {
            println!("+ sudo launchctl bootout {service}");
            c.arg("launchctl");
        }
        let _ = c.args(["bootout", service]).stdout(Stdio::null()).stderr(Stdio::null()).status();
    }

    /// Bootstrap `plist` into `domain`, retrying while an old one that was
    /// just booted out is still going (bootout returns before it's gone).
    fn bootstrap(sudo: bool, domain: &str, plist: &Path) -> anyhow::Result<bool> {
        for attempt in 0..20 {
            let mut c = if sudo { Command::new("sudo") } else { Command::new("launchctl") };
            if sudo {
                c.arg("launchctl");
            }
            c.args(["bootstrap", domain]).arg(plist);
            // Quietly until the last try: the early ones fail while the
            // old one is still going, and launchctl says so on stderr.
            if attempt < 19 {
                c.stderr(Stdio::null());
            }
            if c.status().context("running launchctl")?.success() {
                return Ok(true);
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        Ok(false)
    }

    /// Run `sudo ARGS`, saying what it does; sudo asks for a password in
    /// this terminal if it needs one.
    fn sudo(args: &[&str]) -> anyhow::Result<()> {
        println!("+ sudo {}", args.join(" "));
        let status = Command::new("sudo").args(args).status().context("running sudo")?;
        if !status.success() {
            bail!("`sudo {}` failed", args.join(" "));
        }
        Ok(())
    }

    struct Me {
        uid: nix::unistd::Uid,
        name: String,
        home: PathBuf,
    }

    fn me() -> anyhow::Result<Me> {
        let uid = nix::unistd::getuid();
        if uid.is_root() {
            bail!(
                "run this as the user the daemon is for, not as root (with --system it runs sudo for the parts that \
                 need it)"
            );
        }
        let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
        let name = nix::unistd::User::from_uid(uid)?
            .map(|u| u.name)
            .or_else(|| std::env::var("USER").ok())
            .context("who is this? (no user name for this uid)")?;
        Ok(Me { uid, name, home })
    }

    pub fn install(start: bool, daemon_args: &[String], reset: bool, system: bool) -> anyhow::Result<()> {
        let me = me()?;
        let exe = super::copy_binaries(&me.home)?;
        let agents = me.home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents)?;
        let logs = me.home.join("Library/Logs");
        fs::create_dir_all(&logs)?;
        let log = logs.join("illogicald.log");
        let agent = agents.join(format!("{LABEL}.plist"));
        let daemon = system_plist(&me.name);
        // A LaunchDaemon from an earlier `--system` stays one: going back
        // to an agent is `illogicald uninstall` first (both need sudo).
        let system = system || {
            let had = daemon.is_file();
            if had {
                println!(
                    "keeping the LaunchDaemon from the last install (--system); `illogicald uninstall` removes it"
                );
            }
            had
        };
        let args = super::args_to_install(daemon_args, reset, || {
            plist_args(&fs::read_to_string(&daemon).or_else(|_| fs::read_to_string(&agent)).ok()?)
        });
        let gui = format!("gui/{}", me.uid);
        let user = format!("user/{}", me.uid);
        // The desktop app's own launch agent runs the daemon (M46): the
        // copy just put in ~/.local/bin is what its bundled one hands on to
        // (#391), so restart that agent rather than start a second daemon.
        let app_agent = format!("{gui}/{}", crate::selfupdate::APP_AGENT);
        if !system && !agent.is_file() && has(&app_agent) {
            println!("the illogical app's launch agent runs the daemon here; it runs {} from now on", exe.display());
            if start {
                let out = Command::new("launchctl").args(["kickstart", "-k", &app_agent]).output()?;
                if !out.status.success() {
                    bail!("launchctl kickstart -k {app_agent}: {}", String::from_utf8_lossy(&out.stderr).trim());
                }
                println!("restarted {app_agent}");
            }
            return Ok(());
        }
        let mode = if system {
            Mode::System { user: me.name.clone(), home: me.home.display().to_string() }
        } else if has(&gui) {
            Mode::Gui
        } else {
            Mode::Background
        };
        let text = plist_text(&exe.display().to_string(), &args, &log.display().to_string(), &mode);
        let logs = log.display().to_string();

        if let Mode::System { .. } = mode {
            println!(
                "--system: a LaunchDaemon, {}, runs illogicald as {} from boot, with nobody logged in. Writing and \
                 loading it needs root, so this runs sudo (it may ask for your password):",
                daemon.display(),
                me.name
            );
            let tmp = agents.join(format!(".{LABEL}.{}.plist", std::process::id()));
            fs::write(&tmp, &text)?;
            let tmp_s = tmp.display().to_string();
            let daemon_s = daemon.display().to_string();
            let wrote = sudo(&["install", "-m", "644", "-o", "root", "-g", "wheel", &tmp_s, &daemon_s]);
            let _ = fs::remove_file(&tmp);
            wrote?;
            // One daemon per user: the agent would start a second one at the
            // next login.
            for d in [&gui, &user] {
                bootout(false, &format!("{d}/{LABEL}"));
            }
            if agent.is_file() {
                fs::remove_file(&agent)?;
                println!("removed {} (the LaunchDaemon replaces it)", agent.display());
            }
            let service = format!("system/{}", mode.label());
            if start {
                bootout(true, &service);
                sudo(&["launchctl", "enable", &service])?;
                if !bootstrap(true, "system", &daemon)? {
                    bail!("`sudo launchctl bootstrap system {daemon_s}` failed; see {logs}");
                }
                println!("started {service}");
                print!("{}", super::next_steps(&args, &logs));
            } else {
                println!("it starts at the next boot (or: sudo launchctl bootstrap system {daemon_s})");
            }
            return Ok(());
        }

        fs::write(&agent, &text)?;
        println!("wrote {}", agent.display());
        let domain = if mode == Mode::Gui { &gui } else { &user };
        if start {
            let service = format!("{domain}/{LABEL}");
            // In case it was disabled (`launchctl disable`) before.
            let _ = Command::new("launchctl").args(["enable", &service]).status();
            // Unload the old one, in either domain (so a switch between
            // them leaves one), so the new binary and plist are used; its
            // panes' shims keep them for the new one.
            for d in [&gui, &user] {
                bootout(false, &format!("{d}/{LABEL}"));
            }
            if !bootstrap(false, domain, &agent)? {
                bail!(
                    "launchctl bootstrap {domain} {} failed; see {logs}, or run `{}` in a terminal to see why it stops",
                    agent.display(),
                    exe.display()
                );
            }
            println!("started {service}");
            print!("{}", super::next_steps(&args, &logs));
        } else if mode == Mode::Gui {
            println!("it starts at your next login (or: launchctl bootstrap {domain} {})", agent.display());
        } else {
            println!("start it with: launchctl bootstrap {domain} {}", agent.display());
        }
        if mode == Mode::Background {
            println!("{}", background_note(&me.name));
        }
        Ok(())
    }

    /// Stop and remove whichever of the three is installed.
    pub fn uninstall() -> anyhow::Result<()> {
        let me = me()?;
        let mut found = false;
        for d in [format!("gui/{}", me.uid), format!("user/{}", me.uid)] {
            let service = format!("{d}/{LABEL}");
            if has(&service) {
                bootout(false, &service);
                println!("stopped {service}");
                found = true;
            }
        }
        let agent = me.home.join(format!("Library/LaunchAgents/{LABEL}.plist"));
        if agent.is_file() {
            fs::remove_file(&agent)?;
            println!("removed {}", agent.display());
            found = true;
        }
        let daemon = system_plist(&me.name);
        if daemon.is_file() {
            println!("removing the LaunchDaemon {}: that needs root, so this runs sudo:", daemon.display());
            bootout(true, &format!("system/{LABEL}.{}", me.name));
            sudo(&["rm", "-f", &daemon.display().to_string()])?;
            found = true;
        }
        if found {
            println!(
                "illogicald is no longer a service here; {} and the panes' state (~/.local/state/illogical) are kept",
                me.home.join(".local/bin").display()
            );
        } else {
            println!("illogicald isn't installed as a service here");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn plist_carries_daemon_args() {
        let t = super::launchd::plist_text(
            "/Users/me/.local/bin/illogicald",
            &["--listen".into(), "127.0.0.1:9000".into(), "a<b".into()],
            "/Users/me/Library/Logs/illogicald.log",
            &super::launchd::Mode::Gui,
        );
        assert!(t.contains(
            "<string>/Users/me/.local/bin/illogicald</string>\n    <string>--listen</string>\n    <string>127.0.0.1:9000</string>\n    <string>a&lt;b</string>\n  </array>"
        ));
        assert!(t.contains("<key>RunAtLoad</key>"));
        assert!(t.contains("<key>ILLOGICAL_KEEP_PANES</key>\n    <string>true</string>"));
        assert_eq!(super::launchd::plist_args(&t).unwrap(), ["--listen", "127.0.0.1:9000", "a<b"]);
        assert!(!t.contains("LimitLoadToSessionType") && !t.contains("UserName"));
        let bare = super::launchd::plist_text("/x/illogicald", &[], "/x/log", &super::launchd::Mode::Gui);
        assert_eq!(super::launchd::plist_args(&bare).unwrap(), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn plist_modes() {
        use super::launchd::{Mode, plist_args, plist_text};
        let args = ["--listen".to_string(), "127.0.0.1:9000".to_string()];
        let bg = plist_text("/x/illogicald", &args, "/x/log", &Mode::Background);
        assert!(bg.contains("<key>Label</key>\n  <string>illogicald</string>"));
        assert!(bg.contains("<key>LimitLoadToSessionType</key>\n  <string>Background</string>"));
        assert!(!bg.contains("UserName"));
        assert_eq!(plist_args(&bg).unwrap(), args);

        let sys = Mode::System { user: "illo".into(), home: "/Users/illo".into() };
        assert_eq!(super::launchd::system_plist("illo").to_str(), Some("/Library/LaunchDaemons/illogicald.illo.plist"));
        let t = plist_text("/x/illogicald", &args, "/x/log", &sys);
        assert!(t.contains("<key>Label</key>\n  <string>illogicald.illo</string>"));
        assert!(t.contains("<key>UserName</key>\n  <string>illo</string>"));
        assert!(t.contains("<key>HOME</key>\n    <string>/Users/illo</string>"));
        assert!(t.contains("<key>ILLOGICAL_KEEP_PANES</key>\n    <string>true</string>"));
        assert!(!t.contains("LimitLoadToSessionType"));
        assert_eq!(plist_args(&t).unwrap(), args);
    }

    #[cfg(unix)]
    #[test]
    fn background_note_says_reboot_and_system() {
        let n = super::launchd::background_note("illo");
        assert!(n.starts_with("note: illo has no GUI login"));
        assert!(n.contains("after a reboot it won't start until illo logs in"));
        assert!(n.contains("`illogicald install --system`") && n.contains("sudo"));
        assert!(!n.contains('\n'), "one line, so `illogical --ssh` can pass it through");
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
