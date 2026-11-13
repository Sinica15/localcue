//! End-to-end tests: real server on loopback, real WebSocket clients.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use localcue_lib::hub::Hub;
use localcue_lib::server::{ServerManager, ServerOptions};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

struct Fixture {
    hub: Arc<Hub>,
    server: ServerManager,
    port: u16,
    token: String,
}

async fn start_with(opts: ServerOptions, hub: Arc<Hub>) -> Fixture {
    let server = ServerManager::new(
        hub.clone(),
        ServerOptions {
            enable_lan: false,
            ..opts
        },
    );
    let info = server.start().await;
    let port = info.loopback_port.expect("loopback listener");
    let token = server.token();
    Fixture {
        hub,
        server,
        port,
        token,
    }
}

async fn start() -> Fixture {
    start_with(
        ServerOptions {
            preferred_port: 0,
            ..Default::default()
        },
        Arc::new(Hub::new()),
    )
    .await
}

async fn client(f: &Fixture, role: &str) -> Ws {
    let url = format!("ws://127.0.0.1:{}/ws?t={}&role={role}", f.port, f.token);
    connect_async(url).await.expect("ws connect").0
}

async fn recv(ws: &mut Ws) -> Value {
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(3), ws.next())
            .await
            .expect("timed out waiting for a message")
            .expect("socket closed")
            .expect("ws error");
        if let Message::Text(t) = msg {
            return serde_json::from_str(&t).unwrap();
        }
    }
}

/// Next message of the given type (skipping e.g. `clients` notifications).
async fn recv_t(ws: &mut Ws, t: &str) -> Value {
    loop {
        let v = recv(ws).await;
        if v["t"] == t {
            return v;
        }
    }
}

async fn say(ws: &mut Ws, v: Value) {
    ws.send(Message::Text(v.to_string().into())).await.unwrap();
}

/// Assert that no message of type `t` arrives soon.
async fn expect_silence(ws: &mut Ws, t: &str) {
    let r = tokio::time::timeout(Duration::from_millis(300), async {
        loop {
            if recv(ws).await["t"] == t {
                return;
            }
        }
    })
    .await;
    assert!(r.is_err(), "unexpected `{t}` message");
}

async fn http_get(port: u16, path: &str) -> (u16, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    s.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes(),
    )
    .await
    .unwrap();
    let mut buf = String::new();
    s.read_to_string(&mut buf).await.unwrap();
    let status = buf.split_whitespace().nth(1).unwrap().parse().unwrap();
    (status, buf)
}

#[tokio::test]
async fn serves_the_phone_page_only_with_the_token() {
    let f = start().await;
    let (code, body) = http_get(f.port, &format!("/view?t={}", f.token)).await;
    assert_eq!(code, 200);
    assert!(body.contains("LocalCue") && body.contains("/assets/mobile.js"));
    assert!(body.to_lowercase().contains("content-security-policy"));

    assert_eq!(http_get(f.port, "/view").await.0, 403);
    assert_eq!(http_get(f.port, "/view?t=wrong").await.0, 403);

    for asset in ["mobile.js", "textpos.js", "wsclient.js", "nosleep.js"] {
        let (code, body) = http_get(f.port, &format!("/assets/{asset}")).await;
        assert_eq!(code, 200, "{asset}");
        assert!(body.contains("javascript"), "{asset} content type");
    }
    f.server.shutdown().await;
}

#[tokio::test]
async fn http_exposes_no_files() {
    let f = start().await;
    for path in [
        "/",
        "/Cargo.toml",
        "/assets/../Cargo.toml",
        "/assets/%2e%2e/Cargo.toml",
        "/etc/passwd",
        "/assets/nope.js",
    ] {
        assert_eq!(http_get(f.port, path).await.0, 404, "{path}");
    }
    f.server.shutdown().await;
}

#[tokio::test]
async fn websocket_requires_token_and_a_local_desktop() {
    let f = start().await;
    let bad = format!("ws://127.0.0.1:{}/ws?t=nope&role=mobile", f.port);
    assert!(connect_async(bad).await.is_err());
    let none = format!("ws://127.0.0.1:{}/ws?role=mobile", f.port);
    assert!(connect_async(none).await.is_err());
    // wrong browser origin is refused even with a valid token
    let mut req = tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(
        format!("ws://127.0.0.1:{}/ws?t={}&role=mobile", f.port, f.token),
    )
    .unwrap();
    req.headers_mut()
        .insert("Origin", "https://evil.example".parse().unwrap());
    assert!(connect_async(req).await.is_err());
    f.server.shutdown().await;
}

#[tokio::test]
async fn new_client_gets_the_full_state() {
    let f = start().await;
    let mut desktop = client(&f, "desktop").await;
    recv_t(&mut desktop, "init").await;
    say(
        &mut desktop,
        json!({"t":"text_update","text":"Hello, teleprompter"}),
    )
    .await;
    say(
        &mut desktop,
        json!({"t":"settings_update","fontSize":60,"flipped":false}),
    )
    .await;
    say(&mut desktop, json!({"t":"position","pos":7.5})).await;
    say(
        &mut desktop,
        json!({"t":"play_control","action":"speed","speed":42}),
    )
    .await;

    // a phone arriving later sees everything (poll: the desktop's messages are async)
    let mut init = Value::Null;
    for _ in 0..50 {
        let mut phone = client(&f, "mobile").await;
        init = recv_t(&mut phone, "init").await;
        if init["speed"] == 42 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(init["text"], "Hello, teleprompter");
    assert_eq!(init["settings"]["fontSize"], 60);
    assert_eq!(init["settings"]["flipped"], false);
    assert_eq!(init["settings"]["marginPct"], 15);
    assert_eq!(init["pos"], 7.5);
    assert_eq!(init["speed"], 42);
    assert_eq!(init["playing"], false);
    assert_eq!(init["capabilities"]["voice"], false);
    f.server.shutdown().await;
}

#[tokio::test]
async fn text_scroll_and_playback_reach_every_phone_but_not_the_sender() {
    let f = start().await;
    let mut desktop = client(&f, "desktop").await;
    let mut p1 = client(&f, "mobile").await;
    let mut p2 = client(&f, "mobile").await;
    for ws in [&mut desktop, &mut p1, &mut p2] {
        recv_t(ws, "init").await;
    }

    say(
        &mut desktop,
        json!({"t":"text_update","text":"one two three"}),
    )
    .await;
    say(&mut desktop, json!({"t":"position","pos":4.0})).await;
    say(
        &mut desktop,
        json!({"t":"settings_update","brightness":80,"marginPct":5}),
    )
    .await;
    say(
        &mut desktop,
        json!({"t":"play_control","action":"play","speed":55}),
    )
    .await;
    say(
        &mut desktop,
        json!({"t":"play_control","action":"skip","speed":55,"seconds":-6}),
    )
    .await;

    for ws in [&mut p1, &mut p2] {
        assert_eq!(recv_t(ws, "text").await["text"], "one two three");
        assert_eq!(recv_t(ws, "position").await["pos"], 4.0);
        let s = recv_t(ws, "settings").await;
        assert_eq!(
            (s["brightness"].clone(), s["marginPct"].clone()),
            (json!(80), json!(5))
        );
        let play = recv_t(ws, "play_control").await;
        assert_eq!(
            (play["action"].clone(), play["speed"].clone()),
            (json!("play"), json!(55))
        );
        let skip = recv_t(ws, "play_control").await;
        assert_eq!(
            (skip["action"].clone(), skip["seconds"].clone()),
            (json!("skip"), json!(-6.0))
        );
    }
    // the sender never hears its own updates (no feedback loop)
    for t in ["text", "position", "settings", "play_control"] {
        expect_silence(&mut desktop, t).await;
    }

    // phone (p2 reports first and holds the lead) scrolls while playing: desktop follows, other phone is left alone
    say(&mut p2, json!({"t":"position","pos":9.0})).await;
    assert_eq!(recv_t(&mut desktop, "position").await["pos"], 9.0);
    expect_silence(&mut p1, "position").await;
    expect_silence(&mut p2, "position").await;
    // a phone without the lead cannot move anyone
    say(&mut p1, json!({"t":"position","pos":1.0})).await;
    expect_silence(&mut desktop, "position").await;

    // phone reaches the end of the script: everyone learns playback stopped
    say(&mut p2, json!({"t":"play_state","playing":false})).await;
    assert_eq!(recv_t(&mut desktop, "play_state").await["playing"], false);
    assert_eq!(recv_t(&mut p1, "play_state").await["playing"], false);
    f.server.shutdown().await;
}

#[tokio::test]
async fn phones_cannot_control_the_prompter() {
    let f = start().await;
    let mut desktop = client(&f, "desktop").await;
    let mut phone = client(&f, "mobile").await;
    recv_t(&mut desktop, "init").await;
    recv_t(&mut phone, "init").await;
    say(&mut phone, json!({"t":"text_update","text":"hijack"})).await;
    say(&mut phone, json!({"t":"play_control","action":"play"})).await;
    say(&mut phone, json!({"t":"settings_update","fontSize":96})).await;
    say(&mut phone, json!("garbage")).await;
    say(&mut phone, json!({"t":"no_such_message"})).await;
    expect_silence(&mut desktop, "text").await;
    expect_silence(&mut desktop, "play_control").await;
    expect_silence(&mut desktop, "settings").await;
    // and the connection survived the garbage
    say(&mut phone, json!({"t":"ping"})).await;
    recv_t(&mut phone, "pong").await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn desktop_sees_how_many_phones_are_connected() {
    let f = start().await;
    let mut desktop = client(&f, "desktop").await;
    assert_eq!(recv_t(&mut desktop, "init").await["mobile"], 0);
    // wait for the next `clients` message that reports `n` phones
    async fn until(ws: &mut Ws, n: u64) {
        loop {
            if recv_t(ws, "clients").await["mobile"] == n {
                return;
            }
        }
    }
    let p1 = client(&f, "mobile").await;
    until(&mut desktop, 1).await;
    let p2 = client(&f, "mobile").await;
    until(&mut desktop, 2).await;
    drop(p1);
    until(&mut desktop, 1).await;
    drop(p2);
    until(&mut desktop, 0).await;
    f.server.shutdown().await;
}

#[tokio::test]
async fn a_reconnecting_phone_recovers_the_current_state() {
    let f = start().await;
    let mut desktop = client(&f, "desktop").await;
    recv_t(&mut desktop, "init").await;
    say(
        &mut desktop,
        json!({"t":"text_update","text":"before the drop"}),
    )
    .await;

    let mut phone = client(&f, "mobile").await;
    recv_t(&mut phone, "init").await;
    drop(phone); // Wi-Fi interruption

    // things change while it is away
    say(
        &mut desktop,
        json!({"t":"text_update","text":"after the drop"}),
    )
    .await;
    say(&mut desktop, json!({"t":"settings_update","fontSize":72})).await;
    say(&mut desktop, json!({"t":"position","pos":6.0})).await;
    say(
        &mut desktop,
        json!({"t":"play_control","action":"play","speed":20}),
    )
    .await;
    // wait until the server applied them
    for _ in 0..100 {
        if f.hub.session().speed == 20 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mut phone = client(&f, "mobile").await;
    let init = recv_t(&mut phone, "init").await;
    assert_eq!(init["text"], "after the drop");
    assert_eq!(init["settings"]["fontSize"], 72);
    assert_eq!(init["pos"], 6.0);
    assert_eq!(init["playing"], true);
    assert_eq!(init["speed"], 20);
    f.server.shutdown().await;
}

#[tokio::test]
async fn busy_ports_fall_back_to_another_one() {
    // Occupy a port, then ask the server for it.
    let blocker = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = blocker.local_addr().unwrap().port();
    let f = start_with(
        ServerOptions {
            preferred_port: busy,
            ..Default::default()
        },
        Arc::new(Hub::new()),
    )
    .await;
    assert_ne!(f.port, busy);
    assert_eq!(
        http_get(f.port, &format!("/view?t={}", f.token)).await.0,
        200
    );
    f.server.shutdown().await;
}

#[tokio::test]
async fn shutdown_closes_clients_and_frees_the_port() {
    let f = start().await;
    let mut phone = client(&f, "mobile").await;
    recv_t(&mut phone, "init").await;

    f.server.shutdown().await;

    // the client sees the connection end
    let end = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match phone.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                _ => {}
            }
        }
    })
    .await;
    assert!(end.is_ok(), "client was not disconnected");
    // and the port can be bound again
    assert!(std::net::TcpListener::bind(("127.0.0.1", f.port)).is_ok());
    assert!(connect_async(format!(
        "ws://127.0.0.1:{}/ws?t={}&role=mobile",
        f.port, f.token
    ))
    .await
    .is_err());
}

#[tokio::test]
async fn server_can_be_restarted_and_keeps_its_state() {
    let hub = Arc::new(Hub::new());
    let f = start_with(
        ServerOptions {
            preferred_port: 0,
            ..Default::default()
        },
        hub.clone(),
    )
    .await;
    let mut desktop = client(&f, "desktop").await;
    recv_t(&mut desktop, "init").await;
    say(&mut desktop, json!({"t":"text_update","text":"persist me"})).await;
    for _ in 0..100 {
        if hub.session().text == "persist me" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    f.server.shutdown().await;

    let f2 = start_with(
        ServerOptions {
            preferred_port: 0,
            token: Some(f.token.clone()),
            ..Default::default()
        },
        hub,
    )
    .await;
    let mut phone = client(&f2, "mobile").await;
    assert_eq!(recv_t(&mut phone, "init").await["text"], "persist me");
    f2.server.shutdown().await;
}

#[tokio::test]
async fn rotating_the_token_disconnects_phones_and_invalidates_old_links() {
    let f = start().await;
    let mut phone = client(&f, "mobile").await;
    recv_t(&mut phone, "init").await;
    let old = f.token.clone();

    f.server.rotate_token().await;

    let ended = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match phone.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                _ => {}
            }
        }
    })
    .await;
    assert!(ended.is_ok());
    assert!(
        connect_async(format!("ws://127.0.0.1:{}/ws?t={old}&role=mobile", f.port))
            .await
            .is_err()
    );
    assert_eq!(http_get(f.port, &format!("/view?t={old}")).await.0, 403);
    assert_eq!(
        http_get(f.port, &format!("/view?t={}", f.server.token()))
            .await
            .0,
        200
    );
    f.server.shutdown().await;
}
