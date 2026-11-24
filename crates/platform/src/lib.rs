//! Capture Button's platform layer: the only platform crate the app depends on.
//!
//! It is composed of three pieces:
//! - `platform-core` — OS-neutral value types and policy;
//! - `platform-windows` — the Win32 backend: the OS calls and native windows;
//! - this crate's glue, for the few modules that need both.
//!
//! The application is Windows-only. `platform-core` stays free of Win32 types
//! and calls; everything that touches the operating system lives in the backend.

pub use platform_core::types::{
    AudioDevice, Format, Frame, OutputCategory, OutputFolders, Rect, UpdateState,
};
pub use platform_core::{frame, geom, palette, types};

pub mod audio;
pub mod camera;
pub mod i18n;
pub mod logging;
pub mod outline;
pub mod region;
pub mod screen;
pub mod store;
pub mod update;

mod backend {
    pub use platform_windows::{audio, i18n, os, outline, region, screen, shell, store};
}

pub use backend::{os, shell};
