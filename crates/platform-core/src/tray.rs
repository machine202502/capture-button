//! The tray menu model shared by every backend.
//!
//! A backend turns these into its native menu: Win32 pops up an owner-drawn
//! popup, a Linux backend builds a StatusNotifierItem. The entries, glyphs and
//! badges — and therefore what the user sees — are decided here.

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TrayBadge {
    Idle,
    Recording,
    Saving,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TrayGlyph {
    Window,
    Area,
    Shot,
    Camera,
    Video,
    CameraVideo,
    Audio,
    Stop,
    Exit,
}

pub enum TrayEntry {
    Item {
        id: i32,
        text: String,
        glyph: TrayGlyph,
    },
    Separator,
}

/// A paint-guard nudge delivered to the UI toolkit's host window.
pub enum PaintEvent {
    /// The native client area changed size.
    Size(u32, u32),
    /// The window was restored or shown; force a redraw.
    Redraw,
}

/// Everything the host message loop hands to the application.
pub enum ShellEvent {
    Command { id: i32, notification: u16 },
    Timer(usize),
    TrayLeft,
    TrayContext { x: i32, y: i32 },
    Region,
    Ui,
    RefreshTray,
    Activate,
    Close,
    Quit,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_glyph_has_a_distinct_entry_kind() {
        let glyphs = [
            TrayGlyph::Window,
            TrayGlyph::Area,
            TrayGlyph::Shot,
            TrayGlyph::Video,
            TrayGlyph::Audio,
            TrayGlyph::Stop,
            TrayGlyph::Exit,
        ];
        for (index, glyph) in glyphs.iter().enumerate() {
            assert!(
                !glyphs[..index].contains(glyph),
                "duplicate glyph at {index}"
            );
        }
    }

    #[test]
    fn a_command_event_keeps_id_and_notification_apart() {
        let event = ShellEvent::Command {
            id: 130,
            notification: 7,
        };
        match event {
            ShellEvent::Command { id, notification } => {
                assert_eq!(id, 130);
                assert_eq!(notification, 7);
            }
            _ => panic!("a command stays a command"),
        }
    }
}
