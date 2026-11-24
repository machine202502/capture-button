//! The area-selection overlay as the application sees it.
//!
//! Two renderers draw the same [`Model`]: the native overlay in the platform
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

pub use crate::backend::region::{monitor_rects, open, overlay_fullscreen};

pub use platform_core::region::{
    caption, handles, icon, role_enabled, text_handles, Action, Button, Drag, FontSeed, Handle,
    Key, Metrics, Model, Outcome, Overlay, Point, Renderer, RendererKind, Renderers, Role,
    Selection, Shape, ShapeKind, Tool,
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
/// The native overlay owns the annotation drawing; every renderer reuses it so
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

/// The renderers this build offers, in menu order, and the platform default.
pub fn overlay_renderers() -> Renderers {
    crate::backend::region::overlay_renderers()
}

/// The renderer a fresh profile starts with.
pub fn default_renderer() -> Renderer {
    let renderers = overlay_renderers();
    renderers
        .available
        .iter()
        .find(|renderer| renderer.id == renderers.default)
        .or_else(|| renderers.available.first())
        .copied()
        .expect("a platform offers at least one overlay renderer")
}

/// The renderer a stored settings value names.
///
/// The value is a renderer [`id`](Renderer::id), so `0` stays the native
/// overlay of the platform and `1` the Slint one. An id this build does not
/// offer falls back to the platform default, which keeps a settings file copied
/// from another OS usable.
pub fn renderer_by_setting(value: u32) -> Renderer {
    let renderers = overlay_renderers();
    u8::try_from(value)
        .ok()
        .and_then(|id| {
            renderers
                .available
                .iter()
                .find(|renderer| renderer.id == id)
        })
        .copied()
        .unwrap_or_else(default_renderer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_renderer_is_available_here() {
        let default = default_renderer();
        let offered = overlay_renderers()
            .available
            .iter()
            .any(|renderer| renderer.id == default.id);
        assert!(offered, "the default renderer is not in the list");
        assert_eq!(renderer_by_setting(u32::from(default.id)).id, default.id);
    }

    #[test]
    fn the_slint_setting_maps_to_slint_where_it_exists() {
        let slint = renderer_by_setting(1);
        assert_eq!(slint.id, 1);
        assert_eq!(slint.name, "Slint");
        assert_eq!(slint.kind, RendererKind::Slint);
    }

    #[test]
    fn an_unknown_setting_falls_back_to_the_default() {
        let default = default_renderer();
        assert_eq!(renderer_by_setting(99).id, default.id);
        assert_eq!(renderer_by_setting(u32::MAX).id, default.id);
    }

    #[test]
    fn a_chosen_setting_names_an_offered_renderer() {
        for value in [0, 1, 99] {
            let chosen = renderer_by_setting(value);
            assert!(
                overlay_renderers()
                    .available
                    .iter()
                    .any(|renderer| renderer.id == chosen.id),
                "setting {value} resolved to a renderer the platform does not offer"
            );
        }
    }
}
