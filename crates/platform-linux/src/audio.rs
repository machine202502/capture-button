//! Linux audio capture.
//!
//! Microphones come from cpal/ALSA as before. System audio cannot: ALSA has no
//! loopback of the playback stream, and cpal's "default" device opens the
//! default *capture* source for an input stream, so a recording with "System
//! audio" on was silently recording the microphone. The monitor of the chosen
//! sink is captured with `parec` instead, which is the PulseAudio/PipeWire way
//! to read exactly what the speakers are playing.
//!
//! [`system_sources`] lists the sinks `pactl` knows, so the Settings window can
//! let the user record a specific output instead of whatever is current.
//!
//! Both paths start their source and return at once: nothing waits for the first
//! samples. A device that is still spinning up, or a sink that is simply silent,
//! contributes nothing yet, and the recorder fills the timeline with silence by
//! the recording clock, so a take keeps its duration and the picture stays in
//! sync. [`AudioIn::finish`] stops the source and lets the reader drain what it
//! already captured, so the end of a stop is not cut off.
//!
//! Each buffer also publishes its peak into the shared level meter, which the
//! window draws inside the volume sliders.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use platform_core::geom::{self, AudioQueue};
use platform_core::i18n::{tr, trf};
use platform_core::level::{Level, MICROPHONE, SYSTEM};
use std::collections::HashMap;
use std::io::{ErrorKind, Read};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Monitor capture is requested with `--latency-msec=50`; the same value is
/// subtracted from the read timestamp so a sample stays on the moment it was
/// played instead of the moment its bytes reached the process.
const MONITOR_LATENCY: Duration = Duration::from_millis(50);
const MONITOR_RATE: u64 = 48_000;
const MONITOR_CHANNELS: u16 = 2;
/// float32 stereo: one frame is two 4-byte samples.
const MONITOR_FRAME_BYTES: usize = MONITOR_CHANNELS as usize * 4;

/// One PulseAudio/PipeWire sink the system audio can be recorded from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSource {
    pub id: String,
    pub name: String,
}

/// Lists the playback sinks `pactl` reports.
///
/// `id` is the sink name, which is also the `parec` device without the
/// `.monitor` suffix; `name` is the human description when `pactl list sinks`
/// provides one. The list is empty when there is no PulseAudio/PipeWire server;
/// the UI then keeps only "Auto (current output)".
pub fn system_sources() -> Vec<AudioSource> {
    let short = Command::new("pactl")
        .args(["list", "short", "sinks"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let detail = Command::new("pactl")
        .args(["list", "sinks"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    parse_sinks(&short, &detail)
}

/// Turns `pactl list short sinks` and `pactl list sinks` into the source list.
fn parse_sinks(short: &str, detail: &str) -> Vec<AudioSource> {
    let descriptions = sink_descriptions(detail);
    short
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().nth(1)?;
            if name.is_empty() {
                return None;
            }
            let label = descriptions
                .get(name)
                .filter(|description| !description.is_empty())
                .cloned()
                .unwrap_or_else(|| name.to_string());
            Some(AudioSource {
                id: name.to_string(),
                name: label,
            })
        })
        .collect()
}

/// The `Description:` of every sink block, keyed by the block's `Name:`.
fn sink_descriptions(detail: &str) -> HashMap<String, String> {
    let mut descriptions = HashMap::new();
    let mut name = None;
    for line in detail.lines() {
        let line = line.trim();
        if let Some(value) = line.strip_prefix("Name:") {
            name = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Description:") {
            if let Some(name) = name.take() {
                descriptions.insert(name, value.trim().to_string());
            }
        }
    }
    descriptions
}

pub struct AudioIn {
    pub queue: Arc<Mutex<AudioQueue>>,
    source: Option<Source>,
    level: &'static Level,
}

enum Source {
    Microphone(cpal::Stream),
    Monitor(Monitor),
}

impl AudioIn {
    /// Stops capture after every sample the device already produced has been
    /// queued. The recording loop flushes the queue right after this call.
    pub fn finish(&mut self) {
        let Some(source) = self.source.take() else {
            return;
        };
        match source {
            Source::Microphone(stream) => {
                // The worker thread holds one period that has been captured but
                // not delivered yet; pausing immediately would drop it.
                std::thread::sleep(Duration::from_millis(40));
                let _ = stream.pause();
            }
            Source::Monitor(monitor) => monitor.stop(),
        }
        // Nothing is open any more, so the slider goes back to empty instead of
        // holding the last peak of the finished take.
        self.level.clear();
    }
}

impl Drop for AudioIn {
    fn drop(&mut self) {
        self.finish();
    }
}

/// A metering-only input: the same device a take would open, but only the peak
/// of each buffer is published; no samples are kept. The window opens one of
/// these behind each volume slider while it is idle.
pub struct AudioPreview {
    source: Option<PreviewSource>,
    level: Arc<Level>,
}

enum PreviewSource {
    Microphone(cpal::Stream),
    Monitor(Monitor),
}

impl AudioPreview {
    /// The peak the preview has published, in dBFS; `-inf` until the first
    /// buffer and after its 300 ms quiet window.
    pub fn level(&self) -> f32 {
        self.level.db()
    }

    pub fn stop(&mut self) {
        let Some(source) = self.source.take() else {
            return;
        };
        match source {
            PreviewSource::Microphone(stream) => {
                let _ = stream.pause();
            }
            PreviewSource::Monitor(monitor) => monitor.stop(),
        }
        self.level.clear();
    }
}

impl Drop for AudioPreview {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Opens the device only to meter it: nothing is queued, so an idle window
/// does not hold the recording buffers or the take's clock.
pub fn open_preview(system: bool, device_name: Option<&str>) -> Result<AudioPreview, String> {
    // A preview gets its own meter: several sections can watch different
    // devices at once, so they cannot share the take's two statics.
    let level = Arc::new(Level::new());
    if system {
        let (monitor, _) = Monitor::start(
            device_name,
            Instant::now(),
            Arc::new(AtomicU32::new(100)),
            Some(level.clone()),
        )?;
        return Ok(AudioPreview {
            source: Some(PreviewSource::Monitor(monitor)),
            level,
        });
    }
    let (device, supported) = microphone_device(device_name)?;
    let channels = supported.channels();
    let rate = supported.sample_rate();
    let config = supported.config();
    let stream = match supported.sample_format() {
        SampleFormat::F32 => build_preview::<f32>(&device, config, channels, rate, &level),
        SampleFormat::I16 => build_preview::<i16>(&device, config, channels, rate, &level),
        SampleFormat::I32 => build_preview::<i32>(&device, config, channels, rate, &level),
        SampleFormat::U16 => build_preview::<u16>(&device, config, channels, rate, &level),
        other => Err(trf(
            "Unsupported audio format {other}",
            &[("other", &other.to_string())],
        )),
    }?;
    stream
        .play()
        .map_err(|error| trf("Audio stream: {error}", &[("error", &error.to_string())]))?;
    Ok(AudioPreview {
        source: Some(PreviewSource::Microphone(stream)),
        level,
    })
}

pub fn open_stream(
    system: bool,
    device_name: Option<&str>,
    clock: Instant,
    gain: Arc<AtomicU32>,
) -> Result<AudioIn, String> {
    let level = if system { &SYSTEM } else { &MICROPHONE };
    if system {
        let (monitor, queue) = Monitor::start(device_name, clock, gain, None)?;
        return Ok(AudioIn {
            queue,
            source: Some(Source::Monitor(monitor)),
            level,
        });
    }
    open_microphone(device_name, clock, gain, level)
}

/// Resolves the microphone the profile names, or the current default one.
fn microphone_device(
    device_name: Option<&str>,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let host = cpal::default_host();
    if let Some(name) = device_name {
        let device = host
            .input_devices()
            .map_err(|error| error.to_string())?
            .find(|device| device.to_string() == name)
            .ok_or_else(|| trf("Microphone \"{name}\" was not found", &[("name", name)]))?;
        let supported = device
            .default_input_config()
            .map_err(|error| trf("Microphone: {error}", &[("error", &error.to_string())]))?;
        Ok((device, supported))
    } else {
        let device = host.default_input_device().ok_or(tr("No microphone"))?;
        let supported = device
            .default_input_config()
            .map_err(|error| trf("Microphone: {error}", &[("error", &error.to_string())]))?;
        Ok((device, supported))
    }
}

fn open_microphone(
    device_name: Option<&str>,
    clock: Instant,
    gain: Arc<AtomicU32>,
    level: &'static Level,
) -> Result<AudioIn, String> {
    let (device, supported) = microphone_device(device_name)?;
    let channels = supported.channels();
    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config = supported.config();
    let queue = Arc::new(Mutex::new(AudioQueue::default()));
    let target = queue.clone();
    let stream = match format {
        SampleFormat::F32 => {
            build::<f32>(&device, config, channels, rate, target, clock, gain, level)
        }
        SampleFormat::I16 => {
            build::<i16>(&device, config, channels, rate, target, clock, gain, level)
        }
        SampleFormat::I32 => {
            build::<i32>(&device, config, channels, rate, target, clock, gain, level)
        }
        SampleFormat::U16 => {
            build::<u16>(&device, config, channels, rate, target, clock, gain, level)
        }
        other => Err(trf(
            "Unsupported audio format {other}",
            &[("other", &other.to_string())],
        )),
    }?;
    stream
        .play()
        .map_err(|error| trf("Audio stream: {error}", &[("error", &error.to_string())]))?;
    Ok(AudioIn {
        queue,
        source: Some(Source::Microphone(stream)),
        level,
    })
}

fn apply_live_gain(samples: &mut [f32], gain: &AtomicU32) {
    // Read the shared value for every buffer, so moving the volume control
    // during a recording changes the next mixed block, not only a later take.
    geom::apply_gain(samples, gain.load(Ordering::Relaxed));
}

#[allow(clippy::too_many_arguments)]
fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: u16,
    rate: u32,
    target: Arc<Mutex<AudioQueue>>,
    clock: Instant,
    gain: Arc<AtomicU32>,
    level: &'static Level,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                let converted: Vec<f32> = data
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .collect();
                let mut ready = geom::to_stereo_48k(&converted, channels, rate);
                apply_live_gain(&mut ready, &gain);
                let frames = ready.len() / 2;
                if frames == 0 {
                    return;
                }
                level.publish(&ready);
                let end = geom::sample_index(clock.elapsed().as_nanos(), 48_000);
                let start = end.saturating_sub(frames as u64);
                let mut guard = target.lock().unwrap_or_else(|error| error.into_inner());
                guard.append(start, &ready);
                let end = guard.end();
                if end.saturating_sub(guard.origin) > 48_000 * 10 {
                    guard.drop_before(end - 48_000 * 10);
                }
            },
            |_| {},
            None,
        )
        .map_err(|error| format!("Audio stream: {error}"))
}

/// Builds a metering-only microphone stream: the device is read at its own
/// level (the profile's gain belongs to a take), and only the peak is kept.
fn build_preview<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: u16,
    rate: u32,
    level: &Arc<Level>,
) -> Result<cpal::Stream, String>
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let level = level.clone();
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data, _| {
                let converted: Vec<f32> = data
                    .iter()
                    .map(|sample| sample.to_sample::<f32>())
                    .collect();
                let ready = geom::to_stereo_48k(&converted, channels, rate);
                level.publish(&ready);
            },
            |_| {},
            None,
        )
        .map_err(|error| format!("Audio stream: {error}"))
}

/// `parec` arguments for one sink's monitor at the format the mixer wants.
/// `None` asks for whatever sink is currently the default. Kept in one place so
/// the live process and the tests describe the same command.
fn monitor_arguments(device: Option<&str>) -> Vec<String> {
    let device = match device {
        Some(name) if !name.is_empty() => format!("--device={name}.monitor"),
        _ => "--device=@DEFAULT_MONITOR@".to_string(),
    };
    [
        device,
        "--format=float32le".into(),
        "--rate=48000".into(),
        "--channels=2".into(),
        "--raw".into(),
        "--latency-msec=50".into(),
        "--process-time-msec=10".into(),
        "--stream-name=CaptureButton system audio".into(),
    ]
    .into()
}

struct Monitor {
    child: Child,
    reader: Option<JoinHandle<()>>,
}

impl Monitor {
    fn start(
        device: Option<&str>,
        clock: Instant,
        gain: Arc<AtomicU32>,
        preview_level: Option<Arc<Level>>,
    ) -> Result<(Self, Arc<Mutex<AudioQueue>>), String> {
        let mut child = Command::new("parec")
            .args(monitor_arguments(device))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| {
                trf(
                    "System audio: {error}",
                    &[("error", &format!("parec: {error}"))],
                )
            })?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| trf("System audio: {error}", &[("error", "no stdout")]))?;
        let queue = Arc::new(Mutex::new(AudioQueue::default()));
        let target = queue.clone();
        let reader = match std::thread::Builder::new()
            .name("CaptureButton-monitor".into())
            .spawn(move || read_monitor(&mut stdout, target, clock, gain, preview_level))
        {
            Ok(reader) => reader,
            Err(error) => {
                stop_child(&mut child);
                return Err(trf(
                    "System audio: {error}",
                    &[("error", &error.to_string())],
                ));
            }
        };
        Ok((
            Self {
                child,
                reader: Some(reader),
            },
            queue,
        ))
    }

    fn stop(mut self) {
        // Killing the writer closes stdout; the reader consumes what is still
        // in the pipe before it sees EOF, so no captured block is thrown away.
        stop_child(&mut self.child);
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn stop_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn read_monitor(
    stdout: &mut std::process::ChildStdout,
    queue: Arc<Mutex<AudioQueue>>,
    clock: Instant,
    gain: Arc<AtomicU32>,
    preview_level: Option<Arc<Level>>,
) {
    let record = preview_level.is_none();
    let mut buffer = vec![0u8; 16 * 1024];
    loop {
        match stdout.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => {
                let usable = n & !(MONITOR_FRAME_BYTES - 1);
                if usable == 0 {
                    continue;
                }
                let mut samples = decode_f32le(&buffer[..usable]);
                apply_live_gain(&mut samples, &gain);
                match &preview_level {
                    Some(level) => level.publish(&samples),
                    None => SYSTEM.publish(&samples),
                }
                if record {
                    let frames = usable / MONITOR_FRAME_BYTES;
                    let (start, _) = monitor_span(clock.elapsed().as_nanos(), frames);
                    let mut guard = queue.lock().unwrap_or_else(|error| error.into_inner());
                    guard.append(start, &samples);
                    let end = guard.end();
                    if end.saturating_sub(guard.origin) > MONITOR_RATE * 10 {
                        guard.drop_before(end - MONITOR_RATE * 10);
                    }
                }
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => break,
        }
    }
}

/// Little-endian `float32` PCM to mono samples (interleaved, unchanged).
fn decode_f32le(bytes: &[u8]) -> Vec<f32> {
    bytes
        .as_chunks::<4>()
        .0
        .iter()
        .map(|sample| f32::from_le_bytes(*sample))
        .collect()
}

/// Timeline span covered by `frames` monitor frames that were read at
/// `elapsed_ns`. The stream latency is removed so the block keeps the capture
/// time, not the time its bytes were read.
fn monitor_span(elapsed_ns: u128, frames: usize) -> (u64, u64) {
    let captured = elapsed_ns.saturating_sub(MONITOR_LATENCY.as_nanos());
    let end = geom::sample_index(captured, MONITOR_RATE);
    (end.saturating_sub(frames as u64), end)
}

/// Reads a whole number of monitor frames from `stdout`, for tests that drive
/// the reader without a child process.
#[cfg(test)]
fn take_whole_frames(data: &[u8]) -> (&[u8], usize) {
    let usable = data.len() & !(MONITOR_FRAME_BYTES - 1);
    (&data[..usable], usable / MONITOR_FRAME_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_audio_records_the_default_sink_monitor() {
        let arguments = monitor_arguments(None);
        assert!(arguments.contains(&"--device=@DEFAULT_MONITOR@".to_string()));
        assert!(arguments.contains(&"--format=float32le".to_string()));
        assert!(arguments.contains(&"--rate=48000".to_string()));
        assert!(arguments.contains(&"--channels=2".to_string()));
        assert_eq!(arguments.len(), 8, "every argument is passed to parec");
    }

    #[test]
    fn a_chosen_sink_is_recorded_through_its_monitor() {
        let arguments = monitor_arguments(Some("alsa_output.pci-0000_00_1f.3.analog-stereo"));
        assert_eq!(
            arguments.first().map(String::as_str),
            Some("--device=alsa_output.pci-0000_00_1f.3.analog-stereo.monitor")
        );
        assert!(!arguments.contains(&"--device=@DEFAULT_MONITOR@".to_string()));
    }

    #[test]
    fn pactl_sinks_use_the_description_as_the_label() {
        let short = "0\talsa_output.hdmi\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tSUSPENDED\n\
                     1\talsa_output.analog\tmodule-alsa-card.c\ts16le 2ch 48000Hz\tRUNNING\n";
        let detail = "\
Sink #0
\tState: SUSPENDED
\tName: alsa_output.hdmi
\tDescription: HDMI Output
\tDriver: PipeWire
Sink #1
\tState: RUNNING
\tName: alsa_output.analog
\tDescription: Built-in Audio Analog Stereo
";
        let sources = parse_sinks(short, detail);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].id, "alsa_output.hdmi");
        assert_eq!(sources[0].name, "HDMI Output");
        assert_eq!(sources[1].id, "alsa_output.analog");
        assert_eq!(sources[1].name, "Built-in Audio Analog Stereo");
    }

    #[test]
    fn a_sink_without_a_description_falls_back_to_its_name() {
        let sources = parse_sinks("0\talsa_output.analog\tmodule-alsa-card.c\n", "");
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, sources[0].id);
    }

    #[test]
    fn monitor_samples_are_little_endian_floats() {
        let mut bytes = Vec::new();
        for sample in [0.5f32, -0.25, 0.0, 1.0] {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        assert_eq!(decode_f32le(&bytes), [0.5, -0.25, 0.0, 1.0]);
    }

    #[test]
    fn a_short_read_is_rounded_down_to_whole_stereo_frames() {
        let bytes = [0u8; MONITOR_FRAME_BYTES * 3 + 4];
        let (whole, frames) = take_whole_frames(&bytes);
        assert_eq!(frames, 3);
        assert_eq!(whole.len(), MONITOR_FRAME_BYTES * 3);
    }

    #[test]
    fn a_monitor_block_keeps_its_capture_time() {
        let (start, end) = monitor_span(1_000_000_000, 4_800);
        assert_eq!(end, 45_600, "one second minus the stream latency");
        assert_eq!(start, 40_800, "a hundred milliseconds of frames");
    }

    #[test]
    fn a_short_monitor_block_starts_at_zero() {
        let (start, end) = monitor_span(0, 4_800);
        assert_eq!((start, end), (0, 0));
    }

    #[test]
    fn the_microphone_volume_is_read_again_for_each_audio_buffer() {
        let gain = AtomicU32::new(100);
        let mut at_unity = [0.25f32, -0.5];
        apply_live_gain(&mut at_unity, &gain);
        assert_eq!(at_unity, [0.25, -0.5]);
        let mut doubled = [0.25f32, -0.5];
        gain.store(200, Ordering::SeqCst);
        apply_live_gain(&mut doubled, &gain);
        assert_eq!(doubled, [0.5, -1.0]);
        let mut muted = [0.25f32, -0.5];
        gain.store(0, Ordering::SeqCst);
        apply_live_gain(&mut muted, &gain);
        assert_eq!(muted, [0.0, 0.0]);
    }

    #[test]
    fn the_system_volume_only_scales_the_monitor_samples() {
        let system = AtomicU32::new(100);
        let microphone = AtomicU32::new(100);
        let mut monitor = [0.2f32, -0.4];
        let mut microphone_samples = [0.2f32, -0.4];
        system.store(300, Ordering::SeqCst);
        apply_live_gain(&mut monitor, &system);
        apply_live_gain(&mut microphone_samples, &microphone);
        assert_eq!(monitor, [0.6, -1.2]);
        assert_eq!(
            microphone_samples,
            [0.2, -0.4],
            "the microphone keeps its own gain"
        );
        microphone.store(0, Ordering::SeqCst);
        let mut muted_microphone = microphone_samples;
        apply_live_gain(&mut muted_microphone, &microphone);
        assert_eq!(muted_microphone, [0.0, 0.0]);
        assert_eq!(monitor, [0.6, -1.2], "the system keeps its own gain");
    }
}
