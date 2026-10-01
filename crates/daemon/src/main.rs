//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod api;
mod history;
mod install;
mod keys;
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
use tracing::info;

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
    let socket = state_dir.join("sock");
    let subject = format!("mailto:{}", owner_login.clone().unwrap_or_else(|| "illogical@localhost".into()));
    let push = match push::Push::open(state_dir.join("push"), subject) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::warn!(error = %e, "web push is off");
            None
        }
    };
    let config = mux::Config {
        shell,
        shell_args,
        home: home(),
        manager_env: !args.no_manager_env,
        launch,
        integration,
        socket: socket.clone(),
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
