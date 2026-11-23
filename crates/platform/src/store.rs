//! Persistent settings storage.
//!
//! This is the API the application uses; the backend owns the actual key/value
//! store (the Windows registry today, a file under the XDG config directory on
//! a future Linux backend). The settings schema itself lives in the app.

pub use platform_core::runtime::test_mode;

/// An open settings database.
///
/// Holding one keeps the underlying key open; dropping it releases the handle.
pub struct Store {
    inner: crate::backend::store::Registry,
}

impl Store {
    /// Opens the store, or `None` when it is unavailable (a locked profile, for
    /// example). Callers then run with defaults.
    pub fn open() -> Option<Self> {
        crate::backend::store::Registry::open().map(|inner| Self { inner })
    }

    pub fn dword(&self, name: &str, default: u32) -> u32 {
        self.inner.dword(name, default)
    }

    pub fn set_dword(&self, name: &str, value: u32) {
        self.inner.set_dword(name, value);
    }

    pub fn string(&self, name: &str, default: &str) -> String {
        self.inner.string(name, default)
    }

    pub fn set_string(&self, name: &str, value: &str) {
        self.inner.set_string(name, value);
    }
}

/// Registers or removes the "start with the system" entry.
pub fn update_autostart(enabled: bool, minimized: bool) {
    crate::backend::store::update_autostart(enabled, minimized);
}
