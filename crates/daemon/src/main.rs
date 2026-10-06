//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod acl;
mod agent;
mod api;
mod apps;
mod args;
mod authz;
mod block;
mod browser;
mod classify;
mod control;
// Windows panes on a pseudoconsole (M56).
#[cfg(windows)]
mod conpty;
mod conversations;
mod dial;
mod e2e;
mod editor;
mod forge;
mod fountain;
mod fs;
mod gate;
mod guest_ssh;
mod hand;
mod heap;
mod history;
#[cfg(unix)]
mod holder;
// The pane host on Windows (M58): the shim's part there.
#[cfg(windows)]
mod host;
mod hosts;
mod ide;
mod install;
mod inventory;
mod invite;
mod keys;
mod localauth;
mod machine;
mod mcp;
mod mux;
mod osc;
mod pane;
mod paths;
mod perm;
// The local socket on Windows: a named pipe (M56).
#[cfg(windows)]
mod pipe;
mod ports;
mod procinfo;
mod provider;
mod provider_tunnel;
mod push;
mod remote;
mod resident;
mod resume;
mod review;
mod roots;
mod rules;
// The tailnet sandbox supervisor: Linux boxes.
mod calls;
#[cfg(unix)]
mod sandbox;
mod seal;
mod selfupdate;
mod server;
mod setup;
mod share;
mod shellenv;
mod shellint;
mod shim;
mod sites;
mod store;
mod sync;
mod sys;
mod tailscale;
mod threads;
mod tls;
mod update;
#[cfg(unix)]
mod upload;
mod workspace;

use std::{net::SocketAddr, path::PathBuf};

use args::{Args, BlockArgs, Command, ReachArgs, RunArgs};
use axum::serve::ListenerExt;
use clap::FromArgMatches;
use tracing::{info, warn};

/// Start serving block sites, if asked to.
fn start_sites(
    b: &BlockArgs,
    app: &access::Access,
    owner: Option<String>,
    listen: SocketAddr,
    state_dir: &std::path::Path,
) -> anyhow::Result<()> {
    let Some(mut addr) = b.block_listen else { return Ok(()) };
    // Port 0: bound now, so blocks are named with the port it got (#67).
    let mut bound = None;
    if addr.port() == 0 {
        let l = std::net::TcpListener::bind(addr).map_err(|e| anyhow::anyhow!("can't listen on {addr}: {e}"))?;
        l.set_nonblocking(true)?;
        addr = l.local_addr()?;
        bound = Some(l);
        if let Err(e) = store::write_atomic(&state_dir.join("block-listen"), addr.to_string().as_bytes()) {
            warn!(error = %e, "can't record the block sites' address");
        }
    }
    let (scheme, tls) = match &b.block_domain {
        None => {
            if !addr.ip().is_loopback() {
                anyhow::bail!("--block-listen {addr}: without --block-domain, block sites are loopback only");
            }
            (sites::Scheme::Dev { port: addr.port() }, None)
        }
        Some(domain) => {
            let domain = domain.trim_matches('.').to_ascii_lowercase();
            let store = match (&b.block_cert, &b.block_key, &b.block_acme_cloudflare_token_file) {
                (Some(cert), Some(key), _) => {
                    let store = tls::CertStore::new(cert.clone(), key.clone());
                    store.load()?;
                    tokio::spawn(store.clone().watch());
                    store
                }
                (_, _, Some(token)) => {
                    let directory = match b.block_acme_directory.as_str() {
                        "staging" => tls::LETS_ENCRYPT_STAGING.to_owned(),
                        d => d.to_owned(),
                    };
                    let acme = tls::Acme {
                        domain: domain.clone(),
                        email: b.block_acme_email.clone(),
                        directory,
                        dir: state_dir.join("acme"),
                        dns: tls::Cloudflare::from_file(token)?,
                    };
                    let store = tls::CertStore::new(acme.cert_file(), acme.key_file());
                    tokio::spawn(acme.run(store.clone()));
                    store
                }
                _ => anyhow::bail!(
                    "--block-domain needs a certificate: --block-cert/--block-key or --block-acme-cloudflare-token-file"
                ),
            };
            if owner.is_none() {
                warn!("no tailnet owner: block sites will refuse everyone");
            }
            (sites::Scheme::Tailnet { domain, port: addr.port() }, Some(tls::server_config(store)?))
        }
    };
    let settings =
        sites::Settings { scheme, owner, app_origins: app.origins(), reserved: vec![listen.port(), addr.port()] };
    // Known before any block is restored; served once the address is up.
    let sites = sites::install(settings);
    tokio::spawn(async move {
        // The tailnet address may not be up yet at boot.
        let listener = loop {
            if let Some(l) = bound.take() {
                match tokio::net::TcpListener::from_std(l) {
                    Ok(l) => break l,
                    Err(e) => warn!(%addr, error = %e, "can't serve block sites"),
                }
            }
            match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => break l,
                Err(e) => {
                    warn!(%addr, error = %e, "can't listen for block sites yet");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        };
        info!(%addr, "serving block sites");
        sites::serve(sites, listener, tls).await;
    });
    Ok(())
}

/// Dial out to the home daemon and push history there, if asked to.
fn start_reach(r: &ReachArgs, app: &std::sync::Arc<server::App>, name: String) -> anyhow::Result<()> {
    if let (Some(peer), Some(token_file)) = (&r.peer, &r.token) {
        dial::dial_url(peer)?;
        let opts = dial::PeerOpts { url: peer.clone(), token_file: token_file.clone(), join: r.join.clone(), name };
        let (accept, streams) = tokio::sync::mpsc::unbounded_channel();
        info!(peer, "dialing out to the home daemon");
        tokio::spawn(axum::serve(dial::Streams(streams), server::tunnel_router(app.clone())).into_future());
        tokio::spawn(dial::keep_dialing(opts, accept));
    }
    if r.sync {
        let origin = match (&r.sync_to, &r.peer) {
            (Some(to), _) => to.trim_end_matches('/').to_owned(),
            (None, Some(peer)) => dial::home_origin(peer)?,
            (None, None) => anyhow::bail!("--sync needs --peer or --sync-to"),
        };
        let opts = sync::PushOpts {
            origin,
            token_file: r.token.clone().expect("clap requires --token"),
            live: r.sync_live,
            every: std::time::Duration::from_secs(r.sync_every.max(1)),
        };
        info!(to = opts.origin, live = opts.live, "syncing history to the home daemon");
        tokio::spawn(sync::keep_pushing(opts, app.mux.store.clone()));
    }
    // M70: uploads older than a day go, from now.
    #[cfg(unix)]
    tokio::spawn(upload::keep_sweeping());
    // Synced history expires a day at a time.
    let synced = app.synced.clone();
    tokio::spawn(async move {
        let mut day = tokio::time::interval(std::time::Duration::from_secs(24 * 3600));
        day.tick().await;
        loop {
            day.tick().await;
            synced.prune(sync::RETAIN_MS);
        }
    });
    Ok(())
}

/// A random name for this daemon's state directory, kept in it: the
/// machines it creates carry it, so it never sweeps away another daemon's.
fn daemon_id(store: &store::StateDir) -> String {
    let path = store.root().join("daemon-id");
    if let Ok(id) = std::fs::read_to_string(&path)
        && !id.trim().is_empty()
    {
        return id.trim().to_owned();
    }
    let b = push::random::<4>();
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    if let Err(e) = store::write_atomic(&path, id.as_bytes()) {
        warn!(error = %e, "can't save the daemon id");
    }
    id
}

/// `$SHELL`, else the login shell from the user database: launchd and some
/// service managers don't set `$SHELL`, and macOS's `/bin/bash` is 3.2.
/// Windows: PowerShell 7 if it's installed, else Windows PowerShell, else
/// `%COMSPEC%` (cmd).
#[cfg(windows)]
fn login_shell() -> String {
    let on_path =
        |exe: &str| std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(exe).is_file()));
    for exe in ["pwsh.exe", "powershell.exe"] {
        if on_path(exe) {
            return exe.trim_end_matches(".exe").into();
        }
    }
    std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".into())
}

/// What the login shell starts with: a login shell on Unix; PowerShell
/// without its banner.
fn login_args(shell: &str) -> Vec<String> {
    if cfg!(windows) {
        let name = std::path::Path::new(shell).file_stem().map(|s| s.to_string_lossy().to_lowercase());
        return match name.as_deref() {
            Some("pwsh" | "powershell") => vec!["-NoLogo".into()],
            _ => vec![],
        };
    }
    vec!["-l".into()]
}

#[cfg(unix)]
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| {
            let user = nix::unistd::User::from_uid(nix::unistd::getuid()).ok()??;
            Some(user.shell.to_string_lossy().into_owned()).filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "/bin/bash".into())
}

/// The CLI's socket: `sock` in the state directory, unless that path is too
/// long for a Unix socket (about 108 bytes); then `sock` in a directory of
/// our own (0700) in `$XDG_RUNTIME_DIR` (else /tmp), named by a hash of the
/// state directory, recorded in `sock.path`.
#[cfg(unix)]
fn socket_path(state_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let plain = state_dir.join("sock");
    let record = state_dir.join("sock.path");
    if plain.as_os_str().len() < 100 {
        let _ = std::fs::remove_file(record);
        return Ok(plain);
    }
    // FNV-1a: stable across runs, unlike std's hasher.
    let hash = state_dir
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let base = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let uid = nix::unistd::geteuid().as_raw();
    let dir = base.join(format!("illogical-{uid}-{hash:016x}"));
    private_socket_dir(&dir)?;
    let socket = dir.join("sock");
    if let Err(e) = store::write_atomic(&record, socket.as_os_str().as_encoded_bytes()) {
        warn!(error = %e, "can't record the socket's path");
    }
    Ok(socket)
}

/// Windows: a named pipe, named by a hash of the state directory (so test
/// daemons with their own state each have one), recorded in `sock.path`
/// for the CLI.
#[cfg(windows)]
fn socket_path(state_dir: &std::path::Path) -> anyhow::Result<PathBuf> {
    let hash = state_dir
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let user = std::env::var("USERNAME").unwrap_or_default().to_lowercase();
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect();
    let pipe = PathBuf::from(format!(r"\\.\pipe\illogical-{user}-{hash:016x}"));
    if let Err(e) = store::write_atomic(&state_dir.join("sock.path"), pipe.as_os_str().as_encoded_bytes()) {
        warn!(error = %e, "can't record the socket's path");
    }
    Ok(pipe)
}

/// `dir`, made 0700, or there already as a directory (not a link) of ours,
/// made 0700: in a shared directory like /tmp, one someone else made first
/// is refused, never used.
#[cfg(unix)]
fn private_socket_dir(dir: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => anyhow::bail!("can't make {}: {e}", dir.display()),
    }
    let m = std::fs::symlink_metadata(dir)?;
    if !m.file_type().is_dir() || m.uid() != nix::unistd::geteuid().as_raw() {
        anyhow::bail!("{} isn't a directory of this account's: remove it and start again", dir.display());
    }
    if m.mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// A daemon is listening on `state_dir`'s CLI socket.
#[cfg(unix)]
fn daemon_running(state_dir: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStringExt;
    let path = std::fs::read(state_dir.join("sock.path"))
        .map(|b| PathBuf::from(std::ffi::OsString::from_vec(b)))
        .unwrap_or_else(|_| state_dir.join("sock"));
    std::os::unix::net::UnixStream::connect(path).is_ok()
}

/// `<state>/editors/sock`, in a 0700 directory with nothing else in it, for
/// a dev container to mount (M28).
#[cfg(unix)]
fn editors_socket(state_dir: &std::path::Path) -> std::io::Result<tokio::net::UnixListener> {
    use std::os::unix::fs::PermissionsExt;
    let dir = state_dir.join("editors");
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let sock = dir.join("sock");
    let _ = std::fs::remove_file(&sock);
    let l = tokio::net::UnixListener::bind(&sock)?;
    std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

fn default_state_dir() -> PathBuf {
    // Windows: beside the desktop app, which its installer puts in
    // %LOCALAPPDATA%\illogical (M54).
    #[cfg(windows)]
    if let Some(d) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(d).join("illogical").join("state");
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"))
        .join("illogical")
}

/// A daemon is serving `state_dir`'s named pipe.
#[cfg(windows)]
fn daemon_running(state_dir: &std::path::Path) -> bool {
    std::fs::read_to_string(state_dir.join("sock.path")).is_ok_and(|p| pipe::answering(p.trim()))
}

/// A command line from words, quoted as Windows programs split them (cmd's
/// `/k` takes one). On Unix, the words joined (never used there).
fn conpty_command_line(argv: &[String]) -> String {
    #[cfg(windows)]
    return argv.split_first().map(|(p, rest)| conpty::command_line(p, rest)).unwrap_or_default();
    #[cfg(not(windows))]
    argv.join(" ")
}

/// This computer's name, for joining.
fn hostname() -> Option<String> {
    #[cfg(unix)]
    return nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok());
    #[cfg(not(unix))]
    std::env::var("COMPUTERNAME").ok()
}

/// `$HOME`, or `%USERPROFILE%` on Windows.
pub(crate) fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

/// `ILLOGICAL_LOG_FILE` (or `--log-file`): stdout and stderr appended to
/// that file (a leading `~/` is the home directory). The desktop app's
/// launch agent sets it (M46): launchd can't put a log in each user's home
/// itself; Windows' logon task passes the flag (M59).
fn log_to_file(argv: &[String]) {
    let flag = argv.iter().position(|a| a == "--log-file").and_then(|i| argv.get(i + 1)).map(std::ffi::OsString::from);
    let Some(path) = flag.or_else(|| std::env::var_os("ILLOGICAL_LOG_FILE")).filter(|p| !p.is_empty()) else {
        return;
    };
    let path = PathBuf::from(path);
    let path = match path.strip_prefix("~") {
        Ok(rest) => home().join(rest),
        Err(_) => path,
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) else { return };
    #[cfg(unix)]
    {
        let _ = nix::unistd::dup2_stdout(&f);
        let _ = nix::unistd::dup2_stderr(&f);
    }
    // Windows: the std handles become the file (std's stdout and stderr
    // look them up on every write). It stays open for the process's life.
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        use windows_sys::Win32::System::Console::{STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
        let h = f.into_raw_handle();
        // SAFETY: a handle we own and never close.
        unsafe {
            SetStdHandle(STD_OUTPUT_HANDLE, h);
            SetStdHandle(STD_ERROR_HANDLE, h);
        }
    }
    // Panes don't inherit it.
    unsafe { std::env::remove_var("ILLOGICAL_LOG_FILE") };
}

/// The version, findable in the binary's bytes: the testnet tests read it
/// from a box's static build they can't run here (#259).
#[used]
static VERSION_MARK: &str = concat!("\0illogical-version=", env!("CARGO_PKG_VERSION"), "\0");

fn main() -> anyhow::Result<()> {
    // ARUGULA_X for ILLOGICAL_X (#504), before any thread exists.
    // SAFETY: nothing else runs yet.
    unsafe { illogical_proto::rename::alias_env() };
    // The pane shim forks, so it runs before any threads exist.
    let argv: Vec<String> = std::env::args().collect();
    #[cfg(unix)]
    if argv.get(1).map(String::as_str) == Some("_shim") {
        shim::run(&argv[2..]);
    }
    #[cfg(windows)]
    if argv.get(1).map(String::as_str) == Some("_host") {
        host::run(&argv[2..]);
    }
    // The macOS app's agent, onto a newer daemon an update put in place.
    #[cfg(target_os = "macos")]
    selfupdate::hand_on();
    log_to_file(&argv);
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "illogicald=info".into()),
        )
        .init();
    // `just vsix`: illogical's VS Code extension, for the marketplaces.
    if argv.get(1).map(String::as_str) == Some("_vsix") && argv.len() >= 3 {
        let out = std::path::Path::new(&argv[2]).join(editor::vsix::file_name());
        std::fs::write(&out, editor::vsix::build())?;
        println!("{}", out.display());
        return Ok(());
    }
    // Claude Code's IDE connections, kept across daemon restarts (M28).
    #[cfg(unix)]
    if argv.get(1).map(String::as_str) == Some("_ide_relay") && argv.len() >= 4 {
        let args = ide::relay::Args { dir: argv[2].clone().into(), lock_dir: argv[3].clone().into() };
        return Ok(tokio::runtime::Runtime::new()?.block_on(ide::relay::run(args))?);
    }
    let args =
        Args::from_arg_matches(&args::labs_command(args::labs_here()).get_matches()).unwrap_or_else(|e| e.exit());
    match args.command {
        #[cfg(unix)]
        Some(Command::Install {
            tailnet: Some(authkey),
            home,
            join,
            owner,
            hostname,
            port,
            no_serve,
            daemon_args,
            ..
        }) => {
            sandbox::install(sandbox::TailnetOpts { authkey, hostname, home, join, owner, port, no_serve, daemon_args })
        }
        Some(Command::Install { no_start, reset_args, system, daemon_args, .. }) => {
            install::install(!no_start, &daemon_args, reset_args, system)
        }
        Some(Command::Uninstall) => install::uninstall(),
        Some(Command::Update { yes }) => selfupdate::cli(yes, &args.run.update_url),
        #[cfg(unix)]
        Some(Command::Sandbox) => sandbox::supervise(),
        #[cfg(not(unix))]
        Some(Command::Sandbox) => anyhow::bail!("the sandbox supervisor is for Linux boxes"),
        Some(Command::Join { url, name, team, account, ticket, state_dir }) => {
            let name = name.unwrap_or_else(|| hostname().unwrap_or_else(|| "illogical".into()));
            let dir = state_dir.unwrap_or_else(default_state_dir);
            tokio::runtime::Runtime::new()?.block_on(control::join(
                &url,
                &name,
                team.as_deref(),
                account.as_deref(),
                ticket.as_deref(),
                &dir,
            ))?;
            if daemon_running(&dir) {
                println!("  The running daemon picks this up within a few seconds.");
            } else {
                println!("  illogicald isn't running here: start it with `illogicald install`.");
            }
            Ok(())
        }
        Some(Command::JoinRequest { name, out, state_dir }) => {
            control::join_request(&name, &out, &state_dir.unwrap_or_else(default_state_dir))
        }
        Some(Command::Leave { state_dir }) => {
            let dir = state_dir.unwrap_or_else(default_state_dir);
            let listen = std::fs::read_to_string(dir.join("listen")).unwrap_or_else(|_| "127.0.0.1:7681".into());
            tokio::runtime::Runtime::new()?.block_on(control::leave(&dir, listen.trim()))
        }
        // Nothing is kept across a restart on Windows yet (M58, #221).
        #[cfg(windows)]
        None => {
            heap::tune();
            tokio::runtime::Runtime::new()?.block_on(run(args.run, Default::default()))
        }
        #[cfg(unix)]
        None => {
            heap::tune();
            // Pane terminals kept for us across a restart; taken before any
            // threads start.
            let kept = sys::take_listen_fds();
            tokio::runtime::Runtime::new()?.block_on(run(args.run, kept))
        }
    }
}

async fn run(mut args: RunArgs, mut kept: std::collections::HashMap<String, pane::Kept>) -> anyhow::Result<()> {
    // Bound first: a port that's taken fails at once, and port 0 is known
    // before anything uses it (#66).
    let listener = tokio::net::TcpListener::bind(args.listen)
        .await
        .map_err(|e| anyhow::anyhow!("can't listen on {}: {e}", args.listen))?;
    args.listen = listener.local_addr()?;
    let mut public_hosts = args.public_hosts.clone();
    let mut owner = args.owner.clone();
    let local_api = tailscale::LocalApi::find(args.tailscale_socket.as_deref());
    let status = match &local_api {
        Some(api) => api
            .settled_status(std::time::Duration::from_secs(30))
            .await
            .map_err(|e| info!(error = %e, "no tailnet"))
            .ok(),
        None => None,
    };
    let mut direct_urls = args.direct_urls.clone();
    for u in &mut direct_urls {
        let mut host = reqwest::Url::parse(u).map_err(|e| anyhow::anyhow!("--direct-url {u}: {e}"))?;
        // Port 0: the one --listen got (#67).
        if host.port() == Some(0) {
            let _ = host.set_port(Some(args.listen.port()));
            *u = host.as_str().trim_end_matches('/').to_owned();
        }
        let host = match (host.host_str(), host.port()) {
            (Some(h), Some(p)) => format!("{h}:{p}"),
            (Some(h), None) => h.to_owned(),
            _ => anyhow::bail!("--direct-url {u} has no host"),
        };
        public_hosts.push(host);
    }
    if let Some(t) = &status {
        info!(host = %t.host, userspace = t.userspace, "accepting tailnet host");
        public_hosts.push(t.host.clone());
        direct_urls.push(format!("https://{}", t.host));
        owner = owner.or(t.login.clone());
    }
    info!(owner = owner.as_deref().unwrap_or("<none: tailnet requests refused>"), "tailnet owner");
    let owner_login = owner.clone();
    // Reachable without serve, on our own port: when not bound to loopback
    // (at the listen address, or bound to every address, at this node's
    // tailnet ones), or when tailscaled's netstack forwards the port to us.
    let userspace = status.as_ref().is_some_and(|t| t.userspace);
    let everywhere = args.listen.ip().is_unspecified();
    let mut direct: Vec<String> = access::direct_address(args.listen).into_iter().collect();
    if everywhere || userspace {
        direct.extend(status.iter().flat_map(|t| &t.ips).map(|ip| access::host_name(*ip)));
    }
    if !direct.is_empty() || everywhere || userspace {
        direct.extend(public_hosts.iter().cloned());
    }
    let mut access =
        access::Access::new(args.listen.port(), &public_hosts, &direct, &args.allow_origins, owner.clone());
    if let Some(digest) = &args.provider_token_sha256 {
        access = access.require_tunnel_token(digest)?;
        info!("resident: loopback connections need the home daemon's tunnel token");
    }
    if let Some(t) = &status {
        access = access.with_tailnet_name(&t.host);
    }
    // Loopback is everyone's on this machine: callers there show the local
    // token (a resident's tunnel token stands in for it).
    let local_token_file = localauth::path(&args.state_dir.clone().unwrap_or_else(default_state_dir));
    if !access.tunnelled() {
        let file = &local_token_file;
        let token = localauth::load_or_create(file)
            .map_err(|e| anyhow::anyhow!("the local token ({}): {e:#}", file.display()))?;
        access = access.require_local_token(&token, args.listen);
        info!(file = %file.display(), "loopback callers need the local token; `illogical web` opens the page signed in");
    }
    let identify = tailscale::Identify::new(local_api, userspace);
    let name = args.name.clone().unwrap_or_else(|| {
        status
            .as_ref()
            .and_then(|t| t.host.split('.').next().map(str::to_owned))
            .or_else(hostname)
            .unwrap_or_else(|| "illogical".into())
    });
    info!(name, "this host");

    let (shell, shell_args) = match &args.shell {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(String::from);
            let program = words.next().ok_or_else(|| anyhow::anyhow!("--shell is empty"))?;
            (program, words.collect())
        }
        None => {
            let shell = login_shell();
            let args = login_args(&shell);
            (shell, args)
        }
    };
    let state_dir = args.state_dir.clone().unwrap_or_else(default_state_dir);
    let store = store::StateDir::open(state_dir.clone())?;
    info!(state = %state_dir.display(), "state directory");
    if let Err(e) = store::write_atomic(&state_dir.join("listen"), args.listen.to_string().as_bytes()) {
        warn!(error = %e, "can't record the listen address");
    }
    start_sites(&args.blocks, &access, owner, args.listen, &state_dir)?;
    #[cfg_attr(unix, allow(unused_mut))]
    let mut launch = pane::Launcher::detect(args.keep_panes);
    // Windows: pane hosts run from a copy of this exe (M58).
    #[cfg(windows)]
    {
        launch.host = host::exe(&state_dir);
    }
    #[cfg(unix)]
    if launch.hold {
        // Terminals the last daemon's pane shims kept, adopted like the FD
        // store's.
        kept.extend(holder::collect(&state_dir));
    }
    // Windows: panes whose hosts outlived the last daemon (M58).
    #[cfg(windows)]
    kept.extend(host::collect(&state_dir));
    info!(scopes = launch.scopes, fd_store = launch.fd_store, hold = launch.hold, kept = kept.len(), "pane launcher");
    store.prune_closed(store::CLOSED_RETENTION_MS);
    let integration = if args.no_shell_integration {
        None
    } else {
        match shellint::Integration::install(state_dir.join("shell")) {
            Ok(i) => Some(i),
            Err(e) => {
                tracing::warn!(error = %e, "can't install shell integration; panes run without it");
                None
            }
        }
    };
    let socket = socket_path(&state_dir)?;
    editor::server::install(editor::server::Settings {
        dir: state_dir.join("editor"),
        // Beside the CLI's socket, which is kept short enough.
        socket: PathBuf::from(format!("{}-code", socket.display())),
        binary: args.editors.code_server.clone(),
        cache: std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".cache"))
            .join("illogical/code-server"),
        releases: args.editors.code_server_releases.clone(),
        idle: args.editors.editor_idle,
        // Long enough to ride out a daemon restart, short enough that a
        // closed block's extension host goes.
        grace: 300,
        launch: launch.clone(),
    });
    update::start(update::Settings {
        state_dir: state_dir.clone(),
        url: args.update_url.clone(),
        enabled: !args.no_update_check,
    });
    let subject = format!("mailto:{}", owner_login.clone().unwrap_or_else(|| "illogical@localhost".into()));
    let push = match push::Push::open(state_dir.join("push"), subject) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "web push is off");
            None
        }
    };
    let token_file = args.wisp_token_file.clone().unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("wisp/token")
    });
    let provider: Option<std::sync::Arc<dyn provider::Provider>> =
        provider::sprites::Sprites::open(&args.wisp_url, &token_file)
            .map(|p| std::sync::Arc::new(p) as std::sync::Arc<dyn provider::Provider>);
    info!(url = args.wisp_url, on = provider.is_some(), "VM panes");
    let secrets = {
        let config = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".config"))
            .join("illogical");
        block::Secrets {
            anthropic_key: args.anthropic_key_file.clone().unwrap_or_else(|| config.join("anthropic-key")),
            claude_token: args.claude_token_file.clone().unwrap_or_else(|| config.join("claude-oauth-token")),
        }
    };
    // Studio apps (M35): the token that lists them and mints ways in.
    let studio_file = args.studio_file.clone().unwrap_or_else(|| state_dir.join("studio.json"));
    apps::studio::install(studio_file.clone());
    // What `fs` never serves, besides the state directory.
    let private = vec![
        token_file.clone(),
        secrets.anthropic_key.clone(),
        secrets.claude_token.clone(),
        studio_file.clone(),
        local_token_file,
    ];
    let acl = std::sync::Arc::new(acl::Acl::open(&state_dir));
    let mcp_tokens = mcp::Tokens::open(&state_dir);
    // Agent blocks reach MCP on loopback (M16); not where loopback needs
    // the provider's tunnel token (a resident daemon).
    let mcp_link = args.provider_token_sha256.is_none().then(|| {
        let ip = match args.listen.ip() {
            std::net::IpAddr::V4(v4) if v4.is_unspecified() => std::net::Ipv4Addr::LOCALHOST.into(),
            std::net::IpAddr::V6(v6) if v6.is_unspecified() => std::net::Ipv6Addr::LOCALHOST.into(),
            ip => ip,
        };
        let cli = std::env::current_exe()
            .map(|e| e.with_file_name(format!("illogical{}", std::env::consts::EXE_SUFFIX)))
            .ok()
            .filter(|c| c.exists());
        mcp::Link {
            url: format!("http://{}/mcp", SocketAddr::new(ip, args.listen.port())),
            cli: cli.unwrap_or_else(|| "illogical".into()),
            socket: socket.clone(),
            tokens: mcp_tokens.clone(),
            serve: Default::default(),
        }
    });
    let mcp_serve = mcp_link.as_ref().map(|l| l.serve.clone());
    let control = control::Control::new(
        &state_dir,
        direct_urls.clone(),
        args.control_url.trim_end_matches('/').to_owned(),
        acl.clone(),
        args.no_relay,
    );
    // M40: forge blocks' live updates (control's GitHub App, hooks here).
    forge::live::init(state_dir.clone(), Some(&control), direct_urls.clone());
    // Claude Code's IDE (M28): its relay keeps the connections.
    let ide = if args.no_claude_ide {
        None
    } else {
        let lock_dir = args.claude_ide_dir.clone().unwrap_or_else(|| ide::default_lock_dir(&home()));
        match ide::Ide::start(state_dir.join("ide"), lock_dir, launch.clone()).await {
            Ok(i) => Some(i),
            Err(e) => {
                warn!(error = %e, "can't be Claude Code's IDE");
                None
            }
        }
    };
    let invite_hook = invite::Hook::default();
    let config = mux::Config {
        acl: acl.clone(),
        control: control.clone(),
        sandbox_of_control: args.sandbox_of_control,
        owner_name: owner_login.clone().unwrap_or_else(|| "owner".into()),
        owner_pic: None,
        guest_machines: args.guest_machines,
        shell,
        shell_args,
        home: home(),
        manager_env: !args.no_manager_env,
        launch,
        integration,
        socket: socket.clone(),
        provider: provider.clone(),
        daemon_id: daemon_id(&store),
        secrets,
        private,
        mcp: mcp_link,
        invite: invite_hook.clone(),
        ide: ide.clone(),
    };
    let mux = mux::start(config, store, kept, push.clone());
    if let Some(i) = &ide {
        i.run(mux.clone());
    }

    let hosts = hosts::Hosts::open(&state_dir, name.clone(), provider);
    hosts.spawn_probe();
    let shares = share::Shares::open(&state_dir);
    let guest_listen = match args.guest_ssh.as_str() {
        "off" => None,
        a => Some(a.parse::<SocketAddr>().map_err(|e| anyhow::anyhow!("--guest-ssh {a}: {e}"))?),
    };
    let guests = guest_ssh::Guests::open(&state_dir, guest_listen, args.guest_ssh_host.clone());
    let synced = sync::Synced::new(&state_dir, args.reach.sync_key_file.clone());
    synced.prune(sync::RETAIN_MS);
    let static_dir = args.static_dir.clone().unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("illogical/static")
    });
    let binaries = static_dir.join("illogicald").exists().then_some(resident::Binaries { dir: static_dir });
    let app = server::App::new(
        access,
        identify,
        mux.clone(),
        push,
        hosts,
        shares,
        synced,
        binaries,
        control.clone(),
        acl.clone(),
        mcp_tokens,
        guests,
        hand::Hands::open(&state_dir),
    );
    app.guests.run(&app);
    control.start(app.clone());
    if let Some(serve) = mcp_serve {
        let _ = serve.set(mcp::pipe_server(&app));
    }
    let _ = invite_hook.set(std::sync::Arc::downgrade(&app));
    // Read-only links end on time (M19).
    {
        let (acl, mux, control) = (acl.clone(), mux.clone(), control.clone());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                if acl.prune_links() {
                    mux.send(mux::Cmd::AclChanged);
                    control.poke();
                }
            }
        });
    }
    start_reach(&args.reach, &app, name)?;
    // TCP_NODELAY on every accepted connection (axum leaves Nagle on).
    // Dial-out tunnels write a DATA frame and a GRANT back to back, and
    // with Nagle the second waits for the peer's delayed ACK: 40 ms on
    // every keystroke relayed through `/h/<name>/ws` (found in S15).
    let listener = listener.tap_io(|tcp| {
        let _ = tcp.set_nodelay(true);
    });
    info!(addr = %args.listen, "listening");
    #[cfg(unix)]
    {
        // The CLI's socket: replace a stale one from a previous run.
        let _ = std::fs::remove_file(&socket);
        let local = tokio::net::UnixListener::bind(&socket)?;
        {
            // Owner only, wherever it is (its directory is private too).
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600))?;
        }
        tokio::spawn(axum::serve(local, server::local_router(app.clone())).into_future());
        // Editors in dev containers join here (M28): a directory of its own.
        match editors_socket(&state_dir) {
            Ok(l) => {
                tokio::spawn(axum::serve(l, server::editors_router(app.clone())).into_future());
            }
            Err(e) => warn!(error = %e, "no socket for editors in containers"),
        }
    }
    // Windows: a named pipe this user alone can open. Another daemon on
    // the same state directory has it already: say so rather than share.
    #[cfg(windows)]
    {
        let local = pipe::PipeListener::bind(&socket.display().to_string())
            .map_err(|e| anyhow::anyhow!("can't serve {} (another illogicald here?): {e}", socket.display()))?;
        tokio::spawn(axum::serve(local, server::local_router(app.clone())).into_future());
    }
    info!(socket = %socket.display(), "listening");
    sys::notify("READY=1");
    tokio::select! {
        r = axum::serve(listener, server::router(app).into_make_service_with_connect_info::<SocketAddr>()) => r?,
        _ = signalled() => {
            sys::notify("STOPPING=1");
            info!("shutting down: saving every pane");
            mux.shutdown().await;
        }
    }
    // Returning drops open connections; panes' shells are hung up as their
    // terminals close, after everything is saved.
    Ok(())
}

/// `POST /api/daemon/stop` (on the local socket): stop as on a signal.
static STOP: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Windows: Ctrl-C, the console closing, logoff or shutdown.
#[cfg(windows)]
async fn signalled() {
    use tokio::signal::windows;
    let (mut close, mut shutdown, mut logoff) = (
        windows::ctrl_close().expect("ctrl_close"),
        windows::ctrl_shutdown().expect("ctrl_shutdown"),
        windows::ctrl_logoff().expect("ctrl_logoff"),
    );
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = STOP.notified() => {}
        _ = close.recv() => {}
        _ = shutdown.recv() => {}
        _ = logoff.recv() => {}
    }
}

#[cfg(unix)]
async fn signalled() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = STOP.notified() => {}
        _ = term.recv() => {}
    }
}
