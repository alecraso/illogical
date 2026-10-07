//! HTTP: the embedded web client, and the WebSocket protocol at `/ws`.

use std::{
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use arugula_proto::{ClientId, ClientMsg, Frame, FrameKind};
use axum::{
    Extension, Router,
    body::Body,
    extract::{
        ConnectInfo, Request, State,
        ws::{Message, WebSocket, WebSocketUpgrade},
    },
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::Embed;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::{
    access::Access,
    acl::Principal,
    hosts::Hosts,
    mux::{Cmd, MuxHandle},
    pane::{Subscriber, ToClient, client_queue},
    tailscale::Identify,
};

#[derive(Embed)]
#[folder = "../../web/dist"]
#[allow_missing = true]
struct Assets;

pub struct App {
    pub access: Access,
    /// Who is on the other end of a TCP connection (tailscaled's WhoIs).
    pub identify: Identify,
    pub mux: MuxHandle,
    pub push: Option<crate::push::Push>,
    pub hosts: Arc<Hosts>,
    /// The static binaries a daemon made resident in a sandbox runs.
    pub binaries: Option<crate::resident::Binaries>,
    /// Dial-out hosts connected to us (M4c).
    pub dial_outs: Arc<crate::dial::DialOuts>,
    /// Read-only share links.
    pub shares: Arc<crate::share::Shares>,
    /// History other hosts synced to us.
    pub synced: Arc<crate::sync::Synced>,
    /// Enrollment in Arugula control: trusted devices, the relay.
    pub control: Arc<crate::control::Control>,
    /// Who else may reach which sessions (M12).
    pub acl: Arc<crate::acl::Acl>,
    /// MCP's tokens (M16).
    pub mcp: Arc<crate::mcp::Tokens>,
    /// Invites for guests with only OpenSSH (M65).
    pub guests: Arc<crate::guest_ssh::Guests>,
    /// Devices lending their tools to agents (S33).
    pub hands: Arc<crate::hand::Hands>,
    next_client: AtomicU64,
    /// The owner has reached us over the tailnet (#110: the phone step).
    pub tailnet_seen: std::sync::atomic::AtomicBool,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        access: Access,
        identify: Identify,
        mux: MuxHandle,
        push: Option<crate::push::Push>,
        hosts: Arc<Hosts>,
        shares: Arc<crate::share::Shares>,
        synced: Arc<crate::sync::Synced>,
        binaries: Option<crate::resident::Binaries>,
        control: Arc<crate::control::Control>,
        acl: Arc<crate::acl::Acl>,
        mcp: Arc<crate::mcp::Tokens>,
        guests: Arc<crate::guest_ssh::Guests>,
        hands: Arc<crate::hand::Hands>,
    ) -> Arc<Self> {
        Arc::new(Self {
            access,
            identify,
            mux,
            push,
            hosts,
            dial_outs: Default::default(),
            shares,
            synced,
            binaries,
            control,
            acl,
            mcp,
            guests,
            hands,
            next_client: AtomicU64::new(1),
            tailnet_seen: Default::default(),
        })
    }

    pub fn new_client_id(&self) -> ClientId {
        self.next_client.fetch_add(1, Ordering::Relaxed)
    }
}

/// What a daemon serves to its owner: its own API, and MCP (M16).
fn own_routes(app: &Arc<App>) -> Router<Arc<App>> {
    crate::api::routes()
        .merge(crate::mcp::routes(app))
        .merge(crate::fs::routes())
        .merge(crate::hosts::routes())
        .merge(crate::share::api_routes())
        .merge(crate::guest_ssh::routes())
        .merge(crate::acl::api::routes())
        .merge(crate::invite::routes())
        .merge(crate::setup::routes())
        .merge(crate::update::routes())
        .merge(crate::selfupdate::routes())
}

/// Plus what makes it a home daemon: hosts dialing in and pushing history,
/// the way through to dial-out hosts (`/h/NAME`), its provider's sandboxes,
/// and the provider tunnel to resident daemons in them (`/tunnel/NAME`).
fn api_routes(app: &Arc<App>) -> Router<Arc<App>> {
    own_routes(app)
        .merge(crate::dial::routes())
        .merge(crate::sync::routes())
        .merge(crate::resident::routes())
        .merge(crate::provider_tunnel::routes())
}

/// Over TCP (loopback, behind `tailscale serve`, or a tailnet address):
/// every request passes the access checks, and API calls from a browser
/// must come from one of our origins. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()`.
pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/ws", get(ws))
        .route(crate::e2e::PATH, get(crate::e2e::ws))
        .route(crate::localauth::AUTH_PATH, get(signin))
        .merge(
            api_routes(&app)
                .layer(middleware::from_fn_with_state(app.clone(), crate::authz::check))
                .layer(middleware::from_fn_with_state(app.clone(), api_origin)),
        )
        .merge(crate::share::viewer_routes())
        // M40: forges' webhooks, by their signatures (the handler checks).
        .route(crate::forge::live::FORGEJO_PATH, axum::routing::post(crate::forge::live::forgejo_hook))
        .route(crate::forge::live::GITLAB_PATH, axum::routing::post(crate::forge::live::gitlab_hook))
        .fallback(asset)
        .layer(middleware::from_fn_with_state(app.clone(), cors))
        .layer(middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Over the Unix socket (the CLI, programs in panes): the socket lives in
/// the user's private state directory, so reaching it is the check.
pub fn local_router(app: Arc<App>) -> Router {
    Router::new()
        .route("/ws", get(local_ws))
        // Editors on this machine join the swarm here (M28).
        .route("/api/editors/connect", get(crate::editor::link::connect))
        // `arugula web`: only over the socket, which is the owner's.
        .route("/api/signin-link", get(signin_link))
        // Stop: save every pane and exit, as on Ctrl-C (an upgrade on
        // Windows, where there's no service manager to ask; M59).
        .route("/api/daemon/stop", axum::routing::post(stop))
        .merge(api_routes(&app))
        .with_state(app)
}

#[cfg(unix)]
/// Editors only (M28): `<state>/editors/sock`, the one socket a dev
/// container gets (its directory mounted): joining the swarm as an editor
/// is all it can do there, not drive the daemon.
pub fn editors_router(app: Arc<App>) -> Router {
    Router::new().route("/api/editors/connect", get(crate::editor::link::connect)).with_state(app)
}

/// Over the tunnel to the home daemon (a dial-out host): the home daemon
/// checked who is asking. Our own WebSocket and API only: nothing that
/// would make this host a way to anywhere else (no `/h/`, no dialing in).
pub fn tunnel_router(app: Arc<App>) -> Router {
    Router::new().route("/ws", get(local_ws)).merge(own_routes(&app)).with_state(app)
}

/// Inside an end-to-end channel (`e2e.rs`): the device was checked by the
/// handshake. The daemon's own API only, as over the tunnel.
pub fn channel_router(app: Arc<App>) -> Router {
    own_routes(&app).layer(middleware::from_fn_with_state(app.clone(), crate::authz::check)).with_state(app)
}

/// An embedded web client file.
pub fn asset_file(path: &str) -> Option<Vec<u8>> {
    Assets::get(path).map(|f| f.data.into_owned())
}

/// Cross-site requests can't read our answers, but a POST still lands: so a
/// browser's API call must come from one of our own origins. Programs (no
/// Origin header) are fine.
async fn api_origin(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    match app.access.check_origin(req.headers()) {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, uri = %req.uri(), "rejected API request");
            (status, why).into_response()
        }
    }
}

/// The home daemon's page talks to other daemons' APIs (another origin):
/// browsers allow that only if we say so, and we say so only to origins the
/// access checks accept, exactly. On the tailnet identity is the tailnet's;
/// on this machine it's the sign-in cookie, so credentials are allowed (for
/// those exact origins only).
async fn cors(State(app): State<Arc<App>>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|o| o.to_str().ok())
        .filter(|o| app.access.origin_allowed(o))
        .map(str::to_owned);
    let Some(origin) = origin else { return next.run(req).await };
    let preflight = req.method() == axum::http::Method::OPTIONS
        && req.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    let mut res = if preflight { StatusCode::NO_CONTENT.into_response() } else { next.run(req).await };
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&origin) {
        h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
    }
    h.append(header::VARY, HeaderValue::from_static("origin"));
    h.insert(header::ACCESS_CONTROL_ALLOW_CREDENTIALS, HeaderValue::from_static("true"));
    if preflight {
        h.insert(header::ACCESS_CONTROL_ALLOW_METHODS, HeaderValue::from_static("GET, POST, DELETE"));
        h.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, HeaderValue::from_static("content-type"));
        h.insert(header::ACCESS_CONTROL_MAX_AGE, HeaderValue::from_static("600"));
    }
    res
}

async fn guard(
    State(app): State<Arc<App>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    let peer = app.identify.peer(addr).await;
    let mut req = req;
    let checked = app.access.check_host(req.headers()).and_then(|()| match class(&req, &app.access) {
        Class::Owner => {
            // serve's identity header, on loopback: only from tailscaled.
            if peer == crate::access::Peer::Local
                && !app.access.tunnelled()
                && req.headers().contains_key("tailscale-user-login")
                && !crate::localauth::serve_peer_ok(addr, app.access.port())
            {
                return Err((
                    StatusCode::FORBIDDEN,
                    "a Tailscale-User-Login header from a local account other than tailscaled's".into(),
                ));
            }
            let pic = req.headers().get("tailscale-user-profile-pic").and_then(|v| v.to_str().ok()).map(str::to_owned);
            let who = app.access.check_identity(req.headers(), &peer)?.with_pic(pic);
            // Another user gets in only once something is shared with them.
            if !app.acl.knows(&who) {
                let why = match &who {
                    crate::acl::Principal::User { id, name, .. } if id.starts_with("tailnet:") => {
                        app.access.not_yours(name)
                    }
                    _ => "nothing on this machine is shared with you".into(),
                };
                return Err((StatusCode::FORBIDDEN, why));
            }
            // The phone step is done once the owner comes in over the tailnet (#110).
            if matches!(who, crate::acl::Principal::Owner) && app.access.via_tailnet(req.headers(), &peer) {
                app.tailnet_seen.store(true, Ordering::Relaxed);
            }
            req.extensions_mut().insert(who);
            Ok(())
        }
        Class::Viewer => app.access.check_viewer(req.headers(), &peer),
        Class::Token => Ok(()),
        // The link carries the credential; a browser on this machine only.
        Class::SignIn if peer == crate::access::Peer::Local && !app.access.tunnelled() => Ok(()),
        Class::SignIn => Err((StatusCode::FORBIDDEN, "sign-in links are for this machine's browsers".into())),
        // A browser asking first, for one of our origins (`cors` answers
        // it; it carries no credentials, and its answer gives nothing away).
        Class::Preflight => app.access.check_origin(req.headers()),
        // From this machine or the tailnet; the MCP layer checks the token.
        Class::McpToken if matches!(peer, crate::access::Peer::Other) => {
            Err((StatusCode::FORBIDDEN, "not from this machine or the tailnet".into()))
        }
        Class::McpToken => Ok(()),
    });
    let mut res = match checked {
        Ok(()) => next.run(req).await,
        Err((status, why)) => {
            warn!(%why, ?peer, %addr, uri = %req.uri(), "rejected request");
            // A browser opening the page gets one it can copy the fix from (#109).
            let page = req
                .headers()
                .get(header::ACCEPT)
                .and_then(|v| v.to_str().ok())
                .is_some_and(|a| a.contains("text/html"));
            if page {
                (status, axum::response::Html(crate::access::refusal_page(status, &why))).into_response()
            } else {
                (status, why).into_response()
            }
        }
    };
    // serve authenticates by source, so any page the owner visits could frame
    // the logged-in app (clickjacking). Nothing frames us legitimately.
    // (A stricter policy already set, on a dial-out host's answer, stays.)
    let h = res.headers_mut();
    h.entry(header::CONTENT_SECURITY_POLICY).or_insert(HeaderValue::from_static("frame-ancestors 'none'"));
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    res
}

/// Whose request this can be.
enum Class {
    /// The owner's, like everything by default.
    Owner,
    /// A share link's viewer: the viewer page, its socket and static files.
    Viewer,
    /// A host's, without a user identity: the token it carries (an invite,
    /// or its per-host token) is the credential, checked by the handler.
    /// Or an end-to-end channel, whose handshake is the credential.
    /// Joining is how a tagged sandbox node adds itself; dialing in and
    /// pushing history are how a dial-out host reaches us. A forge's
    /// webhook (M40) is signed with a secret only this daemon and the forge
    /// hold.
    Token,
    /// MCP with a bearer token (M16): an MCP client without a tailnet
    /// identity of its own, or an agent block's.
    McpToken,
    /// The sign-in link (`localauth.rs`), whose token the handler checks.
    SignIn,
    /// A CORS preflight.
    Preflight,
}

/// Exactly `/share/<token>`, `/share/<token>/ws`, `/assets/<file>` or the
/// icon: nothing with dots that a router or proxy might resolve elsewhere.
fn viewer_path(path: &str) -> bool {
    let plain = |s: &str| !s.is_empty() && !s.starts_with('.') && !s.contains('%');
    match path.trim_start_matches('/').split('/').collect::<Vec<_>>().as_slice() {
        ["share", token] | ["share", token, "ws"] => plain(token),
        ["assets", file] => plain(file),
        ["icon.svg"] | ["favicon.ico"] => true,
        _ => false,
    }
}

fn class(req: &Request, access: &Access) -> Class {
    use axum::http::Method;
    let (m, path) = (req.method(), req.uri().path());
    if m == Method::GET && path == crate::localauth::AUTH_PATH {
        return Class::SignIn;
    }
    if m == Method::OPTIONS
        && req.headers().contains_key(header::ACCESS_CONTROL_REQUEST_METHOD)
        && req.headers().contains_key(header::ORIGIN)
    {
        return Class::Preflight;
    }
    if (m == Method::POST && path == crate::hosts::JOIN_PATH)
        || (m == Method::GET && path == crate::dial::DIAL_PATH)
        || (m == Method::GET && path == crate::e2e::PATH)
        || path.starts_with(crate::sync::PUSH_PREFIX)
        || (m == Method::POST && [crate::forge::live::FORGEJO_PATH, crate::forge::live::GITLAB_PATH].contains(&path))
    {
        Class::Token
    } else if path == crate::mcp::PATH
        && req.headers().get(header::AUTHORIZATION).is_some()
        && !access.local_bearer(req.headers())
    {
        Class::McpToken
    } else if m == Method::GET && viewer_path(path) {
        Class::Viewer
    } else {
        Class::Owner
    }
}

#[derive(serde::Deserialize)]
struct SignInQuery {
    token: String,
    next: Option<String>,
}

/// A sign-in link (`/auth?token=…[&next=/path]`): the local token as a
/// cookie for this browser, then the page. A year, so a browser stays
/// signed in; `arugula web` signs it in again after the token changes.
async fn signin(State(app): State<Arc<App>>, axum::extract::Query(q): axum::extract::Query<SignInQuery>) -> Response {
    if !app.access.is_local_token(&q.token) {
        let why = "That sign-in link isn't this daemon's (its token changed, or it's another daemon's).\n\n\
                   Run this in a terminal here for a new one:\n\narugula web";
        return (
            StatusCode::UNAUTHORIZED,
            axum::response::Html(crate::access::refusal_page(StatusCode::UNAUTHORIZED, why)),
        )
            .into_response();
    }
    // Only a path of ours: never another site.
    let next = q.next.filter(|n| n.starts_with('/') && !n.starts_with("//") && !n.contains('\\')).unwrap_or("/".into());
    let cookie =
        format!("{}={}; Path=/; HttpOnly; SameSite=Lax; Max-Age=31536000", app.access.cookie_name(), q.token.trim());
    let mut res = Response::builder().status(StatusCode::SEE_OTHER).header(header::LOCATION, next);
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        res = res.header(header::SET_COOKIE, v);
    }
    res.header(header::CACHE_CONTROL, "no-store")
        .header(header::REFERRER_POLICY, "no-referrer")
        .body(Body::empty())
        .unwrap()
}

async fn stop() -> &'static str {
    crate::STOP.notify_one();
    "stopping"
}

/// `arugula web`'s link: the local token in a sign-in link.
async fn signin_link(State(app): State<Arc<App>>) -> Response {
    match app.access.signin_link() {
        Some(url) => axum::Json(serde_json::json!({ "url": url })).into_response(),
        None => {
            (StatusCode::NOT_FOUND, "this daemon has no local token (it's reached through a tunnel)").into_response()
        }
    }
}

async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    match Assets::get(path) {
        Some(file) => {
            // Vite fingerprints everything under assets/; the rest must revalidate.
            let cache = if path.starts_with("assets/") { "public, max-age=31536000, immutable" } else { "no-cache" };
            Response::builder()
                .header(header::CONTENT_TYPE, file.metadata.mimetype())
                .header(header::CACHE_CONTROL, cache)
                .body(Body::from(file.data))
                .unwrap()
        }
        None if path == "index.html" => (StatusCode::NOT_FOUND, "web client not built: run `just web`").into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn ws(
    State(app): State<Arc<App>>,
    who: Option<Extension<Principal>>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    if let Err((status, why)) = app.access.check_origin(&headers) {
        warn!(%why, "rejected websocket");
        return (status, why).into_response();
    }
    let who = who.map(|Extension(w)| w).unwrap_or(Principal::Owner);
    upgrade.on_upgrade(move |socket| connection(app, socket, who))
}

async fn local_ws(State(app): State<Arc<App>>, upgrade: WebSocketUpgrade) -> Response {
    upgrade.on_upgrade(move |socket| connection(app, socket, Principal::Owner))
}

async fn connection(app: Arc<App>, mut socket: WebSocket, who: Principal) {
    let client: ClientId = app.new_client_id();
    info!(client, who = who.id(), "client connected");
    let (data_tx, mut data_rx) = client_queue();
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel();
    app.hands.connect(client, ctrl_tx.clone(), who.is_owner(), None);
    app.mux.send(Cmd::Connect {
        sub: Subscriber { client, data: data_tx, ctrl: ctrl_tx, principal: who, name: None, device: None },
    });

    loop {
        tokio::select! {
            incoming = socket.recv() => match incoming {
                Some(Ok(msg)) => {
                    if let Err(e) = handle(&app, client, msg) {
                        debug!(client, error = %e, "bad client message");
                    }
                }
                Some(Err(e)) => {
                    debug!(client, error = %e, "websocket error");
                    break;
                }
                None => break,
            },
            Some(out) = ctrl_rx.recv() => {
                if matches!(out, ToClient::Close) || send(&mut socket, out).await.is_err() { break }
            }
            Some(out) = data_rx.recv() => if send(&mut socket, out).await.is_err() { break },
        }
    }
    app.mux.send(Cmd::Disconnect { client });
    app.hands.disconnect(client);
    info!(client, "client disconnected");
}

pub(crate) fn handle(app: &App, client: ClientId, msg: Message) -> anyhow::Result<()> {
    match msg {
        Message::Text(text) => match serde_json::from_str::<ClientMsg>(&text)? {
            ClientMsg::Hand { tools, name } => app.hands.offer(client, tools, name),
            ClientMsg::HandReply { id, result, error } => app.hands.reply(client, id, result, error),
            msg => app.mux.send(Cmd::Msg { client, msg }),
        },
        Message::Binary(bytes) => {
            let frame = Frame::decode(&bytes)?;
            match frame.kind {
                FrameKind::Input => {
                    app.mux.send(Cmd::Input { client: Some(client), pane: frame.pane, data: frame.data })
                }
                k => anyhow::bail!("unexpected frame kind {k:?} from client"),
            }
        }
        _ => {}
    }
    Ok(())
}

async fn send(socket: &mut WebSocket, out: ToClient) -> Result<(), axum::Error> {
    let msg = match out {
        ToClient::Frame(bytes) => Message::Binary(bytes.into()),
        ToClient::Msg(m) => Message::Text(serde_json::to_string(&m).expect("serialize").into()),
        ToClient::Json(t) => Message::Text(t.into()),
        ToClient::Close => return Ok(()),
    };
    socket.send(msg).await
}
