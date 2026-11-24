//! OS-neutral platform vocabulary and policy.
//!
//! Everything here is free of OS calls: value types, the geometry of the
//! interface, translated strings, the palette, the way a capture source is
//! chosen, the region-selection state machine. Nothing in this crate calls the
//! operating system.
//!
//! The crates above it:
//! - `platform-windows` implements the OS calls and the windows;
//! - `platform` composes this crate with the backend and is the only platform
//!   crate the application depends on.

pub mod frame;
pub mod geom;
pub mod i18n;
pub mod level;
pub mod logging;
pub mod palette;
pub mod paths;
pub mod region;
pub mod runtime;
pub mod screen;
pub mod time;
pub mod tray;
pub mod types;

pub use types::{AudioDevice, Format, Frame, OutputCategory, OutputFolders, Rect, UpdateState};
