//! Capture sources: the OS-neutral source rules plus the backend's capture.
//!
//! Which windows may be offered, when a window stops being usable and how a
//! rectangle is cropped to its monitor are decided in `platform-core`; grabbing
//! pixels and running a capture stream is the backend's job.

pub use crate::backend::screen::*;
pub use platform_core::screen::{
    crop_for, monitor_rect, source_row, window_state_usable, MonitorInfo,
};
