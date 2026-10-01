//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod api;
mod history;
mod install;
mod keys;
mod machine;
mod mux;
mod osc;
mod pane;
mod push;
mod server;
mod shellint;
mod shim;
mod store;
mod sys;

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
    /// Install as a systemd user service that starts at boot: copies this
    /// binary to ~/.local/bin, writes the unit, enables and (re)starts it.
    Install {
        /// Write and enable the unit without starting it now.
        #[arg(long)]
        no_start: bool,
        /// Arguments for the daemon in the unit, after `--`.
        #[arg(last = true)]
        daemon_args: Vec<String>,
    },
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

    /// Extra WebSocket origins to accept, e.g. the Vite dev server.
    #[arg(long = "allow-origin")]
    allow_origins: Vec<String>,

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
        Some(Command::Install { no_start, daemon_args }) => install::install(!no_start, &daemon_args),
        None => {
            // Pane terminals kept for us across a restart; taken before any
            // threads start.
            let kept = sys::take_listen_fds();
            tokio::runtime::Runtime::new()?.block_on(run(args.run, kept))
        }
    }
}

async fn run(args: RunArgs, kept: std::collections::HashMap<String, std::os::fd::OwnedFd>) -> anyhow::Result<()> {
    let mut public_hosts = args.public_hosts.clone();
    let mut owner = args.owner.clone();
    if let Some(t) = access::tailnet() {
        info!(host = %t.host, "accepting tailnet host");
        public_hosts.push(t.host);
        owner = owner.or(t.login);
    }
    info!(owner = owner.as_deref().unwrap_or("<none: tailnet requests refused>"), "tailnet owner");
    let owner_login = owner.clone();
    let access = access::Access::new(args.listen.port(), &public_hosts, &args.allow_origins, owner);

    let (shell, shell_args) = match &args.shell {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(String::from);
            let program = words.next().ok_or_else(|| anyhow::anyhow!("--shell is empty"))?;
            (program, words.collect())
        }
        None => (std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()), vec!["-l".into()]),
    };
    let state_dir = args.state_dir.unwrap_or_else(|| {
        std::env::var_os("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home().join(".local/state"))
            .join("illogical")
    });
    let store = store::StateDir::open(state_dir.clone())?;
    info!(state = %state_dir.display(), "state directory");
    let launch = pane::Launcher::detect();
    info!(scopes = launch.scopes, fd_store = launch.fd_store, kept = kept.len(), "pane launcher");
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
    let wisp = machine::Wisp::open(&args.wisp_url, &token_file).map(std::sync::Arc::new);
    info!(url = args.wisp_url, on = wisp.is_some(), "VM panes");
    let config = mux::Config {
        shell,
        shell_args,
        home: home(),
        manager_env: !args.no_manager_env,
        launch,
        integration,
        socket: socket.clone(),
        wisp,
        daemon_id: daemon_id(&store),
    };
    let mux = mux::start(config, store, kept, push.clone());

    let app = server::App::new(access, mux.clone(), push);
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    info!(addr = %args.listen, "listening");
    // The CLI's socket: replace a stale one from a previous run.
    let _ = std::fs::remove_file(&socket);
    let local = tokio::net::UnixListener::bind(&socket)?;
    info!(socket = %socket.display(), "listening");
    tokio::spawn(axum::serve(local, server::local_router(app.clone())).into_future());
    sys::notify("READY=1");
    tokio::select! {
        r = axum::serve(listener, server::router(app)) => r?,
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
