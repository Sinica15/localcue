//! The hub owns the authoritative state and routes messages between clients.
//!
//! Every connected client (the desktop window and any number of phones) gets an
//! id. A message from a client updates the state and is broadcast to everyone
//! *except its sender*, which is what prevents feedback loops: a position the
//! desktop sends is never echoed back to the desktop, and so on.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Notify};

use crate::files;
use crate::protocol::{Capabilities, ClientMsg, PlayAction, ServerMsg, Settings, Snapshot};
use crate::state::AppState;
use crate::voice;

pub type ClientId = u64;

/// `LOCALCUE_DEBUG=1` prints the sync traffic to stderr (for diagnosing glitches).
fn debug_enabled() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("LOCALCUE_DEBUG").is_some())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Desktop,
    Mobile,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Audience {
    All,
    AllExcept(ClientId),
    Desktops,
    Mobiles,
}

#[derive(Clone, Debug)]
pub enum Event {
    Msg {
        audience: Audience,
        msg: Arc<ServerMsg>,
    },
    /// The access token changed: phones must reconnect with the new one.
    KickMobiles,
}

impl Event {
    /// Whether this event is addressed to the given client.
    pub fn is_for(&self, id: ClientId, role: Role) -> bool {
        match self {
            Event::KickMobiles => role == Role::Mobile,
            Event::Msg { audience, .. } => match audience {
                Audience::All => true,
                Audience::AllExcept(x) => *x != id,
                Audience::Desktops => role == Role::Desktop,
                Audience::Mobiles => role == Role::Mobile,
            },
        }
    }
}

/// What the desktop UI needs to know about the open script file.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileInfo {
    pub name: Option<String>,
    pub path: Option<String>,
    /// Content as last loaded from / saved to disk, for the unsaved-changes marker.
    pub saved_text: String,
}

#[derive(Default)]
struct FileState {
    path: Option<PathBuf>,
    saved_text: String,
}

/// What is written to disk for autosave / restore after a restart.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub text: String,
    pub settings: Settings,
    pub speed: u32,
    pub pos: f64,
    pub file_path: Option<String>,
}

struct Inner {
    state: AppState,
    clients: BTreeMap<ClientId, Role>,
    next_id: ClientId,
    file: FileState,
    /// The phone whose position reports currently count, and when it last sent one.
    lead: Option<(ClientId, Instant)>,
}

/// A phone that keeps reporting holds the lead; if it goes quiet for this long
/// (hidden tab, dead connection) any other phone may take over.
const LEAD_LEASE: Duration = Duration::from_millis(1500);

pub struct Hub {
    inner: Mutex<Inner>,
    tx: broadcast::Sender<Event>,
    /// Signalled whenever something worth autosaving changed.
    pub dirty: Notify,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            inner: Mutex::new(Inner {
                state: AppState::default(),
                clients: BTreeMap::new(),
                next_id: 1,
                file: FileState::default(),
                lead: None,
            }),
            tx,
            dirty: Notify::new(),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn send(&self, audience: Audience, msg: ServerMsg) {
        // Err only means nobody is listening.
        let _ = self.tx.send(Event::Msg {
            audience,
            msg: Arc::new(msg),
        });
    }

    fn snapshot_of(inner: &Inner) -> Snapshot {
        let mobile = inner
            .clients
            .values()
            .filter(|r| **r == Role::Mobile)
            .count();
        Snapshot {
            text: inner.state.text().to_owned(),
            settings: inner.state.settings.clone(),
            pos: inner.state.pos(),
            playing: inner.state.playing,
            speed: inner.state.speed,
            mobile,
            capabilities: Capabilities {
                voice: voice::AVAILABLE,
            },
            build: crate::server::build_id().to_owned(),
        }
    }

    pub fn snapshot(&self) -> ServerMsg {
        ServerMsg::Init(Self::snapshot_of(&self.lock()))
    }

    pub fn mobile_count(&self) -> usize {
        self.lock()
            .clients
            .values()
            .filter(|r| **r == Role::Mobile)
            .count()
    }

    /// Register a client. The receiver is subscribed under the same lock that
    /// builds the snapshot, so no update can fall between the two.
    pub fn connect(&self, role: Role) -> (ClientId, broadcast::Receiver<Event>, ServerMsg) {
        let mut inner = self.lock();
        let id = inner.next_id;
        inner.next_id += 1;
        inner.clients.insert(id, role);
        if debug_enabled() {
            eprintln!("connect #{id} {role:?}");
        }
        let rx = self.tx.subscribe();
        let init = ServerMsg::Init(Self::snapshot_of(&inner));
        let mobile = inner
            .clients
            .values()
            .filter(|r| **r == Role::Mobile)
            .count();
        drop(inner);
        self.send(Audience::All, ServerMsg::Clients { mobile });
        (id, rx, init)
    }

    pub fn disconnect(&self, id: ClientId) {
        let mut inner = self.lock();
        inner.clients.remove(&id);
        if debug_enabled() {
            eprintln!("disconnect #{id}");
        }
        let mobile = inner
            .clients
            .values()
            .filter(|r| **r == Role::Mobile)
            .count();
        drop(inner);
        self.send(Audience::All, ServerMsg::Clients { mobile });
    }

    pub fn kick_mobiles(&self) {
        let _ = self.tx.send(Event::KickMobiles);
    }

    /// Which phone's reports count. Other phones run the same animation, but
    /// their reports would only make everyone jitter between slightly different
    /// wrapped positions. Whoever reports first holds a short lease that each of
    /// its reports renews; a silent holder (hidden tab, dead socket) loses it.
    fn claim_lead(inner: &mut Inner, id: ClientId) -> bool {
        let now = Instant::now();
        if let Some((lead, last)) = inner.lead {
            if lead != id
                && inner.clients.contains_key(&lead)
                && now.duration_since(last) < LEAD_LEASE
            {
                return false;
            }
        }
        inner.lead = Some((id, now));
        true
    }

    /// Apply a message from a client. Returns a direct reply, if any.
    pub fn handle(&self, id: ClientId, msg: ClientMsg) -> Option<ServerMsg> {
        let mut inner = self.lock();
        let role = *inner.clients.get(&id)?;
        if debug_enabled() && !matches!(msg, ClientMsg::Ping | ClientMsg::TextUpdate { .. }) {
            let ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() % 1_000_000)
                .unwrap_or(0);
            eprintln!(
                "[{ms:06}] #{id} {role:?} {msg:?} (server: playing={} pos={:.1})",
                inner.state.playing,
                inner.state.pos()
            );
        }
        let is_primary = role == Role::Mobile
            && matches!(
                msg,
                ClientMsg::Position { .. } | ClientMsg::PlayState { .. }
            )
            && Self::claim_lead(&mut inner, id);
        match (role, msg) {
            (_, ClientMsg::Ping) => return Some(ServerMsg::Pong),

            (Role::Desktop, ClientMsg::Position { pos }) => {
                if inner.state.set_pos(pos) {
                    let pos = inner.state.pos();
                    drop(inner);
                    self.send(Audience::AllExcept(id), ServerMsg::Position { pos });
                }
            }
            (Role::Mobile, ClientMsg::Position { pos }) if is_primary => {
                if inner.state.set_pos(pos) {
                    let (pos, playing) = (inner.state.pos(), inner.state.playing);
                    drop(inner);
                    // While scrolling, the other phones animate on their own; only
                    // the desktop follows. Once settled, everyone gets the position.
                    let audience = if playing {
                        Audience::Desktops
                    } else {
                        Audience::AllExcept(id)
                    };
                    self.send(audience, ServerMsg::Position { pos });
                }
            }

            (
                Role::Desktop,
                ClientMsg::PlayControl {
                    action,
                    speed,
                    seconds,
                    lines,
                },
            ) => {
                if let Some(s) = speed {
                    inner.state.set_speed(s);
                }
                match action {
                    PlayAction::Play => inner.state.playing = true,
                    PlayAction::Pause => inner.state.playing = false,
                    PlayAction::Skip
                        if !seconds.is_some_and(f64::is_finite)
                            && !lines.is_some_and(f64::is_finite) =>
                    {
                        return None
                    }
                    PlayAction::Skip | PlayAction::Speed => {}
                }
                let speed = inner.state.speed;
                drop(inner);
                self.send(
                    Audience::AllExcept(id),
                    ServerMsg::PlayControl {
                        action,
                        speed,
                        seconds,
                        lines,
                    },
                );
            }

            (Role::Mobile, ClientMsg::PlayState { playing }) if is_primary => {
                inner.state.playing = playing;
                drop(inner);
                self.send(Audience::AllExcept(id), ServerMsg::PlayState { playing });
            }

            (Role::Desktop, ClientMsg::TextUpdate { text }) => {
                if inner.state.set_text(text.clone()) {
                    drop(inner);
                    self.send(Audience::AllExcept(id), ServerMsg::Text { text });
                    self.dirty.notify_one();
                }
            }

            (Role::Desktop, ClientMsg::SettingsUpdate(patch)) => {
                inner.state.apply_settings(&patch);
                let settings = inner.state.settings.clone();
                drop(inner);
                self.send(Audience::AllExcept(id), ServerMsg::Settings(settings));
                self.dirty.notify_one();
            }

            // Anything else (e.g. a phone trying to change the text) is ignored.
            _ => {}
        }
        None
    }

    // ---- Script files (driven by the desktop's native dialogs) ----

    pub fn file_info(&self) -> FileInfo {
        let inner = self.lock();
        Self::file_info_of(&inner)
    }

    fn file_info_of(inner: &Inner) -> FileInfo {
        FileInfo {
            name: inner
                .file
                .path
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned()),
            path: inner
                .file
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
            saved_text: inner.file.saved_text.clone(),
        }
    }

    fn replace_text_everywhere(
        &self,
        mut inner: MutexGuard<'_, Inner>,
        text: String,
    ) -> Result<(), String> {
        if !inner.state.set_text(text.clone()) {
            return Err("script is too large".into());
        }
        inner.state.set_pos(0.0);
        drop(inner);
        self.send(Audience::All, ServerMsg::Text { text });
        self.send(Audience::All, ServerMsg::Position { pos: 0.0 });
        self.dirty.notify_one();
        Ok(())
    }

    /// Load a file into the script and tell every client.
    pub fn open_file(&self, path: &Path) -> Result<FileInfo, String> {
        let text = files::read_script(path).map_err(|e| format!("Could not open file: {e}"))?;
        let mut inner = self.lock();
        inner.file = FileState {
            path: Some(path.to_path_buf()),
            saved_text: text.clone(),
        };
        let info = Self::file_info_of(&inner);
        self.replace_text_everywhere(inner, text)?;
        Ok(info)
    }

    /// Re-read the current file from disk, discarding unsaved edits.
    pub fn reload_file(&self) -> Result<FileInfo, String> {
        let path = self.lock().file.path.clone().ok_or("No file is open")?;
        self.open_file(&path)
    }

    /// Write `text` to `path` and make it the current file. The text becomes the
    /// authoritative script; phones are updated, the caller (desktop) already has it.
    pub fn save_file(&self, path: &Path, text: String) -> Result<FileInfo, String> {
        files::write_atomic(path, &text).map_err(|e| format!("Could not save file: {e}"))?;
        let mut inner = self.lock();
        if inner.state.text() != text {
            if !inner.state.set_text(text.clone()) {
                return Err("script is too large".into());
            }
            self.send(Audience::Mobiles, ServerMsg::Text { text: text.clone() });
        }
        inner.file = FileState {
            path: Some(path.to_path_buf()),
            saved_text: text,
        };
        let info = Self::file_info_of(&inner);
        drop(inner);
        self.dirty.notify_one();
        Ok(info)
    }

    #[cfg(test)]
    fn session_playing(&self) -> bool {
        self.lock().state.playing
    }

    pub fn current_path(&self) -> Option<PathBuf> {
        self.lock().file.path.clone()
    }

    // ---- Autosave / restore ----

    pub fn session(&self) -> Session {
        let inner = self.lock();
        Session {
            text: inner.state.text().to_owned(),
            settings: inner.state.settings.clone(),
            speed: inner.state.speed,
            pos: inner.state.pos(),
            file_path: inner
                .file
                .path
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned()),
        }
    }

    /// Restore a saved session (before any client connects).
    pub fn restore(&self, s: Session) {
        let mut inner = self.lock();
        inner.state.set_text(s.text);
        inner.state.set_pos(s.pos);
        inner.state.settings = s.settings;
        if s.speed > 0 {
            inner.state.set_speed(s.speed);
        }
        if let Some(p) = s.file_path {
            let path = PathBuf::from(p);
            // If the file vanished, saved_text stays empty so the script shows as unsaved.
            let saved_text = files::read_script(&path).unwrap_or_default();
            inner.file = FileState {
                path: Some(path),
                saved_text,
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::broadcast::error::TryRecvError;

    /// Messages a given client would receive (drains its receiver).
    fn inbox(rx: &mut broadcast::Receiver<Event>, id: ClientId, role: Role) -> Vec<ServerMsg> {
        let mut out = Vec::new();
        loop {
            match rx.try_recv() {
                Ok(ev) => {
                    if let Event::Msg { msg, .. } = &ev {
                        if ev.is_for(id, role) {
                            out.push((**msg).clone());
                        }
                    }
                }
                Err(TryRecvError::Empty) => return out,
                Err(e) => panic!("{e:?}"),
            }
        }
    }

    fn without_clients(v: Vec<ServerMsg>) -> Vec<ServerMsg> {
        v.into_iter()
            .filter(|m| !matches!(m, ServerMsg::Clients { .. }))
            .collect()
    }

    fn pos_msg(pos: f64) -> ClientMsg {
        ClientMsg::Position { pos }
    }

    #[test]
    fn new_client_receives_full_state() {
        let hub = Hub::new();
        let (d, _rx, _) = hub.connect(Role::Desktop);
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "hello world".into(),
            },
        );
        hub.handle(d, pos_msg(6.0));
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Play,
                speed: Some(55),
                seconds: None,
                lines: None,
            },
        );

        let (_, _, init) = hub.connect(Role::Mobile);
        let ServerMsg::Init(s) = init else {
            panic!("expected init")
        };
        assert_eq!(s.text, "hello world");
        assert_eq!(s.pos, 6.0);
        assert!(s.playing);
        assert_eq!(s.speed, 55);
        assert_eq!(s.mobile, 1);
        assert!(!s.capabilities.voice);
    }

    #[test]
    fn broadcasts_skip_the_sender() {
        let hub = Hub::new();
        let (d, mut drx, _) = hub.connect(Role::Desktop);
        let (m, mut mrx, _) = hub.connect(Role::Mobile);
        inbox(&mut drx, d, Role::Desktop);
        inbox(&mut mrx, m, Role::Mobile);

        hub.handle(d, ClientMsg::TextUpdate { text: "abc".into() });
        hub.handle(d, pos_msg(1.0));

        assert!(
            without_clients(inbox(&mut drx, d, Role::Desktop)).is_empty(),
            "no echo to sender"
        );
        let got = inbox(&mut mrx, m, Role::Mobile);
        assert!(matches!(&got[0], ServerMsg::Text { text } if text == "abc"));
        assert!(matches!(&got[1], ServerMsg::Position { pos } if *pos == 1.0));
    }

    #[test]
    fn only_the_primary_phone_reports_position_and_phones_do_not_hear_each_other_while_playing() {
        let hub = Hub::new();
        let (d, mut drx, _) = hub.connect(Role::Desktop);
        let (m1, mut r1, _) = hub.connect(Role::Mobile); // older
        let (m2, mut r2, _) = hub.connect(Role::Mobile); // newest = primary
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "x".repeat(1000),
            },
        );
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Play,
                speed: Some(30),
                seconds: None,
                lines: None,
            },
        );
        inbox(&mut drx, d, Role::Desktop);
        inbox(&mut r2, m2, Role::Mobile);
        inbox(&mut r1, m1, Role::Mobile);

        hub.handle(m2, pos_msg(10.0)); // first to report holds the lead; playing → desktop only
        assert!(
            matches!(without_clients(inbox(&mut drx, d, Role::Desktop))[..], [ServerMsg::Position { pos }] if pos == 10.0)
        );
        hub.handle(m1, pos_msg(500.0)); // lead is active → ignored
        assert_eq!(hub.session().pos, 10.0);
        assert!(without_clients(inbox(&mut r1, m1, Role::Mobile)).is_empty());

        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Pause,
                speed: None,
                seconds: None,
                lines: None,
            },
        );
        inbox(&mut r1, m1, Role::Mobile);
        hub.handle(m2, pos_msg(12.0)); // settled → everyone else
        assert!(without_clients(inbox(&mut r1, m1, Role::Mobile))
            .iter()
            .any(|m| matches!(m, ServerMsg::Position { pos } if *pos == 12.0)));

        // the lead leaves → the other phone takes over at once
        hub.disconnect(m2);
        hub.handle(m1, pos_msg(20.0));
        assert_eq!(hub.session().pos, 20.0);
    }

    #[test]
    fn a_silent_lead_loses_the_lease() {
        let hub = Hub::new();
        let (d, _drx, _) = hub.connect(Role::Desktop);
        let (stale, _r1, _) = hub.connect(Role::Mobile);
        let (live, _r2, _) = hub.connect(Role::Mobile);
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "x".repeat(100),
            },
        );
        hub.handle(stale, pos_msg(5.0));
        hub.handle(live, pos_msg(50.0)); // blocked: `stale` just reported
        assert_eq!(hub.session().pos, 5.0);
        std::thread::sleep(LEAD_LEASE + Duration::from_millis(100));
        hub.handle(live, pos_msg(50.0)); // `stale` went quiet → takeover
        assert_eq!(hub.session().pos, 50.0);
        hub.handle(stale, pos_msg(7.0)); // and now it is the one blocked
        assert_eq!(hub.session().pos, 50.0);
    }

    #[test]
    fn phones_cannot_change_text_settings_or_playback() {
        let hub = Hub::new();
        let (d, _drx, _) = hub.connect(Role::Desktop);
        let (m, _mrx, _) = hub.connect(Role::Mobile);
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "original".into(),
            },
        );
        hub.handle(
            m,
            ClientMsg::TextUpdate {
                text: "hijacked".into(),
            },
        );
        hub.handle(
            m,
            ClientMsg::PlayControl {
                action: PlayAction::Play,
                speed: Some(99),
                seconds: None,
                lines: None,
            },
        );
        hub.handle(
            m,
            ClientMsg::SettingsUpdate(crate::protocol::SettingsPatch {
                font_size: Some(90),
                ..Default::default()
            }),
        );
        let s = hub.session();
        assert_eq!(s.text, "original");
        assert_eq!(s.settings.font_size, 36);
        assert_eq!(s.speed, 30);
    }

    #[test]
    fn mobile_count_tracks_connections() {
        let hub = Hub::new();
        let (d, mut drx, _) = hub.connect(Role::Desktop);
        inbox(&mut drx, d, Role::Desktop);
        let (m1, _r1, _) = hub.connect(Role::Mobile);
        let (_m2, _r2, _) = hub.connect(Role::Mobile);
        assert_eq!(hub.mobile_count(), 2);
        hub.disconnect(m1);
        assert_eq!(hub.mobile_count(), 1);
        let counts: Vec<usize> = inbox(&mut drx, d, Role::Desktop)
            .into_iter()
            .filter_map(|m| {
                if let ServerMsg::Clients { mobile } = m {
                    Some(mobile)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(counts, vec![1, 2, 1]);
    }

    #[test]
    fn play_state_from_the_phone_updates_the_shared_state() {
        let hub = Hub::new();
        let (d, mut drx, _) = hub.connect(Role::Desktop);
        let (m, _r, _) = hub.connect(Role::Mobile);
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Play,
                speed: None,
                seconds: None,
                lines: None,
            },
        );
        inbox(&mut drx, d, Role::Desktop);
        hub.handle(m, ClientMsg::PlayState { playing: false });
        assert!(without_clients(inbox(&mut drx, d, Role::Desktop))
            .iter()
            .any(|m| matches!(m, ServerMsg::PlayState { playing: false })));
        let (_, _, ServerMsg::Init(s)) = hub.connect(Role::Mobile) else {
            panic!()
        };
        assert!(!s.playing);
    }

    #[test]
    fn skip_requires_seconds() {
        let hub = Hub::new();
        let (d, _drx, _) = hub.connect(Role::Desktop);
        let (m, mut mrx, _) = hub.connect(Role::Mobile);
        inbox(&mut mrx, m, Role::Mobile);
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Skip,
                speed: None,
                seconds: None,
                lines: None,
            },
        );
        assert!(without_clients(inbox(&mut mrx, m, Role::Mobile)).is_empty());
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Skip,
                speed: None,
                seconds: Some(2.0),
                lines: None,
            },
        );
        assert_eq!(without_clients(inbox(&mut mrx, m, Role::Mobile)).len(), 1);
        // a skip in lines (the arrow keys) is accepted too, and does not touch play state
        hub.handle(
            d,
            ClientMsg::PlayControl {
                action: PlayAction::Skip,
                speed: None,
                seconds: None,
                lines: Some(-1.0),
            },
        );
        assert_eq!(without_clients(inbox(&mut mrx, m, Role::Mobile)).len(), 1);
        assert!(!hub.session_playing());
    }

    #[test]
    fn open_save_reload_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.txt");
        std::fs::write(&path, "first draft").unwrap();

        let hub = Hub::new();
        let (d, mut drx, _) = hub.connect(Role::Desktop);
        let (m, mut mrx, _) = hub.connect(Role::Mobile);
        inbox(&mut drx, d, Role::Desktop);
        inbox(&mut mrx, m, Role::Mobile);

        let info = hub.open_file(&path).unwrap();
        assert_eq!(info.name.as_deref(), Some("talk.txt"));
        assert_eq!(info.saved_text, "first draft");
        // open reaches the desktop too (it did not send the text)
        assert!(
            matches!(&without_clients(inbox(&mut drx, d, Role::Desktop))[0], ServerMsg::Text { text } if text == "first draft")
        );
        assert!(matches!(
            &without_clients(inbox(&mut mrx, m, Role::Mobile))[0],
            ServerMsg::Text { .. }
        ));

        // edit on the desktop, then save
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "second draft".into(),
            },
        );
        let info = hub.save_file(&path, "second draft".into()).unwrap();
        assert_eq!(info.saved_text, "second draft");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "second draft");

        // someone edits the file elsewhere; reload picks it up and tells everyone
        std::fs::write(&path, "edited elsewhere").unwrap();
        inbox(&mut drx, d, Role::Desktop);
        let info = hub.reload_file().unwrap();
        assert_eq!(info.saved_text, "edited elsewhere");
        assert_eq!(hub.session().text, "edited elsewhere");
        assert!(!without_clients(inbox(&mut drx, d, Role::Desktop)).is_empty());

        // session round-trips through restore, with unsaved edits preserved
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "unsaved edit".into(),
            },
        );
        let session = hub.session();
        let fresh = Hub::new();
        fresh.restore(session);
        assert_eq!(fresh.session().text, "unsaved edit");
        let info = fresh.file_info();
        assert_eq!(info.name.as_deref(), Some("talk.txt"));
        assert_eq!(info.saved_text, "edited elsewhere"); // ≠ text → shown as unsaved
    }

    #[test]
    fn open_errors_are_reported_and_leave_state_alone() {
        let hub = Hub::new();
        let (d, _rx, _) = hub.connect(Role::Desktop);
        hub.handle(
            d,
            ClientMsg::TextUpdate {
                text: "keep me".into(),
            },
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(hub.open_file(&dir.path().join("missing.txt")).is_err());
        assert!(hub.reload_file().is_err());
        assert_eq!(hub.session().text, "keep me");
    }
}
