//! Local wall-clock time, supplied by the platform backend.
//!
//! File names, the history list and the log stamp need the user's local time.
//! The neutral layer cannot read it on its own, so the backend installs a clock
//! once at startup; before that [`now`] returns the zero time.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millis: u16,
}

impl LocalTime {
    /// The six components in the order the interface uses them:
    /// day, month, year, hour, minute, second.
    pub fn parts(self) -> (u16, u16, u16, u16, u16, u16) {
        (
            self.day as u16,
            self.month as u16,
            self.year,
            self.hour as u16,
            self.minute as u16,
            self.second as u16,
        )
    }
}

fn clock() -> &'static OnceLock<fn() -> LocalTime> {
    static CLOCK: OnceLock<fn() -> LocalTime> = OnceLock::new();
    &CLOCK
}

/// Installs the backend's clock. A second call is ignored.
pub fn install(clock: fn() -> LocalTime) {
    let _ = self::clock().set(clock);
}

pub fn now() -> LocalTime {
    clock().get().copied().map(|read| read()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::LocalTime;

    #[test]
    fn parts_keep_the_interface_order() {
        let stamp = LocalTime {
            year: 2026,
            month: 11,
            day: 23,
            hour: 13,
            minute: 5,
            second: 9,
            millis: 250,
        };
        assert_eq!(stamp.parts(), (23, 11, 2026, 13, 5, 9));
    }
}
