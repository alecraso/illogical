//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod acl;
mod agent;
mod api;
mod apps;
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
mod heap;
mod history;
#[cfg(unix)]
mod holder;
mod hosts;
mod ide;
mod install;
mod inventory;
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
#[cfg(unix)]
mod sandbox;
mod seal;
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
mod workspace;

use std::{net::SocketAddr, path::PathBuf};

use axum::serve::ListenerExt;
use clap::{Parser, Subcommand};
use tracing::{info, warn};

#[derive(Parser, Debug)]
#[command(version, about = "illogical daemon: owns terminals that clients attach to")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,

    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Install as a service that starts at boot (a systemd user service) or
    /// at login (a launchd agent on macOS): copies this binary to
    /// ~/.local/bin, writes the unit or plist, enables and (re)starts it. On
    /// a Mac with no GUI login (reached over ssh) the agent runs in the
    /// background session: it outlives the ssh login but not a reboot;
    /// --system starts it at boot instead.
    /// With --tailnet (sandboxes, no systemd): joins the tailnet with a
    /// userspace tailscaled and runs the daemon there, both kept running by
    /// `illogicald sandbox`.
    Install {
        /// Write and enable the unit without starting it now.
        #[arg(long)]
        no_start: bool,
        /// macOS: a LaunchDaemon that runs it as you from boot, with nobody
        /// logged in (/Library/LaunchDaemons/illogicald.USER.plist). Runs
        /// sudo, which may ask for your password. `illogicald uninstall`
        /// removes it.
        #[arg(long, conflicts_with = "tailnet")]
        system: bool,
        /// A Tailscale auth key (ephemeral, tagged), as `file:PATH`, `-` for
        /// stdin, or the key itself (kept off command lines it starts).
        #[arg(long, value_name = "AUTHKEY")]
        tailnet: Option<String>,
        /// The home daemon's URL: its page may use this daemon.
        #[arg(long, requires = "tailnet")]
        home: Option<String>,
        /// An invite from the home daemon (`illogical hosts invite`), or
        /// `file:PATH`: adds this daemon to its host list.
        #[arg(long, requires = "home")]
        join: Option<String>,
        /// The tailnet login allowed in [default: the home daemon's owner,
        /// learned when joining].
        #[arg(long, requires = "tailnet")]
        owner: Option<String>,
        /// This machine's tailnet name (and host name in lists).
        #[arg(long, requires = "tailnet")]
        hostname: Option<String>,
        /// The daemon's port on loopback.
        #[arg(long, default_value_t = 7681, requires = "tailnet")]
        port: u16,
        /// Don't put it behind `tailscale serve` (plain http only).
        #[arg(long, requires = "tailnet")]
        no_serve: bool,
        /// Drop the daemon arguments an earlier install wrote, instead of
        /// keeping them when none are given.
        #[arg(long)]
        reset_args: bool,
        /// Arguments for the daemon in the unit, after `--`. With none, an
        /// earlier install's are kept, so upgrading doesn't drop them.
        #[arg(last = true)]
        daemon_args: Vec<String>,
    },
    /// Stop the service `install` set up and remove it (the LaunchAgent, the
    /// background agent or the --system LaunchDaemon, which needs sudo; on
    /// Linux the systemd user service). The binaries and panes' state stay.
    Uninstall,
    /// Keep tailscaled and the daemon running, as `install --tailnet` set
    /// them up (for machines without systemd); stops on SIGTERM.
    Sandbox,
    /// Add this machine to your account on an illogical control
    /// (`https://control.example.com`): prints a code to approve from a
    /// device that's signed in, then the account's fingerprint to check
    /// against that device. A running daemon picks it up.
    Join {
        url: String,
        /// This machine's name in the directory [default: the hostname].
        #[arg(long)]
        name: Option<String>,
        /// Join it to a team (its id, from the team's page), not your
        /// account alone: the team's members reach it by their team role.
        #[arg(long)]
        team: Option<String>,
        /// The account's fingerprint, as the approving device shows it
        /// (Devices and machines…): checked instead of asking.
        #[arg(long, value_name = "FINGERPRINT")]
        account: Option<String>,
        /// A hosted sandbox's one-time ticket (control passes it).
        #[arg(long, hide = true)]
        ticket: Option<String>,
        /// The daemon's state directory [default: as the daemon's].
        #[arg(long, env = "ILLOGICAL_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
    /// A hosted sandbox (M20): make this daemon's key and write its
    /// certificate request to `out`, for control to fetch through the
    /// provider (it then writes control.json back).
    #[command(hide = true)]
    JoinRequest {
        #[arg(long)]
        name: String,
        #[arg(long)]
        out: PathBuf,
        #[arg(long, env = "ILLOGICAL_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
    /// Take this machine off the control it joined.
    Leave {
        #[arg(long, env = "ILLOGICAL_STATE_DIR")]
        state_dir: Option<PathBuf>,
    },
}

#[derive(clap::Args, Debug)]
struct RunArgs {
    /// Address to listen on. Keep it loopback; `tailscale serve` exposes it.
    /// Port 0 picks a free one, recorded in `listen` in the state directory.
    #[arg(long, default_value = "127.0.0.1:7681", env = "ILLOGICAL_LISTEN")]
    listen: SocketAddr,

    /// Tailscale login allowed through `tailscale serve` [default: the login
    /// that owns this node].
    #[arg(long, env = "ILLOGICAL_OWNER")]
    owner: Option<String>,

    /// Extra Host names to accept (the MagicDNS name is detected).
    #[arg(long = "public-host")]
    public_hosts: Vec<String>,

    /// Don't keep a socket open to control's relay: control reaches this
    /// daemon through its provider's proxy (a hosted sandbox).
    #[arg(long, hide = true)]
    no_relay: bool,

    /// A hosted sandbox (M20): when the last session closes, ask control
    /// to delete it.
    #[arg(long, hide = true)]
    sandbox_of_control: bool,

    /// A URL clients can reach this daemon at directly, for control's
    /// directory (`https://box.lan:7681`); its host is accepted too. The
    /// tailnet name, if any, is listed without this. Port 0 is the port
    /// --listen got.
    #[arg(long = "direct-url", env = "ILLOGICAL_DIRECT_URL", value_delimiter = ',')]
    direct_urls: Vec<String>,

    /// The control Getting started's *Connect* button joins (#207): your
    /// own, say. `illogicald join URL` takes any control regardless.
    #[arg(long = "control", env = "ILLOGICAL_CONTROL", value_name = "URL", default_value = setup::CONTROL)]
    control_url: String,

    /// Extra origins whose pages may use this daemon (WebSocket and API),
    /// exactly as the browser sends them: the Vite dev server, or the home
    /// daemon whose host list this daemon is on (`https://geek.….ts.net`).
    #[arg(long = "allow-origin")]
    allow_origins: Vec<String>,

    /// This daemon's name in host lists [default: its tailnet name, else
    /// the hostname].
    #[arg(long, env = "ILLOGICAL_NAME")]
    name: Option<String>,

    /// tailscaled's socket, for its name and for asking who is connecting
    /// [default: tailscaled's usual one, if present].
    #[arg(long, env = "ILLOGICAL_TAILSCALE_SOCKET")]
    tailscale_socket: Option<PathBuf>,

    /// How many VMs each guest (someone a session is shared with) may have
    /// at once; their panes run on VMs, never this machine.
    #[arg(long, default_value_t = 3, env = "ILLOGICAL_GUEST_MACHINES")]
    guest_machines: usize,

    /// Where the ssh server for invited guests listens (M65: `illogical
    /// share --guest`), only while an invite exists; `off` turns the feature
    /// off. Port 0 picks a free one.
    #[arg(long, env = "ILLOGICAL_GUEST_SSH", default_value = guest_ssh::DEFAULT_LISTEN)]
    guest_ssh: String,

    /// The address guests are told to ssh to [default: the hostname].
    #[arg(long, env = "ILLOGICAL_GUEST_SSH_HOST")]
    guest_ssh_host: Option<String>,

    /// Command line for panes, split on whitespace [default: $SHELL -l].
    #[arg(long)]
    shell: Option<String>,

    /// Where layout, scrollback and checkpoints live [default:
    /// $XDG_STATE_HOME/illogical, else ~/.local/state/illogical].
    #[arg(long, env = "ILLOGICAL_STATE_DIR")]
    state_dir: Option<PathBuf>,

    /// Don't merge the systemd user manager's environment into new panes.
    #[arg(long)]
    no_manager_env: bool,

    /// Without systemd (macOS, containers): keep panes' programs running
    /// while the daemon restarts, by having each pane's shim hold its
    /// terminal. If no daemon comes back within a minute they end as before.
    /// `illogicald install` sets it on macOS.
    #[arg(long, env = "ILLOGICAL_KEEP_PANES")]
    keep_panes: bool,

    /// Start shells without the integration that marks prompts, commands
    /// and exit codes.
    #[arg(long)]
    no_shell_integration: bool,
    /// The wispd that VM panes get their machines from.
    #[arg(long, env = "ILLOGICAL_WISP_URL", default_value = "http://127.0.0.1:7788")]
    wisp_url: String,
    /// Its API token. VM panes are off without one. Default:
    /// `$XDG_DATA_HOME/wisp/token`.
    #[arg(long, env = "ILLOGICAL_WISP_TOKEN_FILE")]
    wisp_token_file: Option<PathBuf>,
    /// Where the static binaries (`just static`) to copy into a sandbox
    /// are, when making a daemon resident there [default:
    /// $XDG_DATA_HOME/illogical/static].
    #[arg(long, env = "ILLOGICAL_STATIC_DIR")]
    static_dir: Option<PathBuf>,
    /// A resident daemon in a sandbox (set when it's made resident): the
    /// SHA-256 of the token the home daemon's tunnel presents. Connections
    /// from this machine (the provider's proxy arrives on loopback) need
    /// it; the Unix socket doesn't.
    #[arg(long, value_name = "HEX")]
    provider_token_sha256: Option<String>,
    /// An Anthropic API key for Claude Code agents in VMs, passed to them as
    /// ANTHROPIC_API_KEY [default: ~/.config/illogical/anthropic-key].
    #[arg(long, env = "ILLOGICAL_ANTHROPIC_KEY_FILE")]
    anthropic_key_file: Option<PathBuf>,
    /// A Claude Code token (`claude setup-token`) for agents in VMs when
    /// there's no API key, passed as CLAUDE_CODE_OAUTH_TOKEN [default:
    /// ~/.config/illogical/claude-oauth-token].
    #[arg(long, env = "ILLOGICAL_CLAUDE_TOKEN_FILE")]
    claude_token_file: Option<PathBuf>,
    /// Where the studio token is kept (M35: `illogical studio login`),
    /// mode 0600, never sent to a client [default: studio.json in the
    /// state directory].
    #[arg(long, env = "ILLOGICAL_STUDIO_FILE")]
    studio_file: Option<PathBuf>,
    /// Don't be Claude Code's IDE (M28). By default Claude Code in a pane
    /// connects to illogicald (`CLAUDE_CODE_SSE_PORT`) and its edits wait
    /// as diff cards beside the terminal's own prompt.
    #[arg(long, env = "ILLOGICAL_NO_CLAUDE_IDE")]
    no_claude_ide: bool,
    /// Where Claude Code looks for IDEs [default: $CLAUDE_CONFIG_DIR/ide,
    /// else ~/.claude/ide].
    #[arg(long, env = "ILLOGICAL_CLAUDE_IDE_DIR", hide = true)]
    claude_ide_dir: Option<PathBuf>,
    /// Don't check for a newer release. Otherwise, at most twice a day,
    /// the daemon asks GitHub which release is the latest (nothing else is
    /// sent) and the web client offers the command that updates.
    #[arg(long, env = "ILLOGICAL_NO_UPDATE_CHECK")]
    no_update_check: bool,
    /// Where the latest release is looked up (tests point it at a fake).
    #[arg(long, env = "ILLOGICAL_UPDATE_URL", default_value = update::LATEST, hide = true)]
    update_url: String,

    #[command(flatten)]
    blocks: BlockArgs,

    #[command(flatten)]
    editors: EditorArgs,

    #[command(flatten)]
    reach: ReachArgs,
}

/// Editor blocks (M27): VS Code as code-server, served like browser blocks
/// on ports (so they need --block-listen).
#[derive(clap::Args, Debug)]
struct EditorArgs {
    /// A code-server to run [default: the release illogical pins, downloaded
    /// to ~/.cache/illogical/code-server the first time an editor opens].
    #[arg(long, env = "ILLOGICAL_CODE_SERVER")]
    code_server: Option<PathBuf>,
    /// Stop an editor server after this many seconds with no editor open
    /// (at least 60).
    #[arg(long, env = "ILLOGICAL_EDITOR_IDLE", default_value_t = 900)]
    editor_idle: u64,
    /// Where code-server releases are downloaded from.
    #[arg(long, env = "ILLOGICAL_CODE_SERVER_RELEASES", default_value = editor::server::RELEASES, hide = true)]
    code_server_releases: String,
}

/// M4c: reaching a home daemon from a host that can only dial out, and
/// keeping history there.
#[derive(clap::Args, Debug)]
struct ReachArgs {
    /// Dial out to this home daemon (`wss://geek.….ts.net`) and serve this
    /// daemon through it, for when nothing can connect in. Redials with
    /// backoff; this daemon works on its own meanwhile.
    #[arg(long, env = "ILLOGICAL_PEER", requires = "token")]
    peer: Option<String>,
    /// The per-host token for --peer (and --sync), in a file. Mint one on
    /// the home daemon with `illogical hosts token NAME`.
    #[arg(long, env = "ILLOGICAL_TOKEN_FILE", value_name = "FILE")]
    token: Option<PathBuf>,
    /// An invite (`illogical hosts invite`) to trade for a token when the
    /// --token file doesn't exist yet; the token is saved there.
    #[arg(long, requires = "peer")]
    join: Option<String>,
    /// Push closed panes' history to the home daemon, which keeps it
    /// encrypted after this host is gone.
    #[arg(long, requires = "token")]
    sync: bool,
    /// Push open panes' history too, as it grows.
    #[arg(long, requires = "sync")]
    sync_live: bool,
    /// Where to push [default: the --peer's https:// origin].
    #[arg(long, requires = "sync")]
    sync_to: Option<String>,
    /// Seconds between pushes.
    #[arg(long, default_value_t = 30, requires = "sync")]
    sync_every: u64,
    /// Home daemon: the key ring synced history is sealed with [default:
    /// <state>/synced/key, made on first use, 0600].
    #[arg(long, env = "ILLOGICAL_SYNC_KEY_FILE")]
    sync_key_file: Option<PathBuf>,
}

/// Browser blocks on ports: each is served on its own origin by a listener
/// of ours (see `sites.rs`).
#[derive(clap::Args, Debug)]
struct BlockArgs {
    /// Serve browser blocks on ports here [default: off]. Without
    /// --block-domain this must be loopback, and blocks are
    /// `http://b-<id>-<key>.localhost:<port>`. Port 0 picks a free one,
    /// recorded in `block-listen` in the state directory.
    #[arg(long, env = "ILLOGICAL_BLOCK_LISTEN")]
    block_listen: Option<SocketAddr>,
    /// Name blocks `b-<id>.<DOMAIN>`, over HTTPS, for the owner on the
    /// tailnet. `*.<DOMAIN>` must resolve to --block-listen's address.
    #[arg(long, env = "ILLOGICAL_BLOCK_DOMAIN", requires = "block_listen")]
    block_domain: Option<String>,
    /// A certificate for `*.<DOMAIN>` (PEM, with its chain); replaced files
    /// are picked up.
    #[arg(long, requires = "block_key", requires = "block_domain")]
    block_cert: Option<PathBuf>,
    /// The certificate's key (PEM).
    #[arg(long, requires = "block_cert")]
    block_key: Option<PathBuf>,
    /// Get and renew the certificate from an ACME CA with a DNS-01
    /// challenge, through Cloudflare with this API token (Zone:Read and
    /// DNS:Edit).
    #[arg(long, env = "ILLOGICAL_BLOCK_ACME_TOKEN_FILE", conflicts_with = "block_cert", requires = "block_domain")]
    block_acme_cloudflare_token_file: Option<PathBuf>,
    /// The ACME account's contact.
    #[arg(long, env = "ILLOGICAL_BLOCK_ACME_EMAIL")]
    block_acme_email: Option<String>,
    /// The ACME directory URL, or `staging` for Let's Encrypt's test CA.
    #[arg(long, env = "ILLOGICAL_BLOCK_ACME_DIRECTORY", default_value = tls::LETS_ENCRYPT)]
    block_acme_directory: String,
}

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

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

/// `ILLOGICAL_LOG_FILE`: stdout and stderr appended to that file (a
/// leading `~/` is the home directory). The desktop app's launch agent
/// sets it (M46): launchd can't put a log in each user's home itself.
fn log_to_file() {
    let Some(path) = std::env::var_os("ILLOGICAL_LOG_FILE").filter(|p| !p.is_empty()) else { return };
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
    // Windows: the service's log comes with its logon task (M59, #222).
    #[cfg(not(unix))]
    drop(f);
    // Panes don't inherit it.
    unsafe { std::env::remove_var("ILLOGICAL_LOG_FILE") };
}

/// The version, findable in the binary's bytes: the testnet tests read it
/// from a box's static build they can't run here (#259).
#[used]
static VERSION_MARK: &str = concat!("\0illogical-version=", env!("CARGO_PKG_VERSION"), "\0");

fn main() -> anyhow::Result<()> {
    // The pane shim forks, so it runs before any threads exist.
    let argv: Vec<String> = std::env::args().collect();
    #[cfg(unix)]
    if argv.get(1).map(String::as_str) == Some("_shim") {
        shim::run(&argv[2..]);
    }
    log_to_file();
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
    let args = Args::parse();
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

async fn run(
    mut args: RunArgs,
    #[cfg_attr(windows, allow(unused_mut))] mut kept: std::collections::HashMap<String, pane::Kept>,
) -> anyhow::Result<()> {
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
    let launch = pane::Launcher::detect(args.keep_panes);
    #[cfg(unix)]
    if launch.hold {
        // Terminals the last daemon's pane shims kept, adopted like the FD
        // store's.
        kept.extend(holder::collect(&state_dir));
    }
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
    );
    app.guests.run(&app);
    control.start(app.clone());
    if let Some(serve) = mcp_serve {
        let _ = serve.set(mcp::pipe_server(&app));
    }
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
        _ = term.recv() => {}
    }
}
