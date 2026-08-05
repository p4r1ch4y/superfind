//! Session-relative time.
//!
//! Deliberately a plain `f64` of seconds rather than `std::time::Instant`. Three
//! reasons: recorded traces replay bit-identically, tests need no clock, and the
//! type crosses an FFI boundary to Dart and Kotlin without ceremony.

use core::ops::{Add, Sub};

/// Seconds since the tracking session began.
#[derive(Debug, Clone, Copy, PartialEq, PartialOrd, Default)]
pub struct Timestamp(pub f64);

impl Timestamp {
    pub const ZERO: Timestamp = Timestamp(0.0);

    #[inline]
    pub fn seconds(self) -> f64 {
        self.0
    }

    /// Age of this timestamp as of `now`, floored at zero so a clock that
    /// stutters backwards cannot produce a negative age.
    #[inline]
    pub fn age_at(self, now: Timestamp) -> f64 {
        (now.0 - self.0).max(0.0)
    }
}

impl Add<f64> for Timestamp {
    type Output = Timestamp;
    #[inline]
    fn add(self, secs: f64) -> Timestamp {
        Timestamp(self.0 + secs)
    }
}

impl Sub for Timestamp {
    type Output = f64;
    #[inline]
    fn sub(self, other: Timestamp) -> f64 {
        self.0 - other.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_never_goes_negative() {
        let later = Timestamp(10.0);
        let earlier = Timestamp(4.0);
        assert_eq!(later.age_at(Timestamp(12.0)), 2.0);
        // A timestamp from the future is treated as fresh, not as negative age.
        assert_eq!(later.age_at(earlier), 0.0);
    }

    #[test]
    fn arithmetic() {
        assert_eq!((Timestamp(3.0) + 1.5).seconds(), 4.5);
        assert_eq!(Timestamp(5.0) - Timestamp(2.0), 3.0);
    }
}
