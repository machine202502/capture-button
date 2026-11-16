//! Small append-only application log. It is deliberately synchronous: log
//! calls happen on user actions and capture transitions, never per frame, so a
//! buffered writer would add complexity without a measurable gain.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static LOG: OnceLock<Mutex<File>> = OnceLock::new();

pub fn dir() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    root.join("CaptureButton").join("logs")
}

fn path() -> PathBuf {
    dir().join("CaptureButton.log")
}

pub fn init() {
    let _ = std::fs::create_dir_all(dir());
    let file_path = path();
    if std::fs::metadata(&file_path).is_ok_and(|meta| meta.len() > 5 * 1024 * 1024) {
        let _ = std::fs::rename(&file_path, file_path.with_extension("log.old"));
    }
    let Ok(file) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
    else {
        return;
    };
    let _ = LOG.set(Mutex::new(file));
    write("INFO", "Capture Button started");
}

fn write(level: &str, message: &str) {
    let stamp = unsafe {
        // SAFETY: GetLocalTime fills a plain SYSTEMTIME value.
        windows::Win32::System::SystemInformation::GetLocalTime()
    };
    let line = format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03} {level} {message}\r\n",
        stamp.wYear,
        stamp.wMonth,
        stamp.wDay,
        stamp.wHour,
        stamp.wMinute,
        stamp.wSecond,
        stamp.wMilliseconds
    );
    if let Some(file) = LOG.get() {
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(line.as_bytes());
            let _ = file.flush();
        }
    }
}

pub fn info(message: impl AsRef<str>) {
    write("INFO", message.as_ref());
}

pub fn warn(message: impl AsRef<str>) {
    write("WARN", message.as_ref());
}

pub fn error(message: impl AsRef<str>) {
    write("ERROR", message.as_ref());
}

pub fn open_dir() {
    let _ = std::fs::create_dir_all(dir());
    let _ = std::process::Command::new("explorer.exe")
        .arg(dir())
        .spawn();
}
