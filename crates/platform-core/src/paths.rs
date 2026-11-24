//! Where output files go and how they are named.
//!
//! The folder settings, the per-category fallback and the file name format are
//! the same on every OS. Only the built-in default folder (Documents on
//! Windows) comes from the backend.

use crate::time::LocalTime;
use crate::types::{OutputCategory, OutputFolders};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

static FOLDERS: Mutex<OutputFolders> = Mutex::new(OutputFolders::EMPTY);

/// Applies the folder settings loaded at startup or changed in the window.
pub fn set_folders(folders: &OutputFolders) {
    *FOLDERS.lock().unwrap_or_else(|error| error.into_inner()) = folders.clone();
}

/// The folder configured for `category`, if any. `None` means the backend's
/// built-in folder should be used.
pub fn configured(category: OutputCategory) -> Option<PathBuf> {
    FOLDERS
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .resolved(category)
}

/// `Video_2026-11-23_13-05-09.mkv` and `Video_…-2.mkv` for later takes.
pub fn output_name(kind: &str, extension: &str, time: LocalTime, counter: u32) -> String {
    let suffix = if counter <= 1 {
        String::new()
    } else {
        format!("-{counter}")
    };
    format!(
        "{kind}_{:04}-{:02}-{:02}_{:02}-{:02}-{:02}{suffix}.{extension}",
        time.year, time.month, time.day, time.hour, time.minute, time.second
    )
}

/// Creates the folder if needed and returns it.
pub fn ensure_dir(dir: &Path) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|error| format!("{}: {error}", dir.display()))?;
    Ok(dir.to_path_buf())
}

/// The first free name in `dir` for this kind of file.
pub fn unique_path(
    dir: &Path,
    kind: &str,
    extension: &str,
    time: LocalTime,
) -> Result<PathBuf, String> {
    for counter in 1..10000 {
        let path = dir.join(output_name(kind, extension, time, counter));
        if !path.exists() {
            return Ok(path);
        }
    }
    Err("Cannot allocate an output filename".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(second: u8) -> LocalTime {
        LocalTime {
            year: 2026,
            month: 11,
            day: 23,
            hour: 13,
            minute: 5,
            second,
            millis: 0,
        }
    }

    fn directory(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("CaptureButton-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn output_names_have_the_expected_prefixes() {
        for (kind, extension, prefix) in [
            ("Video", "mkv", "Video_2026-11-23_13-05-09.mkv"),
            ("Animation", "gif", "Animation_2026-11-23_13-05-09.gif"),
            ("Animation", "webp", "Animation_2026-11-23_13-05-09.webp"),
            ("Image", "png", "Image_2026-11-23_13-05-09.png"),
            ("Audio", "mp3", "Audio_2026-11-23_13-05-09.mp3"),
            ("Camera", "png", "Camera_2026-11-23_13-05-09.png"),
        ] {
            let name = output_name(kind, extension, at(9), 1);
            assert_eq!(name, prefix);
        }
        assert_eq!(
            output_name("Video", "mkv", at(9), 3),
            "Video_2026-11-23_13-05-09-3.mkv"
        );
    }

    #[test]
    fn a_second_take_gets_its_own_name() {
        let dir = directory("paths");
        let first = unique_path(&dir, "Image", "png", at(9)).unwrap();
        std::fs::write(&first, b"x").unwrap();
        let second = unique_path(&dir, "Image", "png", at(9)).unwrap();
        assert_eq!(
            first.file_name().unwrap().to_string_lossy(),
            "Image_2026-11-23_13-05-09.png"
        );
        assert_eq!(
            second.file_name().unwrap().to_string_lossy(),
            "Image_2026-11-23_13-05-09-2.png"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_category_falls_back_to_the_default_folder() {
        let mut folders = OutputFolders::EMPTY;
        folders.set(OutputCategory::Default, Some(PathBuf::from(r"D:\Captures")));
        assert_eq!(
            folders.resolved(OutputCategory::Screenshots),
            Some(PathBuf::from(r"D:\Captures"))
        );
        folders.set(OutputCategory::Camera, Some(PathBuf::from(r"E:\Cam")));
        assert_eq!(
            folders.resolved(OutputCategory::Camera),
            Some(PathBuf::from(r"E:\Cam"))
        );
        assert_eq!(
            folders.resolved(OutputCategory::Video),
            Some(PathBuf::from(r"D:\Captures"))
        );
        assert_eq!(OutputFolders::EMPTY.resolved(OutputCategory::Sound), None);
    }
}
