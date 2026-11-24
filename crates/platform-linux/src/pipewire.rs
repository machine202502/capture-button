//! A PipeWire consumer for the screen-cast node the portal starts.
//!
//! The portal hands out a file descriptor for its PipeWire remote and the node
//! id of the selected monitor or window. This module connects a `pw_stream`
//! input to that node, negotiates a packed 32-bit BGRx or RGBx format, and
//! converts every frame to the BGRA `Frame` the application expects.
//!
//! The stream runs on its own thread with a PipeWire main loop. The first
//! frame is returned synchronously so callers keep the "start returns the
//! first frame" contract of the X11 backend; later frames go to a callback.

use pipewire as pw;
use platform_core::types::Frame;
use pw::properties::properties;
use pw::spa;
use std::os::fd::OwnedFd;
use std::sync::mpsc::{self, SyncSender};
use std::time::{Duration, Instant};

/// A running PipeWire stream. Dropping it stops the stream and joins its
/// thread, so the caller can rely on no more frames after it is gone.
pub struct PipeWireCapture {
    stop: pw::channel::Sender<()>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl PipeWireCapture {
    pub fn stop(&mut self) {
        if let Some(thread) = self.thread.take() {
            let _ = self.stop.send(());
            let _ = thread.join();
        }
    }
}

impl Drop for PipeWireCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Connects to `node_id` over `fd` and returns the stream plus its first frame.
///
/// `on_frame` receives every later frame at most once per `interval`, so a
/// 60 Hz compositor does not copy more often than the requested capture rate.
pub fn start(
    fd: OwnedFd,
    node_id: u32,
    fps: u32,
    on_frame: impl FnMut(Frame) + Send + 'static,
) -> Result<(PipeWireCapture, Frame), String> {
    let interval = Duration::from_millis(1000 / u64::from(fps.max(1)));
    let (ready, first) = mpsc::sync_channel(1);
    let (stop, stopped) = pw::channel::channel();
    let thread = std::thread::Builder::new()
        .name("capture-button-pipewire".into())
        .spawn(move || run(fd, node_id, interval, on_frame, ready, stopped))
        .map_err(|error| format!("cannot start the PipeWire thread: {error}"))?;
    match first.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(frame)) => Ok((
            PipeWireCapture {
                stop,
                thread: Some(thread),
            },
            frame,
        )),
        Ok(Err(error)) => {
            let _ = stop.send(());
            let _ = thread.join();
            Err(error)
        }
        Err(_) => {
            // A stuck PipeWire negotiation must not hang the caller forever;
            // ask the loop to quit and leave the thread to finish on its own.
            let _ = stop.send(());
            Err("the PipeWire stream did not produce a frame".into())
        }
    }
}

struct UserData<F> {
    format: spa::param::video::VideoInfoRaw,
    first: Option<SyncSender<Result<Frame, String>>>,
    on_frame: F,
    interval: Duration,
    last: Option<Instant>,
}

fn run<F>(
    fd: OwnedFd,
    node_id: u32,
    interval: Duration,
    on_frame: F,
    ready: SyncSender<Result<Frame, String>>,
    stopped: pw::channel::Receiver<()>,
) where
    F: FnMut(Frame) + Send + 'static,
{
    let error = ready.clone();
    if let Err(message) = stream(fd, node_id, interval, on_frame, ready, stopped) {
        let _ = error.send(Err(message));
    }
}

fn stream<F>(
    fd: OwnedFd,
    node_id: u32,
    interval: Duration,
    on_frame: F,
    ready: SyncSender<Result<Frame, String>>,
    stopped: pw::channel::Receiver<()>,
) -> Result<(), String>
where
    F: FnMut(Frame) + Send + 'static,
{
    pw::init();
    let mainloop =
        pw::main_loop::MainLoopRc::new(None).map_err(|error| format!("PipeWire: {error}"))?;
    let context = pw::context::ContextRc::new(&mainloop, None)
        .map_err(|error| format!("PipeWire context: {error}"))?;
    let core = context
        .connect_fd(fd, None)
        .map_err(|error| format!("PipeWire remote: {error}"))?;
    let stream = pw::stream::StreamBox::new(
        &core,
        "capture-button",
        properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
        },
    )
    .map_err(|error| format!("PipeWire stream: {error}"))?;
    let user = UserData {
        format: spa::param::video::VideoInfoRaw::new(),
        first: Some(ready),
        on_frame,
        interval,
        last: None,
    };
    let listener = stream
        .add_local_listener_with_user_data(user)
        .state_changed(|_, user, _old, new| {
            if let pw::stream::StreamState::Error(message) = new {
                if let Some(first) = user.first.take() {
                    let _ = first.send(Err(format!("the PipeWire stream failed: {message}")));
                }
            }
        })
        .param_changed(|_, user, id, param| {
            let Some(param) = param else {
                return;
            };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            if let Ok((media_type, media_subtype)) = spa::param::format_utils::parse_format(param) {
                if media_type == spa::param::format::MediaType::Video
                    && media_subtype == spa::param::format::MediaSubtype::Raw
                {
                    let _ = user.format.parse(param);
                }
            }
        })
        .process(|stream, user| process(stream, user))
        .register()
        .map_err(|error| format!("PipeWire listener: {error}"))?;

    let object = spa::pod::object!(
        spa::utils::SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaType,
            Id,
            spa::param::format::MediaType::Video
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::MediaSubtype,
            Id,
            spa::param::format::MediaSubtype::Raw
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::BGRx,
            spa::param::video::VideoFormat::RGBx,
            spa::param::video::VideoFormat::BGRA,
            spa::param::video::VideoFormat::RGBA,
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            spa::utils::Rectangle {
                width: 1920,
                height: 1080
            },
            spa::utils::Rectangle {
                width: 1,
                height: 1
            },
            spa::utils::Rectangle {
                width: 16384,
                height: 16384
            }
        ),
        spa::pod::property!(
            spa::param::format::FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            spa::utils::Fraction { num: 30, denom: 1 },
            spa::utils::Fraction { num: 0, denom: 1 },
            spa::utils::Fraction {
                num: 1000,
                denom: 1
            }
        ),
    );
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|error| format!("PipeWire format: {error}"))?
    .0
    .into_inner();
    let mut params = [spa::pod::Pod::from_bytes(&values)
        .ok_or_else(|| "PipeWire rejected the video format".to_string())?];

    let quit = mainloop.clone();
    let _stopped = stopped.attach(mainloop.loop_(), move |_| quit.quit());
    stream
        .connect(
            spa::utils::Direction::Input,
            Some(node_id),
            pw::stream::StreamFlags::AUTOCONNECT | pw::stream::StreamFlags::MAP_BUFFERS,
            &mut params,
        )
        .map_err(|error| format!("PipeWire connect: {error}"))?;
    mainloop.run();
    drop(listener);
    Ok(())
}

fn process<F>(stream: &pw::stream::Stream, user: &mut UserData<F>)
where
    F: FnMut(Frame),
{
    let Some(mut buffer) = stream.dequeue_buffer() else {
        return;
    };
    let datas = buffer.datas_mut();
    let Some(data) = datas.first_mut() else {
        return;
    };
    let first = user.first.is_some();
    let now = Instant::now();
    if !first
        && user
            .last
            .is_some_and(|last| now.duration_since(last) < user.interval)
    {
        return;
    }
    let format = user.format.format();
    let size = user.format.size();
    let stride = data.chunk().stride();
    let offset = data.chunk().offset();
    let length = data.chunk().size();
    let bytes = data.data();
    let frame = match frame_from_data(
        format,
        size.width,
        size.height,
        stride,
        offset,
        bytes,
        length,
    ) {
        Ok(frame) => frame,
        Err(error) => {
            if let Some(first) = user.first.take() {
                let _ = first.send(Err(error));
            }
            return;
        }
    };
    if let Some(first) = user.first.take() {
        user.last = Some(now);
        let _ = first.send(Ok(frame));
    } else {
        user.last = Some(now);
        (user.on_frame)(frame);
    }
}

/// Converts one packed 32-bit RGB frame to the BGRA frame the app expects.
///
/// `bytes` is the mapped buffer, `size` the valid number of bytes in it,
/// `offset` the first row and `stride` the distance between rows. The source
/// channel order is BGRx or RGBx (alpha variants are accepted too) and every
/// output pixel is opaque.
fn frame_from_data(
    format: spa::param::video::VideoFormat,
    width: u32,
    height: u32,
    stride: i32,
    offset: u32,
    bytes: Option<&mut [u8]>,
    size: u32,
) -> Result<Frame, String> {
    if width < 2 || height < 2 {
        return Err("the PipeWire source has no usable size".into());
    }
    if stride <= 0 {
        return Err(format!(
            "the PipeWire source uses an unsupported stride of {stride}"
        ));
    }
    let (width, height) = (width as usize, height as usize);
    let row_bytes = width * 4;
    let offset = offset as usize;
    let stride = stride as usize;
    let needed = offset + (height - 1) * stride + row_bytes;
    let Some(bytes) = bytes else {
        return Err("the PipeWire buffer is not mapped".into());
    };
    if (size as usize) < needed || bytes.len() < needed {
        return Err(format!(
            "the PipeWire buffer is too small: {size} bytes for a {width}×{height} frame"
        ));
    }
    let swap = match format {
        spa::param::video::VideoFormat::BGRx | spa::param::video::VideoFormat::BGRA => false,
        spa::param::video::VideoFormat::RGBx | spa::param::video::VideoFormat::RGBA => true,
        other => {
            return Err(format!(
                "the PipeWire source uses {other:?}; expected BGRx or RGBx"
            ));
        }
    };
    let mut data = vec![0u8; width * height * 4];
    for y in 0..height {
        let source = offset + y * stride;
        let target = y * width * 4;
        for x in 0..width {
            let pixel = source + x * 4;
            let target = target + x * 4;
            if swap {
                data[target] = bytes[pixel + 2];
                data[target + 1] = bytes[pixel + 1];
                data[target + 2] = bytes[pixel];
            } else {
                data[target] = bytes[pixel];
                data[target + 1] = bytes[pixel + 1];
                data[target + 2] = bytes[pixel + 2];
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use spa::param::video::VideoFormat;

    #[test]
    fn bgrx_rows_become_opaque_bgra_frames() {
        // 2×2 pixels, one row of padding after each row.
        let mut bytes = vec![0u8; 12 * 2];
        bytes[0..8].copy_from_slice(&[1, 2, 3, 0, 4, 5, 6, 0]);
        bytes[12..20].copy_from_slice(&[7, 8, 9, 0, 10, 11, 12, 0]);
        let frame = frame_from_data(VideoFormat::BGRx, 2, 2, 12, 0, Some(&mut bytes), 24)
            .expect("a valid BGRx frame");
        assert_eq!((frame.width, frame.height), (2, 2));
        assert_eq!(&frame.data[..4], &[1, 2, 3, 255]);
        assert_eq!(&frame.data[4..8], &[4, 5, 6, 255]);
        assert_eq!(&frame.data[8..12], &[7, 8, 9, 255]);
        assert_eq!(&frame.data[12..16], &[10, 11, 12, 255]);
    }

    #[test]
    fn rgbx_swaps_red_and_blue() {
        // 2×2 pixels with a row stride of 12 bytes, as the compositor pads it.
        let mut bytes = vec![0u8; 24];
        bytes[0..4].copy_from_slice(&[10, 20, 30, 0]);
        let frame = frame_from_data(VideoFormat::RGBx, 2, 2, 12, 0, Some(&mut bytes), 24)
            .expect("a valid RGBx frame");
        assert_eq!(&frame.data[..4], &[30, 20, 10, 255]);
    }

    #[test]
    fn a_short_buffer_is_rejected_instead_of_read_past_the_end() {
        let mut bytes = vec![0u8; 4];
        assert!(frame_from_data(VideoFormat::BGRx, 2, 2, 8, 0, Some(&mut bytes), 4).is_err());
    }

    #[test]
    fn a_planar_or_unknown_format_is_rejected() {
        let mut bytes = vec![0u8; 32];
        assert!(frame_from_data(VideoFormat::I420, 2, 2, 8, 0, Some(&mut bytes), 32).is_err());
    }
}
