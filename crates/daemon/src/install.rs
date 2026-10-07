//! `arugulad install`: run as a systemd user service, at boot (with
//! lingering) and after crashes; on macOS, a launchd agent that starts at
//! login and after crashes (or, with `--system`, a LaunchDaemon that starts
//! at boot); on Windows, a scheduled task at logon (or at boot, with
//! `--system`). `arugulad uninstall` removes it.

use std::process::Command;
#[cfg(unix)]
use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
const UNIT: &str = arugula_proto::service::UNIT;
/// What illogical's install called it (#505).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
const OLD_UNIT: &str = arugula_proto::service::OLD_UNIT;

/// The unit, running the daemon with `args`, and with the `Environment=`
/// lines an earlier install's unit had (`env`).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn unit_text(args: &[String], env: &[String]) -> String {
    let args: String = args.iter().map(|a| format!(" {a}")).collect();
    let env: String = env.iter().map(|e| format!("{e}\n")).collect();
    format!(
        "\
[Unit]
Description=arugula: terminals that outlive their windows
# VM panes reattach to wispd's machines; start after it when it's here.
After=wisp.service

[Service]
Type=notify
NotifyAccess=main
{env}ExecStart=%h/.local/bin/arugulad{args}
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
    let status = Command::new("systemctl").arg("--user").args(args).status().context(
        "running systemctl (no systemd here? run `arugulad` directly, or `arugulad install --tailnet` in a sandbox)",
    )?;
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

/// The flags `arugulad install` keeps for the macOS app's launch agent
/// (#550). That agent's plist is in the signed bundle, so it can't carry
/// them as `arugulad install`'s own plist or unit does: the daemon reads
/// them from here when the agent starts it with none (`app_agent_argv`).
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn app_args_file(state: &Path) -> PathBuf {
    state.join("daemon-args.json")
}

/// The flags kept in `file`, if there are any.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn read_app_args(file: &Path) -> Option<Vec<String>> {
    serde_json::from_slice(&fs::read(file).ok()?).ok()
}

/// `argv` with the kept flags added when the app's launch agent started
/// this daemon (`service`, launchd's `XPC_SERVICE_NAME`) with none.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn app_agent_argv(mut argv: Vec<String>, service: Option<&str>, state: &Path) -> Vec<String> {
    if argv.len() == 1 && service == Some(arugula_proto::service::APP_LABEL) {
        argv.extend(read_app_args(&app_args_file(state)).unwrap_or_default());
    }
    argv
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
        "Open http://{} with `arugula web` (it signs your browser in)\nLogs: {logs}\nFrom other devices: `tailscale serve`, or `arugulad join https://control.arugula.io`\n",
        listen_of(args)
    )
}

/// Arguments in a unit `unit_text` wrote, or illogical's (#505).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn unit_args(unit: &str) -> Option<Vec<String>> {
    let line = unit.lines().find_map(|l| {
        let exec = l.strip_prefix("ExecStart=%h/.local/bin/")?;
        exec.strip_prefix("arugulad").or_else(|| exec.strip_prefix("illogicald"))
    })?;
    Some(line.split_whitespace().map(String::from).collect())
}

/// The `Environment=` lines in an earlier install's unit (someone put them
/// there), with illogical's variable names made Arugula's (#505).
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn unit_env(unit: &str) -> Vec<String> {
    unit.lines().filter(|l| l.starts_with("Environment=")).map(renamed_env).collect()
}

/// `ILLOGICAL_X` as `ARUGULA_X` where a variable's name starts: after `=`,
/// a space or a quote (#505).
#[cfg_attr(windows, allow(dead_code))]
fn renamed_env(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("ILLOGICAL_") {
        let starts = i == 0 || matches!(rest.as_bytes()[i - 1], b'=' | b' ' | b'"' | b'\'');
        out.push_str(&rest[..i]);
        out.push_str(if starts { "ARUGULA_" } else { "ILLOGICAL_" });
        rest = &rest[i + "ILLOGICAL_".len()..];
    }
    out.push_str(rest);
    out
}

/// One step of putting the systemd unit in place (#505).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
enum UnitStep {
    /// Write `arugulad.service`, with the flags and environment of the
    /// last install's unit (illogical's, when there's no new one yet).
    Write,
    /// illogical's drop-ins (`illogicald.service.d`) become the new unit's.
    MoveDropIns,
    /// `illogicald.service` becomes a link to `arugulad.service`. On the
    /// reload systemd takes a running illogicald for arugulad (one unit,
    /// two names), its FD store and so its panes with it.
    AliasOld,
    /// Not started at login under the old name.
    UnwantOld,
    Reload,
    Enable,
    /// Restart onto the new binary; the FD store keeps the panes.
    Restart,
    /// Remove the old name's link, and reload, once systemd has the unit
    /// under the new name.
    DropAlias,
}

/// What `install` does with the units, given what's there: whether
/// illogical's unit is (`old`), its drop-ins and the new unit's.
#[cfg_attr(any(target_os = "macos", windows), allow(dead_code))]
fn unit_plan(old: bool, old_dropins: bool, new_dropins: bool, start: bool) -> Vec<UnitStep> {
    use UnitStep::*;
    let mut steps = vec![Write];
    if old {
        if old_dropins && !new_dropins {
            steps.push(MoveDropIns);
        }
        steps.extend([AliasOld, UnwantOld]);
    }
    steps.extend([Reload, Enable]);
    if start {
        steps.push(Restart);
    }
    if old {
        steps.push(DropAlias);
    }
    steps
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

/// Windows (M59, #222): the binaries in `%LOCALAPPDATA%\Programs\arugula`,
/// and a scheduled task, `arugulad`, that starts the daemon at logon as
/// this user (no admin), with no window (`conhost --headless`), logging to
/// `arugulad.log` in the state directory. Logging off ends it, as with
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

    const TASK: &str = "arugulad";
    /// illogical's task (#505).
    const OLD_TASK: &str = "illogicald";
    /// What goes in, from beside this exe: each file and its name there.
    const FILES: [(&str, &str); 4] = [
        ("arugulad.exe", "arugulad.exe"),
        ("arugula.exe", "arugula.exe"),
        ("conpty.dll", "conpty.dll"),
        ("OpenConsole.exe", "OpenConsole.exe"),
    ];
    /// What goes in illogical's folder, under its old names (#505): Claude
    /// Code hooks and older tools run `illogical.exe` by name, and that
    /// folder is on PATH. Copies, not links: install.ps1 can do the same
    /// (Copy-Item), they work across volumes, and replacing one that's
    /// running is the same rename as for the rest.
    const OLD_FILES: [(&str, &str); 4] = [
        ("arugulad.exe", "illogicald.exe"),
        ("arugula.exe", "illogical.exe"),
        ("conpty.dll", "conpty.dll"),
        ("OpenConsole.exe", "OpenConsole.exe"),
    ];

    fn local() -> PathBuf {
        PathBuf::from(std::env::var_os("LOCALAPPDATA").unwrap_or_default())
    }

    pub fn programs() -> PathBuf {
        local().join("Programs").join("arugula")
    }

    /// illogical's install folder (#505).
    pub fn old_programs() -> PathBuf {
        local().join("Programs").join(arugula_proto::rename::OLD)
    }

    /// The files, into `programs()`, and into illogical's folder under the
    /// old names where a machine has it.
    pub fn copy_binaries() -> anyhow::Result<PathBuf> {
        let me = std::env::current_exe()?.canonicalize()?;
        let from = me.parent().context("this exe has no directory")?.to_owned();
        if !from.join("arugula.exe").is_file() {
            println!("note: no `arugula` CLI next to {}; build it with `cargo build -p arugula`", me.display());
        }
        let dir = programs();
        copy_files(&from, &dir, &FILES)?;
        if old_programs().is_dir() {
            copy_files(&from, &old_programs(), &OLD_FILES)?;
        }
        Ok(dir.join("arugulad.exe"))
    }

    /// `files` from `from` into `dir`. One in use (the running daemon's
    /// exe, its ConPTY) can't be overwritten but can be renamed: it moves to
    /// `old\` first, and goes once nothing runs it.
    fn copy_files(from: &Path, dir: &Path, files: &[(&str, &str)]) -> anyhow::Result<()> {
        let old = dir.join("old");
        fs::create_dir_all(&old)?;
        let stamp = crate::store::now_ms();
        for (name, as_name) in files {
            let src = from.join(name);
            let dest = dir.join(as_name);
            if !src.is_file() {
                continue;
            }
            if dest.canonicalize().is_ok_and(|d| d == src.canonicalize().unwrap_or_default()) {
                continue;
            }
            let tmp = dir.join(format!("{as_name}.new"));
            fs::copy(&src, &tmp).with_context(|| format!("copying {}", src.display()))?;
            if dest.exists() && fs::remove_file(&dest).is_err() {
                fs::rename(&dest, old.join(format!("{as_name}.{stamp}")))
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
        Ok(())
    }

    fn task_exists(name: &str) -> bool {
        Command::new("schtasks").args(["/Query", "/TN", name]).output().is_ok_and(|o| o.status.success())
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
  <RegistrationInfo><Description>arugulad: Arugula's daemon, which keeps your terminals running</Description></RegistrationInfo>
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
        // Ours, else the ones illogical's install wrote (#505).
        let old_args = old_programs().join("daemon-args.json");
        let args = super::args_to_install(daemon_args, reset, || {
            serde_json::from_slice(&fs::read(&args_file).or_else(|_| fs::read(&old_args)).ok()?).ok()
        });
        let had_old_task = task_exists(OLD_TASK);
        crate::store::write_atomic(&args_file, &serde_json::to_vec(&args)?)?;
        let state = crate::default_state_dir();
        fs::create_dir_all(&state)?;
        let log = state.join("arugulad.log");
        let task = task_xml(&exe, &log, &args, system, &user()?);
        // Task Scheduler reads it as UTF-16.
        let file = dir.join("arugulad-task.xml");
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
                        println!("the running arugulad (pid {pid}) didn't stop when asked; ending it");
                        crate::procinfo::kill(pid);
                    }
                    ended = true;
                }
                if asked.elapsed() > Duration::from_secs(20) {
                    bail!("the running arugulad didn't stop; stop it (or log off and on) and run this again");
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            let _ = schtasks(&["/End", "/TN", TASK]);
            // illogical's task goes once its daemon has (#505): its panes'
            // hosts wait for the new one.
            if had_old_task {
                let _ = schtasks(&["/End", "/TN", OLD_TASK]);
                retire_old_task();
            }
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
            // Left running (ending it would end its panes), but it won't
            // start again at logon.
            if had_old_task {
                retire_old_task();
            }
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
            println!("added {} to your PATH (new terminals have `arugula`)", dir.display());
        } else {
            println!("note: add {} to your PATH for `arugula`", dir.display());
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

    /// Remove illogical's task (#505).
    fn retire_old_task() {
        match schtasks(&["/Delete", "/TN", OLD_TASK, "/F"]) {
            Ok(()) => println!("removed the scheduled task {OLD_TASK} ({TASK} replaces it)"),
            Err(e) => println!("note: couldn't remove the scheduled task {OLD_TASK}: {e:#}"),
        }
    }

    pub fn uninstall() -> anyhow::Result<()> {
        ask_to_stop(&crate::default_state_dir());
        let mut found = false;
        for task in [TASK, OLD_TASK] {
            let _ = schtasks(&["/End", "/TN", task]);
            found |= schtasks(&["/Delete", "/TN", task, "/F"]).is_ok();
        }
        if !found {
            println!("arugulad isn't installed as a scheduled task here");
            return Ok(());
        }
        println!(
            "arugulad is no longer a scheduled task; {} and the panes' state ({}) are kept",
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
                Path::new(r"C:\Users\a b\AppData\Local\Programs\arugula\arugulad.exe"),
                Path::new(r"C:\s\arugulad.log"),
                &["--listen".into(), "127.0.0.1:7681".into()],
                false,
                r"BOX\a b",
            );
            assert!(t.contains("<LogonTrigger>") && t.contains("<LogonType>InteractiveToken</LogonType>"));
            assert!(t.contains("<UserId>BOX\\a b</UserId>"));
            assert!(t.contains(
                "<Arguments>--headless &quot;C:\\Users\\a b\\AppData\\Local\\Programs\\arugula\\arugulad.exe&quot; --log-file C:\\s\\arugulad.log --listen 127.0.0.1:7681</Arguments>"
            ));
            let boot = task_xml(Path::new("x.exe"), Path::new("l"), &[], true, "u");
            assert!(boot.contains("<BootTrigger>") && boot.contains("<LogonType>S4U</LogonType>"));
        }
    }
}

/// Stop and remove the systemd user service (and illogical's, #505); the
/// binaries and state stay.
#[cfg(all(unix, not(target_os = "macos")))]
pub fn uninstall() -> anyhow::Result<()> {
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?);
    let dir = home.join(".config/systemd/user");
    let mut found = false;
    for name in [UNIT, OLD_UNIT] {
        let unit = dir.join(name);
        if fs::symlink_metadata(&unit).is_err() {
            continue;
        }
        // A link (an old name left for the new unit) is only removed.
        if !fs::symlink_metadata(&unit)?.file_type().is_symlink() {
            systemctl(&["disable", "--now", name])?;
        }
        fs::remove_file(&unit)?;
        println!("removed {}", unit.display());
        found = true;
    }
    if !found {
        println!("arugulad isn't installed as a service here");
        return Ok(());
    }
    systemctl(&["daemon-reload"])?;
    println!(
        "arugulad is no longer a service here; {} and the panes' state ({}) are kept",
        home.join(".local/bin").display(),
        crate::default_state_dir().display()
    );
    Ok(())
}

/// What `systemctl --user show -p Id` says `name` is.
#[cfg(all(unix, not(target_os = "macos")))]
fn unit_id(name: &str) -> Option<String> {
    let out = Command::new("systemctl").args(["--user", "show", "-p", "Id", "--value", name]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn active(name: &str) -> bool {
    Command::new("systemctl").args(["--user", "is-active", "--quiet", name]).status().is_ok_and(|s| s.success())
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
    // illogical's unit, which this one replaces (#505).
    let old = unit_dir.join(OLD_UNIT);
    let had_old = fs::symlink_metadata(&old).is_ok();
    let earlier = fs::read_to_string(&unit).or_else(|_| fs::read_to_string(&old)).ok();
    let args = args_to_install(daemon_args, reset, || unit_args(earlier.as_deref()?));
    let env = earlier.as_deref().map(unit_env).unwrap_or_default();
    let dropins = |u: &str| unit_dir.join(format!("{u}.d"));
    // The old unit, still running under its own name: systemd didn't take
    // it for the new one (older systemd), so it's restarted as it is.
    let mut apart = false;
    for step in unit_plan(had_old, dropins(OLD_UNIT).is_dir(), dropins(UNIT).is_dir(), start) {
        match step {
            UnitStep::Write => {
                fs::write(&unit, unit_text(&args, &env))?;
                println!("wrote {}", unit.display());
            }
            UnitStep::MoveDropIns => fs::rename(dropins(OLD_UNIT), dropins(UNIT))?,
            UnitStep::AliasOld => {
                let tmp = unit_dir.join(format!(".{OLD_UNIT}.link"));
                let _ = fs::remove_file(&tmp);
                std::os::unix::fs::symlink(UNIT, &tmp)?;
                fs::rename(&tmp, &old)?;
            }
            UnitStep::UnwantOld => {
                let _ = fs::remove_file(unit_dir.join("default.target.wants").join(OLD_UNIT));
            }
            UnitStep::Reload => systemctl(&["daemon-reload"])?,
            UnitStep::Enable => systemctl(&["enable", UNIT])?,
            UnitStep::Restart => {
                apart = had_old && active(OLD_UNIT) && unit_id(OLD_UNIT).as_deref() != Some(UNIT);
                // Restart picks up a new binary; running panes are adopted
                // by the new daemon and keep running.
                let name = if apart { OLD_UNIT } else { UNIT };
                systemctl(&["restart", name])?;
                println!("started {name}");
                print!("{}", next_steps(&args, "journalctl --user -u arugulad -e"));
            }
            UnitStep::DropAlias => {
                apart = apart || (active(OLD_UNIT) && unit_id(OLD_UNIT).as_deref() != Some(UNIT));
                if apart {
                    println!("{OLD_UNIT} runs on as a link to {UNIT}; from the next login it's {UNIT}");
                } else {
                    fs::remove_file(&old)?;
                    systemctl(&["daemon-reload"])?;
                    println!("replaced {OLD_UNIT}");
                }
            }
        }
    }
    if !start {
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
    let dest = bin_dir.join("arugulad");
    let exe = std::env::current_exe()?.canonicalize()?;
    fs::create_dir_all(&bin_dir)?;
    // A link (the Mac app's, into its bundle, #550) becomes a copy: an
    // installed daemon updates apart from the app.
    let link = fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink());
    if link || exe != dest.canonicalize().unwrap_or_default() {
        // Copy then rename, so a running daemon's binary is replaced whole.
        let tmp = bin_dir.join(".arugulad.new");
        fs::copy(&exe, &tmp).with_context(|| format!("copying {}", exe.display()))?;
        crate::perm::set(&tmp, 0o755)?;
        fs::rename(&tmp, &dest)?;
        println!("installed {}", dest.display());
    }

    // The CLI, built next to the daemon, goes next to it too (panes find it
    // on PATH there).
    if let Some(cli) = exe.parent().map(|d| d.join("arugula")).filter(|p| p.exists()) {
        let tmp = bin_dir.join(".arugula.new");
        fs::copy(&cli, &tmp).with_context(|| format!("copying {}", cli.display()))?;
        crate::perm::set(&tmp, 0o755)?;
        fs::rename(&tmp, bin_dir.join("arugula"))?;
        println!("installed {}", bin_dir.join("arugula").display());
    } else {
        println!("note: no `arugula` CLI next to {}; build it with `cargo build -p arugula`", exe.display());
    }
    link_old_names(&bin_dir)?;
    Ok(dest)
}

/// `illogicald` and `illogical` in `bin_dir`, as links to `arugulad` and
/// `arugula` (#505): Claude Code hooks, scripts and older clients (`illogical
/// --ssh` from another machine) call them by those names. What illogical
/// installed there is a file; the link replaces it in one rename.
#[cfg(unix)]
fn link_old_names(bin_dir: &Path) -> anyhow::Result<()> {
    for (old, new) in [("illogicald", "arugulad"), ("illogical", "arugula")] {
        if !bin_dir.join(new).is_file() || fs::read_link(bin_dir.join(old)).is_ok_and(|t| t == Path::new(new)) {
            continue;
        }
        let tmp = bin_dir.join(format!(".{old}.link"));
        let _ = fs::remove_file(&tmp);
        std::os::unix::fs::symlink(new, &tmp)?;
        fs::rename(&tmp, bin_dir.join(old)).with_context(|| format!("linking {old} to {new}"))?;
    }
    Ok(())
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

    pub const LABEL: &str = "arugulad";
    /// illogical's label, for its agent and LaunchDaemon (#505).
    pub const OLD_LABEL: &str = "illogicald";

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

    /// illogical's LaunchDaemon for `user` (#505).
    pub fn old_system_plist(user: &str) -> PathBuf {
        PathBuf::from(format!("/Library/LaunchDaemons/{OLD_LABEL}.{user}.plist"))
    }

    /// What to do about illogical's launchd jobs (#505), so that only one
    /// daemon uses the state directory.
    #[derive(Debug, Default, PartialEq, Eq)]
    pub struct Retire {
        /// Jobs to stop (`launchctl bootout`) before the new one starts:
        /// the old daemon saves its panes and goes, and its shims keep the
        /// terminals for the new one (`holder::GRACE`).
        pub bootout: Vec<String>,
        /// The same, needing sudo (a LaunchDaemon).
        pub sudo_bootout: Vec<String>,
        /// Plists to remove, so they don't start it again.
        pub remove: Vec<PathBuf>,
        pub sudo_remove: Vec<PathBuf>,
    }

    /// Given which of illogical's plists are there, and whether its app's
    /// agent is loaded (`old_app`): without `start`, a
    /// running one is left to run (stopping it would end its panes with
    /// nothing to take them), and only its plist goes.
    pub fn retire(
        uid: u32,
        user: &str,
        old_agent: Option<PathBuf>,
        old_daemon: Option<PathBuf>,
        old_app: bool,
        start: bool,
    ) -> Retire {
        let mut r = Retire::default();
        if let Some(agent) = old_agent {
            if start {
                r.bootout = [format!("gui/{uid}"), format!("user/{uid}")]
                    .into_iter()
                    .map(|d| format!("{d}/{OLD_LABEL}"))
                    .collect();
            }
            r.remove.push(agent);
        }
        // illogical's app's own agent (`OLD_APP_LABEL`): it runs the daemon
        // from inside illogical.app, which install.sh replaces with
        // Arugula.app. Its plist is in that bundle, so there's none to remove.
        if old_app && start {
            r.bootout.push(format!("gui/{uid}/{}", arugula_proto::service::OLD_APP_LABEL));
        }
        if let Some(daemon) = old_daemon {
            if start {
                r.sudo_bootout.push(format!("system/{OLD_LABEL}.{user}"));
            }
            r.sudo_remove.push(daemon);
        }
        r
    }

    /// The `EnvironmentVariables` a plist sets, but for those `plist_text`
    /// writes itself; illogical's names made Arugula's (#505).
    pub fn plist_env(plist: &str) -> Vec<(String, String)> {
        let Some(at) = plist.find("<key>EnvironmentVariables</key>") else { return Vec::new() };
        let rest = &plist[at..];
        let Some(dict) = rest.find("<dict>").and_then(|a| Some(&rest[a + 6..a + rest[a..].find("</dict>")?])) else {
            return Vec::new();
        };
        let unxml = |s: &str| s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&");
        let mut out = Vec::new();
        for entry in dict.split("<key>").skip(1) {
            let Some((k, v)) = entry.split_once("</key>") else { continue };
            let Some(v) = v.split_once("<string>").and_then(|(_, v)| v.split_once("</string>")).map(|(v, _)| v) else {
                continue;
            };
            let k = super::renamed_env(&unxml(k.trim()));
            if !["ARUGULA_KEEP_PANES", "HOME", "USER", "LOGNAME"].contains(&k.as_str()) {
                out.push((k, unxml(v)));
            }
        }
        out
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

    pub fn plist_text(exe: &str, args: &[String], env: &[(String, String)], log: &str, mode: &Mode) -> String {
        let args: String = std::iter::once(exe)
            .chain(args.iter().map(String::as_str))
            .map(|a| format!("\n    <string>{}</string>", xml(a)))
            .collect();
        let extra: String =
            env.iter().map(|(k, v)| format!("\n    <key>{}</key>\n    <string>{}</string>", xml(k), xml(v))).collect();
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
    <key>ARUGULA_KEEP_PANES</key>
    <string>true</string>{env}{extra}
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

    /// The one-line warning a background install prints. `arugula
    /// --ssh` passes `note:` lines through.
    pub fn background_note(user: &str) -> String {
        format!(
            "note: {user} has no GUI login on this Mac (only ssh), so arugulad runs as a background agent: it keeps \
             running after you log out, but after a reboot it won't start until {user} logs in to the desktop or runs \
             `arugulad install` again. `arugulad install --system` starts it at boot instead (a LaunchDaemon; \
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
        let log = logs.join("arugulad.log");
        let agent = agents.join(format!("{LABEL}.plist"));
        let daemon = system_plist(&me.name);
        // illogical's, which these replace (#505).
        let old_agent = Some(agents.join(format!("{OLD_LABEL}.plist"))).filter(|p| p.is_file());
        let old_daemon = Some(old_system_plist(&me.name)).filter(|p| p.is_file());
        // A LaunchDaemon from an earlier `--system` stays one: going back
        // to an agent is `arugulad uninstall` first (both need sudo).
        let system = system || {
            let had = daemon.is_file() || old_daemon.is_some();
            if had {
                println!("keeping the LaunchDaemon from the last install (--system); `arugulad uninstall` removes it");
            }
            had
        };
        // The last install's plist: ours, else illogical's.
        let earlier = [Some(&daemon), Some(&agent), old_daemon.as_ref(), old_agent.as_ref()]
            .into_iter()
            .flatten()
            .find_map(|p| fs::read_to_string(p).ok());
        let args = super::args_to_install(daemon_args, reset, || plist_args(earlier.as_deref()?));
        let env = earlier.as_deref().map(plist_env).unwrap_or_default();
        let gui = format!("gui/{}", me.uid);
        let user = format!("user/{}", me.uid);
        // The desktop app's own launch agent runs the daemon (M46): the
        // copy just put in ~/.local/bin is what its bundled one hands on to
        // (#391), so restart that agent rather than start a second daemon.
        // Only the app's agent under its new label: illogical's app's is
        // replaced like illogical's own agent (#505).
        let app_agent = format!("{gui}/{}", arugula_proto::service::APP_LABEL);
        let old_app = has(&format!("{gui}/{}", arugula_proto::service::OLD_APP_LABEL));
        if !system && !agent.is_file() && old_agent.is_none() && has(&app_agent) {
            println!("the Arugula app's launch agent runs the daemon here; it runs {} from now on", exe.display());
            // Its plist is the app's, so the flags go where the daemon
            // reads them when that agent starts it (#550).
            let state = crate::default_state_dir();
            let file = super::app_args_file(&state);
            let args = super::args_to_install(daemon_args, reset, || super::read_app_args(&file));
            fs::create_dir_all(&state)?;
            crate::store::write_atomic(&file, &serde_json::to_vec(&args)?)?;
            if !args.is_empty() {
                println!("it runs with {} (kept in {})", args.join(" "), file.display());
            }
            if start {
                let out = Command::new("launchctl").args(["kickstart", "-k", &app_agent]).output()?;
                if !out.status.success() {
                    bail!("launchctl kickstart -k {app_agent}: {}", String::from_utf8_lossy(&out.stderr).trim());
                }
                println!("restarted {app_agent}");
                print!("{}", super::next_steps(&args, &log.display().to_string()));
            } else {
                println!(
                    "they apply when it restarts: the Daemon menu's Restart, or `launchctl kickstart -k {app_agent}`"
                );
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
        let text = plist_text(&exe.display().to_string(), &args, &env, &log.display().to_string(), &mode);
        let logs = log.display().to_string();
        let old = retire(me.uid.as_raw(), &me.name, old_agent, old_daemon, old_app, start);

        if let Mode::System { .. } = mode {
            println!(
                "--system: a LaunchDaemon, {}, runs arugulad as {} from boot, with nobody logged in. Writing and \
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
                for l in [LABEL, OLD_LABEL] {
                    bootout(false, &format!("{d}/{l}"));
                }
            }
            for a in [&agent].into_iter().chain(&old.remove) {
                if a.is_file() {
                    fs::remove_file(a)?;
                    println!("removed {} (the LaunchDaemon replaces it)", a.display());
                }
            }
            let service = format!("system/{}", mode.label());
            if start {
                for s in &old.bootout {
                    bootout(false, s);
                }
                for s in &old.sudo_bootout {
                    bootout(true, s);
                }
            }
            for p in &old.sudo_remove {
                sudo(&["rm", "-f", &p.display().to_string()])?;
            }
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
            // panes' shims keep them for the new one. illogical's too.
            for d in [&gui, &user] {
                bootout(false, &format!("{d}/{LABEL}"));
            }
            for s in &old.bootout {
                bootout(false, s);
            }
        }
        for p in &old.remove {
            fs::remove_file(p)?;
            println!("removed {} ({} replaces it)", p.display(), agent.display());
        }
        if start {
            if !bootstrap(false, domain, &agent)? {
                bail!(
                    "launchctl bootstrap {domain} {} failed; see {logs}, or run `{}` in a terminal to see why it stops",
                    agent.display(),
                    exe.display()
                );
            }
            println!("started {domain}/{LABEL}");
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

    /// Stop and remove whichever of the three is installed, under either
    /// name (#505).
    pub fn uninstall() -> anyhow::Result<()> {
        let me = me()?;
        let mut found = false;
        for label in [LABEL, OLD_LABEL] {
            for d in [format!("gui/{}", me.uid), format!("user/{}", me.uid)] {
                let service = format!("{d}/{label}");
                if has(&service) {
                    bootout(false, &service);
                    println!("stopped {service}");
                    found = true;
                }
            }
            let agent = me.home.join(format!("Library/LaunchAgents/{label}.plist"));
            if agent.is_file() {
                fs::remove_file(&agent)?;
                println!("removed {}", agent.display());
                found = true;
            }
        }
        for (label, daemon) in [(LABEL, system_plist(&me.name)), (OLD_LABEL, old_system_plist(&me.name))] {
            if daemon.is_file() {
                println!("removing the LaunchDaemon {}: that needs root, so this runs sudo:", daemon.display());
                bootout(true, &format!("system/{label}.{}", me.name));
                sudo(&["rm", "-f", &daemon.display().to_string()])?;
                found = true;
            }
        }
        if found {
            println!(
                "arugulad is no longer a service here; {} and the panes' state ({}) are kept",
                me.home.join(".local/bin").display(),
                crate::default_state_dir().display()
            );
        }
        // The app's own agent is the app's to remove (#550): SMAppService
        // registered it for the app's bundle, and booting it out here would
        // only last until the next login.
        // Loaded, or (with nothing else here) stopped from the app's Daemon
        // menu, to run again at the next login.
        let app_agent = arugula_proto::service::find().filter(|s| {
            s.kind == arugula_proto::service::Kind::AppAgent
                && s.target.ends_with(arugula_proto::service::APP_LABEL)
                && (s.loaded || !found)
        });
        if app_agent.is_some() {
            let note = app_agent_note();
            if found {
                println!("{note}");
            } else {
                bail!("{note}");
            }
        } else if !found {
            println!("arugulad isn't installed as a service here");
        }
        Ok(())
    }

    /// What to do about the app's launch agent, which `uninstall` leaves.
    pub fn app_agent_note() -> String {
        format!(
            "the Arugula app runs arugulad here, as its own launch agent ({}), which `arugulad uninstall` doesn't \
             remove. To stop it, use the Daemon menu (in the app's menu, or the menu bar icon) > Stop; it starts \
             again at your next login. To keep it from starting at login, turn Arugula off under System Settings > \
             General > Login Items > Allow in the Background (opening the app sets a daemon up again), or move \
             Arugula.app to the Trash.",
            arugula_proto::service::APP_LABEL
        )
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    #[test]
    fn plist_carries_daemon_args() {
        let t = super::launchd::plist_text(
            "/Users/me/.local/bin/arugulad",
            &["--listen".into(), "127.0.0.1:9000".into(), "a<b".into()],
            &[],
            "/Users/me/Library/Logs/arugulad.log",
            &super::launchd::Mode::Gui,
        );
        assert!(t.contains(
            "<string>/Users/me/.local/bin/arugulad</string>\n    <string>--listen</string>\n    <string>127.0.0.1:9000</string>\n    <string>a&lt;b</string>\n  </array>"
        ));
        assert!(t.contains("<key>RunAtLoad</key>"));
        assert!(t.contains("<key>ARUGULA_KEEP_PANES</key>\n    <string>true</string>"));
        assert_eq!(super::launchd::plist_args(&t).unwrap(), ["--listen", "127.0.0.1:9000", "a<b"]);
        assert!(!t.contains("LimitLoadToSessionType") && !t.contains("UserName"));
        let bare = super::launchd::plist_text("/x/arugulad", &[], &[], "/x/log", &super::launchd::Mode::Gui);
        assert_eq!(super::launchd::plist_args(&bare).unwrap(), Vec::<String>::new());
    }

    #[cfg(unix)]
    #[test]
    fn plist_modes() {
        use super::launchd::{Mode, plist_args, plist_text};
        let args = ["--listen".to_string(), "127.0.0.1:9000".to_string()];
        let bg = plist_text("/x/arugulad", &args, &[], "/x/log", &Mode::Background);
        assert!(bg.contains("<key>Label</key>\n  <string>arugulad</string>"));
        assert!(bg.contains("<key>LimitLoadToSessionType</key>\n  <string>Background</string>"));
        assert!(!bg.contains("UserName"));
        assert_eq!(plist_args(&bg).unwrap(), args);

        let sys = Mode::System { user: "illo".into(), home: "/Users/illo".into() };
        assert_eq!(super::launchd::system_plist("illo").to_str(), Some("/Library/LaunchDaemons/arugulad.illo.plist"));
        let t = plist_text("/x/arugulad", &args, &[], "/x/log", &sys);
        assert!(t.contains("<key>Label</key>\n  <string>arugulad.illo</string>"));
        assert!(t.contains("<key>UserName</key>\n  <string>illo</string>"));
        assert!(t.contains("<key>HOME</key>\n    <string>/Users/illo</string>"));
        assert!(t.contains("<key>ARUGULA_KEEP_PANES</key>\n    <string>true</string>"));
        assert!(!t.contains("LimitLoadToSessionType"));
        assert_eq!(plist_args(&t).unwrap(), args);
    }

    #[cfg(unix)]
    #[test]
    fn background_note_says_reboot_and_system() {
        let n = super::launchd::background_note("illo");
        assert!(n.starts_with("note: illo has no GUI login"));
        assert!(n.contains("after a reboot it won't start until illo logs in"));
        assert!(n.contains("`arugulad install --system`") && n.contains("sudo"));
        assert!(!n.contains('\n'), "one line, so `arugula --ssh` can pass it through");
    }

    #[test]
    fn unit_carries_daemon_args() {
        let t = super::unit_text(&["--listen".into(), "127.0.0.1:9000".into()], &[]);
        assert!(t.contains("ExecStart=%h/.local/bin/arugulad --listen 127.0.0.1:9000\n"));
        assert!(t.contains("KillMode=mixed"));
        assert!(t.contains("Type=notify"));
        assert!(t.contains("FileDescriptorStoreMax="));
        assert_eq!(super::unit_args(&t).unwrap(), ["--listen", "127.0.0.1:9000"]);
        assert_eq!(super::unit_args(&super::unit_text(&[], &[])).unwrap(), Vec::<String>::new());
    }

    #[test]
    fn unit_reads_illogicals_args_and_env() {
        // #505: what illogical's install wrote, and an Environment= someone added.
        let old = "[Service]\nType=notify\nEnvironment=ILLOGICAL_LOG=debug \"ILLOGICAL_X=1\" KEEP=MY_ILLOGICAL_Y\nExecStart=%h/.local/bin/illogicald --listen 0.0.0.0:7681\n";
        assert_eq!(super::unit_args(old).unwrap(), ["--listen", "0.0.0.0:7681"]);
        let env = super::unit_env(old);
        assert_eq!(env, ["Environment=ARUGULA_LOG=debug \"ARUGULA_X=1\" KEEP=MY_ILLOGICAL_Y"]);
        let t = super::unit_text(&["--listen".into(), "0.0.0.0:7681".into()], &env);
        assert!(t.contains("NotifyAccess=main\nEnvironment=ARUGULA_LOG=debug \"ARUGULA_X=1\" KEEP=MY_ILLOGICAL_Y\nExecStart=%h/.local/bin/arugulad --listen 0.0.0.0:7681\n"));
        assert_eq!(super::unit_env(&t), env);
    }

    #[test]
    fn unit_plan_swaps_illogicals_unit_for_ours() {
        use super::UnitStep::*;
        // A fresh machine, or one already on arugulad.service.
        assert_eq!(super::unit_plan(false, false, false, true), [Write, Reload, Enable, Restart]);
        assert_eq!(super::unit_plan(false, true, false, false), [Write, Reload, Enable]);
        // #505: illogicald.service becomes an alias first, so the running one
        // is restarted as arugulad.service with its FD store; then it goes.
        assert_eq!(
            super::unit_plan(true, false, false, true),
            [Write, AliasOld, UnwantOld, Reload, Enable, Restart, DropAlias]
        );
        assert_eq!(
            super::unit_plan(true, true, false, true),
            [Write, MoveDropIns, AliasOld, UnwantOld, Reload, Enable, Restart, DropAlias]
        );
        // Drop-ins of its own already: illogical's stay where they are.
        assert_eq!(super::unit_plan(true, true, true, false), [Write, AliasOld, UnwantOld, Reload, Enable, DropAlias]);
    }

    #[cfg(unix)]
    #[test]
    fn old_binary_names_become_links() {
        let bin = std::env::temp_dir().join(format!("arugula-install-links-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&bin);
        std::fs::create_dir_all(&bin).unwrap();
        for f in ["arugulad", "arugula", "illogicald", "illogical"] {
            std::fs::write(bin.join(f), f).unwrap();
        }
        super::link_old_names(&bin).unwrap();
        assert_eq!(std::fs::read_link(bin.join("illogicald")).unwrap(), std::path::Path::new("arugulad"));
        assert_eq!(std::fs::read_link(bin.join("illogical")).unwrap(), std::path::Path::new("arugula"));
        assert_eq!(std::fs::read_to_string(bin.join("illogical")).unwrap(), "arugula");
        // Again: nothing to do, nothing left behind.
        super::link_old_names(&bin).unwrap();
        let mut names: Vec<_> = std::fs::read_dir(&bin).unwrap().map(|e| e.unwrap().file_name()).collect();
        names.sort();
        assert_eq!(names, ["arugula", "arugulad", "illogical", "illogicald"]);
        // No CLI installed: no link to it.
        std::fs::remove_file(bin.join("arugula")).unwrap();
        std::fs::remove_file(bin.join("illogical")).unwrap();
        super::link_old_names(&bin).unwrap();
        assert!(std::fs::symlink_metadata(bin.join("illogical")).is_err());
        std::fs::remove_dir_all(&bin).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn launchd_retires_illogicals_jobs() {
        use super::launchd::{Retire, retire};
        let agent = std::path::PathBuf::from("/Users/illo/Library/LaunchAgents/illogicald.plist");
        let daemon = std::path::PathBuf::from("/Library/LaunchDaemons/illogicald.illo.plist");
        assert_eq!(retire(501, "illo", None, None, false, true), Retire::default());
        assert_eq!(
            retire(501, "illo", Some(agent.clone()), None, false, true),
            Retire {
                bootout: vec!["gui/501/illogicald".into(), "user/501/illogicald".into()],
                remove: vec![agent.clone()],
                ..Default::default()
            }
        );
        // illogical's app's agent, running the daemon from illogical.app.
        assert_eq!(
            retire(501, "illo", None, None, true, true),
            Retire { bootout: vec!["gui/501/wtf.widgets.illogical.daemon".into()], ..Default::default() }
        );
        // Not starting the new one: the old one runs on; only its plist goes.
        assert_eq!(
            retire(501, "illo", Some(agent.clone()), None, true, false),
            Retire { remove: vec![agent], ..Default::default() }
        );
        assert_eq!(
            retire(501, "illo", None, Some(daemon.clone()), false, true),
            Retire {
                sudo_bootout: vec!["system/illogicald.illo".into()],
                sudo_remove: vec![daemon],
                ..Default::default()
            }
        );
    }

    #[cfg(unix)]
    #[test]
    fn plists_carry_illogicals_env() {
        use super::launchd::{Mode, plist_env, plist_text};
        let old = "<key>ProgramArguments</key>\n<array>\n<string>/x/illogicald</string>\n<string>--headless</string>\n</array>\n\
                   <key>EnvironmentVariables</key>\n  <dict>\n    <key>ILLOGICAL_KEEP_PANES</key>\n    <string>true</string>\n    \
                   <key>HOME</key>\n    <string>/Users/illo</string>\n    <key>ILLOGICAL_LOG</key>\n    <string>a&amp;b</string>\n  </dict>";
        assert_eq!(super::launchd::plist_args(old).unwrap(), ["--headless"]);
        let env = plist_env(old);
        assert_eq!(env, [("ARUGULA_LOG".to_string(), "a&b".to_string())]);
        let t = plist_text("/x/arugulad", &[], &env, "/x/log", &Mode::Gui);
        assert!(t.contains("<key>ARUGULA_KEEP_PANES</key>\n    <string>true</string>\n    <key>ARUGULA_LOG</key>\n    <string>a&amp;b</string>\n  </dict>"));
        assert_eq!(plist_env(&t), env);
    }

    #[test]
    fn next_steps_name_the_listen_address() {
        assert!(
            super::next_steps(&[], "logs")
                .starts_with("Open http://127.0.0.1:7681 with `arugula web` (it signs your browser in)\nLogs: logs\n")
        );
        assert!(
            super::next_steps(&["--listen".into(), "127.0.0.1:9000".into()], "l")
                .starts_with("Open http://127.0.0.1:9000 with")
        );
        assert!(super::next_steps(&["--listen=0.0.0.0:1".into()], "l").starts_with("Open http://0.0.0.0:1 with"));
    }

    /// #550: `arugulad install -- FLAGS` with the Mac app's agent running
    /// the daemon keeps them in the state dir, and the daemon that agent
    /// starts with none reads them back.
    #[cfg(unix)]
    #[test]
    fn the_app_agents_daemon_runs_with_the_installed_flags() {
        use super::{app_agent_argv, app_args_file, args_to_install, read_app_args};
        let state = std::env::temp_dir().join(format!("arugula-app-args-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
        std::fs::create_dir_all(&state).unwrap();
        let file = app_args_file(&state);
        let label = Some(arugula_proto::service::APP_LABEL);
        let bare = || vec!["/Applications/Arugula.app/Contents/MacOS/arugulad".to_string()];
        assert_eq!(app_agent_argv(bare(), label, &state), bare());

        let given = ["--block-listen".to_string(), "1.2.3.4:7443".to_string()];
        let args = args_to_install(&given, false, || read_app_args(&file));
        crate::store::write_atomic(&file, &serde_json::to_vec(&args).unwrap()).unwrap();
        let mut want = bare();
        want.extend(given.iter().cloned());
        assert_eq!(app_agent_argv(bare(), label, &state), want);
        // Installing again keeps them; --reset-args drops them.
        assert_eq!(args_to_install(&[], false, || read_app_args(&file)), given);
        assert!(args_to_install(&[], true, || read_app_args(&file)).is_empty());
        // Only for the app's agent, and only when it gave none.
        assert_eq!(app_agent_argv(bare(), Some("arugulad"), &state), bare());
        assert_eq!(app_agent_argv(bare(), None, &state), bare());
        let mut with = bare();
        with.push("--headless".into());
        assert_eq!(app_agent_argv(with.clone(), label, &state), with);
        let _ = std::fs::remove_dir_all(&state);
    }

    #[cfg(unix)]
    #[test]
    fn uninstall_says_how_to_stop_the_apps_agent() {
        let note = super::launchd::app_agent_note();
        assert!(note.contains(arugula_proto::service::APP_LABEL));
        assert!(note.contains("Daemon menu"));
        assert!(note.contains("System Settings > General > Login Items"));
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
