//! Which windows and rectangles may become a capture source.
//!
//! These are the product rules: a minimized, hidden or tiny window is not
//! offered, the application never offers itself, and a recording rectangle is
//! clamped to the monitor that holds it. The backend only supplies the facts.

use crate::types::Rect;

/// A window is usable while it exists, is on screen, is not a tooltip-like
/// cloaked window and is not minimized, and it is at least 16×16.
pub fn window_state_usable(
    exists: bool,
    visible: bool,
    iconic: bool,
    cloaked: bool,
    minimized: bool,
    width: i32,
    height: i32,
) -> bool {
    exists && visible && !iconic && !cloaked && !minimized && width >= 16 && height >= 16
}

/// A window earns a row in the source picker: big enough, titled, and not this
/// application (whose own window must never be offered as a source).
pub fn source_row(iconic: bool, cloaked: bool, width: i32, height: i32, title: &str) -> bool {
    if iconic || cloaked || width < 80 || height < 80 {
        return false;
    }
    let title = title.trim();
    !title.is_empty() && title != "Capture Button" && !title.starts_with("Capture Button ")
}

/// The offset of `rect` inside the monitor whose origin and size are given, as
/// `(x, y, width, height)`. `None` means "no crop": the rectangle already starts
/// at the monitor origin and covers it, so the whole monitor frame is the area.
pub fn crop_for(
    rect: Rect,
    origin_x: i32,
    origin_y: i32,
    monitor_w: u32,
    monitor_h: u32,
) -> Option<(u32, u32, u32, u32)> {
    let x = (rect.x - origin_x).max(0) as u32;
    let y = (rect.y - origin_y).max(0) as u32;
    if x == 0 && y == 0 && rect.w >= monitor_w && rect.h >= monitor_h {
        None
    } else {
        Some((x, y, rect.w, rect.h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_minimized_closed_or_hidden_window_is_not_usable() {
        assert!(!window_state_usable(true, true, true, false, false, 800, 600));
        assert!(!window_state_usable(true, true, false, false, true, 800, 600));
        assert!(!window_state_usable(false, false, false, false, false, 0, 0));
        assert!(!window_state_usable(true, false, false, false, false, 800, 600));
        assert!(window_state_usable(true, true, false, false, false, 800, 600));
    }

    #[test]
    fn minimized_and_tiny_windows_are_not_offered_as_sources() {
        assert!(!source_row(true, false, 1200, 800, "Mail"));
        assert!(!source_row(false, true, 1200, 800, "Clock"));
        assert!(!source_row(false, false, 314, 50, "Clock"));
        assert!(!source_row(false, false, 314, 50, "Kimi"));
        assert!(!source_row(false, false, 800, 600, "Capture Button"));
        assert!(source_row(false, false, 1280, 720, "Notes"));
    }

    #[test]
    fn a_crop_is_only_needed_for_a_part_of_the_monitor() {
        let whole = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        assert_eq!(crop_for(whole, 0, 0, 1920, 1080), None);
        let part = Rect {
            x: 100,
            y: 50,
            w: 400,
            h: 300,
        };
        assert_eq!(crop_for(part, 0, 0, 1920, 1080), Some((100, 50, 400, 300)));
        let negative = Rect {
            x: -40,
            y: -10,
            w: 300,
            h: 200,
        };
        assert_eq!(crop_for(negative, 0, 0, 1920, 1080), Some((0, 0, 300, 200)));
    }

    }
