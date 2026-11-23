//! Interface translation, composed for the selected backend.
//!
//! The catalogs themselves are OS-neutral and live in `platform-core`; this
//! module supplies the system language and the folders to search for locale
//! files from the platform backend.

pub use platform_core::i18n::{available, tr, trf, wide};

use crate::backend;

/// Pick the catalog for `preferred` (empty = the OS UI language) and make it
/// active. Returns the applied code.
pub fn init(preferred: &str) -> String {
    let system = backend::i18n::system_language();
    let dirs = backend::i18n::locale_dirs();
    platform_core::i18n::init(preferred, &system, &dirs)
}
