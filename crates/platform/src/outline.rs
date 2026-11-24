//! The recording outline: the selected backend draws it, and this module says
//! whether the platform can.
//!
//! The frame itself lives in the backend. The app only needs the window type and
//! the capability check, so the setting can be hidden where the frame cannot be
//! drawn at all.

pub use crate::backend::outline::RegionOutline;

/// Whether this platform can draw the recording outline.
pub fn supported() -> bool {
    crate::backend::outline::supported()
}
