//! The native area overlay has no Linux backend: the application draws the
//! Slint overlay instead. This module keeps the module contract and implements
//! the pieces that do have a meaning here (monitor list, save dialog, still
//! annotations).

use platform_core::types::{Frame, Rect};
pub use platform_core::region::{Outcome, Overlay};

/// The monitors of the X screen.
pub fn monitor_rects() -> Vec<Rect> {
    crate::screen::monitor_rects()
}

/// The native overlay is not available; the caller falls back to Slint.
pub fn open(
    _overlay: Overlay,
    _allow_record: bool,
    _palette: platform_core::palette::Palette,
) -> Result<(), String> {
    Err("the native overlay is not available on Linux; use the Slint overlay".into())
}

pub fn active() -> bool {
    false
}

pub fn take_outcome() -> Option<Outcome> {
    None
}

pub fn cancel() {}

/// Bakes annotation strokes into the still. The Linux renderer is Slint, so the
/// annotated still is produced from the Slint scene; this backend has nothing
/// to add yet.
pub fn draw_annotations(_frame: &mut Frame, _shapes: &[platform_core::region::Shape], _offset: platform_core::region::Point) -> bool {
    false
}

/// A save dialog through zenity, the same helper the folder picker uses.
pub fn save_dialog() -> Option<std::path::PathBuf> {
    let start = suggested_image_path();
    let output = std::process::Command::new("zenity")
        .args([
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            &format!("--filename={start}"),
            "--file-filter=Images|*.png *.webp *.jpg",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!path.is_empty()).then(|| std::path::PathBuf::from(path))
}

pub fn suggested_image_path() -> String {
    crate::os::output_path(
        platform_core::types::OutputCategory::Screenshots,
        "Image",
        "png",
    )
    .map(|path| path.display().to_string())
    .unwrap_or_else(|_| "Image.png".into())
}
