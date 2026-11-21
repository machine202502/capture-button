use crate::media::{self, Frame};
use crate::tr;
use nokhwa::pixel_format::YuyvFormat;
pub use nokhwa::utils::CameraIndex;
use nokhwa::utils::{ApiBackend, FrameFormat, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use std::path::PathBuf;

pub struct CameraInfo {
    pub name: String,
    pub index: CameraIndex,
}

pub fn list() -> Result<Vec<CameraInfo>, String> {
    let devices =
        nokhwa::query(ApiBackend::MediaFoundation).map_err(|error| camera_error(&error))?;
    Ok(devices
        .into_iter()
        .map(|device| CameraInfo {
            name: device.human_name(),
            index: device.index().clone(),
        })
        .collect())
}

pub struct CameraSource {
    camera: Camera,
}

impl CameraSource {
    pub fn open(index: &CameraIndex) -> Result<Self, String> {
        let format = RequestedFormat::with_formats(
            RequestedFormatType::AbsoluteHighestFrameRate,
            &[FrameFormat::MJPEG, FrameFormat::YUYV, FrameFormat::NV12],
        );
        let mut camera =
            Camera::new(index.clone(), format).map_err(|error| camera_error(&error))?;
        camera.open_stream().map_err(|error| camera_error(&error))?;
        Ok(Self { camera })
    }

    pub fn frame(&mut self) -> Result<Frame, String> {
        let buffer = self.camera.frame().map_err(|error| camera_error(&error))?;
        match buffer.source_frame_format() {
            FrameFormat::MJPEG => decode_mjpeg(buffer.buffer()),
            FrameFormat::NV12 => decode_nv12(buffer.buffer(), buffer.resolution()),
            FrameFormat::YUYV => {
                let image = buffer
                    .decode_image::<YuyvFormat>()
                    .map_err(|error| camera_error(&error))?;
                frame_from_rgb(image.dimensions(), image.into_raw())
            }
            format => Err(format!("{}: {format:?}", tr("Unsupported camera format"))),
        }
    }
}

pub fn photo(index: &CameraIndex) -> Result<PathBuf, String> {
    let mut source = CameraSource::open(index)?;
    let frame = source.frame()?;
    let path = media::output_path("Image", "png")?;
    media::save_frame(&frame, &path)?;
    Ok(path)
}

fn frame_from_rgb((width, height): (u32, u32), rgb: Vec<u8>) -> Result<Frame, String> {
    if width < 2 || height < 2 {
        return Err(tr("The camera returned an empty frame"));
    }
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for (index, source) in rgb.chunks(3).enumerate() {
        let target = &mut data[index * 4..index * 4 + 4];
        target[0] = source[2];
        target[1] = source[1];
        target[2] = source[0];
        target[3] = 255;
    }
    Ok(Frame {
        width,
        height,
        data,
    })
}

fn decode_nv12(encoded: &[u8], resolution: nokhwa::utils::Resolution) -> Result<Frame, String> {
    let width = resolution.width() as usize;
    let height = resolution.height() as usize;
    if width < 2 || height < 2 || encoded.len() < width * height * 3 / 2 {
        return Err(tr("The camera returned an empty frame"));
    }
    let y_plane = &encoded[..width * height];
    let uv_plane = &encoded[width * height..];
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let luma = y_plane[y * width + x] as f32;
            let uv = ((y / 2) * width + (x / 2) * 2) & !1;
            let u = uv_plane[uv] as f32 - 128.0;
            let v = uv_plane[uv + 1] as f32 - 128.0;
            let r = (luma + 1.402 * v).clamp(0.0, 255.0) as u8;
            let g = (luma - 0.344_136 * u - 0.714_136 * v).clamp(0.0, 255.0) as u8;
            let b = (luma + 1.772 * u).clamp(0.0, 255.0) as u8;
            let target = (y * width + x) * 4;
            data[target] = b;
            data[target + 1] = g;
            data[target + 2] = r;
            data[target + 3] = 255;
        }
    }
    Ok(Frame {
        width: width as u32,
        height: height as u32,
        data,
    })
}

/// UVC cameras send MJPEG as ordinary JPEG frames, so the pure-Rust decoder is
/// enough. FFmpeg is built for this app without the `mjpeg` decoder on purpose,
/// and every camera that negotiates MJPEG would otherwise fail here.
fn decode_mjpeg(encoded: &[u8]) -> Result<Frame, String> {
    let decoded = image::load_from_memory_with_format(encoded, image::ImageFormat::Jpeg)
        .map_err(|error| format!("{}: {error}", tr("The camera frame could not be decoded")))?;
    let rgb = decoded.into_rgb8();
    frame_from_rgb(rgb.dimensions(), rgb.into_raw())
}

fn camera_error(error: &nokhwa::NokhwaError) -> String {
    format!("{}: {error}", tr("Camera"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 32×24 JPEG: red left half, blue right half.
    const MJPEG_FRAME: &[u8] = include_bytes!("../tests/data/camera-frame.jpg");

    fn pixel(frame: &Frame, x: usize, y: usize) -> [u8; 4] {
        let index = (y * frame.width as usize + x) * 4;
        frame.data[index..index + 4].try_into().unwrap()
    }

    #[test]
    fn an_mjpeg_frame_becomes_bgra_without_ffmpeg() {
        let frame = decode_mjpeg(MJPEG_FRAME).expect("the fixture decodes");
        assert_eq!((frame.width, frame.height), (32, 24));
        assert_eq!(frame.data.len(), 32 * 24 * 4);

        let red = pixel(&frame, 8, 12);
        assert!(red[2] > 150 && red[1] < 90 && red[0] < 90, "{red:?}");
        let blue = pixel(&frame, 24, 12);
        assert!(blue[0] > 150 && blue[1] < 110 && blue[2] < 90, "{blue:?}");
        assert_eq!(red[3], 255);
        assert_eq!(blue[3], 255);
    }

    #[test]
    fn a_broken_frame_reports_the_translated_message() {
        let error = decode_mjpeg(b"not a jpeg").unwrap_err();
        assert!(
            error.starts_with("The camera frame could not be decoded"),
            "{error}"
        );
    }
}
