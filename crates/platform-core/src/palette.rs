//! Color palette used by the native overlays and the tray.
//!
//! Colors are stored as `0x00BBGGRR` `u32` values, the canonical form of the
//! application palette, so this crate stays free of platform types. A backend
//! converts them to its native color type where it draws.

#[derive(Clone, Copy)]
#[allow(non_snake_case)]
pub struct Palette {
    pub SURFACE: u32,
    pub ACCENT: u32,
    pub ACCENT_DEEP: u32,
    pub ON_ACCENT: u32,
    pub DISABLED: u32,
    pub TEXT: u32,
    pub ALERT: u32,
    pub WHITE: u32,
    pub BLACK: u32,
    pub CHROMA: u32,
    pub BADGE: u32,
    pub BADGE_TEXT: u32,
    pub HINT_SHADOW: u32,
    pub OVERLAY: u32,
    pub OVERLAY_INK: u32,
    pub OVERLAY_DISABLED: u32,
    pub OVERLAY_DISABLED_INK: u32,
    pub OVERLAY_EDGE: u32,
    pub COPY: u32,
    pub CANCEL: u32,
    pub MARKER: u32,
    pub RECORDING: u32,
    pub RECORDING_DIM: u32,
    pub BADGE_RECORDING: u32,
    pub BADGE_RECORDING_EDGE: u32,
    pub BADGE_SAVING: u32,
    pub BADGE_SAVING_EDGE: u32,
    pub SWATCHES: [u32; 8],
}

impl Palette {
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        surface: u32,
        accent: u32,
        accent_deep: u32,
        on_accent: u32,
        disabled: u32,
        text: u32,
        alert: u32,
        white: u32,
        black: u32,
        chroma: u32,
        badge: u32,
        badge_text: u32,
        hint_shadow: u32,
        overlay: u32,
        overlay_ink: u32,
        overlay_disabled: u32,
        overlay_disabled_ink: u32,
        overlay_edge: u32,
        copy: u32,
        cancel: u32,
        marker: u32,
        recording: u32,
        recording_dim: u32,
        badge_recording: u32,
        badge_recording_edge: u32,
        badge_saving: u32,
        badge_saving_edge: u32,
        swatches: [u32; 8],
    ) -> Self {
        Self {
            SURFACE: surface,
            ACCENT: accent,
            ACCENT_DEEP: accent_deep,
            ON_ACCENT: on_accent,
            DISABLED: disabled,
            TEXT: text,
            ALERT: alert,
            WHITE: white,
            BLACK: black,
            CHROMA: chroma,
            BADGE: badge,
            BADGE_TEXT: badge_text,
            HINT_SHADOW: hint_shadow,
            OVERLAY: overlay,
            OVERLAY_INK: overlay_ink,
            OVERLAY_DISABLED: overlay_disabled,
            OVERLAY_DISABLED_INK: overlay_disabled_ink,
            OVERLAY_EDGE: overlay_edge,
            COPY: copy,
            CANCEL: cancel,
            MARKER: marker,
            RECORDING: recording,
            RECORDING_DIM: recording_dim,
            BADGE_RECORDING: badge_recording,
            BADGE_RECORDING_EDGE: badge_recording_edge,
            BADGE_SAVING: badge_saving,
            BADGE_SAVING_EDGE: badge_saving_edge,
            SWATCHES: swatches,
        }
    }
}

/// BGRA bytes for a 32-bit bitmap. The palette stores `0x00BBGGRR`, so red is
/// the low byte.
pub const fn bgra(color: u32) -> [u8; 4] {
    [(color >> 16) as u8, (color >> 8) as u8, color as u8, 255]
}

/// The canonical palette, used as a fallback until the app installs its own
/// (and by unit tests that never run through `open`).
pub const DEFAULT: Palette = Palette::new(
    0x00fefefc, // SURFACE
    0x0088740e, // ACCENT
    0x006b5a0a, // ACCENT_DEEP
    0x00ffffff, // ON_ACCENT
    0x00aca793, // DISABLED
    0x00372f10, // TEXT
    0x003a45d8, // ALERT
    0x00ffffff, // WHITE
    0x00000000, // BLACK
    0x00778bf1, // CHROMA
    0x0039300f, // BADGE
    0x00ffffff, // BADGE_TEXT
    0x001e1907, // HINT_SHADOW
    0x00312a0c, // OVERLAY
    0x00f7f6e9, // OVERLAY_INK
    0x003d3515, // OVERLAY_DISABLED
    0x00aea78a, // OVERLAY_DISABLED_INK
    0x0060552c, // OVERLAY_EDGE
    0x005f8517, // COPY
    0x002b39c0, // CANCEL
    0x004dc2f0, // MARKER
    0x005e4dff, // RECORDING
    0x003122a0, // RECORDING_DIM
    0x002e3be2, // BADGE_RECORDING
    0x00121a8e, // BADGE_RECORDING_EDGE
    0x00249af2, // BADGE_SAVING
    0x000c5ca5, // BADGE_SAVING_EDGE
    [
        0x00303bef, 0x00e88c0f, 0x00e8cd12, 0x0062b82f, 0x003a7a1f, 0x00a65ad9, 0x00ffffff,
        0x00000000,
    ],
);
