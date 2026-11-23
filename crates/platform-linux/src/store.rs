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
        let _guard = crate::store::LOCK.lock().unwrap_or_else(|error| error.into_inner());
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
        "[Desktop Entry]\nType=Application\nName=Capture Button\nExec=\"{}\"{}nX-GNOME-Autostart-enabled=true\n",
        exe.display(),
        extra
    );
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::write(&file, entry);
}
