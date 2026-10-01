//! illogicald: owns the terminals; clients attach over WebSocket.

mod access;
mod mux;
mod pane;
mod server;

use std::{net::SocketAddr, path::PathBuf};

use clap::Parser;
use tracing::info;

#[derive(Parser, Debug)]
#[command(
    version,
    about = "illogical daemon: owns terminals that clients attach to"
)]
struct Args {
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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "illogicald=info".into()),
        )
        .init();
    let args = Args::parse();

    let mut public_hosts = args.public_hosts.clone();
    let mut owner = args.owner.clone();
    if let Some(t) = access::tailnet() {
        info!(host = %t.host, "accepting tailnet host");
        public_hosts.push(t.host);
        owner = owner.or(t.login);
    }
    info!(
        owner = owner
            .as_deref()
            .unwrap_or("<none: tailnet requests refused>"),
        "tailnet owner"
    );
    let access = access::Access::new(
        args.listen.port(),
        &public_hosts,
        &args.allow_origins,
        owner,
    );

    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into());
    let spawn = match &args.shell {
        Some(cmd) => {
            let mut words = cmd.split_whitespace().map(String::from);
            let program = words
                .next()
                .ok_or_else(|| anyhow::anyhow!("--shell is empty"))?;
            pane::Spawn {
                program,
                args: words.collect(),
                cwd: home,
            }
        }
        None => pane::Spawn {
            program: std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
            args: vec!["-l".into()],
            cwd: home,
        },
    };
    let app = server::App::new(access, mux::start(spawn));
    let listener = tokio::net::TcpListener::bind(args.listen).await?;
    info!(addr = %args.listen, "listening");
    axum::serve(listener, server::router(app))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let mut term =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("SIGTERM");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
    info!("shutting down");
}
