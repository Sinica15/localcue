//! Voice-follow is not available in this build.
//!
//! The original Python app used Vosk (offline speech recognition) fed from
//! PulseAudio/PipeWire. A cross-platform Rust port would need an audio-capture
//! crate (e.g. `cpal`), a bundled native Vosk or Whisper runtime plus a ~40 MB
//! model, and macOS/Windows microphone permission handling — far too heavy to
//! ship half-working. The word-matching logic is kept in `legacy/voice.py` for
//! a future port.
//!
//! Everything that depends on voice goes through `AVAILABLE`: it is sent to the
//! clients as `capabilities.voice`, and the UI only shows voice controls when it
//! is true. Flip it (and add the implementation here) when voice-follow lands.

pub const AVAILABLE: bool = false;
