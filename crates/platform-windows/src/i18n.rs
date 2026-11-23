//! Windows UI language and the folders that may hold locale catalogs.
//!
//! The catalog logic is OS-neutral and lives in `platform-core`; this module
//! supplies the two facts it cannot know on its own.

use std::path::PathBuf;

/// Language code of the Windows UI: "ru", "en", "zh", "ja", "ko" or empty.
pub fn system_language() -> String {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // SAFETY: the call has no arguments and only reads the current user's
    // display language.
    let langid = unsafe { GetUserDefaultUILanguage() };
    // The primary language is the low 10 bits of the LANGID.
    match langid & 0x3FF {
        0x19 => "ru",
        0x09 => "en",
        0x04 => "zh",
        0x11 => "ja",
        0x12 => "ko",
        _ => "",
    }
    .to_string()
}

/// Folders searched for catalogs: the app's own `locales/` next to the
/// executable, then a user-editable copy in `%LOCALAPPDATA%\CaptureButton\locales`.
pub fn locale_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            dirs.push(dir.join("locales"));
        }
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        dirs.push(PathBuf::from(local).join("CaptureButton").join("locales"));
    }
    dirs
}
