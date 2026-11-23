//! Win32 view of the neutral palette.
//!
//! [`platform_core::palette::Palette`] stores `0x00BBGGRR` as `u32`; GDI wants
//! `COLORREF`. This module converts once at install time, so every painting
//! function keeps using the native type.

use platform_core::palette;
use windows::Win32::Foundation::COLORREF;

#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub struct Palette {
    pub SURFACE: COLORREF,
    pub ACCENT: COLORREF,
    pub ACCENT_DEEP: COLORREF,
    pub ON_ACCENT: COLORREF,
    pub DISABLED: COLORREF,
    pub TEXT: COLORREF,
    pub ALERT: COLORREF,
    pub WHITE: COLORREF,
    pub BLACK: COLORREF,
    pub CHROMA: COLORREF,
    pub BADGE: COLORREF,
    pub BADGE_TEXT: COLORREF,
    pub HINT_SHADOW: COLORREF,
    pub OVERLAY: COLORREF,
    pub OVERLAY_INK: COLORREF,
    pub OVERLAY_DISABLED: COLORREF,
    pub OVERLAY_DISABLED_INK: COLORREF,
    pub OVERLAY_EDGE: COLORREF,
    pub COPY: COLORREF,
    pub CANCEL: COLORREF,
    pub MARKER: COLORREF,
    pub RECORDING: COLORREF,
    pub RECORDING_DIM: COLORREF,
    pub BADGE_RECORDING: COLORREF,
    pub BADGE_RECORDING_EDGE: COLORREF,
    pub BADGE_SAVING: COLORREF,
    pub BADGE_SAVING_EDGE: COLORREF,
    pub SWATCHES: [COLORREF; 8],
}

impl From<palette::Palette> for Palette {
    fn from(colors: palette::Palette) -> Self {
        Self {
            SURFACE: COLORREF(colors.SURFACE),
            ACCENT: COLORREF(colors.ACCENT),
            ACCENT_DEEP: COLORREF(colors.ACCENT_DEEP),
            ON_ACCENT: COLORREF(colors.ON_ACCENT),
            DISABLED: COLORREF(colors.DISABLED),
            TEXT: COLORREF(colors.TEXT),
            ALERT: COLORREF(colors.ALERT),
            WHITE: COLORREF(colors.WHITE),
            BLACK: COLORREF(colors.BLACK),
            CHROMA: COLORREF(colors.CHROMA),
            BADGE: COLORREF(colors.BADGE),
            BADGE_TEXT: COLORREF(colors.BADGE_TEXT),
            HINT_SHADOW: COLORREF(colors.HINT_SHADOW),
            OVERLAY: COLORREF(colors.OVERLAY),
            OVERLAY_INK: COLORREF(colors.OVERLAY_INK),
            OVERLAY_DISABLED: COLORREF(colors.OVERLAY_DISABLED),
            OVERLAY_DISABLED_INK: COLORREF(colors.OVERLAY_DISABLED_INK),
            OVERLAY_EDGE: COLORREF(colors.OVERLAY_EDGE),
            COPY: COLORREF(colors.COPY),
            CANCEL: COLORREF(colors.CANCEL),
            MARKER: COLORREF(colors.MARKER),
            RECORDING: COLORREF(colors.RECORDING),
            RECORDING_DIM: COLORREF(colors.RECORDING_DIM),
            BADGE_RECORDING: COLORREF(colors.BADGE_RECORDING),
            BADGE_RECORDING_EDGE: COLORREF(colors.BADGE_RECORDING_EDGE),
            BADGE_SAVING: COLORREF(colors.BADGE_SAVING),
            BADGE_SAVING_EDGE: COLORREF(colors.BADGE_SAVING_EDGE),
            SWATCHES: [
                COLORREF(colors.SWATCHES[0]),
                COLORREF(colors.SWATCHES[1]),
                COLORREF(colors.SWATCHES[2]),
                COLORREF(colors.SWATCHES[3]),
                COLORREF(colors.SWATCHES[4]),
                COLORREF(colors.SWATCHES[5]),
                COLORREF(colors.SWATCHES[6]),
                COLORREF(colors.SWATCHES[7]),
            ],
        }
    }
}

/// The palette used until the application installs its own (and by the unit
/// tests, which never run through the real entry point).
pub fn default() -> &'static Palette {
    static FALLBACK: std::sync::OnceLock<Palette> = std::sync::OnceLock::new();
    FALLBACK.get_or_init(|| Palette::from(palette::DEFAULT))
}

/// BGRA bytes for a 32-bit bitmap. The palette stores `0x00BBGGRR`, so red is
/// the low byte.
pub fn bgra(color: COLORREF) -> [u8; 4] {
    palette::bgra(color.0)
}
