//! Sound levels of the two audio inputs, shared by the backends and the window.
//!
//! An audio callback publishes the peak of every buffer it queues; the window
//! reads both values on its own timer and draws them inside the volume sliders.
//! The state is a pair of atomics in dBFS, so neither side waits for the other
//! and a take costs no extra channel.
//!
//! The bar position is OS-neutral: [`FLOOR_DB`] is the empty end and 0 dBFS the
//! full one. [`Meter`] holds a peak for a moment and then lets it fall so a
//! short sound stays readable instead of flickering for one callback.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// The empty end of the scale, in dBFS.
pub const FLOOR_DB: f32 = -60.0;

/// `f32::NEG_INFINITY.to_bits()`: what a source that has never played reports.
const SILENT: u32 = 0xff80_0000;

/// A source that has not published for this long is treated as silent.
///
/// A WASAPI loopback stops calling back while nothing is playing, so the last
/// peak would otherwise stay on the bar forever. Audio callbacks arrive every
/// few milliseconds, therefore no playing source ever goes quiet that long.
const STALE: Duration = Duration::from_millis(300);

/// How long a peak stays on the bar before it starts to fall.
const HOLD: Duration = Duration::from_millis(400);

/// How fast the bar falls once the hold has passed.
const FALL_DB_PER_SECOND: f32 = 30.0;

/// Milliseconds since this process started, the time base of [`Level`].
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// One input's peak level in dBFS, written by its audio callback.
///
/// The stored value is a `f32` of decibels (`20 * log10(peak)`); silence is
/// `-inf`. A callback overwrites it, so the reader always sees a recent peak.
/// A source that stops publishing is silently treated as silent after
/// [`STALE`], which is what a loopback does while the output is idle.
#[derive(Debug)]
pub struct Level {
    db: AtomicU32,
    last_ms: AtomicU64,
}

impl Level {
    pub const fn new() -> Self {
        Self {
            db: AtomicU32::new(SILENT),
            last_ms: AtomicU64::new(0),
        }
    }

    /// Publishes the peak of one interleaved stereo buffer.
    pub fn publish(&self, samples: &[f32]) {
        self.publish_at(now_ms(), samples);
    }

    fn publish_at(&self, now: u64, samples: &[f32]) {
        let mut peak = 0.0f32;
        for sample in samples {
            peak = peak.max(sample.abs());
        }
        let db = if peak > 0.0 {
            20.0 * peak.log10()
        } else {
            f32::NEG_INFINITY
        };
        self.db.store(db.to_bits(), Ordering::Relaxed);
        self.last_ms.store(now, Ordering::Relaxed);
    }

    /// The last published peak in dBFS; `-inf` before the first buffer and once
    /// the source has been quiet for [`STALE`].
    pub fn db(&self) -> f32 {
        self.db_at(now_ms())
    }

    fn db_at(&self, now: u64) -> f32 {
        let fresh =
            now.saturating_sub(self.last_ms.load(Ordering::Relaxed)) <= STALE.as_millis() as u64;
        if fresh {
            f32::from_bits(self.db.load(Ordering::Relaxed))
        } else {
            f32::NEG_INFINITY
        }
    }

    /// Forgets the level, for a source that is closed again.
    pub fn clear(&self) {
        self.db.store(SILENT, Ordering::Relaxed);
        self.last_ms.store(now_ms(), Ordering::Relaxed);
    }
}

impl Default for Level {
    fn default() -> Self {
        Self::new()
    }
}

/// The microphone, as published by the cpal or WASAPI input.
pub static MICROPHONE: Level = Level::new();
/// The system audio, as published by the loopback or the sink monitor.
pub static SYSTEM: Level = Level::new();

/// Peak level in dBFS of the microphone and of the system audio.
pub fn levels() -> (f32, f32) {
    (MICROPHONE.db(), SYSTEM.db())
}

/// Where a level sits on a `0..=1` bar between [`FLOOR_DB`] and 0 dBFS.
pub fn bar(db: f32) -> f32 {
    let db = db.clamp(FLOOR_DB, 0.0);
    (db - FLOOR_DB) / -FLOOR_DB
}

/// Peak-hold and fall in front of the bar.
///
/// [`Self::update`] takes the measured peak and the time since the previous
/// call and returns the bar position to draw. A new peak is held for [`HOLD`];
/// afterwards the bar falls at [`FALL_DB_PER_SECOND`] until it meets the current
/// level. Running it slower than the audio callbacks is intended: the peak is
/// what the reader is after.
#[derive(Debug)]
pub struct Meter {
    shown_db: f32,
    hold_ms: u32,
}

impl Meter {
    pub const fn new() -> Self {
        Self {
            shown_db: f32::NEG_INFINITY,
            hold_ms: 0,
        }
    }

    /// Clears the bar for a take that has not started or has just ended.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn update(&mut self, measured_db: f32, elapsed: Duration) -> f32 {
        let measured = if measured_db.is_finite() {
            measured_db
        } else if measured_db > 0.0 {
            0.0
        } else {
            FLOOR_DB - 60.0
        };
        let dt = elapsed.as_secs_f32().clamp(0.0, 0.5);
        let ms = (dt * 1000.0) as u32;
        if measured >= self.shown_db {
            self.shown_db = measured;
            self.hold_ms = HOLD.as_millis() as u32;
        } else if self.hold_ms > 0 {
            self.hold_ms = self.hold_ms.saturating_sub(ms);
        } else {
            self.shown_db = (self.shown_db - FALL_DB_PER_SECOND * dt).max(measured);
        }
        bar(self.shown_db)
    }
}

impl Default for Meter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_of(amplitude: f32) -> f32 {
        20.0 * amplitude.log10()
    }

    #[test]
    fn a_buffer_reports_its_largest_sample_in_decibels() {
        let level = Level::new();
        level.publish(&[0.0, 0.25, -0.5, 0.1]);
        assert!((level.db() - db_of(0.5)).abs() < 0.001);
        level.publish(&[0.0, 0.0]);
        assert_eq!(level.db(), f32::NEG_INFINITY, "silence is the empty level");
    }

    #[test]
    fn clearing_returns_the_empty_level() {
        let level = Level::new();
        level.publish(&[1.0]);
        level.clear();
        assert_eq!(level.db(), f32::NEG_INFINITY);
        assert_eq!(bar(level.db()), 0.0);
    }

    #[test]
    fn a_source_that_stops_publishing_falls_silent() {
        let level = Level::new();
        level.publish_at(1_000, &[0.5]);
        assert!((level.db_at(1_100) - db_of(0.5)).abs() < 0.001);
        assert_eq!(
            level.db_at(1_300),
            db_of(0.5),
            "the last peak is still inside the stale window"
        );
        assert_eq!(
            level.db_at(1_301),
            f32::NEG_INFINITY,
            "a loopback that stopped calling back is silence"
        );
    }

    #[test]
    fn the_bar_runs_from_minus_sixty_to_full_scale() {
        assert_eq!(bar(FLOOR_DB), 0.0);
        assert!((bar(-30.0) - 0.5).abs() < 0.001);
        assert!((bar(0.0) - 1.0).abs() < 0.001);
        assert_eq!(bar(-120.0), 0.0, "below the floor is still empty");
        assert_eq!(bar(6.0), 1.0, "louder than full scale is still full");
        assert_eq!(bar(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn a_peak_is_held_before_the_bar_falls() {
        let mut meter = Meter::new();
        let tick = Duration::from_millis(50);
        let held = meter.update(-20.0, tick);
        assert!((held - bar(-20.0)).abs() < 0.001);
        // Eight 50 ms ticks cover the 400 ms hold; the bar does not move yet.
        for _ in 0..8 {
            let value = meter.update(-60.0, tick);
            assert!((value - bar(-20.0)).abs() < 0.001, "the peak is still held");
        }
        // Then it falls at 30 dB per second, which is 1.5 dB per tick.
        let fallen = meter.update(-60.0, tick);
        assert!(
            (fallen - bar(-21.5)).abs() < 0.001,
            "30 dB/s after the hold"
        );
        let later = meter.update(-60.0, tick);
        assert!((later - bar(-23.0)).abs() < 0.001);
    }

    #[test]
    fn a_new_peak_restarts_the_hold() {
        let mut meter = Meter::new();
        let tick = Duration::from_millis(50);
        meter.update(-40.0, tick);
        for _ in 0..10 {
            meter.update(-60.0, tick);
        }
        let loud = meter.update(-6.0, tick);
        assert!((loud - bar(-6.0)).abs() < 0.001);
        let held = meter.update(-60.0, tick);
        assert!((held - bar(-6.0)).abs() < 0.001);
    }

    #[test]
    fn a_meter_that_has_not_been_fed_reads_empty() {
        let mut meter = Meter::new();
        assert_eq!(
            meter.update(f32::NEG_INFINITY, Duration::from_millis(50)),
            0.0
        );
        meter.update(-10.0, Duration::from_millis(50));
        meter.reset();
        assert_eq!(
            meter.update(f32::NEG_INFINITY, Duration::from_millis(50)),
            0.0
        );
    }
}
