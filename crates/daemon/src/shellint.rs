//! Shell integration: the scripts that make bash, zsh and fish report
//! prompts, commands, exit codes and the working directory, and how a pane's
//! shell is started so it loads them without touching the user's dotfiles.
//! The scheme is Ghostty's:
//!
//! - **bash** starts with `--posix` and `ENV` pointing at our script; the
//!   script turns POSIX mode off and reads the startup files bash would have
//!   read (the login profile, or `.bashrc`).
//! - **zsh** gets `ZDOTDIR` pointing at our `.zshenv`, which restores the
//!   user's `ZDOTDIR` and reads their `.zshenv`.
//! - **fish** gets our directory prepended to `XDG_DATA_DIRS`, where it loads
//!   `fish/vendor_conf.d/arugula.fish`.
//! - **PowerShell** (pwsh, Windows PowerShell) gets `-NoExit -EncodedCommand`
//!   and our script inline: Windows' default execution policy runs no script
//!   file, but doesn't cover an inline command (S29).
//!
//! On a machine (a VM pane) our files aren't there, so bash gets the script
//! in an environment variable, and `ENV` (which bash expands, command
//! substitution included) writes it to a temporary file and names that.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::pane::Spawn;

const BASH: &str = include_str!("../shell/bash/arugula.bash");
const ZSH: &str = include_str!("../shell/zsh/.zshenv");
const FISH: &str = include_str!("../shell/fish/vendor_conf.d/arugula.fish");
const PWSH: &str = include_str!("../shell/pwsh/arugula.ps1");

#[derive(Debug, Clone)]
pub struct Integration {
    dir: PathBuf,
}

impl Integration {
    /// Write the scripts under `dir` (refreshed on every start, so they
    /// match this binary).
    pub fn install(dir: PathBuf) -> io::Result<Self> {
        for (path, text) in
            [("bash/arugula.bash", BASH), ("zsh/.zshenv", ZSH), ("fish/vendor_conf.d/arugula.fish", FISH)]
        {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(&p, text)?;
            crate::perm::set(&p, 0o644)?;
        }
        // An older daemon's fish script, if this is its directory too:
        // fish loads every file there, and both would report (#505).
        let _ = fs::remove_file(dir.join("fish/vendor_conf.d/illogical.fish"));
        Ok(Self { dir })
    }

    /// Make an interactive shell load the integration. Commands (`-c`) and
    /// shells we don't know are left alone.
    pub fn apply(&self, spawn: &mut Spawn) {
        if spawn.args.iter().any(|a| a == "-c") {
            return;
        }
        if powershell(&spawn.program) {
            // Anything it was told to run already (a command, a file).
            let runs = |a: &String| {
                let a = a.to_lowercase();
                ["-command", "-encodedcommand", "-file", "-ec", "-f"].contains(&a.as_str())
            };
            if !spawn.args.iter().any(runs) {
                spawn.args.extend(["-NoExit".into(), "-EncodedCommand".into(), encoded(PWSH)]);
            }
            return;
        }
        match shell_name(&spawn.program) {
            "bash" => {
                let mut args = Vec::new();
                for a in std::mem::take(&mut spawn.args) {
                    match a.as_str() {
                        "-l" | "--login" => spawn.env.push(("ARUGULA_BASH_LOGIN".into(), "1".into())),
                        "--norc" => spawn.env.push(("ARUGULA_BASH_NORC".into(), "1".into())),
                        "--noprofile" => spawn.env.push(("ARUGULA_BASH_NOPROFILE".into(), "1".into())),
                        _ => args.push(a),
                    }
                }
                args.insert(0, "--posix".into());
                spawn.args = args;
                spawn.env.push(("ENV".into(), self.dir.join("bash/arugula.bash").display().to_string()));
                spawn.env.push(("ARUGULA_BASH_INJECT".into(), "1".into()));
            }
            "zsh" => {
                if let Ok(z) = std::env::var("ZDOTDIR") {
                    spawn.env.push(("ARUGULA_ZDOTDIR".into(), z));
                }
                spawn.env.push(("ZDOTDIR".into(), self.dir.join("zsh").display().to_string()));
            }
            "fish" => {
                let rest = std::env::var("XDG_DATA_DIRS").unwrap_or_else(|_| "/usr/local/share:/usr/share".into());
                spawn.env.push(("XDG_DATA_DIRS".into(), format!("{}:{rest}", self.dir.display())));
            }
            _ => {}
        }
    }
}

/// Integration for a login bash on a machine, which has none of our files.
pub fn apply_guest(spawn: &mut Spawn) {
    if shell_name(&spawn.program) != "bash" || spawn.args.iter().any(|a| a == "-c") {
        return;
    }
    spawn.args.retain(|a| a != "-l" && a != "--login");
    spawn.args.insert(0, "--posix".into());
    spawn.env.extend([
        ("ARUGULA_BASH_LOGIN".into(), "1".into()),
        ("ARUGULA_BASH_INJECT".into(), "1".into()),
        ("ARUGULA_BASH_SCRIPT".into(), BASH.into()),
        (
            "ENV".into(),
            r#"$(f=$(mktemp "${TMPDIR:-/tmp}/arugula.XXXXXX") && printf %s "$ARUGULA_BASH_SCRIPT" >"$f" && echo "$f")"#
                .into(),
        ),
    ]);
}

/// Have an interactive PowerShell run `script` first, after the integration
/// if it has ours.
pub fn powershell_then(spawn: &mut Spawn, script: &str) {
    let ours = spawn.args.iter().position(|a| a == "-EncodedCommand").filter(|i| *i + 1 < spawn.args.len());
    match ours {
        Some(i) => spawn.args[i + 1] = encoded(&format!("{PWSH}\n{script}")),
        None => spawn.args.extend(["-NoExit".into(), "-Command".into(), script.into()]),
    }
}

/// pwsh or Windows PowerShell, by the program's name.
fn powershell(program: &str) -> bool {
    let name = program.rsplit(['/', '\\']).next().unwrap_or(program).to_lowercase();
    matches!(name.strip_suffix(".exe").unwrap_or(&name), "pwsh" | "powershell")
}

/// A script as `-EncodedCommand` takes it: base64 of its UTF-16LE.
fn encoded(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn shell_name(program: &str) -> &str {
    Path::new(program).file_name().and_then(|n| n.to_str()).unwrap_or(program)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spawn(program: &str, args: &[&str]) -> Spawn {
        Spawn {
            program: program.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            cwd: "/".into(),
            env: vec![],
        }
    }

    // Unix: bash's integration (PowerShell's is M60, #223).
    #[cfg(unix)]
    #[test]
    fn bash_login_becomes_posix_with_env() {
        let i = Integration { dir: "/x".into() };
        let mut s = spawn("/bin/bash", &["-l"]);
        i.apply(&mut s);
        assert_eq!(s.args, vec!["--posix"]);
        assert!(s.env.contains(&("ENV".into(), "/x/bash/arugula.bash".into())));
        assert!(s.env.contains(&("ARUGULA_BASH_LOGIN".into(), "1".into())));

        let mut s = spawn("bash", &["--norc", "--noprofile"]);
        i.apply(&mut s);
        assert_eq!(s.args, vec!["--posix"]);
        assert!(s.env.contains(&("ARUGULA_BASH_NORC".into(), "1".into())));
    }

    /// #505: fish loads every script in the directory, so an older
    /// daemon's goes.
    #[test]
    fn an_older_daemons_fish_script_goes() {
        let dir = std::env::temp_dir().join(format!("arugula-shellint-{}", std::process::id()));
        let old = dir.join("fish/vendor_conf.d/illogical.fish");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        std::fs::write(&old, "").unwrap();
        Integration::install(dir.clone()).unwrap();
        assert!(!old.exists());
        assert!(dir.join("fish/vendor_conf.d/arugula.fish").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn commands_and_unknown_shells_are_untouched() {
        let i = Integration { dir: "/x".into() };
        let mut s = spawn("bash", &["-l", "-c", "make"]);
        i.apply(&mut s);
        assert_eq!(s.args, vec!["-l", "-c", "make"]);
        let mut s = spawn("nu", &[]);
        i.apply(&mut s);
        assert!(s.env.is_empty());
        let mut s = spawn("/usr/bin/fish", &[]);
        i.apply(&mut s);
        assert!(s.env[0].1.starts_with("/x:"));
        let mut s = spawn("pwsh", &["-NoLogo", "-Command", "Get-Date"]);
        i.apply(&mut s);
        assert_eq!(s.args, vec!["-NoLogo", "-Command", "Get-Date"]);
    }

    fn decoded(b64: &str) -> String {
        use base64::Engine;
        let bytes = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        String::from_utf16(&bytes.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect::<Vec<_>>()).unwrap()
    }

    #[test]
    fn powershell_gets_the_script_inline_and_runs_a_command_after_it() {
        let i = Integration { dir: "/x".into() };
        let mut s = spawn(r"C:\Program Files\PowerShell\7\pwsh.exe", &["-NoLogo"]);
        i.apply(&mut s);
        assert_eq!(&s.args[..3], ["-NoLogo", "-NoExit", "-EncodedCommand"]);
        assert_eq!(decoded(&s.args[3]), PWSH);
        assert!(s.env.is_empty());

        powershell_then(&mut s, "make");
        assert_eq!(s.args.len(), 4);
        assert!(decoded(&s.args[3]).ends_with("\n\nmake"));

        // Without the integration: a plain -Command.
        let mut s = spawn("powershell", &[]);
        powershell_then(&mut s, "make");
        assert_eq!(s.args, vec!["-NoExit", "-Command", "make"]);
    }
}
