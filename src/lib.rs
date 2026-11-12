//! dex core: talks to an upstream `scrcpy-server` running on the phone.
//!
//! One [`session::Session`] = one Android virtual display = one window on the
//! desktop. The server encodes the display to H.264, we decode it with ffmpeg
//! and send input back over the control socket.

pub mod adb;
pub mod audio;
pub mod backend;
pub mod control;
pub mod decode;
pub mod desktop_theme;
pub mod session;
pub mod shell;
