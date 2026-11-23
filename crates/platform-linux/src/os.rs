//! Files, time, clipboard and dialogs on Linux.

use platform_core::time::LocalTime;
use platform_core::types::{AudioDevice, Frame, OutputCategory, OutputFolders, Rect};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

/// The app's per-user configuration directory.
pub fn config_dir() -> PathBuf {
    base_dir("XDG_CONFIG_HOME", ".config").join("capture-button")
}

/// Where the log file lives.
pub fn log_dir() -> PathBuf {
    base_dir("XDG_STATE_HOME", ".local/state")
        .join("capture-button")
        .join("logs")
}

fn base_dir(variable: &str, fallback: &str) -> PathBuf {
    if let Some(value) = std::env::var_os(variable) {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return path;
        }
    }
    home().join(fallback)
}

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn open_log_dir() {
    let _ = open_file(&log_dir());
}

/// The current local time, used in file names and log lines.
pub fn local_time() -> LocalTime {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    #[cfg(unix)]
    {
        let now = seconds.as_secs() as libc::time_t;
        let mut broken = unsafe {
            // SAFETY: The pointer is a valid, writable `tm` owned by this frame.
            std::mem::zeroed::<libc::tm>()
        };
        unsafe {
            // SAFETY: `now` and `broken` are valid locals; localtime_r writes
            // into `broken` and never keeps the pointers.
            libc::localtime_r(&now, &mut broken);
        }
        LocalTime {
            year: (broken.tm_year + 1900) as u16,
            month: (broken.tm_mon + 1) as u8,
            day: broken.tm_mday as u8,
            hour: broken.tm_hour as u8,
            minute: broken.tm_min as u8,
            second: broken.tm_sec as u8,
            millis: seconds.subsec_millis() as u16,
        }
    }
    #[cfg(not(unix))]
    {
        // Only for a host-side `cargo check`; the Linux build uses the branch above.
        let time = seconds.as_secs() % 86_400;
        LocalTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: (time / 3600) as u8,
            minute: ((time % 3600) / 60) as u8,
            second: (time % 60) as u8,
            millis: seconds.subsec_millis() as u16,
            ..Default::default()
        }
    }
}

/// The monitor under the pointer.
pub fn cursor_monitor_rect() -> Rect {
    crate::screen::monitor_under_pointer().unwrap_or_else(crate::screen::primary_monitor)
}

/// The whole screen (or one monitor) without the pointer baked in.
pub fn grab(rect: Rect) -> Result<Frame, String> {
    crate::screen::grab_region(rect)
}

pub fn grab_without_cursor(rect: Rect) -> Result<Frame, String> {
    crate::screen::grab_region(rect)
}

static FOLDERS: OnceLock<OutputFolders> = OnceLock::new();

pub fn set_output_folders(folders: &OutputFolders) {
    platform_core::paths::set_folders(folders);
    let _ = FOLDERS.set(folders.clone());
}

fn configured(category: OutputCategory) -> Option<PathBuf> {
    platform_core::paths::configured(category)
}

fn default_dir(category: OutputCategory) -> PathBuf {
    let home = home();
    match category {
        OutputCategory::Screenshots => home.join("Pictures/Screenshots"),
        OutputCategory::Video => home.join("Videos"),
        OutputCategory::Camera => home.join("Pictures/Camera"),
        OutputCategory::Sound => home.join("Music/Capture Button"),
        OutputCategory::Default => home.join("Pictures/Capture Button"),
    }
}

pub fn output_dir() -> Result<PathBuf, String> {
    output_dir_for(OutputCategory::Screenshots)
}

pub fn output_dir_for(category: OutputCategory) -> Result<PathBuf, String> {
    let dir = configured(category).unwrap_or_else(|| default_dir(category));
    platform_core::paths::ensure_dir(&dir)
}

pub fn output_path(
    category: OutputCategory,
    kind: &str,
    extension: &str,
) -> Result<PathBuf, String> {
    let dir = output_dir_for(category)?;
    platform_core::paths::unique_path(&dir, kind, extension, local_time())
}

/// Puts text into the X11 clipboard (or the Wayland one through `wl-copy`).
pub fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    if command_with_stdin("wl-copy", &[], text.as_bytes()).is_ok() {
        return Ok(());
    }
    command_with_stdin("xclip", &["-selection", "clipboard"], text.as_bytes())
}

/// Puts a file reference into the clipboard as a `text/uri-list`.
pub fn copy_file_to_clipboard(path: &Path) -> Result<(), String> {
    let uri = format!("file://{}\r\n", path.display());
    if command_with_stdin("wl-copy", &["-t", "text/uri-list"], uri.as_bytes()).is_ok() {
        return Ok(());
    }
    command_with_stdin(
        "xclip",
        &["-selection", "clipboard", "-t", "text/uri-list"],
        uri.as_bytes(),
    )
}

pub fn open_file(path: &Path) -> Result<(), String> {
    let status = Command::new("xdg-open")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| error.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("xdg-open exited with {status}"))
    }
}

pub fn reveal_in_folder(path: &Path) -> Result<(), String> {
    let folder = path.parent().unwrap_or(path);
    open_file(folder)
}

/// Sound feedback is a no-op until the Linux backend ships its own sounds.
pub fn screenshot_sound(_volume: u32) {}

pub fn completion_sound(_volume: u32) {}

/// Audio devices are enumerated by the cpal glue, not here.
pub fn microphones() -> Vec<AudioDevice> {
    Vec::new()
}

pub fn device_name(_id: &str) -> Option<String> {
    None
}

fn command_with_stdin(program: &str, args: &[&str], input: &[u8]) -> Result<(), String> {
    use std::io::Write;

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("{program}: {error}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .map_err(|error| format!("{program}: {error}"))?;
    }
    let status = child
        .wait_with_output()
        .map_err(|error| format!("{program}: {error}"))?;
    if status.status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {}", status.status))
    }
}
