//! How panes run their shell: the dialects of the shells a pane can start,
//! the environment it gets, and how a restored, run-only or guest pane is built.

use super::exec_tag;
use crate::{
    pane::{self, Spawn, Start},
    provider::Provider,
    shellint::Integration,
    store::PaneMeta,
    sys,
};
use illogical_proto::{PaneId, Policy};
use std::{path::PathBuf, sync::Arc};
use tracing::info;

/// How panes run their shell.
#[derive(Clone, Debug)]
pub struct Config {
    /// Who else may reach which sessions (M12).
    pub acl: Arc<crate::acl::Acl>,
    /// Illogical control: notifications through it go to people's
    /// devices (M21).
    pub control: Arc<crate::control::Control>,
    /// A hosted sandbox (M20): when its last session closes, it's done.
    pub sandbox_of_control: bool,
    /// What to call the owner to others (M13): their login, else "owner".
    pub owner_name: String,
    /// The owner's picture, if the tailnet gave one.
    pub owner_pic: Option<String>,
    /// How many VMs each guest may have at once (M14).
    pub guest_machines: usize,
    pub shell: String,
    /// Arguments for an interactive shell, e.g. `["-l"]`.
    pub shell_args: Vec<String>,
    pub home: PathBuf,
    /// Merge the systemd user manager's environment into new panes.
    pub manager_env: bool,
    pub launch: pane::Launcher,
    pub integration: Option<Integration>,
    /// The CLI's socket, for `ILLOGICAL_SOCK` in panes.
    pub socket: PathBuf,
    /// Where VM panes get their machines; `None` if not set up.
    pub provider: Option<Arc<dyn Provider>>,
    /// Names this daemon's sprites, so a crash sweep only touches ours.
    pub daemon_id: String,
    /// Where agents in VMs get their credentials from.
    pub secrets: crate::block::Secrets,
    /// Secrets the `fs` methods never serve (the provider's token, agents'
    /// credentials); the state directory is added to these.
    pub private: Vec<PathBuf>,
    /// Where agent blocks reach MCP (M16); `None`: they don't.
    pub mcp: Option<crate::mcp::Link>,
    /// What runs an invite the owner sent from an agent's card (#234).
    pub invite: crate::invite::Hook,
    /// illogicald as Claude Code's IDE (M28); `None`: off.
    pub ide: Option<Arc<crate::ide::Ide>>,
}

/// How a shell takes a command to run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dialect {
    /// `-c SCRIPT`, `"$@"`, `exec` (bash, zsh, sh, ksh…).
    Posix,
    Fish,
    /// pwsh and Windows PowerShell: `-Command`, `-NoExit`.
    PowerShell,
    /// cmd: `/c`, `/k`.
    Cmd,
}

impl Dialect {
    pub(super) fn of(shell: &str) -> Self {
        // The last part of the path either way it's written, less `.exe`.
        let name = shell.rsplit(['/', '\\']).next().unwrap_or(shell).to_lowercase();
        match name.strip_suffix(".exe").unwrap_or(&name) {
            "fish" => Self::Fish,
            "pwsh" | "powershell" => Self::PowerShell,
            "cmd" => Self::Cmd,
            _ => Self::Posix,
        }
    }
}

impl Config {
    pub(super) fn env(&self, pane: PaneId) -> Vec<(String, String)> {
        let mut env = if self.manager_env { sys::manager_env() } else { vec![] };
        // The CLI is installed next to the daemon; panes find it on PATH and
        // know which pane (and which daemon) they are.
        let base = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        if let Some(bin) = self.launch.exe.parent() {
            // `:` on Unix, `;` on Windows.
            let mut dirs: Vec<PathBuf> = std::env::split_paths(&base).collect();
            if !dirs.iter().any(|d| d == bin) {
                dirs.insert(0, bin.to_owned());
            }
            let path = std::env::join_paths(dirs).map(|p| p.to_string_lossy().into_owned()).unwrap_or(base);
            env.retain(|(k, _)| k != "PATH");
            env.push(("PATH".into(), path));
        }
        env.push(("ILLOGICAL_PANE".into(), pane.to_string()));
        env.push(("ILLOGICAL_SOCK".into(), self.socket.display().to_string()));
        // A box reached over ssh (M51) has no agent of its own: its panes
        // use the one at a fixed path beside the socket, which `illogical
        // bridge` points at the owner's forwarded agent while they're
        // connected. An agent this machine has (a desktop's) is kept.
        #[cfg(unix)]
        let live = |p: &str| std::os::unix::net::UnixStream::connect(p).is_ok();
        // Windows' ssh agent is a named pipe: panes keep the one they're given.
        #[cfg(not(unix))]
        let live = |_: &str| true;
        let has_agent = !cfg!(unix)
            || env
                .iter()
                .find(|(k, _)| k == "SSH_AUTH_SOCK")
                .map(|(_, v)| v.clone())
                .or_else(|| std::env::var("SSH_AUTH_SOCK").ok())
                .is_some_and(|p| live(&p));
        if !has_agent {
            env.retain(|(k, _)| k != "SSH_AUTH_SOCK");
            env.push(("SSH_AUTH_SOCK".into(), self.socket.with_file_name("agent.sock").display().to_string()));
        }
        // Claude Code in a pane finds us as its IDE (M28), and only us.
        if let Some(ide) = &self.ide {
            env.retain(|(k, _)| k != "CLAUDE_CODE_SSE_PORT");
            env.push(("CLAUDE_CODE_SSE_PORT".into(), ide.port.to_string()));
        }
        env
    }

    /// An interactive shell, with integration unless it's off for the pane.
    pub(super) fn shell(&self, pane: PaneId, cwd: PathBuf, integrate: bool) -> Spawn {
        let mut s = Spawn { program: self.shell.clone(), args: self.shell_args.clone(), cwd, env: self.env(pane) };
        if integrate && let Some(i) = &self.integration {
            i.apply(&mut s);
        }
        s
    }

    /// Run `command`, then carry on with an interactive shell in the pane.
    fn run_then_shell(&self, pane: PaneId, cwd: PathBuf, command: &str, integrate: bool) -> Spawn {
        let shell = self.shell(pane, cwd, integrate);
        match Dialect::of(&self.shell) {
            Dialect::PowerShell => {
                let mut shell = shell;
                crate::shellint::powershell_then(&mut shell, command);
                return shell;
            }
            Dialect::Cmd => {
                let mut args = shell.args.clone();
                args.extend(["/k".into(), command.into()]);
                return Spawn { args, ..shell };
            }
            Dialect::Posix | Dialect::Fish => {}
        }
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let mut args: Vec<String> = self.shell_args.iter().filter(|a| *a != "--posix").cloned().collect();
        args.extend(["-c".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))]);
        Spawn { args, ..shell }
    }

    /// Run `argv` (no shell text: each word its own argument, so nothing in
    /// it is read by the shell), then carry on with an interactive shell
    /// (#146). With `note`, print it first and run nothing else.
    fn argv_then_shell(&self, pane: PaneId, cwd: PathBuf, run: Run, integrate: bool) -> Spawn {
        let shell = self.shell(pane, cwd, integrate);
        match Dialect::of(&self.shell) {
            Dialect::PowerShell => {
                // `& 'program' 'arg'…`: each word quoted, nothing in it read.
                let quoted = |w: &str| format!("'{}'", w.replace('\'', "''"));
                let script = match &run {
                    Run::Argv(argv) => format!("& {}", argv.iter().map(|w| quoted(w)).collect::<Vec<_>>().join(" ")),
                    Run::Note(note) => format!("Write-Host -ForegroundColor DarkGray ('[' + {} + ']')", quoted(note)),
                };
                let mut shell = shell;
                crate::shellint::powershell_then(&mut shell, &script);
                return shell;
            }
            Dialect::Cmd => {
                let script = match &run {
                    Run::Argv(argv) => crate::conpty_command_line(argv),
                    Run::Note(note) => format!("echo [{note}]"),
                };
                let mut args = shell.args.clone();
                args.extend(["/k".into(), script]);
                return Spawn { args, ..shell };
            }
            Dialect::Posix | Dialect::Fish => {}
        }
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let then = then.collect::<Vec<_>>().join(" ");
        let fish = std::path::Path::new(&self.shell).file_name().is_some_and(|n| n == "fish");
        // With job control the program is the terminal's foreground group,
        // as if typed: without it the shell is, and the pane reads as
        // running no agent (#376).
        let (script, words) = match (run, fish) {
            (Run::Argv(argv), false) => (format!("set -m; \"$@\"; exec {then}"), argv),
            (Run::Argv(argv), true) => (format!("status job-control full; $argv; exec {then}"), argv),
            (Run::Note(note), false) => (format!("printf '\\033[2m[%s]\\033[0m\\n' \"$1\"; exec {then}"), vec![note]),
            (Run::Note(note), true) => (format!("printf '\\033[2m[%s]\\033[0m\\n' $argv[1]; exec {then}"), vec![note]),
        };
        let mut args: Vec<String> = self.shell_args.iter().filter(|a| *a != "--posix").cloned().collect();
        args.extend(["-c".into(), script]);
        // `sh -c SCRIPT NAME ARGS…`: NAME is $0. fish has no $0.
        if !fish {
            args.push("illogical".into());
        }
        args.extend(words);
        Spawn { args, ..shell }
    }

    /// Run `command` by itself (`illogical run`): the pane holds when it
    /// ends, so its output and exit code can still be read.
    pub(super) fn run_only(&self, pane: PaneId, cwd: PathBuf, command: &str) -> Spawn {
        let mut args = self.shell_args.clone();
        match Dialect::of(&self.shell) {
            // Its exit code is the program's (`$LASTEXITCODE`), or 1 when a
            // cmdlet failed, as `sh -c` gives the last command's.
            Dialect::PowerShell => args.extend([
                "-Command".into(),
                format!(
                    "{command}\n$illogicalOk = $?; if ($LASTEXITCODE) {{ exit $LASTEXITCODE }}; if (-not $illogicalOk) {{ exit 1 }}"
                ),
            ]),
            Dialect::Cmd => args.extend(["/d".into(), "/c".into(), command.into()]),
            Dialect::Posix | Dialect::Fish => args.extend(["-c".into(), command.into()]),
        }
        Spawn { program: self.shell.clone(), args, cwd, env: self.env(pane) }
    }

    /// What a restored pane does, by its policy.
    pub(super) fn restore(&self, pane: PaneId, meta: &PaneMeta) -> Start {
        let cwd = meta.cwd.as_ref().map(PathBuf::from).unwrap_or_else(|| self.home.clone());
        let on = meta.integration.unwrap_or(true);
        let shell = match meta.host {
            Some(_) => self.guest_shell(pane, on, None),
            None => self.shell(pane, cwd.clone(), on),
        };
        let then = |command: &str| match meta.host {
            Some(_) => self.guest_run_then_shell(pane, command, on),
            None => self.run_then_shell(pane, cwd.clone(), command, on),
        };
        let note = |s: &str| format!("\x1b[2m[{s}]\x1b[0m\r\n");
        // The agent conversation it ran, by its session id (#146).
        if meta.host.is_none() {
            let transcript = |id: &str| {
                let mut ix = crate::conversations::Index::new(crate::conversations::Dirs::from_env());
                ix.find(id).ok().map(|c| c.path.display().to_string())
            };
            let cwd = crate::resume::dir(meta).map(PathBuf::from).unwrap_or_else(|| cwd.clone());
            match crate::resume::plan(meta, transcript) {
                Some(Ok(argv)) => {
                    info!(pane, ?argv, "resuming its agent's conversation");
                    return Start::Now(self.argv_then_shell(pane, cwd, Run::Argv(argv), on));
                }
                Some(Err(why)) => {
                    info!(pane, why, "not resuming its agent's conversation");
                    return Start::Now(self.argv_then_shell(pane, cwd, Run::Note(why), on));
                }
                None => {}
            }
        }
        match (&meta.policy, &meta.command) {
            (Policy::None, _) => {
                Start::Wait { banner: note("press Enter for a shell"), enter: shell, text: None, escape: None }
            }
            // Recorded as the pane's command, so the next restart runs it
            // again too.
            (Policy::Rerun { confirm: true }, Some(cmd)) => Start::Wait {
                banner: note(&format!("press Enter to re-run: {cmd}  ·  Esc for a shell")),
                enter: then(cmd),
                text: Some(cmd.clone()),
                escape: Some(shell),
            },
            (Policy::Rerun { confirm: false }, Some(cmd)) => Start::Rerun { spawn: then(cmd), text: cmd.clone() },
            (Policy::Hook { command }, _) => Start::Now(then(command)),
            (Policy::Shell | Policy::Rerun { .. } | Policy::Resume, _) => Start::Now(shell),
        }
    }

    /// The environment of a pane's program on a machine: what the terminal
    /// is, and a tag `process` finds its shell by (machines are shared, so
    /// it names the pane). None of this host's.
    fn guest_env(&self, pane: PaneId) -> Vec<(String, String)> {
        vec![
            ("TERM".into(), "xterm-256color".into()),
            ("COLORTERM".into(), "truecolor".into()),
            ("ILLOGICAL_EXEC".into(), exec_tag(&self.daemon_id, pane)),
        ]
    }

    /// The sprite names this daemon's machines get.
    pub(super) fn sprite_prefix(&self) -> String {
        format!("illogical-eph-{}-", self.daemon_id)
    }

    /// A login shell on a machine, in its home directory unless given one
    /// of its own directories.
    pub(super) fn guest_shell(&self, pane: PaneId, integrate: bool, cwd: Option<PathBuf>) -> Spawn {
        let (program, args, env) = ("bash".into(), vec!["-l".into()], self.guest_env(pane));
        let mut s = Spawn { program, args, cwd: cwd.unwrap_or_default(), env };
        if integrate && self.integration.is_some() {
            crate::shellint::apply_guest(&mut s);
        }
        s
    }

    pub(super) fn guest_run(&self, pane: PaneId, cwd: Option<PathBuf>, command: &str) -> Spawn {
        Spawn {
            program: "bash".into(),
            args: vec!["-lc".into(), command.into()],
            cwd: cwd.unwrap_or_default(),
            env: self.guest_env(pane),
        }
    }

    fn guest_run_then_shell(&self, pane: PaneId, command: &str, integrate: bool) -> Spawn {
        let shell = self.guest_shell(pane, integrate, None);
        let then = std::iter::once(shell.program.as_str()).chain(shell.args.iter().map(String::as_str));
        let args = vec!["-lc".into(), format!("{command}; exec {}", then.collect::<Vec<_>>().join(" "))];
        Spawn { args, ..shell }
    }
}

/// What [`Config::argv_then_shell`] runs before the shell.
pub(super) enum Run {
    Argv(Vec<String>),
    Note(String),
}

/// Where `run --split %N --join` puts the new pane.
pub(super) enum Join {
    /// This host.
    Here,
    /// The split pane's tab's machine.
    TabMachine,
    /// A sandbox the split pane has a shell on: borrowed again.
    Borrow(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shells_by_dialect() {
        assert_eq!(Dialect::of("/bin/bash"), Dialect::Posix);
        assert_eq!(Dialect::of("fish"), Dialect::Fish);
        assert_eq!(Dialect::of("pwsh"), Dialect::PowerShell);
        assert_eq!(Dialect::of(r"C:\Program Files\PowerShell\7\pwsh.exe"), Dialect::PowerShell);
        assert_eq!(Dialect::of("powershell.exe"), Dialect::PowerShell);
        assert_eq!(Dialect::of(r"C:\WINDOWS\system32\cmd.exe"), Dialect::Cmd);
    }
}
