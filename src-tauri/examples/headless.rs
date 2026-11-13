//! Runs the LAN server without the desktop window (development and CI smoke tests).
//!
//!     cargo run --example headless [port]
//!
//! Prints the phone URL and the desktop WebSocket URL, then serves until killed.

use std::sync::Arc;

use localcue_lib::hub::Hub;
use localcue_lib::server::{ServerManager, ServerOptions};

#[tokio::main]
async fn main() {
    let port = std::env::args()
        .nth(1)
        .and_then(|p| p.parse().ok())
        .unwrap_or(5858);
    let server = ServerManager::new(
        Arc::new(Hub::new()),
        ServerOptions {
            preferred_port: port,
            // Fixed token (LOCALCUE_TOKEN) so clients can reconnect across restarts.
            token: std::env::var("LOCALCUE_TOKEN").ok(),
            ..Default::default()
        },
    );
    let info = server.start().await;
    println!(
        "phone:   {}",
        info.mobile_url.as_deref().unwrap_or("(no LAN address)")
    );
    println!(
        "desktop: {}",
        info.desktop_ws.as_deref().unwrap_or("(not running)")
    );
    if let Some(w) = info.warning.or(info.error) {
        println!("note:    {w}");
    }
    std::future::pending::<()>().await;
}
