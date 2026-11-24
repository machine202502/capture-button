//! The Linux camera interface.

use platform_core::camera::CameraApi;

pub fn api() -> CameraApi {
    CameraApi::Video4Linux
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_linux_camera_opens_the_video4linux_interface() {
        assert_eq!(api(), CameraApi::Video4Linux);
    }
}
