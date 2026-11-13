//! JSON messages exchanged over the WebSocket (`{"t": "<type>", ...}`).
//!
//! These replace the Socket.IO events of the original Flask app. Field names are
//! camelCase so the JavaScript clients can use them as-is.

use serde::{Deserialize, Serialize};

/// Display settings shown on the phones.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub font_size: u32,
    pub margin_pct: u32,
    pub flipped: bool,
    pub brightness: u32,
    /// Clockwise rotation of the phone display: 0, 90, 180 or 270.
    pub rotation: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            font_size: 36,
            margin_pct: 15,
            flipped: true,
            brightness: 25,
            rotation: 0,
        }
    }
}

/// A partial settings update; absent fields keep their current value.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SettingsPatch {
    pub font_size: Option<u32>,
    pub margin_pct: Option<u32>,
    pub flipped: Option<bool>,
    pub brightness: Option<u32>,
    pub rotation: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PlayAction {
    Play,
    Pause,
    Skip,
    Speed,
}

/// Messages sent by clients.
#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Position on the reading line, as a fractional UTF-16 character offset.
    Position {
        pos: f64,
    },
    /// Desktop → phones: play / pause / skip / speed.
    PlayControl {
        action: PlayAction,
        speed: Option<u32>,
        /// Skip distance in seconds of scrolling at the current speed…
        seconds: Option<f64>,
        /// …or in text lines (what the arrow keys use).
        lines: Option<f64>,
    },
    /// Phone → everyone: auto-scroll stopped by itself (end of script).
    PlayState {
        playing: bool,
    },
    TextUpdate {
        text: String,
    },
    SettingsUpdate(SettingsPatch),
    Ping,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    /// Voice-follow is not available in this build (see `voice.rs`).
    pub voice: bool,
}

/// Full application state, sent to every client right after it connects.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub text: String,
    pub settings: Settings,
    pub pos: f64,
    pub playing: bool,
    pub speed: u32,
    pub mobile: usize,
    pub capabilities: Capabilities,
    /// Build id of the phone page this server serves (see `server::build_id`).
    pub build: String,
}

/// Messages sent by the server.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum ServerMsg {
    Init(Snapshot),
    Text {
        text: String,
    },
    Settings(Settings),
    Position {
        pos: f64,
    },
    PlayControl {
        action: PlayAction,
        speed: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        seconds: Option<f64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        lines: Option<f64>,
    },
    PlayState {
        playing: bool,
    },
    /// Number of connected phones.
    Clients {
        mobile: usize,
    },
    Pong,
}
