//! Application log, composed for the selected backend.
//!
//! The writer is OS-neutral and lives in `platform-core`; this module installs
//! the backend's clock and gives it the log folder.

pub use platform_core::logging::{error, info, warn};

use crate::backend;

/// Opens the log file in the backend's log folder and starts the clock the log
/// stamps use.
pub fn init() {
    platform_core::time::install(backend::os::local_time);
    platform_core::logging::init(backend::os::log_dir());
}

pub fn open_dir() {
    backend::os::open_log_dir();
}
