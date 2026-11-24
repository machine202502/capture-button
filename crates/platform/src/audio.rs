//! Audio capture: WASAPI loopback and microphones, delivered into a shared
//! [`AudioQueue`].
//!
//! Windows records the system audio by building an input stream on a render
//! endpoint, and enumerates those render endpoints for the Settings window.
//!
//! The system-audio device and volume are chosen per capture profile.
//!
//! The backend also publishes the peak of each buffer it queues, so the window
//! can draw the live level inside both volume sliders.

pub use platform_core::level::Meter;

/// One selectable system-audio source.
///
/// `id` is the WASAPI endpoint id the backend passes to its capture API;
/// `name` is the text the Settings window shows.
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

/// Enumerates the playback devices the system audio can be recorded from.
pub fn system_sources() -> Vec<AudioSource> {
    crate::backend::audio::render_endpoints()
        .into_iter()
        .map(|device| AudioSource {
            id: device.id,
            name: device.name,
        })
        .collect()
}

pub use crate::backend::audio::{open_preview, open_stream, AudioIn, AudioPreview};
