//! Capture Button's platform layer: the only platform crate the app depends on.
//!
//! It is composed of three pieces:
//! - `platform-core` — OS-neutral value types and policy shared by every OS;
//! - the selected `platform-<os>` backend — the OS calls and native windows;
//! - this crate's glue, for the few modules that need both.
//!
//! Adding an OS means adding a `platform-<os>` crate with the same modules and
//! pointing the `backend` module at it here.

pub use platform_core::types::{
    AudioDevice, Format, Frame, OutputCategory, OutputFolders, Rect, UpdateState,
};
pub use platform_core::{frame, geom, palette, types};

pub mod audio;
pub mod camera;
pub mod i18n;
pub mod logging;
pub mod region;
pub mod screen;
pub mod store;
pub mod update;

#[cfg(target_os = "windows")]
mod backend {
    pub use platform_windows::{i18n, os, outline, region, screen, shell, store};
}

#[cfg(target_os = "windows")]
pub use backend::{os, outline, shell};

#[cfg(target_os = "linux")]
compile_error!(
    "platform-linux is not implemented yet. Add a platform-linux crate exposing the same \
     modules as platform-windows and re-export it from the `backend` module."
);

#[cfg(target_os = "macos")]
compile_error!(
    "platform-macos is not implemented yet. Add a platform-macos crate exposing the same \
     modules as platform-windows and re-export it from the `backend` module."
);

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
compile_error!("Capture Button does not support this target operating system");
