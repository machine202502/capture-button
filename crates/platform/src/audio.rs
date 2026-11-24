//! Audio capture: WASAPI loopback and microphones on Windows, the default
//! sink monitor and microphones on Linux, delivered into a shared
//! [`AudioQueue`].
//!
//! Windows keeps cpal's loopback. Linux cannot: ALSA has no playback loopback,
//! so the Linux backend reads the PulseAudio/PipeWire monitor with `parec`.
//!
//! The system-audio device and volume are chosen per capture profile. The Linux
//! backend records a named sink's monitor; the Windows backend enumerates
//! render endpoints (see `system_sources`).
//!
//! Every backend also publishes the peak of each buffer it queues, so the
//! window can draw the live level inside both volume sliders.

pub use platform_core::level::Meter;

/// One selectable system-audio source.
///
/// `id` is what the backend passes to its capture API (a PulseAudio/PipeWire
/// sink name on Linux, a WASAPI endpoint id on Windows); `name` is the text the
/// Settings window shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioSource {
    pub id: String,
    pub name: String,
}

/// Peak level in dBFS of the microphone and of the system audio, in that order.
///
/// A source that is silent, or that no open stream is feeding, reports
/// `f32::NEG_INFINITY`; the window maps either to an empty bar.
pub fn levels() -> (f32, f32) {
    platform_core::level::levels()
}

#[cfg(target_os = "linux")]
pub use platform_linux::audio::{open_preview, open_stream, AudioIn, AudioPreview};

#[cfg(target_os = "linux")]
pub fn system_sources() -> Vec<AudioSource> {
    platform_linux::audio::system_sources()
        .into_iter()
        .map(|source| AudioSource {
            id: source.id,
            name: source.name,
        })
        .collect()
}

/// Enumerates the playback devices the system audio can be recorded from.
///
/// Linux returns the PulseAudio/PipeWire sinks and their descriptions; the
/// Windows backend returns the WASAPI render endpoints.
#[cfg(not(target_os = "linux"))]
pub fn system_sources() -> Vec<AudioSource> {
    crate::backend::audio::render_endpoints()
        .into_iter()
        .map(|device| AudioSource {
            id: device.id,
            name: device.name,
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
pub use crate::backend::audio::{open_preview, open_stream, AudioIn, AudioPreview};
