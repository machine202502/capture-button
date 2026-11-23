//! Interface translations.
//!
//! English is compiled into the executable, so the program is fully usable
//! without any locale files. Other languages are plain JSON catalogs in a
//! `locales/` folder next to the executable, so they can be fixed or added
//! without touching the binary; a missing catalog falls back to the built-in
//! English.
//!
//! The catalog logic is OS-neutral. The platform layer supplies the language of
//! the system and the folders to search.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{OnceLock, RwLock};

#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub code: String,
    /// Native name shown in the language picker, e.g. "Русский".
    pub name: String,
    pub strings: BTreeMap<String, String>,
}

/// English catalog compiled into the executable. It is always available, even
/// when the `locales/` folder is missing or was deleted; `locales/en.json` on
/// disk still overrides it when present.
const BUILTIN_EN: &str = include_str!("../../../locales/en.json");

impl Catalog {
    fn english() -> Self {
        Self {
            code: "en".to_string(),
            name: "English".to_string(),
            strings: BTreeMap::new(),
        }
    }
}

fn active() -> &'static RwLock<Catalog> {
    static ACTIVE: OnceLock<RwLock<Catalog>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Catalog::english()))
}

fn catalogs() -> &'static RwLock<Vec<Catalog>> {
    static CATALOGS: OnceLock<RwLock<Vec<Catalog>>> = OnceLock::new();
    CATALOGS.get_or_init(|| RwLock::new(Vec::new()))
}

/// Translate one English source string.
pub fn tr(key: &str) -> String {
    let guard = active().read().unwrap_or_else(|error| error.into_inner());
    match guard.strings.get(key) {
        Some(text) => text.clone(),
        None => key.to_string(),
    }
}

/// Translate and substitute `{name}` placeholders.
pub fn trf(key: &str, args: &[(&str, &str)]) -> String {
    let mut text = tr(key);
    for (name, value) in args {
        text = text.replace(&format!("{{{name}}}"), value);
    }
    text
}

/// UTF-16 encoding with a trailing NUL, for native calls that copy the buffer.
pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn parse_catalog(text: &str) -> Option<Catalog> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    let code = value
        .get("code")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let name = value
        .get("name")
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string();
    let mut strings = BTreeMap::new();
    if let Some(map) = value.get("strings").and_then(|value| value.as_object()) {
        for (key, value) in map {
            if let Some(text) = value.as_str() {
                strings.insert(key.clone(), text.to_string());
            }
        }
    }
    Some(Catalog {
        code,
        name,
        strings,
    })
}

fn load_catalogs(dirs: &[PathBuf]) -> Vec<Catalog> {
    let mut by_code: BTreeMap<String, Catalog> = BTreeMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Some(mut catalog) = parse_catalog(&text) else {
                continue;
            };
            if catalog.code.is_empty() {
                catalog.code = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .unwrap_or("en")
                    .to_string();
            }
            if catalog.name.is_empty() {
                catalog.name = catalog.code.clone();
            }
            by_code.insert(catalog.code.clone(), catalog);
        }
    }
    by_code.into_values().collect()
}

/// English from the executable, used when no `en.json` is on disk.
fn builtin_english() -> Catalog {
    parse_catalog(BUILTIN_EN).unwrap_or_else(Catalog::english)
}

/// Pick the catalog for `preferred` (empty = `system_language`) and make it
/// active. `dirs` are the folders that may hold JSON catalogs. Returns the
/// applied code.
pub fn init(preferred: &str, system_language: &str, dirs: &[PathBuf]) -> String {
    let wanted = if preferred.is_empty() {
        system_language.to_string()
    } else {
        preferred.to_string()
    };
    let mut found = load_catalogs(dirs);
    if !found.iter().any(|catalog| catalog.code == "en") {
        found.push(builtin_english());
    }
    found.sort_by(|left, right| left.code.cmp(&right.code));
    let chosen = found
        .iter()
        .find(|catalog| catalog.code == wanted)
        .or_else(|| found.iter().find(|catalog| catalog.code == "en"))
        .cloned()
        .unwrap_or_else(Catalog::english);
    let code = chosen.code.clone();
    *active().write().unwrap_or_else(|error| error.into_inner()) = chosen;
    *catalogs()
        .write()
        .unwrap_or_else(|error| error.into_inner()) = found;
    code
}

/// Catalogs available for the picker, English always included.
pub fn available() -> Vec<(String, String)> {
    let guard = catalogs().read().unwrap_or_else(|error| error.into_inner());
    let mut list: Vec<(String, String)> = guard
        .iter()
        .map(|catalog| (catalog.code.clone(), catalog.name.clone()))
        .collect();
    if !list.iter().any(|(code, _)| code == "en") {
        list.push(("en".to_string(), "English".to_string()));
    }
    list
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_translations_fall_back_to_the_english_key() {
        *active().write().unwrap_or_else(|error| error.into_inner()) = Catalog::english();
        assert_eq!(tr("Settings"), "Settings");
        assert_eq!(trf("Saved: {path}", &[("path", "a.png")]), "Saved: a.png");
    }

    #[test]
    fn the_built_in_english_catalog_is_inside_the_binary() {
        let catalog = parse_catalog(BUILTIN_EN).expect("the embedded English catalog parses");
        assert_eq!(catalog.code, "en");
        assert_eq!(catalog.name, "English");
        assert_eq!(
            catalog.strings.get("Settings").map(String::as_str),
            Some("Settings")
        );
        assert!(catalog.strings.len() > 50);
    }

    #[test]
    fn a_catalog_reads_code_name_and_strings() {
        let catalog =
            parse_catalog(r#"{"code":"ru","name":"Русский","strings":{"Settings":"Настройки"}}"#)
                .unwrap();
        assert_eq!(catalog.code, "ru");
        assert_eq!(catalog.name, "Русский");
        assert_eq!(
            catalog.strings.get("Settings").map(String::as_str),
            Some("Настройки")
        );
    }
}
