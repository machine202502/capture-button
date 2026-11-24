//! Linux implementation of the platform layer.
//!
//! The application talks to the same module set as on Windows: `store`,
//! `i18n`, `os`, `shell`, `screen`, `region` and `outline`. The area overlay
//! itself is the Slint one, so `region` here only carries the native-overlay
//! operations that have a meaning on this platform (monitor list, save dialog,
//! baked annotations).
//!
//! Graphics go through X11 on an X11 session and through the desktop portal's
//! screen-cast API on Wayland. Wayland-only features report "unsupported"
//! instead of pretending: there is no in-app window list and no native area
//! overlay, while the portal can still share a monitor or a window.

#![cfg(target_os = "linux")]
#![allow(unused_must_use)]

mod annotate;
pub mod audio;
pub mod camera;
mod desktop;
pub mod hotkeys_portal;
pub mod i18n;
pub mod os;
pub mod outline;
pub mod pipewire;
pub mod portal;
pub mod region;
pub mod screen;
pub mod shell;
pub mod store;
pub mod tray;
