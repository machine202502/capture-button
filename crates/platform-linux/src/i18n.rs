//! Linux UI language and the folders that may hold locale catalogs.
//!
//! The catalog logic is OS-neutral and lives in `platform-core`; this module
//! supplies the two facts it cannot know on its own.

use std::path::PathBuf;

/// Language code from the POSIX locale variables: "ru", "en", "zh", "ja", "ko"
/// or empty when the locale says nothing useful (`C`, `POSIX`, unset).
pub fn system_language() -> String {
    let raw = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|name| std::env::var(name).ok())
        .unwrap_or_default();
    if raw.is_empty() || raw == "C" || raw == "POSIX" {
        return String::new();
    }
    let code = raw
        .split(['_', '.', '@'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match code.as_str() {
        "ru" | "en" | "zh" | "ja" | "ko" => code,
        _ => String::new(),
    }
}

/// Folders searched for catalogs: the app's own `locales/` next to the
/// executable, then a user-editable copy under the XDG config directory.
pub fn locale_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("locales"));
        }
    }
    dirs.push(crate::os::config_dir().join("locales"));
    dirs
}
