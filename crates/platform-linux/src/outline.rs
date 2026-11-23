//! The recording outline: a thin frame drawn around the recorded area.
//!
//! Drawing it needs a shaped, click-through X11 window. That window is not
//! implemented yet, so the outline is tracked but not drawn; recording itself
//! and the tray status are unaffected.

use platform_core::types::Rect;

pub struct RegionOutline {
    rect: Option<Rect>,
}

impl RegionOutline {
    pub fn new(_palette: platform_core::palette::Palette) -> Self {
        Self { rect: None }
    }

    pub fn sync(&mut self, rect: Option<Rect>, _elapsed_ms: u128) {
        self.rect = rect;
    }

    pub fn hide(&mut self) {
        self.rect = None;
    }
}
