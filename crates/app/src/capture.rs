//! The recording engine: owns the Live loop that pulls frames from the screen
//! backend, samples from the audio backend and a camera, and muxes them through
//! the FFmpeg writer. Everything OS-specific comes in through `platform`.

use crate::camera::CameraSource;
use crate::media::{self, Format, Frame, RecordOptions, Rect, VideoWriter};
use platform::audio::{open_stream, AudioIn};
use platform::frame::{blank, fit, frame_consistent};
use platform::geom::{audio_samples_before_frame, frame_slot, mix_from};
use platform::i18n::tr;
use platform::screen::{
    crop_for, monitor_of, start_monitor, start_window, take_window, window_usable, ScreenCapture,
    Window,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

fn microphone_name(options: &RecordOptions) -> Option<String> {
    let id = options.profile.audio.device.as_str();
    if id == "Auto" {
        None
    } else {
        media::device_name(id)
    }
}

struct Prepared {
    output: PathBuf,
    scratch: PathBuf,
    format: Format,
    fps: u32,
    height: u32,
    animate: bool,
}

fn working_path(output: &Path, animate: bool) -> PathBuf {
    if animate {
        return output.with_extension("working.mkv");
    }
    let name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("CaptureButton.bin");
    std::env::temp_dir().join(format!("CaptureButton-{name}"))
}

fn publish_file(from: &Path, to: &Path) -> Result<(), String> {
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to).map_err(|error| error.to_string())?;
    let _ = std::fs::remove_file(from);
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SourceFate {
    Live,
    Card,
    Hold,
    Stop,
}

fn source_fate(tracked: bool, signal: bool, keep: bool, show_card: bool) -> SourceFate {
    if !tracked || signal {
        SourceFate::Live
    } else if !keep {
        SourceFate::Stop
    } else if show_card {
        SourceFate::Card
    } else {
        SourceFate::Hold
    }
}

/// A tracked source still has a picture when a frame just arrived, or when the
/// window is on screen and the capture session is running. A minimized, hidden
/// or closed window with nothing new arriving has no signal.
fn signal_present(tracked: bool, usable: bool, capture_alive: bool, frame_arrived: bool) -> bool {
    if !tracked {
        return true;
    }
    frame_arrived || (usable && capture_alive)
}

fn record(
    options: RecordOptions,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<(), String>>,
) -> Result<Prepared, String> {
    match prepare(&options, &stop) {
        Ok(session) => {
            let partial = session.scratch.clone();
            let animate = session.animate;
            let _ = ready.send(Ok(()));
            match session.run(options, stop) {
                Ok(prepared) => Ok(prepared),
                Err(error) => {
                    if !animate {
                        let _ = std::fs::remove_file(&partial);
                    }
                    Err(error)
                }
            }
        }
        Err(error) => {
            let _ = ready.send(Err(error.clone()));
            Err(error)
        }
    }
}

struct Live {
    rect: Rect,
    output: PathBuf,
    scratch: PathBuf,
    animate: bool,
    clock: Instant,
    inputs: Vec<AudioIn>,
    video: Option<ScreenCapture>,
    camera: Option<CameraSource>,
    writer: VideoWriter,
    last: Option<Frame>,
    recover: Option<Window>,
    resilient: bool,
    show_no_signal: bool,
    show_card: bool,
    cursor: bool,
    placeholder: Option<Frame>,
    next_retry: Instant,
    was_usable: bool,
}

impl Live {
    fn try_recover(&mut self, fps: u32, force: bool) {
        let Some(window) = self.recover else {
            if !force
                && self
                    .video
                    .as_ref()
                    .is_some_and(|source| source.capture_alive())
            {
                return;
            }
            if let Some(source) = &mut self.video {
                source.stop();
            }
            if let Ok((capture, _)) = start_monitor(monitor_of(self.rect).0, None, fps, self.cursor)
            {
                self.video = Some(capture);
            }
            return;
        };
        if !window_usable(window) {
            return;
        }
        if !force && self.video.as_ref().is_some_and(|source| source.available()) {
            return;
        }
        if let Some(source) = &mut self.video {
            source.stop();
        }
        if let Ok((capture, _)) = start_window(window, fps, self.cursor) {
            self.video = Some(capture);
        } else {
            self.video = None;
        }
    }

    fn pull_audio(&mut self, cursor: &mut u64, record_from: u64, until: u64) -> Result<(), String> {
        loop {
            if *cursor >= until {
                return Ok(());
            }
            let mut queues = Vec::with_capacity(self.inputs.len());
            for input in &self.inputs {
                queues.push(
                    input
                        .queue
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                        .clone(),
                );
            }
            let mixed = mix_from(&mut queues, *cursor, until, 48_000);
            if mixed.is_empty() {
                return Ok(());
            }
            let frames = (mixed.len() / 2) as u64;
            self.writer
                .push_audio_at(cursor.saturating_sub(record_from), &mixed)?;
            *cursor += frames;
            for input in &self.inputs {
                input
                    .queue
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .drop_before(*cursor);
            }
            if frames < 48_000 {
                return Ok(());
            }
        }
    }

    fn run(mut self, options: RecordOptions, stop: Arc<AtomicBool>) -> Result<Prepared, String> {
        let origin_ns = self.clock.elapsed().as_nanos();
        let record_from = platform::geom::sample_index(origin_ns, 48_000);
        for input in &self.inputs {
            input
                .queue
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .drop_before(record_from);
        }
        let mut cursor = record_from;
        let mut last_slot = -1i64;
        let fps = options.profile.video.fps.max(1) as u64;
        let frame_time = Duration::from_nanos(1_000_000_000 / fps);
        let mut next_deadline = Instant::now();
        let loop_result = loop {
            let mut fate = SourceFate::Live;
            if !options.audio_only {
                if let Some(source) = self.camera.as_mut() {
                    match source.frame() {
                        Ok(frame) => {
                            self.last = Some(fit(frame, self.rect.w, self.rect.h));
                        }
                        Err(error) => break Err(error),
                    }
                } else {
                    let usable = self.recover.map(window_usable).unwrap_or(true);
                    let resumed = self.resilient && usable && !self.was_usable;
                    self.was_usable = usable;
                    let alive = self
                        .video
                        .as_ref()
                        .is_some_and(|source| source.capture_alive());
                    if self.resilient
                        && usable
                        && (resumed || !alive)
                        && Instant::now() >= self.next_retry
                    {
                        // Restoring a minimized window restarts capture and blocks this loop.
                        // Audio captured during that wait is muxed before the frame stamped after it.
                        self.try_recover(options.profile.video.fps, resumed);
                        self.next_retry = Instant::now() + Duration::from_millis(250);
                    }
                    let arrived = self.video.as_mut().and_then(|source| source.latest());
                    let alive = self
                        .video
                        .as_ref()
                        .is_some_and(|source| source.capture_alive());
                    let signal = signal_present(self.resilient, usable, alive, arrived.is_some());
                    fate = source_fate(self.resilient, signal, self.show_no_signal, self.show_card);
                    if let Some(frame) = arrived {
                        if fate == SourceFate::Live {
                            self.last = Some(if self.resilient {
                                fit_into(frame, self.rect.w, self.rect.h)
                            } else {
                                fit(frame, self.rect.w, self.rect.h)
                            });
                        }
                    } else if !self.resilient {
                        let grabbed = if self.cursor {
                            media::grab(self.rect)
                        } else {
                            media::grab_without_cursor(self.rect)
                        };
                        if let Ok(frame) = grabbed {
                            self.last = Some(frame);
                        }
                    }
                }
            }
            let since_run = self
                .clock
                .elapsed()
                .as_nanos()
                .saturating_sub(origin_ns)
                .saturating_sub(200_000_000);
            let slot = frame_slot(since_run, fps);
            let horizon = if options.audio_only {
                platform::geom::sample_index(self.clock.elapsed().as_nanos(), 48_000)
                    .saturating_sub(48_000 / 5)
            } else {
                record_from.saturating_add(audio_samples_before_frame(slot, fps))
            };
            if let Err(error) = self.pull_audio(&mut cursor, record_from, horizon) {
                break Err(error);
            }
            if !options.audio_only {
                if fate == SourceFate::Stop {
                    if slot > last_slot {
                        if let Some(frame) = self.last.as_ref() {
                            if let Err(error) = self.writer.push_at(frame, slot) {
                                break Err(error);
                            }
                        }
                    }
                    break Ok(());
                }
                if slot > last_slot {
                    if fate == SourceFate::Card && self.placeholder.is_none() {
                        self.placeholder = Some(no_signal(self.rect.w, self.rect.h));
                    }
                    let frame = if fate == SourceFate::Card {
                        self.placeholder.as_ref()
                    } else {
                        self.last.as_ref()
                    };
                    if let Some(frame) = frame {
                        if let Err(error) = self.writer.push_at(frame, slot) {
                            break Err(error);
                        }
                    }
                    last_slot = slot;
                }
            }
            if stop.load(Ordering::SeqCst) {
                break Ok(());
            }
            next_deadline += frame_time;
            let now = Instant::now();
            if next_deadline > now {
                std::thread::sleep(next_deadline - now);
            } else {
                next_deadline = now;
            }
        };
        let flush_result = self.pull_audio(&mut cursor, record_from, u64::MAX);
        self.inputs.clear();
        if let Some(source) = &mut self.video {
            source.stop();
        }
        let written = self.writer.finish();
        let written = loop_result.and(flush_result).and(written)?;
        if !self.animate {
            let keep = if options.audio_only {
                written.audio_packets > 0
            } else {
                written.video_packets > 0
            };
            if !keep {
                let _ = std::fs::remove_file(&self.scratch);
                return Err(if options.audio_only {
                    tr("Recording produced no audio")
                } else {
                    tr("Recording produced no video")
                });
            }
            publish_file(&self.scratch, &self.output)?;
        }
        Ok(Prepared {
            output: self.output.clone(),
            scratch: self.scratch.clone(),
            format: options.profile.format,
            fps: options.profile.video.fps,
            height: options.profile.video.height,
            animate: self.animate,
        })
    }
}

fn live_threads(requested: u32, width: u32, height: u32, fps: u32) -> u32 {
    if requested != 0 {
        return requested.max(1);
    }
    let cores = std::thread::available_parallelism()
        .map(|count| count.get() as u32)
        .unwrap_or(4);
    let heavy = u64::from(width) * u64::from(height) * u64::from(fps) > 1280 * 720 * 30;
    if heavy {
        cores.clamp(2, 8)
    } else {
        cores.clamp(1, 4)
    }
}

fn prepare(options: &RecordOptions, _stop: &AtomicBool) -> Result<Live, String> {
    let mut rect = options.rect.unwrap_or_else(media::cursor_monitor_rect);
    rect.w &= !1;
    rect.h &= !1;
    if !options.audio_only && (rect.w < 16 || rect.h < 16) {
        return Err(tr("Capture area is too small"));
    }
    let output = if options.audio_only {
        media::output_path(media::OutputCategory::Sound, "Audio", "mp3")?
    } else if options.camera.is_some() {
        media::output_path(
            media::OutputCategory::Camera,
            "Camera",
            options.profile.format.extension(),
        )?
    } else {
        media::output_path(
            media::OutputCategory::Video,
            if options.profile.format == Format::Mkv {
                "Video"
            } else {
                "Animation"
            },
            options.profile.format.extension(),
        )?
    };
    let animate = !options.audio_only && options.profile.format != Format::Mkv;
    let scratch = working_path(&output, animate);
    let audio_requested = (options.audio_only || options.profile.format == Format::Mkv)
        && (options.profile.audio.system || options.profile.audio.microphone);
    if options.audio_only && !audio_requested {
        return Err(tr(
            "Enable System audio or Microphone before capturing audio",
        ));
    }
    let clock = Instant::now();
    let mut inputs = Vec::new();
    if audio_requested && options.profile.audio.system {
        inputs.push(open_stream(
            true,
            None,
            clock,
            Arc::new(AtomicU32::new(100)),
        )?);
    }
    if audio_requested && options.profile.audio.microphone {
        inputs.push(open_stream(
            false,
            microphone_name(options).as_deref(),
            clock,
            options.mic_gain.clone(),
        )?);
    }
    let mut recover = None;
    let mut camera = None;
    let mut camera_frame = None;
    if !options.audio_only {
        if let Some(index) = options.camera.as_ref() {
            let mut source = CameraSource::open(index)?;
            let frame = source.frame()?;
            rect.w = frame.width.max(2) & !1;
            rect.h = frame.height.max(2) & !1;
            camera_frame = Some(frame);
            camera = Some(source);
        }
    }
    let mut video = if options.audio_only || camera.is_some() {
        None
    } else if options.window {
        let window = take_window().ok_or(tr("The window is no longer available"))?;
        let (capture, frame) = start_window(window, options.profile.video.fps, options.cursor)?;
        rect.w = frame.width.max(2) & !1;
        rect.h = frame.height.max(2) & !1;
        recover = Some(window);
        Some(capture)
    } else {
        let (monitor, origin_x, origin_y, monitor_w, monitor_h) = monitor_of(rect);
        match start_monitor(
            monitor,
            crop_for(rect, origin_x, origin_y, monitor_w, monitor_h),
            options.profile.video.fps,
            options.cursor,
        ) {
            Ok((capture, _)) => Some(capture),
            Err(error) => {
                if media::grab(rect).is_err() {
                    return Err(error);
                }
                None
            }
        }
    };
    let threads = live_threads(options.threads, rect.w, rect.h, options.profile.video.fps);
    let writer_rect = if options.audio_only {
        Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 16,
        }
    } else {
        rect
    };
    let writer = match VideoWriter::new(
        &scratch,
        writer_rect,
        options.profile.video,
        options.profile.format,
        threads,
        !options.audio_only,
        audio_requested,
    ) {
        Ok(writer) => writer,
        Err(error) => {
            let _ = std::fs::remove_file(&scratch);
            return Err(error);
        }
    };
    let last = if options.audio_only {
        None
    } else {
        let raw = camera_frame
            .take()
            .or_else(|| video.as_mut().and_then(|source| source.latest()))
            .or_else(|| {
                if options.cursor {
                    media::grab(rect).ok()
                } else {
                    media::grab_without_cursor(rect).ok()
                }
            })
            .unwrap_or(Frame {
                width: rect.w,
                height: rect.h,
                data: vec![0; rect.w as usize * rect.h as usize * 4],
            });
        // An odd window size is locked to the next even size. The stored frame must already
        // be that size, or the encoder slice panics and the release build aborts.
        Some(if options.window || options.resilient {
            fit_into(raw, rect.w, rect.h)
        } else {
            fit(raw, rect.w, rect.h)
        })
    };
    Ok(Live {
        rect,
        output,
        scratch,
        animate,
        clock,
        inputs,
        video,
        camera,
        writer,
        last,
        recover,
        resilient: (options.resilient || options.window) && options.camera.is_none(),
        show_no_signal: options.show_no_signal,
        show_card: options.show_card,
        cursor: options.cursor,
        placeholder: None,
        next_retry: Instant::now(),
        was_usable: true,
    })
}

pub struct Session {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<Prepared, String>>>,
    error: std::sync::mpsc::Receiver<String>,
}

impl Session {
    pub fn start(options: RecordOptions) -> Result<Self, String> {
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (error_tx, error_rx) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let thread = std::thread::Builder::new()
            .name("CaptureButton-capture".into())
            .spawn(move || {
                let result = record(options, worker_stop, ready_tx);
                if let Err(error) = &result {
                    let _ = error_tx.send(error.clone());
                }
                result
            })
            .map_err(|error| error.to_string())?;
        match ready_rx.recv_timeout(Duration::from_secs(8)) {
            Ok(Ok(())) => Ok(Self {
                stop,
                thread: Some(thread),
                error: error_rx,
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                Err(error)
            }
            Err(_) => {
                stop.store(true, Ordering::SeqCst);
                let _ = thread.join();
                Err(tr("Capture did not start"))
            }
        }
    }

    pub fn tick(&mut self) -> Result<(), String> {
        match self.error.try_recv() {
            Ok(error) => Err(error),
            Err(_) => Ok(()),
        }
    }

    pub fn finished(&self) -> bool {
        self.thread
            .as_ref()
            .is_some_and(|thread| thread.is_finished())
    }

    pub fn stop(mut self) -> Result<Finalizer, String> {
        self.stop.store(true, Ordering::SeqCst);
        let prepared = self
            .thread
            .take()
            .ok_or(tr("Capture is already stopping"))?
            .join()
            .map_err(|_| tr("Capture thread stopped unexpectedly").to_string())??;
        if prepared.animate {
            let scratch = prepared.scratch;
            let output = prepared.output;
            let format = prepared.format;
            let fps = prepared.fps;
            let height = prepared.height;
            let worker = std::thread::Builder::new()
                .name("CaptureButton-encode".into())
                .spawn(move || {
                    media::convert_animation(&scratch, &output, format, fps, height)?;
                    let _ = std::fs::remove_file(&scratch);
                    Ok(output)
                })
                .map_err(|error| error.to_string())?;
            Ok(Finalizer {
                worker: Some(worker),
                ready: None,
            })
        } else {
            Ok(Finalizer {
                worker: None,
                ready: Some(Ok(prepared.output)),
            })
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub struct Finalizer {
    worker: Option<JoinHandle<Result<PathBuf, String>>>,
    ready: Option<Result<PathBuf, String>>,
}

impl Finalizer {
    pub fn step(&mut self) -> Result<Option<PathBuf>, String> {
        if let Some(result) = self.ready.take() {
            return result.map(Some);
        }
        let Some(worker) = &self.worker else {
            return Ok(None);
        };
        if !worker.is_finished() {
            return Ok(None);
        }
        let worker = self.worker.take().unwrap();
        Ok(Some(worker.join().map_err(|_| {
            tr("Encoder thread stopped unexpectedly")
        })??))
    }
}

fn fit_into(frame: Frame, width: u32, height: u32) -> Frame {
    let width = width & !1;
    let height = height & !1;
    if !frame_consistent(&frame) {
        return blank(width, height);
    }
    if frame.width == width && frame.height == height {
        return frame;
    }
    let scale =
        (width as f64 / frame.width.max(1) as f64).min(height as f64 / frame.height.max(1) as f64);
    let (sw, sh) = if scale < 1.0 {
        (
            ((frame.width as f64 * scale).round() as u32).clamp(1, width),
            ((frame.height as f64 * scale).round() as u32).clamp(1, height),
        )
    } else {
        (frame.width.min(width), frame.height.min(height))
    };
    let scaled = if sw == frame.width && sh == frame.height {
        frame
    } else {
        stretch(&frame, sw, sh)
    };
    let mut out = vec![0u8; width as usize * height as usize * 4];
    let x0 = (width - scaled.width) / 2;
    let y0 = (height - scaled.height) / 2;
    for row in 0..scaled.height as usize {
        let from = row * scaled.width as usize * 4;
        let to = ((y0 as usize + row) * width as usize + x0 as usize) * 4;
        out[to..to + scaled.width as usize * 4]
            .copy_from_slice(&scaled.data[from..from + scaled.width as usize * 4]);
    }
    Frame {
        width,
        height,
        data: out,
    }
}

fn stretch(frame: &Frame, width: u32, height: u32) -> Frame {
    let mut data = vec![0u8; width as usize * height as usize * 4];
    for y in 0..height {
        let sy = y as usize * frame.height as usize / height.max(1) as usize;
        for x in 0..width {
            let sx = x as usize * frame.width as usize / width.max(1) as usize;
            let from = (sy * frame.width as usize + sx) * 4;
            let to = (y as usize * width as usize + x as usize) * 4;
            data[to..to + 4].copy_from_slice(&frame.data[from..from + 4]);
        }
    }
    Frame {
        width,
        height,
        data,
    }
}

fn no_signal(width: u32, height: u32) -> Frame {
    let (source_w, source_h, pixels) = decoded_card();
    stretch(
        &Frame {
            width: *source_w,
            height: *source_h,
            data: pixels.clone(),
        },
        width,
        height,
    )
}

fn decoded_card() -> &'static (u32, u32, Vec<u8>) {
    static CARD: std::sync::OnceLock<(u32, u32, Vec<u8>)> = std::sync::OnceLock::new();
    CARD.get_or_init(|| {
        let decoder = png::Decoder::new(std::io::Cursor::new(include_bytes!(
            "../../../assets/no-signal.png"
        )));
        let mut reader = decoder.read_info().expect("no-signal.png");
        let mut buffer = vec![0; reader.output_buffer_size()];
        let info = reader.next_frame(&mut buffer).expect("no-signal.png");
        buffer.truncate(info.buffer_size());
        let pixels = match info.color_type {
            png::ColorType::Rgba => buffer,
            png::ColorType::Rgb => buffer
                .chunks(3)
                .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 255])
                .collect(),
            _ => buffer
                .chunks(4)
                .flat_map(|pixel| {
                    [
                        pixel[0],
                        pixel.get(1).copied().unwrap_or(0),
                        pixel.get(2).copied().unwrap_or(0),
                        255,
                    ]
                })
                .collect(),
        };
        (info.width, info.height, pixels)
    })
}

#[cfg(test)]
mod tests {
    use super::{fit_into, no_signal, signal_present, source_fate, SourceFate};
    use crate::media::Frame;
    use crate::settings::Settings;
    use platform::frame::fit;

    struct SourceRecording {
        tracked: bool,
        show_no_signal: bool,
        show_card: bool,
        frames: Vec<SourceFate>,
        stopped: bool,
    }

    impl SourceRecording {
        fn tracked(show_no_signal: bool) -> Self {
            Self {
                tracked: true,
                show_no_signal,
                show_card: true,
                frames: Vec::new(),
                stopped: false,
            }
        }

        fn see(&mut self, available: bool) {
            if self.stopped {
                return;
            }
            match source_fate(self.tracked, available, self.show_no_signal, self.show_card) {
                SourceFate::Stop => self.stopped = true,
                fate => self.frames.push(fate),
            }
        }
    }

    #[test]
    fn no_signal_is_only_for_a_window_that_stopped_sending_frames() {
        assert!(
            signal_present(true, true, true, false),
            "a quiet window that is still open keeps its picture"
        );
        assert!(
            signal_present(true, false, true, true),
            "a frame that just arrived is a real signal"
        );
        assert!(
            !signal_present(true, false, true, false),
            "minimized or closed with no new frame"
        );
        assert!(!signal_present(true, true, false, false));
        assert!(signal_present(false, false, false, false));
        assert_eq!(
            source_fate(true, false, true, true),
            SourceFate::Card,
            "the card replaces the picture only when the signal is gone"
        );
        assert_eq!(source_fate(true, false, true, false), SourceFate::Hold);
        assert_eq!(source_fate(true, false, false, true), SourceFate::Stop);
        assert_eq!(source_fate(true, true, true, true), SourceFate::Live);
    }

    #[test]
    fn the_no_signal_card_is_on_by_default() {
        assert!(Settings::default().show_no_signal);
        assert!(Settings::default().show_card);
    }

    #[test]
    fn a_lost_source_keeps_recording_when_the_card_is_on() {
        let mut recording = SourceRecording::tracked(true);
        recording.see(true);
        recording.see(false);
        recording.see(false);
        recording.see(true);
        recording.see(false);
        assert!(!recording.stopped);
        assert_eq!(
            recording.frames,
            [
                SourceFate::Live,
                SourceFate::Card,
                SourceFate::Card,
                SourceFate::Live,
                SourceFate::Card,
            ]
        );
        let card = no_signal(320, 180);
        assert_eq!((card.width, card.height), (320, 180));
        assert!(card
            .data
            .chunks(4)
            .any(|pixel| pixel[0] > 20 || pixel[1] > 20 || pixel[2] > 20));
    }

    #[test]
    fn an_ordinary_capture_keeps_going_without_a_tracked_source() {
        let mut recording = SourceRecording {
            tracked: false,
            show_no_signal: false,
            show_card: true,
            frames: Vec::new(),
            stopped: false,
        };
        recording.see(false);
        recording.see(false);
        assert!(!recording.stopped);
        assert_eq!(recording.frames, [SourceFate::Live, SourceFate::Live]);
    }

    #[test]
    fn a_lost_source_keeps_the_last_frame_when_the_card_is_turned_off() {
        let mut recording = SourceRecording::tracked(true);
        recording.show_card = false;
        recording.see(true);
        recording.see(false);
        recording.see(true);
        assert!(!recording.stopped);
        assert_eq!(
            recording.frames,
            [SourceFate::Live, SourceFate::Hold, SourceFate::Live]
        );
    }

    #[test]
    fn a_lost_source_stops_recording_when_the_card_is_off() {
        let mut recording = SourceRecording::tracked(false);
        recording.see(true);
        recording.see(true);
        recording.see(false);
        recording.see(true);
        assert!(recording.stopped);
        assert_eq!(recording.frames, [SourceFate::Live, SourceFate::Live]);
    }

    #[test]
    fn the_no_signal_card_fills_the_locked_frame() {
        let frame = no_signal(320, 180);
        assert_eq!((frame.width, frame.height), (320, 180));
        assert!(frame
            .data
            .chunks(4)
            .any(|pixel| pixel[0] > 20 || pixel[1] > 20 || pixel[2] > 20));
    }

    #[test]
    fn a_torn_frame_after_restore_does_not_abort() {
        let torn = Frame {
            width: 800,
            height: 600,
            data: vec![9, 8, 7, 255],
        };
        let fitted = fit_into(torn, 640, 480);
        assert_eq!((fitted.width, fitted.height), (640, 480));
        assert_eq!(fitted.data.len(), 640 * 480 * 4);
        let cropped = fit(
            Frame {
                width: 320,
                height: 180,
                data: vec![1, 2, 3, 4],
            },
            200,
            100,
        );
        assert_eq!((cropped.width, cropped.height), (200, 100));
        assert_eq!(cropped.data.len(), 200 * 100 * 4);
    }

    #[test]
    fn a_resized_window_stays_inside_the_original_frame() {
        let source = Frame {
            width: 4,
            height: 2,
            data: [255u8, 0, 0, 255].repeat(8),
        };
        let fitted = fit_into(source, 8, 4);
        assert_eq!(
            (fitted.width, fitted.height, fitted.data.len()),
            (8, 4, 8 * 4 * 4)
        );
        assert!(fitted.data.chunks(4).any(|pixel| pixel[..3] == [0, 0, 0]));
        assert!(fitted.data.chunks(4).any(|pixel| pixel[0] == 255));
    }
}
