//! The area-selection overlay as the application sees it.
//!
//! Two renderers draw the same [`Model`]: the native GDI overlay in the Windows
//! backend, and the Slint overlay in the app. This module is the one API the app
//! uses for both. It forwards the native window's operations to the backend,
//! carries the neutral model and its vocabulary, and owns the outcome channel
//! that either renderer publishes into.
//!
//! The app's rule is unchanged from the days of the single native overlay: an
//! overlay is open while [`active`] is true, and once it is done one outcome
//! waits in [`take_outcome`] until the app consumes it. A renderer without a
//! native window keeps that shape by reporting through
//! [`platform_core::region`].

use platform_core::types::Frame;
use std::path::PathBuf;

pub use crate::backend::region::{monitor_rects, open};

pub use platform_core::region::{
    caption, handles, icon, role_enabled, Action, Button, FontSeed, Key, Metrics, Model, Outcome,
    Overlay, Point, Role, Selection, Shape, ShapeKind, Tool,
};

/// Whether the native overlay window exists.
fn native_active() -> bool {
    crate::backend::region::active()
}

/// Publishes an outcome for the application.
///
/// The native overlay has its own outcome slot and never calls this; a renderer
/// that has no window state calls it instead. The app is woken only when the
/// native overlay is not the one that finished, because the native path wakes it
/// itself.
///
/// An outcome that waits for the app also keeps [`active`] true, so the app
/// cannot start a second selection before it has consumed the first — the native
/// overlay behaves the same way by staying alive until it is taken.
pub fn publish(outcome: Outcome) {
    if !platform_core::region::publish(outcome) {
        return;
    }
    if !native_active() {
        platform_core::region::set_active(true);
        crate::shell::notify_region_closed();
    }
}

/// Takes the outcome the open overlay produced.
///
/// Either renderer may have produced it. Clearing the reported state here keeps
/// the "an outcome is waiting" and "an overlay is open" facts in step.
pub fn take_outcome() -> Option<Outcome> {
    let native = crate::backend::region::take_outcome();
    let reported = platform_core::region::take_outcome();
    if native.is_some() || reported.is_some() {
        platform_core::region::set_active(false);
    }
    native.or(reported)
}

/// Whether an overlay is open, whichever renderer draws it.
pub fn active() -> bool {
    native_active() || platform_core::region::reported_active()
}

/// Records that a renderer without a native window has its overlay open.
///
/// [`active`] stays true until [`take_outcome`] consumes the outcome, so the
/// "open" and "an outcome is waiting" facts keep the shape the native overlay
/// gave them.
pub fn report_open(open: bool) {
    platform_core::region::set_active(open);
}

/// Closes the open overlay.
///
/// The native window is cancelled directly. A reporting renderer gets
/// `Outcome::Cancel` and closes itself when it sees the waiting outcome.
pub fn cancel() {
    if native_active() {
        crate::backend::region::cancel();
    } else {
        publish(Outcome::Cancel);
    }
}

/// Whether an overlay outcome is waiting for the application.
pub fn pending() -> bool {
    platform_core::region::pending()
}

/// Draws annotations onto an already cropped frame, moving them by `offset`.
///
/// The Windows overlay owns the annotation drawing; every renderer reuses it so
/// a saved picture is the same whichever overlay produced it.
pub fn bake_annotations(frame: &mut Frame, shapes: &[Shape], offset: Point) {
    crate::backend::region::draw_annotations(frame, shapes, offset);
}

/// Asks the user for a path to save a still to. `None` when the dialog is
/// cancelled or cannot open.
pub fn ask_save_path() -> Option<PathBuf> {
    crate::backend::region::save_dialog()
}

/// The file name the save dialog starts with.
pub fn suggested_save_path() -> String {
    crate::backend::region::suggested_image_path()
}

/// The annotated still for the model's selection, or `None` when nothing usable
/// is selected.
pub fn render_still(model: &Model) -> Option<Frame> {
    let selection = model.selected()?;
    let mut frame = platform_core::region::crop_pixels(model)?;
    bake_annotations(
        &mut frame,
        &model.shapes,
        Point {
            x: -selection.x,
            y: -selection.y,
        },
    );
    Some(frame)
}

/// A renderer that can draw the area overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayRenderer {
    /// The native overlay the application always had (Windows only).
    Gdi,
    /// The cross-platform Slint overlay.
    Slint,
}

impl OverlayRenderer {
    /// `1` is the Slint renderer, `0` the native one. A value this build cannot
    /// draw falls back to the platform default, so a settings file copied from
    /// another OS still opens an overlay.
    pub fn from_setting(value: u32) -> Self {
        let wanted = if value == 1 { Self::Slint } else { Self::Gdi };
        if overlay_renderers().contains(&wanted) {
            wanted
        } else {
            default_overlay_renderer()
        }
    }

    pub fn to_setting(self) -> u32 {
        match self {
            Self::Gdi => 0,
            Self::Slint => 1,
        }
    }

    /// The name shown in the settings picker.
    pub fn name(self) -> &'static str {
        match self {
            Self::Gdi => "GDI",
            Self::Slint => "Slint",
        }
    }
}

/// The renderers this build offers, in menu order.
///
/// Windows ships both; every other platform has only the Slint overlay until a
/// native one is written.
pub fn overlay_renderers() -> Vec<OverlayRenderer> {
    #[cfg(target_os = "windows")]
    {
        vec![OverlayRenderer::Gdi, OverlayRenderer::Slint]
    }
    #[cfg(not(target_os = "windows"))]
    {
        vec![OverlayRenderer::Slint]
    }
}

/// The renderer a fresh profile starts with: the native overlay on Windows,
/// the Slint one everywhere else.
pub fn default_overlay_renderer() -> OverlayRenderer {
    #[cfg(target_os = "windows")]
    {
        OverlayRenderer::Gdi
    }
    #[cfg(not(target_os = "windows"))]
    {
        OverlayRenderer::Slint
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_renderer_is_available_here() {
        let default = default_overlay_renderer();
        assert!(overlay_renderers().contains(&default));
        assert_eq!(OverlayRenderer::from_setting(default.to_setting()), default);
    }

    #[test]
    fn the_slint_setting_maps_to_slint_where_it_exists() {
        assert_eq!(OverlayRenderer::from_setting(1), OverlayRenderer::Slint);
        assert_eq!(OverlayRenderer::Slint.to_setting(), 1);
        assert_eq!(OverlayRenderer::Gdi.to_setting(), 0);
        // A value for a renderer this build does not have still resolves.
        let unknown = OverlayRenderer::from_setting(99);
        assert!(overlay_renderers().contains(&unknown));
    }
}