#![allow(dead_code)]

//! Tidewater: a deep-ocean masthead over pale glacial water. The accent is a
//! single sea-teal, the cards are foam, and the only saturated fill per view is
//! the primary action. One place for the interface colors.
//!
//! Windows stores COLORREF as 0x00BBGGRR, so the red byte is the low one.

use windows::Win32::Foundation::COLORREF;

/// The window surface: pale water under the cards.
pub const BACKGROUND: COLORREF = COLORREF(0x00f5f4ed);
/// Cards, fields and buttons that sit on the surface.
pub const SURFACE: COLORREF = COLORREF(0x00fefefc);
/// Hairline around cards and fields.
pub const BORDER: COLORREF = COLORREF(0x00ddd9c6);
/// A slightly deeper water, for pressed or inactive controls.
pub const MIST: COLORREF = COLORREF(0x00efede3);
/// Pressed cards and pressed chips.
pub const CARD_HOVER: COLORREF = COLORREF(0x00f6f4ea);
/// Selected format chip fill.
pub const ACCENT_SOFT: COLORREF = COLORREF(0x00f0eedb);
/// The sea-teal accent.
pub const ACCENT: COLORREF = COLORREF(0x0088740e);
/// A hair lighter accent, for hovered primary controls.
pub const ACCENT_HOVER: COLORREF = COLORREF(0x0099830f);
/// Deeper teal, for a pressed primary control and section headings.
pub const ACCENT_DEEP: COLORREF = COLORREF(0x006b5a0a);
/// Ink on the light surfaces.
pub const TEXT: COLORREF = COLORREF(0x00372f10);
/// The scroll bar thumb, and its colour while dragged.
pub const SCROLLBAR: COLORREF = COLORREF(0x00d2cdb4);
pub const SCROLLBAR_ACTIVE: COLORREF = COLORREF(0x00aea67f);
/// Secondary ink: captions and inactive tabs.
pub const MUTED: COLORREF = COLORREF(0x00786f4e);
/// Light ink on the accent.
pub const ON_ACCENT: COLORREF = COLORREF(0x00ffffff);
/// A control that cannot be used.
pub const DISABLED: COLORREF = COLORREF(0x00aca793);

/// The masthead: deep ocean above the water line.
pub const HEADER: COLORREF = COLORREF(0x003a2e0b);
/// Ink on the masthead.
pub const HEADER_INK: COLORREF = COLORREF(0x00f8f7ea);
/// Inactive tab ink on the masthead.
pub const HEADER_MUTED: COLORREF = COLORREF(0x00c8c09c);
/// The hairline above the tide line, and ghost button rims.
pub const HEADER_EDGE: COLORREF = COLORREF(0x0050451b);
/// Hover fill of masthead buttons.
pub const HEADER_HOVER: COLORREF = COLORREF(0x004f4316);
/// Ink of a disabled masthead control.
pub const HEADER_DISABLED: COLORREF = COLORREF(0x00837a5a);
/// The active tab capsule.
pub const ACTIVE_TAB: COLORREF = COLORREF(0x00fefefc);

/// Overlay buttons sit on the screenshot, so they are darker than the window.
pub const OVERLAY: COLORREF = COLORREF(0x00312a0c);
pub const OVERLAY_INK: COLORREF = COLORREF(0x00f7f6e9);
pub const OVERLAY_DISABLED: COLORREF = COLORREF(0x003d3515);
pub const OVERLAY_DISABLED_INK: COLORREF = COLORREF(0x00aea78a);
pub const OVERLAY_EDGE: COLORREF = COLORREF(0x0060552c);
/// The Copy action stands out in sea green; Cancel is red in every overlay.
pub const COPY: COLORREF = COLORREF(0x005f8517);
pub const CANCEL: COLORREF = COLORREF(0x002b39c0);

/// The small size label on the selection.
pub const BADGE: COLORREF = COLORREF(0x0039300f);
pub const BADGE_TEXT: COLORREF = COLORREF(0x00ffffff);

pub const WHITE: COLORREF = COLORREF(0x00ffffff);
pub const BLACK: COLORREF = COLORREF(0x00000000);
/// Shadow under the overlay hint, so the white letters stay readable.
pub const HINT_SHADOW: COLORREF = COLORREF(0x001e1907);

/// Transparent key of the text editor. The caret is the inverse of this color,
/// which makes it the sea-teal accent.
pub const CHROMA: COLORREF = inverse(ACCENT);

/// Pen colors offered in the overlay. They stay distinguishable on screenshots.
pub const SWATCHES: [COLORREF; 8] = [
    COLORREF(0x00303bef),
    COLORREF(0x00e88c0f),
    COLORREF(0x00e8cd12),
    COLORREF(0x0062b82f),
    COLORREF(0x003a7a1f),
    COLORREF(0x00a65ad9),
    WHITE,
    BLACK,
];
/// Highlighter stroke. Amber reads as a marker on any screenshot.
pub const MARKER: COLORREF = COLORREF(0x004dc2f0);

/// Blinking frame around the area being recorded. Recording stays red: it is
/// the one status colour that must not be mistaken for the accent.
pub const RECORDING: COLORREF = COLORREF(0x005e4dff);
pub const RECORDING_DIM: COLORREF = COLORREF(0x003122a0);
/// Stop and exit marks in the tray menu. Stored as 0x00BBGGRR, drawn red.
pub const ALERT: COLORREF = COLORREF(0x003a45d8);
/// Tray badge while a recording is in progress: red fill, darker red rim.
pub const BADGE_RECORDING: COLORREF = COLORREF(0x002e3be2);
pub const BADGE_RECORDING_EDGE: COLORREF = COLORREF(0x00121a8e);
/// Tray badge while the file is being saved: orange fill, darker orange rim.
pub const BADGE_SAVING: COLORREF = COLORREF(0x00249af2);
pub const BADGE_SAVING_EDGE: COLORREF = COLORREF(0x000c5ca5);

const fn inverse(color: COLORREF) -> COLORREF {
    COLORREF(0x00ffffff ^ (color.0 & 0x00ffffff))
}

/// BGRA bytes for a 32-bit bitmap. COLORREF stores the same color as 0x00BBGGRR.
pub const fn bgra(color: COLORREF) -> [u8; 4] {
    let value = color.0;
    [(value >> 16) as u8, (value >> 8) as u8, value as u8, 255]
}
