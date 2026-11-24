//! The Windows camera interface.

use platform_core::camera::CameraApi;

pub fn api() -> CameraApi {
    CameraApi::MediaFoundation
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_windows_camera_opens_the_media_foundation_interface() {
        assert_eq!(api(), CameraApi::MediaFoundation);
    }
}
