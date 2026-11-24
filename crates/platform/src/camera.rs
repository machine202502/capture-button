use crate::i18n::{tr, trf};
use crate::types::Frame;
pub use nokhwa::utils::CameraIndex;
use nokhwa::utils::{ApiBackend, FrameFormat, RequestedFormat, RequestedFormatType};
use nokhwa::Camera;
use std::time::{Duration, Instant};

/// How long opening a camera waits for its first frame before closing the
/// camera and trying once more. Media Foundation can answer "not ready" until
/// the stream is negotiated, which happens shortly after `open_stream`; that
/// wait belongs to opening, not to the recording loop.
///
/// `nokhwa::Camera` is not `Send` and offers no timed read, so `frame()` can
/// block until the first frame arrives and cannot be interrupted. This deadline
/// therefore catches only fast-failing reads (before the stream is negotiated);
/// a blocking read may take longer than two seconds. On a UVC 1920x1080 camera
/// the first frame took 2.1-3.4 s (median 2.8 s) while `open_stream` took
/// 0.00 s, so two seconds plus one retry wait up to about four seconds before
/// giving up. A hard bound would need a read of our own with a timeout instead
/// of nokhwa.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(2);

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
    /// The frame that proved the stream is running. `frame` hands it out
    /// first, so opening does not throw a frame away.
    first: Option<Frame>,
}

/// A failed attempt to open a camera. Only a stream that never produced a
/// first frame is worth reopening; a camera that would not initialise at all
/// fails the same way again.
enum OpenError {
    Setup(String),
    FirstFrame(String),
}

impl CameraSource {
    /// Opens the camera and returns it only once it has delivered a frame.
    /// A caller can therefore treat the first `frame` error as a real fault.
    /// A stream that does not deliver a frame within `FIRST_FRAME_TIMEOUT` is
    /// closed and reopened once.
    pub fn open(index: &CameraIndex) -> Result<Self, String> {
        open_with_retry(|| Self::open_once(index))
    }

    fn open_once(index: &CameraIndex) -> Result<Self, OpenError> {
        let format = RequestedFormat::with_formats(
            RequestedFormatType::AbsoluteHighestFrameRate,
            &[FrameFormat::MJPEG, FrameFormat::YUYV, FrameFormat::NV12],
        );
        let mut camera = Camera::new(index.clone(), format)
            .map_err(|error| OpenError::Setup(camera_error(&error)))?;
        let stream_start = Instant::now();
        camera
            .open_stream()
            .map_err(|error| OpenError::Setup(camera_error(&error)))?;
        let stream_time = stream_start.elapsed();
        let frame_start = Instant::now();
        let first = wait_for_first_frame(FIRST_FRAME_TIMEOUT, || read_frame(&mut camera))
            .map_err(OpenError::FirstFrame)?;
        let frame_time = frame_start.elapsed();
        // One line per open, not per frame: the log is synchronous.
        crate::logging::info(format!(
            "camera open: stream {:.2} s, first frame {:.2} s",
            stream_time.as_secs_f64(),
            frame_time.as_secs_f64()
        ));
        Ok(Self {
            camera,
            first: Some(first),
        })
    }

    pub fn frame(&mut self) -> Result<Frame, String> {
        match self.first.take() {
            Some(frame) => Ok(frame),
            None => read_frame(&mut self.camera),
        }
    }
}

/// Calls `attempt` once and, if the stream opened but stayed silent, once
/// more with a fresh camera. The camera is dropped between the attempts, so
/// the device and its stream are closed before the second open.
fn open_with_retry<T, F>(mut attempt: F) -> Result<T, String>
where
    F: FnMut() -> Result<T, OpenError>,
{
    match attempt() {
        Ok(value) => Ok(value),
        Err(OpenError::Setup(error)) => Err(error),
        Err(OpenError::FirstFrame(_)) => match attempt() {
            Ok(value) => Ok(value),
            Err(OpenError::Setup(error)) => Err(error),
            Err(OpenError::FirstFrame(error)) => Err(format!(
                "{}: {error}",
                trf(
                    "The camera did not start within {seconds} s; the retry did not start within {seconds} s either",
                    &[("seconds", &FIRST_FRAME_TIMEOUT.as_secs_f32().to_string())],
                )
            )),
        },
    }
}

/// Waits for the camera to hand over a decoded frame, retrying until `timeout`
/// expires and returning the last driver error when it does.
///
/// `nokhwa::Camera` is not `Send` in this build: its `Send` impl is behind the
/// `output-threaded` feature, which the platform crate does not enable, and
/// enabling it would also declare the Media Foundation backend shareable
/// across threads. A blocking read in a worker thread, as the screen capture
/// path does, is therefore not available, and nokhwa exposes no timed read.
/// The read itself fails fast until the stream is negotiated, so retrying it
/// with a deadline is the bounded wait that is left. That deadline only bounds
/// fast failures: a blocking read is not interrupted and may outlive it (see
/// `FIRST_FRAME_TIMEOUT`); a hard bound needs a timed read of our own.
fn wait_for_first_frame<F>(timeout: Duration, mut read: F) -> Result<Frame, String>
where
    F: FnMut() -> Result<Frame, String>,
{
    let deadline = Instant::now() + timeout;
    loop {
        match read() {
            Ok(frame) => return Ok(frame),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

fn read_frame(camera: &mut Camera) -> Result<Frame, String> {
    let buffer = camera.frame().map_err(|error| camera_error(&error))?;
    match buffer.source_frame_format() {
        FrameFormat::MJPEG => decode_mjpeg(buffer.buffer()),
        FrameFormat::NV12 => decode_nv12(buffer.buffer(), buffer.resolution()),
        FrameFormat::YUYV => decode_yuyv(buffer.buffer(), buffer.resolution()),
        format => Err(format!("{}: {format:?}", tr("Unsupported camera format"))),
    }
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
        age_ns: 0,
    })
}

/// Converts packed YUYV 4:2:2 to BGRA, tolerating a padded row stride.
fn decode_yuyv(encoded: &[u8], resolution: nokhwa::utils::Resolution) -> Result<Frame, String> {
    let width = resolution.width() as usize;
    let height = resolution.height() as usize;
    if width < 2 || height < 2 || encoded.len() < width * height * 2 {
        return Err(tr("The camera returned an empty frame"));
    }
    let stride = encoded.len() / height;
    if stride < width * 2 {
        return Err(tr("The camera returned an empty frame"));
    }
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        let row = &encoded[y * stride..y * stride + width * 2];
        for pair in 0..width / 2 {
            let y0 = row[pair * 4] as f32;
            let u = row[pair * 4 + 1] as f32 - 128.0;
            let y1 = row[pair * 4 + 2] as f32;
            let v = row[pair * 4 + 3] as f32 - 128.0;
            for (offset, luma) in [(0, y0), (1, y1)] {
                let x = pair * 2 + offset;
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
    }
    Ok(Frame {
        width: width as u32,
        height: height as u32,
        data,
        age_ns: 0,
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
        age_ns: 0,
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
        let error = match decode_mjpeg(b"not a jpeg") {
            Ok(_) => panic!("a non-JPEG must not decode"),
            Err(error) => error,
        };
        assert!(
            error.starts_with("The camera frame could not be decoded"),
            "{error}"
        );
    }

    #[test]
    fn opening_waits_until_the_camera_delivers_its_first_frame() {
        let mut reads = 0;
        let frame = wait_for_first_frame(Duration::from_millis(500), || {
            reads += 1;
            if reads < 3 {
                Err("not ready".to_string())
            } else {
                decode_mjpeg(MJPEG_FRAME)
            }
        })
        .unwrap();
        assert_eq!(reads, 3);
        assert_eq!((frame.width, frame.height), (32, 24));
    }

    #[test]
    fn a_camera_that_never_starts_reports_the_last_driver_error() {
        let started = Instant::now();
        let error = match wait_for_first_frame(Duration::from_millis(50), || {
            Err("not ready".to_string())
        }) {
            Ok(_) => panic!("a camera that never delivers a frame times out"),
            Err(error) => error,
        };
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert_eq!(error, "not ready");
    }

    #[test]
    fn a_ready_camera_is_not_delayed_by_the_wait() {
        let frame =
            wait_for_first_frame(FIRST_FRAME_TIMEOUT, || decode_mjpeg(MJPEG_FRAME)).unwrap();
        assert_eq!((frame.width, frame.height), (32, 24));
    }

    #[test]
    fn a_timed_out_open_retries_exactly_once() {
        let mut attempts = 0;
        let error = match open_with_retry::<(), _>(|| {
            attempts += 1;
            Err(OpenError::FirstFrame(format!(
                "not ready attempt {attempts}"
            )))
        }) {
            Ok(_) => panic!("two timed-out attempts fail to open"),
            Err(error) => error,
        };
        assert_eq!(attempts, 2);
        assert_eq!(error.matches("2 s").count(), 2, "{error}");
        assert!(error.contains("not ready attempt 2"), "{error}");
    }

    #[test]
    fn a_retry_that_delivers_a_frame_succeeds() {
        let mut attempts = 0;
        let frame = open_with_retry(|| {
            attempts += 1;
            if attempts == 1 {
                Err(OpenError::FirstFrame("not ready".to_string()))
            } else {
                decode_mjpeg(MJPEG_FRAME).map_err(OpenError::FirstFrame)
            }
        })
        .unwrap();
        assert_eq!(attempts, 2);
        assert_eq!((frame.width, frame.height), (32, 24));
    }

    #[test]
    fn a_setup_failure_is_not_retried() {
        let mut attempts = 0;
        let error = match open_with_retry::<(), _>(|| {
            attempts += 1;
            Err(OpenError::Setup("device is busy".to_string()))
        }) {
            Ok(_) => panic!("a camera that cannot be set up is an error"),
            Err(error) => error,
        };
        assert_eq!(attempts, 1);
        assert_eq!(error, "device is busy");
    }
}
