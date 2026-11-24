//! The native area overlay has no Linux backend: the application draws the
//! Slint overlay instead. This module keeps the module contract and implements
//! the pieces that do have a meaning here (monitor list, save dialog, still
//! annotations).

pub use platform_core::region::{Outcome, Overlay};
use platform_core::region::{Renderer, RendererKind, Renderers};
use platform_core::types::{Frame, Rect};

/// The monitors of the X screen.
pub fn monitor_rects() -> Vec<Rect> {
    crate::screen::monitor_rects()
}

/// The Slint overlay asks the window manager for fullscreen on Linux so the
/// shell hides its panel and dock.
pub fn overlay_fullscreen() -> bool {
    true
}

/// The only overlay renderer Linux offers: the Slint window the application
/// draws.
static RENDERERS: [Renderer; 1] = [Renderer {
    id: 1,
    name: "Slint",
    kind: RendererKind::Slint,
}];

/// The overlay renderers this platform offers.
pub fn overlay_renderers() -> Renderers {
    Renderers {
        available: &RENDERERS,
        default: 1,
    }
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

/// Bakes annotation strokes and text into the still.
///
/// The Linux overlay is the Slint one, so a still saved from it has no native
/// drawing pass: [`crate::annotate`] paints the same geometry the Slint scene
/// draws into the cropped frame.
pub fn draw_annotations(
    frame: &mut Frame,
    shapes: &[platform_core::region::Shape],
    offset: platform_core::region::Point,
) -> bool {
    crate::annotate::bake(frame, shapes, offset)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_offers_only_the_slint_renderer_and_asks_for_fullscreen() {
        let renderers = overlay_renderers();
        assert_eq!(renderers.available.len(), 1);
        assert_eq!(renderers.available[0].id, 1);
        assert_eq!(renderers.available[0].name, "Slint");
        assert_eq!(renderers.available[0].kind, RendererKind::Slint);
        assert_eq!(renderers.default, 1);
        assert!(overlay_fullscreen());
    }
}
