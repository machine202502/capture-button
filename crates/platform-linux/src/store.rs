//! Persistent settings as a small key/value file under the XDG config
//! directory.
//!
//! The API mirrors the Windows registry store the application grew up with:
//! `dword`/`set_dword` for numbers, `string`/`set_string` for text. The file is
//! a plain `key = value` list so a human can edit it, with a `-test` suffix in
//! test mode, exactly like the two registry keys on Windows.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use platform_core::runtime::test_mode;

pub struct Registry {
    path: PathBuf,
    values: BTreeMap<String, String>,
}

impl Registry {
    pub fn open() -> Option<Self> {
        let path = crate::os::config_dir().join(if test_mode() {
            "settings-test.conf"
        } else {
            "settings.conf"
        });
        let values = std::fs::read_to_string(&path)
            .ok()
            .map(|text| parse(&text))
            .unwrap_or_default();
        Some(Self { path, values })
    }

    pub fn dword(&self, name: &str, default: u32) -> u32 {
        self.values
            .get(name)
            .and_then(|value| value.parse().ok())
            .unwrap_or(default)
    }

    pub fn set_dword(&self, name: &str, value: u32) {
        self.write(name, &value.to_string());
    }

    pub fn string(&self, name: &str, default: &str) -> String {
        self.values
            .get(name)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }

    pub fn set_string(&self, name: &str, value: &str) {
        self.write(name, value);
    }

    fn write(&self, name: &str, value: &str) {
        // The store is shared by the UI and the capture threads, so the file is
        // rewritten from the merged view under a process-wide lock.
        let _guard = crate::store::LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut values = read(&self.path);
        values.insert(name.to_string(), value.to_string());
        let mut text = String::new();
        for (key, value) in &values {
            text.push_str(key);
            text.push_str(" = ");
            text.push_str(value);
            text.push('\n');
        }
        if let Some(parent) = self.path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(&self.path, text);
    }
}

static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn read(path: &PathBuf) -> BTreeMap<String, String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|text| parse(&text))
        .unwrap_or_default()
}

fn parse(text: &str) -> BTreeMap<String, String> {
    let mut values = BTreeMap::new();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        values.insert(key.to_string(), value.trim().to_string());
    }
    values
}

/// The portal's one-shot token that restores the previous screen-cast source
/// without showing the picker. It is stored hex-encoded because the plain
/// `key = value` format splits on the first `=`.
pub fn portal_restore_token() -> Option<String> {
    let registry = Registry::open()?;
    let encoded = registry.string("portal_restore_token", "");
    (!encoded.is_empty()).then(|| decode_token(&encoded))
}

pub fn set_portal_restore_token(token: Option<&str>) {
    if let Some(registry) = Registry::open() {
        registry.set_string(
            "portal_restore_token",
            token.map(encode_token).unwrap_or_default().as_str(),
        );
    }
}

fn encode_token(token: &str) -> String {
    let mut encoded = String::with_capacity(token.len() * 2);
    for byte in token.bytes() {
        use std::fmt::Write as _;
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
}

fn decode_token(encoded: &str) -> String {
    if !encoded.len().is_multiple_of(2) {
        return String::new();
    }
    let bytes: Option<Vec<u8>> = encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let text = std::str::from_utf8(pair).ok()?;
            u8::from_str_radix(text, 16).ok()
        })
        .collect();
    bytes
        .and_then(|bytes| String::from_utf8(bytes).ok())
        .unwrap_or_default()
}

/// Autostart through the XDG autostart directory.
pub fn update_autostart(enabled: bool, minimized: bool) {
    let dir = crate::os::config_dir().join("autostart");
    let file = dir.join("capture-button.desktop");
    if !enabled {
        let _ = std::fs::remove_file(&file);
        return;
    }
    let exe = std::env::current_exe().unwrap_or_default();
    let extra = if minimized { " --minimized" } else { "" };
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Capture Button\nExec=\"{}\"{}\nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        extra
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&file, entry);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restore_token_survives_the_key_value_format() {
        // Portal tokens may contain '=' and spaces, which the plain
        // `key = value` parser would otherwise cut at.
        let token = "abc+/= a==";
        assert_eq!(decode_token(&encode_token(token)), token);
    }

    #[test]
    fn a_garbled_or_empty_token_decodes_to_nothing() {
        assert_eq!(decode_token(""), "");
        assert_eq!(decode_token("xyz"), "");
        assert_eq!(decode_token("zz"), "");
    }
}
