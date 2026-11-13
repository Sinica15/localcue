//! LocalCue desktop app: a Tauri window plus an embedded LAN teleprompter server.

pub mod files;
pub mod hub;
pub mod net;
pub mod protocol;
pub mod server;
pub mod state;
pub mod voice;

use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, RunEvent, State};
use tauri_plugin_dialog::DialogExt;

use hub::{FileInfo, Hub, Session};
use server::{ServerInfo, ServerManager, ServerOptions};

/// Preferences that survive restarts.
#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Config {
    /// Kept across launches so a phone's home-screen shortcut keeps working.
    token: Option<String>,
    interface: Option<String>,
}

struct Services {
    hub: Arc<Hub>,
    server: Arc<ServerManager>,
    session_path: PathBuf,
    config_path: PathBuf,
}

impl Services {
    async fn save_config(&self) {
        let cfg = Config {
            token: Some(self.server.token()),
            interface: self
                .server
                .selected_interface()
                .await
                .map(|ip| ip.to_string()),
        };
        let _ = files::save_json(&self.config_path, &cfg);
    }

    fn save_session(&self) {
        let _ = files::save_json(&self.session_path, &self.hub.session());
    }
}

#[tauri::command]
async fn get_server_info(svc: State<'_, Arc<Services>>) -> Result<ServerInfo, String> {
    Ok(svc.server.info().await)
}

/// Try again after a failure (e.g. the network came back).
#[tauri::command]
async fn restart_server(svc: State<'_, Arc<Services>>) -> Result<ServerInfo, String> {
    Ok(svc.server.start().await)
}

#[tauri::command]
async fn set_interface(
    ip: Option<String>,
    svc: State<'_, Arc<Services>>,
) -> Result<ServerInfo, String> {
    let ip = match ip.filter(|s| !s.is_empty()) {
        Some(s) => Some(
            s.parse::<Ipv4Addr>()
                .map_err(|_| "Invalid network address".to_string())?,
        ),
        None => None,
    };
    let info = svc.server.set_interface(ip).await;
    svc.save_config().await;
    Ok(info)
}

/// New access key: previously scanned QR codes stop working.
#[tauri::command]
async fn rotate_token(svc: State<'_, Arc<Services>>) -> Result<ServerInfo, String> {
    let info = svc.server.rotate_token().await;
    svc.save_config().await;
    Ok(info)
}

#[tauri::command]
fn get_file_info(svc: State<'_, Arc<Services>>) -> FileInfo {
    svc.hub.file_info()
}

// File paths only ever come from the native dialogs (or from the path remembered
// for the open file) — never from the frontend or from the network.

#[tauri::command]
async fn open_script(
    app: AppHandle,
    svc: State<'_, Arc<Services>>,
) -> Result<Option<FileInfo>, String> {
    let picked = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .set_title("Open script")
            .blocking_pick_file()
    })
    .await
    .map_err(|e| e.to_string())?;
    let Some(picked) = picked else {
        return Ok(None);
    };
    let path = picked.into_path().map_err(|e| e.to_string())?;
    svc.hub.open_file(&path).map(Some)
}

#[tauri::command]
async fn save_script(
    app: AppHandle,
    svc: State<'_, Arc<Services>>,
    text: String,
    save_as: bool,
) -> Result<Option<FileInfo>, String> {
    let path = match svc.hub.current_path() {
        Some(p) if !save_as => p,
        _ => {
            let picked = tauri::async_runtime::spawn_blocking(move || {
                app.dialog()
                    .file()
                    .set_title("Save script")
                    .set_file_name("script.txt")
                    .blocking_save_file()
            })
            .await
            .map_err(|e| e.to_string())?;
            let Some(picked) = picked else {
                return Ok(None);
            };
            picked.into_path().map_err(|e| e.to_string())?
        }
    };
    svc.hub.save_file(&path, text).map(Some)
}

#[tauri::command]
fn reload_script(svc: State<'_, Arc<Services>>) -> Result<FileInfo, String> {
    svc.hub.reload_file()
}

pub fn run() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            let session_path = dir.join("session.json");
            let config_path = dir.join("config.json");

            let hub = Arc::new(Hub::new());
            if let Some(s) = files::load_json::<Session>(&session_path) {
                hub.restore(s);
            }
            let cfg: Config = files::load_json(&config_path).unwrap_or_default();
            let server = Arc::new(ServerManager::new(
                hub.clone(),
                ServerOptions {
                    interface: cfg.interface.and_then(|s| s.parse().ok()),
                    token: cfg.token.filter(|t| t.len() >= 16),
                    ..Default::default()
                },
            ));
            // A busy port or missing network is reported in ServerInfo, not fatal.
            tauri::async_runtime::block_on(server.start());

            let svc = Arc::new(Services {
                hub,
                server,
                session_path,
                config_path,
            });
            tauri::async_runtime::block_on(svc.save_config());
            app.manage(svc.clone());

            // Autosave: shortly after the last change.
            let s = svc.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    s.hub.dirty.notified().await;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    s.save_session();
                }
            });

            // Follow the network: new Wi-Fi → new address → new QR code.
            let (s, handle) = (svc, app.handle().clone());
            tauri::async_runtime::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_secs(3)).await;
                    if let Some(info) = s.server.refresh_network().await {
                        let _ = handle.emit("server-info", info);
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_server_info,
            restart_server,
            set_interface,
            rotate_token,
            get_file_info,
            open_script,
            save_script,
            reload_script,
        ])
        .build(tauri::generate_context!())
        .expect("error while building LocalCue");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            if let Some(svc) = handle.try_state::<Arc<Services>>() {
                svc.save_session();
                tauri::async_runtime::block_on(svc.server.shutdown());
            }
        }
    });
}
