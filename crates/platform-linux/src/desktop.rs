//! The per-user desktop entry and icon.
//!
//! An AppImage does not install itself into the application menu, and a build
//! run from the source tree has no menu entry at all. GNOME then cannot match
//! the window (`WM_CLASS` is `CaptureButton`) to an application and draws its
//! placeholder gear in the dock and in the window list instead of the project
//! icon. The application therefore integrates itself for the current user on
//! start-up: it writes the menu entry and the icons unless they are already
//! exactly right.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Serializes installs. The start-up thread writes the entry in the background
/// while the portal probe may ask for it, and the portal must never read a
/// half-written file.
static INSTALLING: Mutex<()> = Mutex::new(());

/// The project icon, embedded so the installed icon does not depend on the
/// working directory or on where the application lives.
pub(crate) const ICON_PNG: &[u8] = include_bytes!("../../../assets/CaptureButton-64.png");

/// The file name the menu entry is installed under.
pub(crate) const DESKTOP_NAME: &str = "capture-button.desktop";

/// The application id the desktop portal knows this application by: the menu
/// entry's file name without the `.desktop` suffix. The portal resolves the
/// app info through it, so it has to match [`DESKTOP_NAME`].
pub(crate) const APP_ID: &str = "capture-button";

/// The icon name the entry points at; the file itself is `capture-button.png`
/// in the hicolor theme.
const ICON_NAME: &str = "capture-button";

/// Writes the menu entry and the icons for the current user when they are
/// missing or differ. Everything is best-effort: a read-only home or a missing
/// helper tool is logged and the application keeps running.
pub(crate) fn install() {
    let _guard = INSTALLING.lock().unwrap_or_else(|error| error.into_inner());
    let Some(executable) = executable_path() else {
        platform_core::logging::warn("desktop: could not resolve the executable path");
        return;
    };
    let Some(data) = data_dir() else {
        platform_core::logging::warn("desktop: neither XDG_DATA_HOME nor HOME is set");
        return;
    };
    let applications = data.join("applications");
    let icons = data.join("icons/hicolor/256x256/apps");
    let entry = applications.join(DESKTOP_NAME);

    let entry_written = write_when_changed(&entry, entry_content(&executable).as_bytes());
    let mut icons_written = write_when_changed(&icons.join(format!("{ICON_NAME}.png")), ICON_PNG);
    // GNOME System Monitor cannot match a process to the menu entry when the
    // entry starts a path instead of a bare command: it compares the entry's
    // executable string with the process name and then falls back to an icon
    // theme lookup by that name. These aliases hold the same PNG, so the
    // task manager draws the application icon instead of its generic gear.
    for name in process_icon_names() {
        icons_written |= write_when_changed(&icons.join(format!("{name}.png")), ICON_PNG);
    }
    if entry_written {
        refresh(std::process::Command::new("update-desktop-database").arg(&applications));
    }
    if icons_written {
        refresh(
            std::process::Command::new("gtk-update-icon-cache")
                .args(["-f", "-t"])
                .arg(data.join("icons/hicolor")),
        );
    }
}

/// The icon names the task manager looks up for the processes this application
/// consists of: the running executable (the app) and the AppImage file (the
/// runtime process that stays around while the mounted app runs).
fn process_icon_names() -> Vec<String> {
    process_icon_names_for(
        appimage_path().as_deref(),
        std::env::current_exe().ok().as_deref(),
    )
}

fn process_icon_names_for(appimage: Option<&Path>, current: Option<&Path>) -> Vec<String> {
    let mut names = Vec::new();
    for path in [current, appimage].into_iter().flatten() {
        let Some(name) = path.file_name() else {
            continue;
        };
        let name = name.to_string_lossy();
        if !name.is_empty() && name != ICON_NAME {
            names.push(name.into_owned());
        }
    }
    names.sort();
    names.dedup();
    names
}

/// The menu entry. It names `CaptureButton` as the window class, which is what
/// GNOME matches `WM_CLASS` against, and the icon installed next to it.
fn entry_content(executable: &Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=Capture Button\n\
         Comment=Take screenshots and record the screen\n\
         Exec={} %U\n\
         Icon={ICON_NAME}\n\
         Terminal=false\n\
         StartupWMClass=CaptureButton\n\
         Categories=Utility;Graphics;\n\
         Keywords=screenshot;screen;capture;record;\n",
        exec_field(executable)
    )
}

/// The `Exec` value: the path in double quotes, with the characters the desktop
/// entry specification reserves inside quotes escaped, followed by `%U`.
fn exec_field(executable: &Path) -> String {
    let path = executable.to_string_lossy();
    let mut field = String::with_capacity(path.len() + 2);
    field.push('"');
    for character in path.chars() {
        match character {
            '"' | '\\' | '$' | '`' => {
                field.push('\\');
                field.push(character);
            }
            // `%U` is a field code, so a literal percent sign has to be doubled.
            '%' => field.push_str("%%"),
            _ => field.push(character),
        }
    }
    field.push('"');
    field
}

/// The path the entry has to start. An AppImage runs the binary from a
/// temporary mount, so its runtime-provided path is the one a later launch has
/// to use; otherwise the running executable is the right one.
fn executable_path() -> Option<PathBuf> {
    appimage_path().or_else(|| std::env::current_exe().ok())
}

/// The AppImage file the runtime started, when the application runs from one.
fn appimage_path() -> Option<PathBuf> {
    std::env::var_os("APPIMAGE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

/// `$XDG_DATA_HOME` or `~/.local/share`, the directory the desktop reads the
/// per-user menu entries and icons from.
fn data_dir() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Some(path);
        }
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| PathBuf::from(home).join(".local/share"))
}

/// Writes `bytes` unless the file already holds exactly them, so a start-up
/// does not touch the modification time of an unchanged entry. Returns whether
/// the file was written; read and write errors are warnings.
fn write_when_changed(path: &Path, bytes: &[u8]) -> bool {
    match std::fs::read(path) {
        Ok(existing) if existing == bytes => return false,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            platform_core::logging::warn(format!(
                "desktop: could not read {}: {error}",
                path.display()
            ));
            return false;
        }
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    if let Err(error) = std::fs::create_dir_all(parent) {
        platform_core::logging::warn(format!(
            "desktop: could not create {}: {error}",
            parent.display()
        ));
        return false;
    }
    if let Err(error) = std::fs::write(path, bytes) {
        platform_core::logging::warn(format!(
            "desktop: could not write {}: {error}",
            path.display()
        ));
        return false;
    }
    true
}

/// Runs a desktop cache helper. A missing tool is not an error: the desktop
/// finds a per-user entry and icon without the caches too.
fn refresh(command: &mut std::process::Command) {
    match command.status() {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            platform_core::logging::warn(format!(
                "desktop: could not run the cache helper: {error}"
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_application_id_is_the_entry_name_without_its_suffix() {
        // The portal resolves the app info through this id, so an entry
        // installed under any other name would break the hotkey dialog.
        assert_eq!(format!("{APP_ID}.desktop"), DESKTOP_NAME);
    }

    #[test]
    fn the_entry_names_the_window_class_and_the_icon() {
        let entry = entry_content(Path::new("/usr/bin/capture-button"));
        for line in [
            "Type=Application",
            "Name=Capture Button",
            "Icon=capture-button",
            "Terminal=false",
            "StartupWMClass=CaptureButton",
            "Categories=Utility;Graphics;",
            "Exec=\"/usr/bin/capture-button\" %U",
        ] {
            assert!(
                entry.lines().any(|existing| existing == line),
                "missing {line} in:\n{entry}"
            );
        }
    }

    #[test]
    fn the_exec_line_quotes_and_escapes_the_executable() {
        let entry = entry_content(Path::new("/home/e2e/My Apps/cap\"$`%\\bin"));
        assert!(
            entry.contains("Exec=\"/home/e2e/My Apps/cap\\\"\\$\\`%%\\\\bin\" %U\n"),
            "{entry}"
        );
    }

    #[test]
    fn an_unchanged_file_is_not_rewritten() {
        let directory = std::env::temp_dir().join(format!(
            "capture-button-desktop-{}-unchanged",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("scratch directory");
        let path = directory.join(DESKTOP_NAME);
        assert!(write_when_changed(&path, b"first"));
        assert!(!write_when_changed(&path, b"first"));
        assert!(write_when_changed(&path, b"second"));
        std::fs::remove_dir_all(&directory).expect("cleanup");
    }

    #[test]
    fn the_task_manager_icon_names_are_the_process_names() {
        let names = process_icon_names_for(
            Some(Path::new(
                "/home/e2e/Applications/CaptureButton-linux-x64.AppImage",
            )),
            Some(Path::new("/tmp/.mount_Captur123/usr/bin/CaptureButton")),
        );
        assert_eq!(
            names,
            vec!["CaptureButton", "CaptureButton-linux-x64.AppImage"]
        );
    }
}
