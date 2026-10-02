//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod agent;
mod api;
mod block;
mod browser;
mod dial;
mod dialout_mux;
mod fs;
mod history;
mod holder;
mod hosts;
mod install;
mod keys;
mod machine;
mod mux;
mod osc;
mod pane;
mod ports;
mod procinfo;
mod provider;
mod provider_tunnel;
mod push;
mod resident;
mod sandbox;
mod seal;
mod server;
mod share;
mod shellint;
mod shim;
mod sites;
mod store;
mod sync;
mod sys;
mod tailscale;
mod tls;

use std::{net::SocketAddr, path::PathBuf};

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
    /// ~/.local/bin, writes the unit or plist, enables and (re)starts it.
    /// With --tailnet (sandboxes, no systemd): joins the tailnet with a
    /// userspace tailscaled and runs the daemon there, both kept running by
    /// `illogicald sandbox`.
    Install {
        /// Write and enable the unit without starting it now.
        #[arg(long)]
        no_start: bool,
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
    /// Keep tailscaled and the daemon running, as `install --tailnet` set
    /// them up (for machines without systemd); stops on SIGTERM.
    Sandbox,
}

#[derive(clap::Args, Debug)]
struct RunArgs {
    /// Address to listen on. Keep it loopback; `tailscale serve` exposes it.
    #[arg(long, default_value = "127.0.0.1:7681", env = "ILLOGICAL_LISTEN")]
    listen: SocketAddr,

    /// Tailscale login allowed through `tailscale serve` [default: the login
    /// that owns this node].
    #[arg(long, env = "ILLOGICAL_OWNER")]
    owner: Option<String>,

    /// Extra Host names to accept (the MagicDNS name is detected).
    #[arg(long = "public-host")]
    public_hosts: Vec<String>,

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

    #[command(flatten)]
    blocks: BlockArgs,

    #[command(flatten)]
    reach: ReachArgs,
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
    /// `http://b-<id>-<key>.localhost:<port>`.
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
    let Some(addr) = b.block_listen else { return Ok(()) };
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
    let mut b = [0u8; 4];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut b));
    let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
    if let Err(e) = store::write_atomic(&path, id.as_bytes()) {
        warn!(error = %e, "can't save the daemon id");
    }
    id
}

/// `$SHELL`, else the login shell from the user database: launchd and some
/// service managers don't set `$SHELL`, and macOS's `/bin/bash` is 3.2.
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
/// long for a Unix socket (about 108 bytes); then one in `$XDG_RUNTIME_DIR`
/// (else /tmp) named by a hash of the directory, recorded in `sock.path`.
fn socket_path(state_dir: &std::path::Path) -> PathBuf {
    let plain = state_dir.join("sock");
    let record = state_dir.join("sock.path");
    if plain.as_os_str().len() < 100 {
        let _ = std::fs::remove_file(record);
        return plain;
    }
    // FNV-1a: stable across runs, unlike std's hasher.
    let hash = state_dir
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .fold(0xcbf29ce484222325u64, |h, b| (h ^ *b as u64).wrapping_mul(0x100000001b3));
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    let socket = dir.join(format!("illogical-{hash:016x}.sock"));
    if let Err(e) = store::write_atomic(&record, socket.as_os_str().as_encoded_bytes()) {
        warn!(error = %e, "can't record the socket's path");
    }
    socket
}

fn home() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| "/".into())
}

fn main() -> anyhow::Result<()> {
    // The pane shim forks, so it runs before any threads exist.
    let argv: Vec<String> = std::env::args().collect();
    if argv.get(1).map(String::as_str) == Some("_shim") {
        shim::run(&argv[2..]);
    }
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "illogicald=info".into()),
        )
        .init();
    let args = Args::parse();
    match args.command {
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
        Some(Command::Install { no_start, reset_args, daemon_args, .. }) => {
            install::install(!no_start, &daemon_args, reset_args)
        }
        Some(Command::Sandbox) => sandbox::supervise(),
        None => {
            // Pane terminals kept for us across a restart; taken before any
            // threads start.
            let kept = sys::take_listen_fds();
            tokio::runtime::Runtime::new()?.block_on(run(args.run, kept))
        }
    }
}

async fn run(args: RunArgs, mut kept: std::collections::HashMap<String, std::os::fd::OwnedFd>) -> anyhow::Result<()> {
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
    if let Some(t) = &status {
        info!(host = %t.host, userspace = t.userspace, "accepting tailnet host");
        public_hosts.push(t.host.clone());
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
    let identify = tailscale::Identify::new(local_api, userspace);
    let name = args.name.clone().unwrap_or_else(|| {
        status
            .as_ref()
            .and_then(|t| t.host.split('.').next().map(str::to_owned))
            .or_else(|| nix::unistd::gethostname().ok().and_then(|h| h.into_string().ok()))
            .unwrap_or_else(|| "illogical".into())
    });
    info!(name, "this host");

    let (shell, shell_args) = match &args.shell {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(String::from);
            let program = words.next().ok_or_else(|| anyhow::anyhow!("--shell is empty"))?;
            (program, words.collect())
        }
        None => (login_shell(), vec!["-l".into()]),
    };
    let state_dir = args.state_dir.unwrap_or_else(|| {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("illogical")
    });
    let store = store::StateDir::open(state_dir.clone())?;
    info!(state = %state_dir.display(), "state directory");
    start_sites(&args.blocks, &access, owner, args.listen, &state_dir)?;
    let launch = pane::Launcher::detect(args.keep_panes);
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
    let socket = socket_path(&state_dir);
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
    // What `fs` never serves, besides the state directory.
    let private = vec![token_file.clone(), secrets.anthropic_key.clone(), secrets.claude_token.clone()];
    let config = mux::Config {
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
    };
    let mux = mux::start(config, store, kept, push.clone());

    let hosts = hosts::Hosts::open(&state_dir, name.clone(), provider);
    hosts.spawn_probe();
    let shares = share::Shares::open(&state_dir);
    let synced = sync::Synced::new(&state_dir, args.reach.sync_key_file.clone());
    synced.prune(sync::RETAIN_MS);
    let static_dir = args.static_dir.clone().unwrap_or_else(|| {
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/share"))
            .join("illogical/static")
    });
    let binaries = static_dir.join("illogicald").exists().then_some(resident::Binaries { dir: static_dir });
    let app = server::App::new(access, identify, mux.clone(), push, hosts, shares, synced, binaries);
    start_reach(&args.reach, &app, name)?;
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    info!(addr = %args.listen, "listening");
    // The CLI's socket: replace a stale one from a previous run.
    let _ = std::fs::remove_file(&socket);
    let local = tokio::net::UnixListener::bind(&socket)?;
    info!(socket = %socket.display(), "listening");
    tokio::spawn(axum::serve(local, server::local_router(app.clone())).into_future());
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

async fn signalled() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
