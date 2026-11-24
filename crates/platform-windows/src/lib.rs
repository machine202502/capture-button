//! Windows implementation of the platform layer.
//!
//! Everything here talks to Win32, WASAPI, Windows Graphics Capture, the
//! registry or Velopack. OS-neutral vocabulary and policy live in
//! `platform-core`, which this crate builds on.

#![cfg(windows)]
#![allow(unused_must_use)]

pub mod audio;
pub mod gdiplus;
pub mod camera;
pub mod i18n;
pub mod os;
pub mod outline;
pub mod palette;
pub mod region;
pub mod screen;
pub mod shell;
pub mod store;
