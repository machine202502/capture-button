//! Data shapes shared by the application and every platform backend.
//!
//! These are OS-neutral value types: pixels, rectangles, the capture format and
//! the folder/category bookkeeping. Keeping them here means `app` and the
//! backend agree on one set of types without either depending on the other.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Mkv,
    Gif,
    Webp,
}

impl Format {
    pub fn index(self) -> usize {
        match self {
            Self::Mkv => 0,
            Self::Gif => 1,
            Self::Webp => 2,
        }
    }

    pub fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Gif,
            2 => Self::Webp,
            _ => Self::Mkv,
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Mkv => "mkv",
            Self::Gif => "gif",
            Self::Webp => "webp",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

#[derive(Clone)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// How long before the application received this frame it was captured, in
    /// nanoseconds. `0` when the platform cannot tell, which the capture loop
    /// treats as "captured now".
    pub age_ns: u64,
}

#[derive(Clone)]
pub struct AudioDevice {
    pub id: String,
    pub name: String,
}

/// The kind of file a folder setting applies to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OutputCategory {
    Screenshots,
    Video,
    Camera,
    Sound,
    /// The folder every other category falls back to.
    Default,
}

impl OutputCategory {
    pub const ALL: [Self; 5] = [
        Self::Screenshots,
        Self::Video,
        Self::Camera,
        Self::Sound,
        Self::Default,
    ];

    pub fn index(self) -> usize {
        match self {
            Self::Screenshots => 0,
            Self::Video => 1,
            Self::Camera => 2,
            Self::Sound => 3,
            Self::Default => 4,
        }
    }
}

/// The folders chosen in Settings. `None` keeps the default folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OutputFolders {
    paths: [Option<PathBuf>; 5],
}

impl OutputFolders {
    pub const EMPTY: Self = Self {
        paths: [None, None, None, None, None],
    };

    pub fn get(&self, category: OutputCategory) -> Option<&Path> {
        self.paths[category.index()].as_deref()
    }

    pub fn set(&mut self, category: OutputCategory, path: Option<PathBuf>) {
        self.paths[category.index()] = path;
    }

    pub fn is_custom(&self, category: OutputCategory) -> bool {
        self.paths[category.index()].is_some()
    }

    /// The folder this category uses: its own, or the default folder's.
    pub fn resolved(&self, category: OutputCategory) -> Option<PathBuf> {
        self.get(category).map(Path::to_path_buf).or_else(|| {
            if category == OutputCategory::Default {
                None
            } else {
                self.get(OutputCategory::Default).map(Path::to_path_buf)
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputCategory, OutputFolders};
    use std::path::PathBuf;

    #[test]
    fn a_category_falls_back_to_the_default_folder() {
        let mut folders = OutputFolders::EMPTY;
        folders.set(OutputCategory::Default, Some(PathBuf::from(r"D:\Captures")));
        assert_eq!(
            folders.resolved(OutputCategory::Screenshots),
            Some(PathBuf::from(r"D:\Captures"))
        );
        assert_eq!(
            folders.resolved(OutputCategory::Default),
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

/// Download progress reported by the platform updater to the app.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateState {
    #[default]
    Idle,
    Checking,
    Available,
    Downloading(u8),
    Installing,
    UpToDate,
    Error,
    Unsupported,
}
