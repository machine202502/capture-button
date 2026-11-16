use crate::media::{self, Frame};
use crate::tr;
use ffmpeg_next as ffmpeg;
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

fn decode_mjpeg(encoded: &[u8]) -> Result<Frame, String> {
    let codec = ffmpeg::decoder::find_by_name("mjpeg")
        .ok_or_else(|| tr("The MJPEG decoder is unavailable").to_string())?;
    let mut decoder = ffmpeg::codec::context::Context::new_with_codec(codec)
        .decoder()
        .video()
        .map_err(|error| error.to_string())?;
    let packet = ffmpeg::Packet::copy(encoded);
    decoder
        .send_packet(&packet)
        .map_err(|error| error.to_string())?;
    let mut decoded = ffmpeg::frame::Video::empty();
    decoder
        .receive_frame(&mut decoded)
        .map_err(|error| error.to_string())?;
    let width = decoded.width();
    let height = decoded.height();
    if width < 2 || height < 2 {
        return Err(tr("The camera returned an empty frame"));
    }
    let mut scaler = ffmpeg::software::scaling::Context::get(
        decoded.format(),
        width,
        height,
        ffmpeg::format::Pixel::BGRA,
        width,
        height,
        ffmpeg::software::scaling::flag::Flags::BILINEAR,
    )
    .map_err(|error| error.to_string())?;
    let mut converted = ffmpeg::frame::Video::empty();
    scaler
        .run(&decoded, &mut converted)
        .map_err(|error| error.to_string())?;
    let stride = converted.stride(0);
    let row = width as usize * 4;
    let mut data = vec![0u8; row * height as usize];
    for y in 0..height as usize {
        let source = &converted.data(0)[y * stride..y * stride + row];
        data[y * row..(y + 1) * row].copy_from_slice(source);
    }
    Ok(Frame {
        width,
        height,
        data,
    })
}

fn camera_error(error: &nokhwa::NokhwaError) -> String {
    format!("{}: {error}", tr("Camera"))
}
