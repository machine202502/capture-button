//! The tray menu model.
//!
//! The Windows host turns these into its owner-drawn popup. The entries,
//! glyphs and badges — and therefore what the user sees — are decided here.

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

/// What a registered global hotkey asks the application to do.
///
/// Adding an action means adding a variant here, one [`HotkeyDef`] row in
/// [`HOTKEY_DEFS`], and a branch in the application's handler for the action.
/// Everything else — the settings key, the UI row, the platform binding — is
/// read from the table.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub enum HotkeyAction {
    /// Start area selection, exactly like the tray "Select area" item.
    Area,
    /// Stop the recording that is running; ignored when nothing is recorded.
    Stop,
}

/// Everything the layers need to know about one hotkey action.
#[derive(Clone, Copy, Debug)]
pub struct HotkeyDef {
    pub action: HotkeyAction,
    /// The key this action's combination is stored under in the settings store.
    pub settings_key: &'static str,
    /// The English source string of the row label (see `i18n::tr`).
    pub label_key: &'static str,
}

/// The hotkey actions, in the order the settings window lists them.
pub const HOTKEY_DEFS: [HotkeyDef; 2] = [
    HotkeyDef {
        action: HotkeyAction::Area,
        settings_key: "hotkey_area",
        label_key: "Select area",
    },
    HotkeyDef {
        action: HotkeyAction::Stop,
        settings_key: "hotkey_stop",
        label_key: "Stop recording",
    },
];

/// The table of hotkey actions, in settings-window order.
pub fn hotkey_defs() -> &'static [HotkeyDef] {
    &HOTKEY_DEFS
}

impl HotkeyAction {
    /// The table row for this action.
    pub fn def(self) -> &'static HotkeyDef {
        hotkey_defs()
            .iter()
            .find(|def| def.action == self)
            .expect("every HotkeyAction has a HOTKEY_DEFS row")
    }

    /// This action's position in the table; the UI uses it as the row key.
    pub fn index(self) -> usize {
        hotkey_defs()
            .iter()
            .position(|def| def.action == self)
            .expect("every HotkeyAction has a HOTKEY_DEFS row")
    }

    /// The action at a table position, as the UI reports it back.
    pub fn from_index(index: usize) -> Option<Self> {
        hotkey_defs().get(index).map(|def| def.action)
    }
}

/// One saved global hotkey: which action and the text stored for it.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct HotkeyBinding {
    pub action: HotkeyAction,
    /// The text saved in the settings for this action; empty means unbound.
    /// It is the canonical label (`Ctrl+Shift+F12`).
    pub combination: String,
}

impl HotkeyBinding {
    /// An unbound row for `action`.
    pub fn unset(action: HotkeyAction) -> Self {
        Self {
            action,
            combination: String::new(),
        }
    }

    pub fn is_set(&self) -> bool {
        !self.combination.trim().is_empty()
    }
}

/// A key in a global hotkey.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum HotkeyKey {
    /// A letter or a digit, stored upper case.
    Char(char),
    /// `F1` to `F24`.
    Function(u8),
    Space,
}

impl HotkeyKey {
    pub fn label(self) -> String {
        match self {
            Self::Char(key) => key.to_string(),
            Self::Function(number) => format!("F{number}"),
            Self::Space => "Space".into(),
        }
    }
}

/// One parsed global hotkey: the modifiers and the key they modify.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Hotkey {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub meta: bool,
    pub key: HotkeyKey,
}

impl Hotkey {
    /// The canonical text stored in the settings and shown in the field.
    pub fn label(self) -> String {
        let mut parts = Vec::with_capacity(5);
        if self.ctrl {
            parts.push("Ctrl".to_string());
        }
        if self.alt {
            parts.push("Alt".to_string());
        }
        if self.shift {
            parts.push("Shift".to_string());
        }
        if self.meta {
            parts.push("Meta".to_string());
        }
        parts.push(self.key.label());
        parts.join("+")
    }
}

/// Parses the text saved in the settings, for example `Ctrl+Shift+A`.
///
/// The spelling is case-insensitive, `Control` and `Super`/`Win` are accepted
/// as aliases, and a combination needs at least one of Ctrl, Alt or Meta: a
/// global grab of a bare letter would swallow ordinary typing in every
/// application. `Shift` alone is not enough for the same reason.
pub fn parse_hotkey(text: &str) -> Option<Hotkey> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut ctrl = false;
    let mut alt = false;
    let mut shift = false;
    let mut meta = false;
    let mut key = None;
    for part in text.split('+') {
        let part = part.trim();
        if part.is_empty() {
            return None;
        }
        let name = part.to_ascii_lowercase();
        match name.as_str() {
            "ctrl" | "control" => {
                if ctrl {
                    return None;
                }
                ctrl = true;
            }
            "alt" => {
                if alt {
                    return None;
                }
                alt = true;
            }
            "shift" => {
                if shift {
                    return None;
                }
                shift = true;
            }
            "meta" | "super" | "win" => {
                if meta {
                    return None;
                }
                meta = true;
            }
            _ => {
                if key.is_some() {
                    return None;
                }
                key = Some(parse_hotkey_key(part)?);
            }
        }
    }
    if !(ctrl || alt || meta) {
        return None;
    }
    Some(Hotkey {
        ctrl,
        alt,
        shift,
        meta,
        key: key?,
    })
}

fn parse_hotkey_key(token: &str) -> Option<HotkeyKey> {
    let mut chars = token.chars();
    let first = chars.next()?;
    if chars.next().is_none() {
        if first.is_ascii_alphanumeric() {
            return Some(HotkeyKey::Char(first.to_ascii_uppercase()));
        }
        if first == ' ' {
            return Some(HotkeyKey::Space);
        }
        return None;
    }
    let name = token.to_ascii_uppercase();
    if name == "SPACE" {
        return Some(HotkeyKey::Space);
    }
    let number = name.strip_prefix('F')?;
    if number.is_empty() || !number.chars().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let number: u8 = number.parse().ok()?;
    (1..=24)
        .contains(&number)
        .then_some(HotkeyKey::Function(number))
}

/// Turns a key press from the UI into the same shape [`parse_hotkey`] returns.
///
/// `text` is Slint's `KeyEvent.text`: one printable character, a private-use
/// character for the function keys, or a control character. A press with only
/// modifiers, Escape, or an unsupported key returns `None`, so the field keeps
/// waiting for a usable combination.
pub fn hotkey_from_press(
    text: &str,
    ctrl: bool,
    alt: bool,
    shift: bool,
    meta: bool,
) -> Option<Hotkey> {
    if !(ctrl || alt || meta) {
        return None;
    }
    let key = match text.chars().next()? {
        // Slint maps F1-F24 to the private-use range U+F704..U+F71B.
        special @ '\u{f704}'..='\u{f71b}' => {
            HotkeyKey::Function((special as u32 - 0xf704 + 1) as u8)
        }
        key if text.chars().count() == 1 => {
            if key.is_ascii_alphanumeric() {
                HotkeyKey::Char(key.to_ascii_uppercase())
            } else if key == ' ' {
                HotkeyKey::Space
            } else {
                return None;
            }
        }
        _ => return None,
    };
    Some(Hotkey {
        ctrl,
        alt,
        shift,
        meta,
        key,
    })
}

/// Everything the host message loop hands to the application.
pub enum ShellEvent {
    Command {
        id: i32,
        notification: u16,
    },
    Timer(usize),
    TrayLeft,
    Region,
    Ui,
    RefreshTray,
    Activate,
    Close,
    /// A registered global hotkey was pressed.
    Hotkey(HotkeyAction),
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

    #[test]
    fn a_hotkey_text_parses_into_the_same_combination() {
        let area = parse_hotkey("Ctrl+Shift+A").expect("a valid combination");
        assert!(area.ctrl && area.shift && !area.alt && !area.meta);
        assert_eq!(area.key, HotkeyKey::Char('A'));
        assert_eq!(area.label(), "Ctrl+Shift+A");

        let stop = parse_hotkey("alt+f9").expect("the spelling is case-insensitive");
        assert!(stop.alt && !stop.ctrl && !stop.shift && !stop.meta);
        assert_eq!(stop.key, HotkeyKey::Function(9));
        assert_eq!(stop.label(), "Alt+F9");

        let alias = parse_hotkey("Control+Super+F12").expect("aliases are accepted");
        assert!(alias.ctrl && alias.meta);
        assert_eq!(alias.key, HotkeyKey::Function(12));
        assert_eq!(alias.label(), "Ctrl+Meta+F12");

        assert_eq!(
            parse_hotkey(" Shift + Ctrl + Space ").map(Hotkey::label),
            Some("Ctrl+Shift+Space".into())
        );
    }

    #[test]
    fn a_bare_key_is_not_a_global_hotkey() {
        for text in [
            "",
            " ",
            "A",
            "F9",
            "Shift+A",
            "Ctrl",
            "Ctrl+Shift",
            "Ctrl+Shift+A+B",
            "Ctrl++A",
            "Ctrl+F25",
            "Ctrl+Enter",
        ] {
            assert_eq!(parse_hotkey(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_key_press_from_the_ui_canonicalizes_like_the_saved_text() {
        let pressed = hotkey_from_press("a", true, false, true, false).expect("Ctrl+Shift+A");
        assert_eq!(pressed.label(), "Ctrl+Shift+A");

        let function = hotkey_from_press("\u{f70c}", false, true, false, false)
            .expect("Slint sends F9 as a private-use character");
        assert_eq!(function.key, HotkeyKey::Function(9));
        assert_eq!(function.label(), "Alt+F9");

        assert_eq!(hotkey_from_press("a", false, false, true, false), None);
        assert_eq!(
            hotkey_from_press("\u{001b}", true, false, false, false),
            None
        );
        assert_eq!(hotkey_from_press("", true, false, false, false), None);
    }

    #[test]
    fn every_action_has_exactly_one_table_row() {
        for (index, def) in hotkey_defs().iter().enumerate() {
            assert_eq!(def.action.index(), index);
            assert_eq!(HotkeyAction::from_index(index), Some(def.action));
        }
        assert!(HotkeyAction::from_index(hotkey_defs().len()).is_none());
        assert_eq!(hotkey_defs().len(), 2);
    }

    #[test]
    fn the_table_settings_keys_are_distinct() {
        let defs = hotkey_defs();
        for (index, def) in defs.iter().enumerate() {
            for other in &defs[index + 1..] {
                assert_ne!(def.settings_key, other.settings_key, "duplicate key");
            }
        }
    }

    #[test]
    fn an_empty_binding_is_unbound() {
        assert!(!HotkeyBinding::unset(HotkeyAction::Area).is_set());
        assert!(HotkeyBinding {
            action: HotkeyAction::Stop,
            combination: " Ctrl+Alt+S ".into(),
        }
        .is_set());
        assert!(!HotkeyBinding {
            action: HotkeyAction::Stop,
            combination: "   ".into(),
        }
        .is_set());
    }
}
