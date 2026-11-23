//! Linux implementation of the platform layer.
//!
//! The application talks to the same module set as on Windows: `store`,
//! `i18n`, `os`, `shell`, `screen`, `region` and `outline`. The area overlay
//! itself is the Slint one, so `region` here only carries the native-overlay
//! operations that have a meaning on this platform (monitor list, save dialog,
//! baked annotations).
//!
//! Graphics go through X11 for now. Wayland-only features report "unsupported"
//! instead of pretending: the area overlay, the window list and the capture
//! stream still work under XWayland, while a pure Wayland session returns a
//! clear error from the capture calls.

#![allow(unused_must_use)]

pub mod i18n;
pub mod os;
pub mod outline;
pub mod region;
pub mod screen;
pub mod shell;
pub mod store;
