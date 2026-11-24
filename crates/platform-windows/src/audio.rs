//! Audio capture through cpal: the WASAPI loopback of a render endpoint and the
//! microphones, delivered into a shared [`AudioQueue`].
//!
//! Windows records system audio by building an input stream on a render device:
//! cpal flags that stream as loopback, so it delivers what the chosen output
//! plays. The device list comes from the WASAPI endpoint enumerator, and the
//! profile's own gain is applied to every buffer of both streams.
//!
//! The stream is returned as soon as it plays: nothing waits for the first
//! samples. A loopback is silent whenever the output is, and a microphone that
//! is still waking up simply has nothing to add yet; the recorder advances the
//! timeline by its own clock, so the take keeps the length of its picture.
//!
//! Each buffer also publishes its peak into the shared level meter, which the
//! window draws inside the volume sliders.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use platform_core::geom::{self, AudioQueue};
use platform_core::i18n::{tr, trf};
use platform_core::level::{Level, MICROPHONE, SYSTEM};
use platform_core::types::AudioDevice;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// One open audio input; the queue keeps filling until `finish` pauses it.
pub struct AudioIn {
    pub queue: Arc<Mutex<AudioQueue>>,
    stream: Option<cpal::Stream>,
    level: &'static Level,
}

impl AudioIn {
    /// Stops the stream after the worker has delivered the period it was
    /// already filling. The recorder flushes the queue right after this.
    pub fn finish(&mut self) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        std::thread::sleep(std::time::Duration::from_millis(40));
        let _ = stream.pause();
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
    stream: Option<cpal::Stream>,
    level: Arc<Level>,
}

impl AudioPreview {
    /// The peak the preview has published, in dBFS; `-inf` until the first
    /// buffer and after its 300 ms quiet window.
    pub fn level(&self) -> f32 {
        self.level.db()
    }

    pub fn stop(&mut self) {
        let Some(stream) = self.stream.take() else {
            return;
        };
        let _ = stream.pause();
        // Nothing is open any more, so the slider goes back to empty.
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
pub fn open_preview(system: bool, device: Option<&str>) -> Result<AudioPreview, String> {
    let (device, supported) = input_stream(system, device)?;
    // A preview gets its own meter: several sections can watch different
    // devices at once, so they cannot share the take's two statics.
    let level = Arc::new(Level::new());
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
        stream: Some(stream),
        level,
    })
}

/// The playback endpoints system audio can be recorded from.
///
/// The id is the device name open_stream matches (cpal::Device::to_string),
/// so a saved profile opens the same endpoint again; the name is the same text
/// the Settings window shows.
pub fn render_endpoints() -> Vec<AudioDevice> {
    crate::os::system_outputs()
        .into_iter()
        .map(|device| AudioDevice {
            id: device.name.clone(),
            name: device.name,
        })
        .collect()
}

/// Opens the system-audio or microphone stream.
///
/// `device` is a WASAPI endpoint id for system audio (or a friendly name, for
/// older saved profiles) and a friendly name for a microphone. `None` follows
/// the current default device of that direction. `gain` is the profile's own
/// volume for this stream, read again for every buffer.
///
/// The call does not wait for samples: a silent loopback is a valid take, and a
/// microphone that stays silent is not an error; the recorder writes silence
/// against the recording clock.
pub fn open_stream(
    system: bool,
    device: Option<&str>,
    clock: Instant,
    gain: Arc<AtomicU32>,
) -> Result<AudioIn, String> {
    let (device, supported) = input_stream(system, device)?;
    let level = if system { &SYSTEM } else { &MICROPHONE };
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
        stream: Some(stream),
        level,
    })
}

/// Resolves the device and the stream format both a take and a preview use.
fn input_stream(
    system: bool,
    device: Option<&str>,
) -> Result<(cpal::Device, cpal::SupportedStreamConfig), String> {
    let host = cpal::default_host();
    if system {
        let device = match device {
            Some(wanted) => output_device(&host, wanted)?,
            None => host
                .default_output_device()
                .ok_or(tr("No playback device for system audio"))?,
        };
        let supported = device
            .default_output_config()
            .map_err(|error| trf("System audio: {error}", &[("error", &error.to_string())]))?;
        Ok((device, supported))
    } else if let Some(name) = device {
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
            .map_err(|error| format!("Microphone: {error}"))?;
        Ok((device, supported))
    }
}

/// Finds the render endpoint the profile selected: cpal exposes the WASAPI
/// endpoint id, and its display name is the friendly name the Settings window
/// shows, so a profile saved before the id list also opens.
fn output_device(host: &cpal::Host, wanted: &str) -> Result<cpal::Device, String> {
    let devices = host.output_devices().map_err(|error| error.to_string())?;
    let mut by_name = None;
    for device in devices {
        if device
            .id()
            .map(|device_id| device_id.id() == wanted)
            .unwrap_or(false)
        {
            return Ok(device);
        }
        if by_name.is_none() && device.to_string() == wanted {
            by_name = Some(device);
        }
    }
    by_name.ok_or_else(|| {
        trf(
            "System audio device \"{name}\" was not found",
            &[("name", wanted)],
        )
    })
}

fn apply_live_gain(samples: &mut [f32], gain: &AtomicU32) {
    // Read the shared value for every buffer, so moving the volume control during a
    // recording changes the next mixed block, not only a later take.
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

/// Builds a metering-only stream: the device is read at its own level (the
/// profile's gain belongs to a take), and only the peak is kept.
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

#[cfg(test)]
mod tests {
    use super::apply_live_gain;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[test]
    fn the_stream_volume_is_read_again_for_each_audio_buffer() {
        let gain = AtomicU32::new(100);
        let mut at_unity = [0.25f32, -0.5];
        apply_live_gain(&mut at_unity, &gain);
        assert_eq!(at_unity, [0.25, -0.5]);
        let mut doubled = [0.25f32, -0.5];
        gain.store(200, Ordering::SeqCst);
        apply_live_gain(&mut doubled, &gain);
        assert_eq!(doubled, [0.5, -1.0]);
        let mut tripled = [0.25f32, -0.5];
        gain.store(300, Ordering::SeqCst);
        apply_live_gain(&mut tripled, &gain);
        assert_eq!(tripled, [0.75, -1.5]);
        let mut muted = [0.25f32, -0.5];
        gain.store(0, Ordering::SeqCst);
        apply_live_gain(&mut muted, &gain);
        assert_eq!(muted, [0.0, 0.0]);
    }
}
