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
//!   `fish/vendor_conf.d/illogical.fish`.
//!
//! On a machine (a VM pane) our files aren't there, so bash gets the script
//! in an environment variable, and `ENV` (which bash expands, command
//! substitution included) writes it to a temporary file and names that.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use crate::pane::Spawn;

const BASH: &str = include_str!("../shell/bash/illogical.bash");
const ZSH: &str = include_str!("../shell/zsh/.zshenv");
const FISH: &str = include_str!("../shell/fish/vendor_conf.d/illogical.fish");

#[derive(Debug, Clone)]
pub struct Integration {
    dir: PathBuf,
}

impl Integration {
    /// Write the scripts under `dir` (refreshed on every start, so they
    /// match this binary).
    pub fn install(dir: PathBuf) -> io::Result<Self> {
        for (path, text) in
            [("bash/illogical.bash", BASH), ("zsh/.zshenv", ZSH), ("fish/vendor_conf.d/illogical.fish", FISH)]
        {
            let p = dir.join(path);
            fs::create_dir_all(p.parent().unwrap())?;
            fs::write(&p, text)?;
            crate::perm::set(&p, 0o644)?;
        }
        Ok(Self { dir })
    }

    /// Make an interactive shell load the integration. Commands (`-c`) and
    /// shells we don't know are left alone.
    pub fn apply(&self, spawn: &mut Spawn) {
        if spawn.args.iter().any(|a| a == "-c") {
            return;
        }
        match shell_name(&spawn.program) {
            "bash" => {
                let mut args = Vec::new();
                for a in std::mem::take(&mut spawn.args) {
                    match a.as_str() {
                        "-l" | "--login" => spawn.env.push(("ILLOGICAL_BASH_LOGIN".into(), "1".into())),
                        "--norc" => spawn.env.push(("ILLOGICAL_BASH_NORC".into(), "1".into())),
                        "--noprofile" => spawn.env.push(("ILLOGICAL_BASH_NOPROFILE".into(), "1".into())),
                        _ => args.push(a),
                    }
                }
                args.insert(0, "--posix".into());
                spawn.args = args;
                spawn.env.push(("ENV".into(), self.dir.join("bash/illogical.bash").display().to_string()));
                spawn.env.push(("ILLOGICAL_BASH_INJECT".into(), "1".into()));
            }
            "zsh" => {
                if let Ok(z) = std::env::var("ZDOTDIR") {
                    spawn.env.push(("ILLOGICAL_ZDOTDIR".into(), z));
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
        ("ILLOGICAL_BASH_LOGIN".into(), "1".into()),
        ("ILLOGICAL_BASH_INJECT".into(), "1".into()),
        ("ILLOGICAL_BASH_SCRIPT".into(), BASH.into()),
        (
            "ENV".into(),
            r#"$(f=$(mktemp "${TMPDIR:-/tmp}/illogical.XXXXXX") && printf %s "$ILLOGICAL_BASH_SCRIPT" >"$f" && echo "$f")"#
                .into(),
        ),
    ]);
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
        assert!(s.env.contains(&("ENV".into(), "/x/bash/illogical.bash".into())));
        assert!(s.env.contains(&("ILLOGICAL_BASH_LOGIN".into(), "1".into())));

        let mut s = spawn("bash", &["--norc", "--noprofile"]);
        i.apply(&mut s);
        assert_eq!(s.args, vec!["--posix"]);
        assert!(s.env.contains(&("ILLOGICAL_BASH_NORC".into(), "1".into())));
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
    }
}
