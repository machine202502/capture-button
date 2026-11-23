//! Audio capture through cpal (WASAPI on Windows): system loopback and
//! microphones, delivered into a shared [`AudioQueue`].

use crate::geom::{self, AudioQueue};
use crate::i18n::{tr, trf};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SampleFormat, SizedSample};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub struct AudioIn {
    pub queue: Arc<Mutex<AudioQueue>>,
    _stream: cpal::Stream,
}

pub fn open_stream(
    system: bool,
    device_name: Option<&str>,
    clock: Instant,
    gain: Arc<AtomicU32>,
) -> Result<AudioIn, String> {
    let host = cpal::default_host();
    let (device, supported) = if system {
        let device = host
            .default_output_device()
            .ok_or(tr("No playback device for system audio"))?;
        let supported = device
            .default_output_config()
            .map_err(|error| trf("System audio: {error}", &[("error", &error.to_string())]))?;
        (device, supported)
    } else if let Some(name) = device_name {
        let device = host
            .input_devices()
            .map_err(|error| error.to_string())?
            .find(|device| device.to_string() == name)
            .ok_or_else(|| trf("Microphone \"{name}\" was not found", &[("name", name)]))?;
        let supported = device
            .default_input_config()
            .map_err(|error| trf("Microphone: {error}", &[("error", &error.to_string())]))?;
        (device, supported)
    } else {
        let device = host.default_input_device().ok_or(tr("No microphone"))?;
        let supported = device
            .default_input_config()
            .map_err(|error| format!("Microphone: {error}"))?;
        (device, supported)
    };
    let channels = supported.channels();
    let rate = supported.sample_rate();
    let format = supported.sample_format();
    let config = supported.config();
    let queue = Arc::new(Mutex::new(AudioQueue::default()));
    let target = queue.clone();
    // Only the microphone channel has a user-facing gain. System audio stays at unity.
    let gain = if system {
        Arc::new(AtomicU32::new(100))
    } else {
        gain
    };
    let stream = match format {
        SampleFormat::F32 => build::<f32>(&device, config, channels, rate, target, clock, gain),
        SampleFormat::I16 => build::<i16>(&device, config, channels, rate, target, clock, gain),
        SampleFormat::I32 => build::<i32>(&device, config, channels, rate, target, clock, gain),
        SampleFormat::U16 => build::<u16>(&device, config, channels, rate, target, clock, gain),
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
        _stream: stream,
    })
}

fn apply_live_gain(samples: &mut [f32], gain: &AtomicU32) {
    // Read the shared value for every buffer, so moving the volume control during a
    // recording changes the next mixed block, not only a later take.
    geom::apply_gain(samples, gain.load(Ordering::Relaxed));
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    channels: u16,
    rate: u32,
    target: Arc<Mutex<AudioQueue>>,
    clock: Instant,
    gain: Arc<AtomicU32>,
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

#[cfg(test)]
mod tests {
    use super::apply_live_gain;
    use std::sync::atomic::{AtomicU32, Ordering};

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
