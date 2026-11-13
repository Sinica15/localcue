//! The single authoritative teleprompter state. Pure data + validation, no I/O.

use crate::protocol::{Settings, SettingsPatch};

pub const MAX_TEXT_UTF16: usize = 5_000_000;
pub const MIN_SPEED: u32 = 1;
pub const MAX_SPEED: u32 = 200;

#[derive(Clone, Debug)]
pub struct AppState {
    text: String,
    /// Length of `text` in UTF-16 code units — the unit of `pos`, matching
    /// JavaScript string indices on every client.
    len16: usize,
    pub settings: Settings,
    pos: f64,
    pub playing: bool,
    pub speed: u32,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            text: String::new(),
            len16: 0,
            settings: Settings::default(),
            pos: 0.0,
            playing: false,
            speed: 30,
        }
    }
}

pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

impl AppState {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn pos(&self) -> f64 {
        self.pos
    }

    /// Replace the script. Returns false (and changes nothing) if it is too large.
    /// The position is kept, clamped to the new length.
    pub fn set_text(&mut self, text: String) -> bool {
        let len16 = utf16_len(&text);
        if len16 > MAX_TEXT_UTF16 {
            return false;
        }
        self.text = text;
        self.len16 = len16;
        self.pos = self.pos.clamp(0.0, len16 as f64);
        true
    }

    /// Set the reading-line position; rejects NaN, clamps to `0..=len`.
    pub fn set_pos(&mut self, pos: f64) -> bool {
        if !pos.is_finite() {
            return false;
        }
        self.pos = pos.clamp(0.0, self.len16 as f64);
        true
    }

    pub fn set_speed(&mut self, speed: u32) {
        self.speed = speed.clamp(MIN_SPEED, MAX_SPEED);
    }

    pub fn apply_settings(&mut self, p: &SettingsPatch) {
        let s = &mut self.settings;
        if let Some(v) = p.font_size {
            s.font_size = v.clamp(8, 200);
        }
        if let Some(v) = p.margin_pct {
            s.margin_pct = v.clamp(0, 45);
        }
        if let Some(v) = p.flipped {
            s.flipped = v;
        }
        if let Some(v) = p.brightness {
            s.brightness = v.clamp(0, 100);
        }
        if let Some(v) = p.rotation {
            s.rotation = (v / 90 % 4) * 90;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_is_clamped_to_text_length_in_utf16_units() {
        let mut s = AppState::default();
        assert!(s.set_text("a😀b".into())); // 😀 is 2 UTF-16 units → length 4
        assert!(s.set_pos(100.0));
        assert_eq!(s.pos(), 4.0);
        assert!(s.set_pos(-5.0));
        assert_eq!(s.pos(), 0.0);
        assert!(s.set_pos(2.5));
        assert_eq!(s.pos(), 2.5);
    }

    #[test]
    fn non_finite_positions_are_rejected() {
        let mut s = AppState::default();
        s.set_text("hello".into());
        s.set_pos(3.0);
        assert!(!s.set_pos(f64::NAN));
        assert!(!s.set_pos(f64::INFINITY));
        assert_eq!(s.pos(), 3.0);
    }

    #[test]
    fn shrinking_the_text_clamps_the_position() {
        let mut s = AppState::default();
        s.set_text("hello world".into());
        s.set_pos(9.0);
        s.set_text("hi".into());
        assert_eq!(s.pos(), 2.0);
    }

    #[test]
    fn oversized_text_is_rejected() {
        let mut s = AppState::default();
        s.set_text("keep".into());
        assert!(!s.set_text("x".repeat(MAX_TEXT_UTF16 + 1)));
        assert_eq!(s.text(), "keep");
    }

    #[test]
    fn settings_patch_is_partial_and_clamped() {
        let mut s = AppState::default();
        s.apply_settings(&SettingsPatch {
            font_size: Some(1000),
            brightness: Some(60),
            ..Default::default()
        });
        assert_eq!(s.settings.font_size, 200);
        assert_eq!(s.settings.brightness, 60);
        assert_eq!(s.settings.margin_pct, 15); // untouched
        assert!(s.settings.flipped);
    }

    #[test]
    fn rotation_snaps_to_quarter_turns() {
        let mut s = AppState::default();
        for (given, want) in [(90, 90), (270, 270), (360, 0), (450, 90), (100, 90), (0, 0)] {
            s.apply_settings(&SettingsPatch {
                rotation: Some(given),
                ..Default::default()
            });
            assert_eq!(s.settings.rotation, want, "{given}");
        }
    }

    #[test]
    fn speed_is_clamped() {
        let mut s = AppState::default();
        s.set_speed(0);
        assert_eq!(s.speed, MIN_SPEED);
        s.set_speed(9999);
        assert_eq!(s.speed, MAX_SPEED);
    }
}
