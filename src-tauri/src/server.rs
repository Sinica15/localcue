//! Embedded HTTP/WebSocket server.
//!
//! * `GET /view?t=TOKEN` – the browser teleprompter for phones
//! * `GET /assets/*`     – its three scripts (embedded in the binary, nothing is read from disk)
//! * `GET /ws?t=TOKEN&role=mobile|desktop` – the sync WebSocket
//!
//! Two listeners share one router: one on 127.0.0.1 for the desktop window and one
//! on the chosen LAN address for phones. The server never binds 0.0.0.0, so it is
//! not reachable through VPN / public interfaces, and a middleware additionally
//! rejects any peer that is not on a private or loopback address.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use futures_util::{SinkExt, StreamExt};
use qrcode::QrCode;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, watch, Mutex};
use tokio::task::JoinHandle;

use crate::hub::{Event, Hub, Role};
use crate::net::{self, Iface};
use crate::protocol::{ClientMsg, ServerMsg};
use crate::voice;

pub const DEFAULT_PORT: u16 = 5858;
const PORT_TRIES: u16 = 20;
const MAX_WS_BYTES: usize = 24 * 1024 * 1024;
/// Clients ping every few seconds; one silent this long is gone (e.g. a phone
/// that dropped off Wi-Fi without closing its socket).
const IDLE_TIMEOUT: Duration = Duration::from_secs(20);
/// Most queued updates handled per wake-up of a client.
const MAX_BATCH: usize = 256;

const MOBILE_HTML: &str = include_str!("../../mobile/mobile.html");
const MOBILE_JS: &str = include_str!("../../mobile/mobile.js");
const TEXTPOS_JS: &str = include_str!("../../shared/textpos.js");
const NOSLEEP_JS: &str = include_str!("../../mobile/nosleep.js");
const WSCLIENT_JS: &str = include_str!("../../shared/wsclient.js");

/// Identifies this build of the phone page. A phone that is still running an older page
/// (the app was updated or restarted while it stayed open) reloads itself when it sees a
/// different id in the `init` message.
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let mut h: u64 = 0xcbf29ce484222325; // FNV-1a
        for part in [MOBILE_HTML, MOBILE_JS, TEXTPOS_JS, WSCLIENT_JS, NOSLEEP_JS] {
            for b in part.bytes() {
                h = (h ^ u64::from(b)).wrapping_mul(0x100000001b3);
            }
        }
        format!("{h:016x}")
    })
}

// ---------------------------------------------------------------- HTTP / WS

#[derive(Clone)]
struct Ctx {
    hub: Arc<Hub>,
    token: Arc<RwLock<String>>,
    shutdown: watch::Receiver<bool>,
}

impl Ctx {
    fn token_ok(&self, given: Option<&str>) -> bool {
        let token = self.token.read().unwrap_or_else(|e| e.into_inner());
        given.is_some_and(|g| constant_time_eq(g.as_bytes(), token.as_bytes()))
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Browsers don't apply the same-origin policy to WebSockets, so refuse pages from
/// other sites. Allowed: no Origin (non-browser client), the same host that served
/// the page, and the desktop webview / dev server.
pub fn origin_allowed(origin: Option<&str>, host: Option<&str>) -> bool {
    let Some(origin) = origin else { return true };
    let authority = origin
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(origin);
    matches!(
        origin,
        "tauri://localhost" | "http://tauri.localhost" | "https://tauri.localhost"
    ) || authority.starts_with("localhost:1420")
        || host.is_some_and(|h| h == authority)
}

async fn lan_only(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if net::is_lan_peer(peer.ip()) {
        next.run(req).await
    } else {
        (
            StatusCode::FORBIDDEN,
            "LocalCue only serves the local network.",
        )
            .into_response()
    }
}

fn router(ctx: Ctx) -> Router {
    Router::new()
        .route("/view", get(view))
        .route("/assets/mobile.js", get(|| async { script(MOBILE_JS) }))
        .route("/assets/textpos.js", get(|| async { script(TEXTPOS_JS) }))
        .route("/assets/wsclient.js", get(|| async { script(WSCLIENT_JS) }))
        .route("/assets/nosleep.js", get(|| async { script(NOSLEEP_JS) }))
        .route("/ws", get(ws_handler))
        .fallback(|| async { (StatusCode::NOT_FOUND, "Not found") })
        .layer(middleware::from_fn(lan_only))
        .with_state(ctx)
}

fn hardened(mut resp: Response, content_type: &'static str) -> Response {
    let h = resp.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'self'; script-src 'self'; media-src data:; style-src 'self' 'unsafe-inline'; connect-src 'self' ws: wss:; frame-ancestors 'none'",
        ),
    );
    resp
}

fn script(body: &'static str) -> Response {
    hardened(body.into_response(), "text/javascript; charset=utf-8")
}

#[derive(Deserialize)]
struct TokenQuery {
    t: Option<String>,
    role: Option<String>,
}

async fn view(State(ctx): State<Ctx>, Query(q): Query<TokenQuery>) -> Response {
    if !ctx.token_ok(q.t.as_deref()) {
        return hardened(
            (StatusCode::FORBIDDEN,
             "<!doctype html><meta name=viewport content='width=device-width'><body style='font-family:sans-serif;padding:2em'>\
              <h2>LocalCue</h2><p>This link is missing its access key. Scan the QR code in the LocalCue desktop app again.</p>")
                .into_response(),
            "text/html; charset=utf-8",
        );
    }
    hardened(
        MOBILE_HTML.replace("__BUILD__", build_id()).into_response(),
        "text/html; charset=utf-8",
    )
}

async fn ws_handler(
    State(ctx): State<Ctx>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Query(q): Query<TokenQuery>,
    ws: WebSocketUpgrade,
) -> Response {
    if !ctx.token_ok(q.t.as_deref()) {
        return (StatusCode::FORBIDDEN, "bad token").into_response();
    }
    let header = |name| headers.get(name).and_then(|v| v.to_str().ok());
    if !origin_allowed(header(header::ORIGIN), header(header::HOST)) {
        return (StatusCode::FORBIDDEN, "origin not allowed").into_response();
    }
    let role = match q.role.as_deref() {
        // Only the local desktop window may act as the controller.
        Some("desktop") if peer.ip().is_loopback() => Role::Desktop,
        Some("desktop") => {
            return (StatusCode::FORBIDDEN, "desktop role is local only").into_response()
        }
        _ => Role::Mobile,
    };
    ws.max_message_size(MAX_WS_BYTES)
        .on_upgrade(move |socket| client_loop(ctx, socket, role))
}

async fn send(sink: &mut (impl SinkExt<Message> + Unpin), msg: &ServerMsg) -> Result<(), ()> {
    let json = serde_json::to_string(msg).map_err(|_| ())?;
    sink.send(Message::Text(json.into())).await.map_err(|_| ())
}

/// Positions are absolute, so of several queued ones only the newest matters: a slow
/// phone skips straight to the latest instead of replaying a backlog.
fn coalesce_positions(events: Vec<Event>) -> Vec<Arc<ServerMsg>> {
    let msgs: Vec<Arc<ServerMsg>> = events
        .into_iter()
        .filter_map(|e| match e {
            Event::Msg { msg, .. } => Some(msg),
            Event::KickMobiles => None,
        })
        .collect();
    let is_pos = |m: &Arc<ServerMsg>| matches!(**m, ServerMsg::Position { .. });
    let last = msgs.iter().rposition(is_pos);
    msgs.into_iter()
        .enumerate()
        .filter(|(i, m)| !is_pos(m) || Some(*i) == last)
        .map(|(_, m)| m)
        .collect()
}

async fn client_loop(ctx: Ctx, socket: WebSocket, role: Role) {
    let (id, mut rx, init) = ctx.hub.connect(role);
    let (mut sink, mut stream) = socket.split();
    let mut shutdown = ctx.shutdown.clone();
    let mut last_heard = tokio::time::Instant::now();
    let mut idle_check = tokio::time::interval(Duration::from_secs(5));

    if send(&mut sink, &init).await.is_ok() {
        loop {
            tokio::select! {
                _ = shutdown.changed() => break,
                _ = idle_check.tick() => {
                    if last_heard.elapsed() > IDLE_TIMEOUT { break; }
                }
                incoming = stream.next() => {
                    last_heard = tokio::time::Instant::now();
                    match incoming {
                    Some(Ok(Message::Text(text))) => {
                        if let Ok(msg) = serde_json::from_str::<ClientMsg>(&text) {
                            if let Some(reply) = ctx.hub.handle(id, msg) {
                                if send(&mut sink, &reply).await.is_err() { break; }
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                    Some(Ok(_)) => {} // pings are answered by axum; binary is ignored
                    }
                },
                event = rx.recv() => {
                    // Take everything already queued for this client in one go.
                    let mut batch = Vec::new();
                    let (mut resync, mut closed) = (false, false);
                    match event {
                        Ok(e) => batch.push(e),
                        Err(broadcast::error::RecvError::Lagged(_)) => resync = true,
                        Err(broadcast::error::RecvError::Closed) => closed = true,
                    }
                    while !closed && batch.len() < MAX_BATCH {
                        match rx.try_recv() {
                            Ok(e) => batch.push(e),
                            Err(broadcast::error::TryRecvError::Lagged(_)) => resync = true,
                            Err(broadcast::error::TryRecvError::Empty) => break,
                            Err(broadcast::error::TryRecvError::Closed) => closed = true,
                        }
                    }
                    let mine: Vec<Event> = batch.into_iter().filter(|e| e.is_for(id, role)).collect();
                    if closed || mine.iter().any(|e| matches!(e, Event::KickMobiles)) { break; }
                    if resync {
                        // Fell behind: the queued updates are older than a fresh snapshot.
                        if send(&mut sink, &ctx.hub.snapshot()).await.is_err() { break; }
                    } else {
                        for msg in coalesce_positions(mine) {
                            if send(&mut sink, &msg).await.is_err() { break; }
                        }
                    }
                }
            }
        }
    }
    ctx.hub.disconnect(id);
}

// ------------------------------------------------------------------ Manager

pub fn new_token() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("OS random number generator unavailable");
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[derive(Clone, Debug)]
pub struct ServerOptions {
    pub preferred_port: u16,
    /// Serve on a LAN address too (tests turn this off).
    pub enable_lan: bool,
    pub interface: Option<Ipv4Addr>,
    pub token: Option<String>,
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            preferred_port: DEFAULT_PORT,
            enable_lan: true,
            interface: None,
            token: None,
        }
    }
}

/// Everything the desktop UI needs to display connection details.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub mobile_url: Option<String>,
    pub qr_svg: Option<String>,
    pub ip: Option<String>,
    pub port: Option<u16>,
    pub desktop_ws: Option<String>,
    pub loopback_port: Option<u16>,
    pub interfaces: Vec<Iface>,
    /// The interface the user picked (None = automatic).
    pub selected_interface: Option<String>,
    /// The desktop window cannot sync (local listener failed).
    pub error: Option<String>,
    /// Phones cannot connect (no network, or no free port).
    pub warning: Option<String>,
    pub voice_available: bool,
}

struct Listener {
    port: u16,
    shutdown: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl Listener {
    async fn stop(mut self) {
        let _ = self.shutdown.send(true);
        if tokio::time::timeout(Duration::from_secs(2), &mut self.task)
            .await
            .is_err()
        {
            self.task.abort();
        }
    }
}

#[derive(Default)]
struct Listeners {
    selected: Option<Ipv4Addr>,
    loopback: Option<Listener>,
    loopback_error: Option<String>,
    lan: Option<(Ipv4Addr, Listener)>,
    lan_error: Option<String>,
    seen: Vec<Iface>,
}

pub struct ServerManager {
    hub: Arc<Hub>,
    token: Arc<RwLock<String>>,
    opts: ServerOptions,
    state: Mutex<Listeners>,
}

async fn bind_with_fallback(ip: Ipv4Addr, preferred: u16) -> Result<TcpListener, String> {
    let mut last = None;
    if preferred != 0 {
        for p in preferred..preferred.saturating_add(PORT_TRIES) {
            match TcpListener::bind((ip, p)).await {
                Ok(l) => return Ok(l),
                Err(e) => last = Some(e),
            }
        }
    }
    // Every preferred port is taken: let the OS choose.
    TcpListener::bind((ip, 0))
        .await
        .map_err(|e| last.unwrap_or(e).to_string())
}

impl ServerManager {
    pub fn new(hub: Arc<Hub>, opts: ServerOptions) -> Self {
        let token = opts.token.clone().unwrap_or_else(new_token);
        let selected = opts.interface;
        Self {
            hub,
            token: Arc::new(RwLock::new(token)),
            opts,
            state: Mutex::new(Listeners {
                selected,
                ..Default::default()
            }),
        }
    }

    pub fn token(&self) -> String {
        self.token.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn selected_interface(&self) -> Option<Ipv4Addr> {
        self.state.lock().await.selected
    }

    fn spawn(&self, listener: TcpListener) -> Listener {
        let port = listener.local_addr().map(|a| a.port()).unwrap_or(0);
        let (tx, rx) = watch::channel(false);
        let ctx = Ctx {
            hub: self.hub.clone(),
            token: self.token.clone(),
            shutdown: rx.clone(),
        };
        let app = router(ctx).into_make_service_with_connect_info::<SocketAddr>();
        let mut stop = rx;
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop.wait_for(|v| *v).await;
                })
                .await;
        });
        Listener {
            port,
            shutdown: tx,
            task,
        }
    }

    /// Start (or restart) all listeners and report what is available.
    pub async fn start(&self) -> ServerInfo {
        let mut st = self.state.lock().await;
        if st.loopback.is_none() {
            match bind_with_fallback(Ipv4Addr::LOCALHOST, self.opts.preferred_port).await {
                Ok(l) => {
                    st.loopback = Some(self.spawn(l));
                    st.loopback_error = None;
                }
                Err(e) => {
                    st.loopback_error = Some(format!("Could not start the local server: {e}"))
                }
            }
        }
        self.restart_lan(&mut st).await;
        self.info_locked(&st)
    }

    /// Which LAN address should be used right now.
    fn desired_ip(st: &Listeners, ifaces: &[Iface]) -> Option<Ipv4Addr> {
        st.selected
            .filter(|ip| ifaces.iter().any(|i| i.ip == *ip))
            .or_else(|| net::auto_pick(ifaces))
    }

    async fn restart_lan(&self, st: &mut Listeners) {
        if let Some((_, l)) = st.lan.take() {
            l.stop().await;
        }
        st.lan_error = None;
        if !self.opts.enable_lan {
            return;
        }
        let ifaces = net::list_interfaces();
        st.seen = ifaces.clone();
        let Some(ip) = Self::desired_ip(st, &ifaces) else {
            st.lan_error = Some(
                "No local network found. Connect this computer to Wi-Fi or Ethernet so phones can reach it.".into(),
            );
            return;
        };
        match bind_with_fallback(ip, self.opts.preferred_port).await {
            Ok(l) => st.lan = Some((ip, self.spawn(l))),
            Err(e) => st.lan_error = Some(format!("Could not open a port on {ip}: {e}")),
        }
    }

    /// Choose the network interface for phones (`None` = automatic).
    pub async fn set_interface(&self, ip: Option<Ipv4Addr>) -> ServerInfo {
        let mut st = self.state.lock().await;
        st.selected = ip;
        self.restart_lan(&mut st).await;
        self.info_locked(&st)
    }

    /// Re-check the machine's addresses; rebinds and returns new info only if
    /// the usable address (or the interface list) changed — e.g. Wi-Fi switched.
    pub async fn refresh_network(&self) -> Option<ServerInfo> {
        if !self.opts.enable_lan {
            return None;
        }
        let mut st = self.state.lock().await;
        let ifaces = net::list_interfaces();
        let want = Self::desired_ip(&st, &ifaces);
        let have = st.lan.as_ref().map(|(ip, _)| *ip);
        // Retry only when the address changed, or when we have none but could have one.
        if want == have && ifaces == st.seen && !(have.is_none() && want.is_some()) {
            return None;
        }
        self.restart_lan(&mut st).await;
        Some(self.info_locked(&st))
    }

    pub async fn info(&self) -> ServerInfo {
        let st = self.state.lock().await;
        self.info_locked(&st)
    }

    /// Issue a new access token; phones using the old one are disconnected.
    pub async fn rotate_token(&self) -> ServerInfo {
        *self.token.write().unwrap_or_else(|e| e.into_inner()) = new_token();
        self.hub.kick_mobiles();
        self.info().await
    }

    fn info_locked(&self, st: &Listeners) -> ServerInfo {
        let token = self.token();
        let lan = st.lan.as_ref().map(|(ip, l)| (*ip, l.port));
        let mobile_url = lan.map(|(ip, port)| format!("http://{ip}:{port}/view?t={token}"));
        ServerInfo {
            qr_svg: mobile_url.as_deref().and_then(qr_svg),
            mobile_url,
            ip: lan.map(|(ip, _)| ip.to_string()),
            port: lan.map(|(_, p)| p),
            desktop_ws: st
                .loopback
                .as_ref()
                .map(|l| format!("ws://127.0.0.1:{}/ws?t={token}&role=desktop", l.port)),
            loopback_port: st.loopback.as_ref().map(|l| l.port),
            interfaces: st.seen.clone(),
            selected_interface: st.selected.map(|ip| ip.to_string()),
            error: st.loopback_error.clone(),
            warning: st.lan_error.clone(),
            voice_available: voice::AVAILABLE,
        }
    }

    /// Stop all listeners and disconnect every client.
    pub async fn shutdown(&self) {
        let mut st = self.state.lock().await;
        if let Some((_, l)) = st.lan.take() {
            l.stop().await;
        }
        if let Some(l) = st.loopback.take() {
            l.stop().await;
        }
    }
}

fn qr_svg(url: &str) -> Option<String> {
    let code = QrCode::new(url.as_bytes()).ok()?;
    Some(
        code.render::<qrcode::render::svg::Color>()
            .min_dimensions(240, 240)
            .quiet_zone(true)
            .build(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_random_hex() {
        let (a, b) = (new_token(), new_token());
        assert_eq!(a.len(), 32);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn constant_time_compare() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"abcd"));
    }

    #[test]
    fn origin_policy() {
        assert!(origin_allowed(None, Some("192.168.1.5:5858")));
        assert!(origin_allowed(
            Some("http://192.168.1.5:5858"),
            Some("192.168.1.5:5858")
        ));
        assert!(origin_allowed(
            Some("tauri://localhost"),
            Some("127.0.0.1:5858")
        ));
        assert!(origin_allowed(
            Some("http://tauri.localhost"),
            Some("127.0.0.1:5858")
        ));
        assert!(!origin_allowed(
            Some("https://evil.example"),
            Some("192.168.1.5:5858")
        ));
        assert!(!origin_allowed(
            Some("http://192.168.1.77:5858"),
            Some("192.168.1.5:5858")
        ));
    }

    #[test]
    fn queued_positions_collapse_to_the_newest() {
        use crate::hub::Audience;
        let ev = |m: ServerMsg| Event::Msg {
            audience: Audience::All,
            msg: Arc::new(m),
        };
        let out = coalesce_positions(vec![
            ev(ServerMsg::Position { pos: 1.0 }),
            ev(ServerMsg::Text { text: "t".into() }),
            ev(ServerMsg::Position { pos: 2.0 }),
            ev(ServerMsg::Position { pos: 3.0 }),
            ev(ServerMsg::PlayState { playing: false }),
        ]);
        let kinds: Vec<String> = out.iter().map(|m| format!("{m:?}")).collect();
        assert_eq!(out.len(), 3, "{kinds:?}");
        assert!(matches!(*out[0], ServerMsg::Text { .. }));
        assert!(matches!(*out[1], ServerMsg::Position { pos } if pos == 3.0));
        assert!(matches!(*out[2], ServerMsg::PlayState { .. }));
    }

    #[test]
    fn qr_svg_is_generated() {
        let svg = qr_svg("http://192.168.1.5:5858/view?t=abc").unwrap();
        assert!(svg.contains("<svg"));
    }
}
