//! Camera photo capture: the camera backend lives in `platform::camera`; this
//! module wraps it and saves one still frame with the app's encoder.

pub use platform::camera::{CameraIndex, CameraInfo, CameraSource};

use std::path::PathBuf;

pub fn list() -> Result<Vec<CameraInfo>, String> {
    platform::camera::list()
}

pub fn photo(index: &CameraIndex) -> Result<PathBuf, String> {
    let mut source = CameraSource::open(index)?;
    let frame = source.frame()?;
    let path = platform::os::output_path(platform::OutputCategory::Camera, "Camera", "png")?;
    crate::media::save_frame(&frame, &path)?;
    Ok(path)
}
