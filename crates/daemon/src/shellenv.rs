//! #74: the user's shell environment, for blocks that run the user's tools.
//!
//! The daemon starts before (and apart from) any shell, so tools set up in
//! rc files (node from mise, nvm or fnm, pyenv) aren't on its `PATH`,
//! though every pane has them. So, as VS Code does, it runs the user's
//! shell once per host, as a login shell and interactive, and keeps the
//! environment it prints:
//!
//! - **Once, then kept.** This host's is resolved in the background when
//!   the daemon starts, and the first block that needs it waits for it. A
//!   machine's is resolved through its provider the first time a block
//!   there needs it, and kept by sprite. Nothing re-reads it on a timer:
//!   a restart or `POST /api/hosts/self/shell-env/refresh` does.
//! - **Between sentinels.** The shell prints `env -0` between two marks,
//!   so whatever the rc files print on stdout is ignored. Its stdin is
//!   `/dev/null`, and it runs in a session of its own, so an interactive
//!   shell can't take the daemon's terminal.
//! - **Within a timeout.** An rc file that hangs, or a shell that fails,
//!   leaves the daemon's own environment, with one warning in the log.
//! - **Only what the shell changed.** Blocks keep the daemon's `ILLOGICAL_*`
//!   variables and the illogical CLI on `PATH`; the shell's variables go
//!   over the rest.
//!
//! Panes don't use this: they run the real shell.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use tokio::{io::AsyncReadExt, sync::OnceCell};

use crate::provider::Provider;

/// How long this host's shell may take to start.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// A machine's, which may first have to wake.
const MACHINE_TIMEOUT: Duration = Duration::from_secs(30);

/// What a resolve found.
#[derive(Debug, Default)]
pub struct Resolved {
    /// The variables the shell set or changed, less its own (`PWD`,
    /// `SHLVL`, `_`) and the daemon's. On a machine, all of them. Empty
    /// when it failed.
    pub vars: Vec<(String, String)>,
    /// Why it fell back to the daemon's environment.
    pub error: Option<String>,
    pub took: Duration,
}

impl Resolved {
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

type Slot = Arc<OnceCell<Arc<Resolved>>>;

/// This host's shell environment, and each machine's, kept until
/// refreshed.
pub struct ShellEnv {
    shell: String,
    /// Its long options from `--shell` (`--norc`), before `-l -i -c`.
    args: Vec<String>,
    home: PathBuf,
    /// What the shell starts with, over the daemon's own environment.
    env: Vec<(String, String)>,
    timeout: Duration,
    local: Mutex<Slot>,
    machines: Mutex<HashMap<String, Slot>>,
}

impl std::fmt::Debug for ShellEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ShellEnv").field("shell", &self.shell).finish_non_exhaustive()
    }
}

impl ShellEnv {
    pub fn new(
        shell: String,
        args: Vec<String>,
        home: PathBuf,
        env: Vec<(String, String)>,
        timeout: Duration,
    ) -> Arc<Self> {
        Arc::new(Self { shell, args, home, env, timeout, local: Default::default(), machines: Default::default() })
    }

    pub fn shell(&self) -> &str {
        &self.shell
    }

    /// Resolve this host's in the background.
    pub fn start(self: &Arc<Self>) {
        let this = self.clone();
        tokio::spawn(async move {
            this.local().await;
        });
    }

    /// Forget what was resolved (here and on every machine) and resolve
    /// this host's again.
    pub fn refresh(self: &Arc<Self>) {
        *self.local.lock().unwrap() = Default::default();
        self.machines.lock().unwrap().clear();
        self.start();
    }

    /// This host's if it's resolved already, without waiting.
    pub fn local_now(&self) -> Option<Arc<Resolved>> {
        self.local.lock().unwrap().get().cloned()
    }

    /// This host's, waiting for it if it's still being resolved.
    pub async fn local(&self) -> Arc<Resolved> {
        let slot = self.local.lock().unwrap().clone();
        slot.get_or_init(|| async {
            let t = Instant::now();
            let r = resolve(&self.shell, &self.args, &self.home, &self.env, self.timeout).await;
            done("this host", &self.shell, r, t)
        })
        .await
        .clone()
    }

    /// A machine's, resolved through its provider the first time.
    pub async fn machine(&self, provider: &Arc<dyn Provider>, sprite: &str) -> Arc<Resolved> {
        self.machine_with(sprite, || async {
            let argv = ["sh", "-c", MACHINE_SH, "sh", &script(&mark())];
            match tokio::time::timeout(MACHINE_TIMEOUT, provider.run(sprite, &argv)).await {
                Err(_) => Err(format!("took longer than {}s", MACHINE_TIMEOUT.as_secs())),
                Ok(Err(e)) => Err(format!("{e:#}")),
                Ok(Ok((out, code))) => parse(&out).ok_or_else(|| format!("printed no environment (exit {code:?})")),
            }
        })
        .await
    }

    /// The cache under [`Self::machine`]: `run` is called once per sprite
    /// until refreshed.
    async fn machine_with<F, Fut>(&self, sprite: &str, run: F) -> Arc<Resolved>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<Vec<(String, String)>, String>>,
    {
        let slot = self.machines.lock().unwrap().entry(sprite.to_owned()).or_default().clone();
        slot.get_or_init(|| async {
            let t = Instant::now();
            let r = run().await.map(|vars| clean(vars, &[]));
            done(sprite, "bash", r, t)
        })
        .await
        .clone()
    }
}

/// Log how it went, once per resolve.
fn done(host: &str, shell: &str, r: Result<Vec<(String, String)>, String>, t: Instant) -> Arc<Resolved> {
    let took = t.elapsed();
    let ms = took.as_millis() as u64;
    Arc::new(match r {
        Ok(vars) => {
            tracing::info!(host, shell, ms, vars = vars.len(), "resolved the shell's environment");
            Resolved { vars, error: None, took }
        }
        Err(e) => {
            tracing::warn!(host, shell, ms, error = %e, "can't resolve the shell's environment; blocks get the daemon's");
            Resolved { vars: vec![], error: Some(e), took }
        }
    })
}

/// A machine runs `bash -l` in its panes, so that's what's read there.
const MACHINE_SH: &str = r#"exec bash -l -i -c "$1" </dev/null 2>/dev/null"#;

/// The arguments before the script, for shells that take `-l -i -c`.
fn flags(shell: &str) -> Option<[&'static str; 3]> {
    let name = Path::new(shell).file_name()?.to_str()?.trim_start_matches('-');
    matches!(name, "bash" | "zsh" | "fish" | "ksh" | "mksh" | "dash" | "sh").then_some(["-l", "-i", "-c"])
}

const BEGIN: &str = "__illogical_env_begin_";
const END: &str = "__illogical_env_end_";

/// A mark for one run. The script writes it in two halves, so it never
/// appears whole in the command line (or in a variable holding it).
fn mark() -> String {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    format!("{:x}{:x}", std::process::id(), nanos)
}

/// What the shell runs (the same in sh, bash, zsh and fish).
fn script(mark: &str) -> String {
    format!("printf '%s%s\\n' {BEGIN} {mark}; env -0; printf '%s%s\\n' {END} {mark}")
}

/// The variables between the marks, if both are there (the first begin
/// mark with its end: rc noise may hold something like one).
fn parse(out: &[u8]) -> Option<Vec<(String, String)>> {
    let find = |hay: &[u8], needle: &[u8]| hay.windows(needle.len()).position(|w| w == needle);
    let mut rest = out;
    let body = loop {
        rest = &rest[find(rest, BEGIN.as_bytes())?..];
        let nl = rest.iter().position(|&b| b == b'\n')?;
        let body = &rest[nl + 1..];
        if let Some(end) = find(body, &[END.as_bytes(), &rest[BEGIN.len()..nl], b"\n"].concat()) {
            break &body[..end];
        }
        rest = &rest[1..];
    };
    Some(
        body.split(|&b| b == 0)
            .filter_map(|kv| {
                let kv = String::from_utf8_lossy(kv);
                let (k, v) = kv.split_once('=')?;
                (!k.is_empty()).then(|| (k.to_owned(), v.to_owned()))
            })
            .collect(),
    )
}

/// Drop the shell's own variables and the daemon's, and any the shell
/// started with unchanged (`given`).
fn clean(vars: Vec<(String, String)>, given: &[(String, String)]) -> Vec<(String, String)> {
    vars.into_iter()
        .filter(|(k, _)| !matches!(k.as_str(), "PWD" | "OLDPWD" | "SHLVL" | "_" | "CLAUDE_CODE_SSE_PORT"))
        .filter(|(k, _)| !k.starts_with("ILLOGICAL_"))
        .filter(|kv| !given.contains(kv))
        .collect()
}

/// Run `shell` as a login shell, interactive, and read its environment.
async fn resolve(
    shell: &str,
    args: &[String],
    home: &Path,
    env: &[(String, String)],
    timeout: Duration,
) -> Result<Vec<(String, String)>, String> {
    let flags = flags(shell).ok_or_else(|| format!("{shell} isn't a shell this reads (bash, zsh, fish, ksh, sh)"))?;
    let mut c = tokio::process::Command::new(shell);
    c.args(args)
        .args(flags)
        .arg(script(&mark()))
        .envs(env.iter().map(|(k, v)| (k, v)))
        .current_dir(home)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    // SAFETY: setsid is async-signal-safe; nothing else runs between fork
    // and exec. A session of its own: no controlling terminal to take.
    unsafe {
        c.pre_exec(|| nix::unistd::setsid().map(|_| ()).map_err(std::io::Error::from));
    }
    let mut child = c.spawn().map_err(|e| format!("can't start {shell}: {e}"))?;
    let pid = child.id();
    let mut stdout = child.stdout.take().expect("piped");
    // Until the second mark, not EOF: something the rc files started in
    // the background may hold stdout open.
    let read = async {
        let (mut out, mut buf) = (Vec::new(), [0u8; 16 << 10]);
        loop {
            match stdout.read(&mut buf).await {
                Ok(0) | Err(_) => return (out, None),
                Ok(n) => out.extend_from_slice(&buf[..n]),
            }
            if let Some(vars) = parse(&out) {
                return (out, Some(vars));
            }
        }
    };
    let given: Vec<(String, String)> = std::env::vars().filter(|(k, _)| !env.iter().any(|(e, _)| e == k)).collect();
    let given = [given, env.to_vec()].concat();
    match tokio::time::timeout(timeout, read).await {
        Ok((_, Some(vars))) => {
            // It ends on its own now; don't wait long.
            let _ = tokio::time::timeout(Duration::from_secs(1), child.wait()).await;
            Ok(clean(vars, &given))
        }
        Ok((_, None)) => {
            let code = tokio::time::timeout(Duration::from_secs(1), child.wait()).await.ok().and_then(Result::ok);
            Err(format!("{shell} printed no environment ({})", code.map_or("no exit".into(), |c| c.to_string())))
        }
        Err(_) => {
            // The shell, and whatever its rc files started with it.
            if let Some(pid) = pid {
                let _ = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid as i32), nix::sys::signal::SIGKILL);
            }
            let _ = child.wait().await;
            Err(format!("{shell} took longer than {}s to start (an rc file waits for something?)", timeout.as_secs()))
        }
    }
}

/// A local block's environment with the shell's over it: `block` is what
/// it would get otherwise (the daemon's additions, `ILLOGICAL_*`), and
/// `bin` (the illogical CLI's directory) stays on `PATH`.
pub fn merge(block: &[(String, String)], shell: &Resolved, bin: Option<&Path>) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = block.to_vec();
    for (k, v) in &shell.vars {
        env.retain(|(e, _)| e != k);
        env.push((k.clone(), v.clone()));
    }
    if let (Some(bin), Some((_, path))) = (bin, env.iter_mut().find(|(k, _)| k == "PATH")) {
        let bin = bin.display().to_string();
        if !path.split(':').any(|p| p == bin) {
            *path = format!("{bin}:{path}");
        }
    }
    env
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// A HOME of its own, removed when dropped (after the shell is gone:
    /// `resolve` waits for it or kills it).
    struct Home(PathBuf);

    impl Home {
        fn new(name: &str, bashrc: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("illogical-shellenv-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            // As Debian's default ~/.profile does: a login bash reads
            // ~/.bashrc only through it.
            std::fs::write(dir.join(".profile"), ". \"$HOME/.bashrc\"\n").unwrap();
            std::fs::write(dir.join(".bashrc"), bashrc).unwrap();
            Self(dir)
        }

        fn env(&self) -> Vec<(String, String)> {
            vec![("HOME".into(), self.0.display().to_string()), ("ILLOGICAL_SOCK".into(), "/nowhere".into())]
        }
    }

    impl Drop for Home {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[tokio::test]
    async fn an_rc_file_adds_to_path_and_its_noise_is_ignored() {
        let home =
            Home::new("path", "echo 'hello from bashrc'\nprintf 'A=B\\0'\nexport PATH=\"$HOME/tools/bin:$PATH\"\n");
        let s = ShellEnv::new("bash".into(), vec![], home.0.clone(), home.env(), Duration::from_secs(10));
        assert!(s.local_now().is_none(), "not resolved until asked");
        let r = s.local().await;
        assert!(Arc::ptr_eq(&r, &s.local_now().unwrap()));
        assert_eq!(r.error, None);
        let path = r.get("PATH").expect("PATH changed");
        assert!(path.split(':').next() == Some(&format!("{}/tools/bin", home.0.display())), "{path}");
        assert_eq!(r.get("A"), None);
        assert_eq!(r.get("HOME"), None, "unchanged, so left out");
        assert_eq!(r.get("ILLOGICAL_SOCK"), None);
        assert_eq!(r.get("PWD"), None);
        // Kept: a second call doesn't run the shell again.
        assert!(Arc::ptr_eq(&r, &s.local().await));

        let block = vec![("ILLOGICAL_PANE".into(), "7".into()), ("PATH".into(), "/opt/illogical:/usr/bin".into())];
        let env = merge(&block, &r, Some(Path::new("/opt/illogical")));
        let path = &env.iter().find(|(k, _)| k == "PATH").unwrap().1;
        assert!(path.starts_with(&format!("/opt/illogical:{}/tools/bin:", home.0.display())), "{path}");
        assert!(env.contains(&("ILLOGICAL_PANE".into(), "7".into())));
    }

    #[tokio::test]
    async fn a_slow_rc_file_falls_back() {
        let home = Home::new("slow", "sleep 30\nexport PATH=\"$HOME/tools/bin:$PATH\"\n");
        let s = ShellEnv::new("bash".into(), vec![], home.0.clone(), home.env(), Duration::from_millis(500));
        let t = Instant::now();
        let r = s.local().await;
        assert!(t.elapsed() < Duration::from_secs(5));
        assert!(r.vars.is_empty());
        assert!(r.error.as_deref().unwrap().contains("took longer"), "{:?}", r.error);
        let block = vec![("PATH".into(), "/usr/bin".into())];
        assert_eq!(merge(&block, &r, None), block);
    }

    #[tokio::test]
    async fn a_broken_shell_falls_back() {
        let home = Home::new("broken", "exit 3\n");
        let missing = ShellEnv::new("/nonexistent/bash".into(), vec![], home.0.clone(), home.env(), TIMEOUT);
        assert!(missing.local().await.error.as_deref().unwrap().starts_with("can't start"));
        let unknown = ShellEnv::new("/usr/bin/nu".into(), vec![], home.0.clone(), home.env(), TIMEOUT);
        assert!(unknown.local().await.error.is_some());
        // A shell whose rc file exits before it prints anything.
        let exits = ShellEnv::new("bash".into(), vec![], home.0.clone(), home.env(), TIMEOUT);
        let e = exits.local().await.error.clone().unwrap();
        assert!(e.contains("printed no environment"), "{e}");
    }

    #[test]
    fn parse_reads_only_between_the_marks() {
        let m = "abc";
        let out = format!("noise {BEGIN}x\n{BEGIN}{m}\nA=1\0B=x=y\nz\0{END}{m}\nmore noise");
        assert_eq!(parse(out.as_bytes()), Some(vec![("A".into(), "1".into()), ("B".into(), "x=y\nz".into())]));
        assert_eq!(parse(format!("{BEGIN}{m}\nA=1\0").as_bytes()), None);
        assert_eq!(parse(b"A=1\0"), None);
        assert!(!script(m).contains(&format!("{BEGIN}{m}")));
    }

    #[tokio::test]
    async fn machines_resolve_once_each_until_refreshed() {
        let s = ShellEnv::new("bash".into(), vec![], "/".into(), vec![], TIMEOUT);
        let runs = AtomicUsize::new(0);
        let run = || async {
            runs.fetch_add(1, Ordering::SeqCst);
            Ok(vec![("PATH".into(), "/home/sprite/.local/bin:/usr/bin".into()), ("PWD".into(), "/".into())])
        };
        let a = s.machine_with("sprite-a", run).await;
        assert_eq!(a.vars, vec![("PATH".into(), "/home/sprite/.local/bin:/usr/bin".into())]);
        s.machine_with("sprite-a", run).await;
        s.machine_with("sprite-b", run).await;
        assert_eq!(runs.load(Ordering::SeqCst), 2);
        s.machines.lock().unwrap().clear();
        let failed = s.machine_with("sprite-a", || async { Err("no".into()) }).await;
        assert_eq!(failed.error.as_deref(), Some("no"));
    }
}
