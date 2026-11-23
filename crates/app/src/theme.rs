//! Tidewater: a deep-ocean masthead over pale glacial water. The accent is a
//! single sea-teal, the cards are foam, and the only saturated fill per view is
//! the primary action. One place for the interface colors.
//!
//! Platform-independent: every value is a plain `u32` in `0x00BBGGRR` form. The
//! GDI renderers convert it to `COLORREF` through [`platform::palette::Palette`].

#![allow(dead_code)]

pub use platform::palette::Palette;

/// The window surface: pale water under the cards.
pub const SURFACE: u32 = 0x00fefefc;
/// Cards, fields and buttons that sit on the surface.
pub const BORDER: u32 = 0x00ddd9c6;
/// A slightly deeper water, for pressed or inactive controls.
pub const MIST: u32 = 0x00efede3;
/// Pressed cards and pressed chips.
pub const CARD_HOVER: u32 = 0x00f6f4ea;
/// Selected format chip fill.
pub const ACCENT_SOFT: u32 = 0x00f0eedb;
/// The sea-teal accent.
pub const ACCENT: u32 = 0x0088740e;
/// A hair lighter accent, for hovered primary controls.
pub const ACCENT_HOVER: u32 = 0x0099830f;
/// Deeper teal, for a pressed primary control and section headings.
pub const ACCENT_DEEP: u32 = 0x006b5a0a;
/// Ink on the light surfaces.
pub const TEXT: u32 = 0x00372f10;
/// The scroll bar thumb, and its colour while dragged.
pub const SCROLLBAR: u32 = 0x00d2cdb4;
pub const SCROLLBAR_ACTIVE: u32 = 0x00aea67f;
/// Secondary ink: captions and inactive tabs.
pub const MUTED: u32 = 0x00786f4e;
/// Light ink on the accent.
pub const ON_ACCENT: u32 = 0x00ffffff;
/// A control that cannot be used.
pub const DISABLED: u32 = 0x00aca793;

/// The masthead: deep ocean above the water line.
pub const HEADER: u32 = 0x003a2e0b;
/// Ink on the masthead.
pub const HEADER_INK: u32 = 0x00f8f7ea;
/// Inactive tab ink on the masthead.
pub const HEADER_MUTED: u32 = 0x00c8c09c;
/// The hairline above the tide line, and ghost button rims.
pub const HEADER_EDGE: u32 = 0x0050451b;
/// Hover fill of masthead buttons.
pub const HEADER_HOVER: u32 = 0x004f4316;
/// Ink of a disabled masthead control.
pub const HEADER_DISABLED: u32 = 0x00837a5a;
/// The active tab capsule.
pub const ACTIVE_TAB: u32 = 0x00fefefc;

/// Overlay buttons sit on the screenshot, so they are darker than the window.
pub const OVERLAY: u32 = 0x00312a0c;
pub const OVERLAY_INK: u32 = 0x00f7f6e9;
pub const OVERLAY_DISABLED: u32 = 0x003d3515;
pub const OVERLAY_DISABLED_INK: u32 = 0x00aea78a;
pub const OVERLAY_EDGE: u32 = 0x0060552c;
/// The Copy action stands out in sea green; Cancel is red in every overlay.
pub const COPY: u32 = 0x005f8517;
pub const CANCEL: u32 = 0x002b39c0;

/// The small size label on the selection.
pub const BADGE: u32 = 0x0039300f;
pub const BADGE_TEXT: u32 = 0x00ffffff;

pub const WHITE: u32 = 0x00ffffff;
pub const BLACK: u32 = 0x00000000;
/// Shadow under the overlay hint, so the white letters stay readable.
pub const HINT_SHADOW: u32 = 0x001e1907;

const fn inverse(color: u32) -> u32 {
    0x00ffffff ^ (color & 0x00ffffff)
}

/// Transparent key of the text editor. The caret is the inverse of this color,
/// which makes it the sea-teal accent.
pub const CHROMA: u32 = inverse(ACCENT);

/// Pen colors offered in the overlay. They stay distinguishable on screenshots.
pub const SWATCHES: [u32; 8] = [
    0x00303bef, 0x00e88c0f, 0x00e8cd12, 0x0062b82f, 0x003a7a1f, 0x00a65ad9, WHITE, BLACK,
];
/// Highlighter stroke. Amber reads as a marker on any screenshot.
pub const MARKER: u32 = 0x004dc2f0;

/// Blinking frame around the area being recorded. Recording stays red: it is
/// the one status colour that must not be mistaken for the accent.
pub const RECORDING: u32 = 0x005e4dff;
pub const RECORDING_DIM: u32 = 0x003122a0;
/// Stop and exit marks in the tray menu. Stored as 0x00BBGGRR, drawn red.
pub const ALERT: u32 = 0x003a45d8;
/// Tray badge while a recording is in progress: red fill, darker red rim.
pub const BADGE_RECORDING: u32 = 0x002e3be2;
pub const BADGE_RECORDING_EDGE: u32 = 0x00121a8e;
/// Tray badge while the file is being saved: orange fill, darker orange rim.
pub const BADGE_SAVING: u32 = 0x00249af2;
pub const BADGE_SAVING_EDGE: u32 = 0x000c5ca5;

/// The palette the GDI renderers consume. Its canonical home is this file.
pub const PALETTE: Palette = Palette::new(
    SURFACE,
    ACCENT,
    ACCENT_DEEP,
    ON_ACCENT,
    DISABLED,
    TEXT,
    ALERT,
    WHITE,
    BLACK,
    CHROMA,
    BADGE,
    BADGE_TEXT,
    HINT_SHADOW,
    OVERLAY,
    OVERLAY_INK,
    OVERLAY_DISABLED,
    OVERLAY_DISABLED_INK,
    OVERLAY_EDGE,
    COPY,
    CANCEL,
    MARKER,
    RECORDING,
    RECORDING_DIM,
    BADGE_RECORDING,
    BADGE_RECORDING_EDGE,
    BADGE_SAVING,
    BADGE_SAVING_EDGE,
    SWATCHES,
);
