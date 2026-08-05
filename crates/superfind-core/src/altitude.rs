//! Which floor it is on.
//!
//! Every locator on the market answers in two dimensions and then leaves you
//! walking the wrong storey of your own house. A barometer settles it: air
//! pressure falls by roughly 12 Pa per metre of ascent near sea level, and phone
//! barometers resolve well under that. The hardware has been in mid-range phones
//! for a decade and almost nothing uses it.
//!
//! ## Only differences are trustworthy
//!
//! Absolute altitude from pressure requires knowing the weather, and being wrong
//! about sea-level pressure by 1 hPa moves the answer by 8 metres — two storeys.
//! So nothing here reports an altitude. It reports the change since a reference
//! taken when the hunt began, which cancels the unknown almost entirely.
//!
//! Weather drifts during a hunt too, but slowly: a fast-moving front is about
//! 1 hPa per hour, and a hunt lasts minutes. A storey is 3 metres, or 36 Pa, so
//! the drift is comfortably below the resolution that matters.
//!
//! ## Deliberately coarse
//!
//! The output is a floor count, not a height, because "one floor up" is what a
//! person can act on and "3.4 m above you" is not. Small differences are
//! reported as [`FloorDelta::SameLevel`] rather than invented into fractions of
//! a storey — a desk and the floor beside it must not read as different levels.

use crate::time::Timestamp;

/// Pressure change per metre of ascent near sea level, in pascals.
///
/// From the barometric formula linearised about 15 °C at sea level. The
/// temperature dependence is real but small over the tens of metres a building
/// spans, and dwarfed by the sensor noise it is competing with.
const PASCALS_PER_METRE: f64 = 12.0;

/// Typical storey height, in metres. Domestic and office construction both sit
/// close to this; the exact figure matters less than being consistent.
const STOREY_M: f64 = 3.1;

/// Where the target is vertically, relative to where the hunt started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FloorDelta {
    /// Within about half a storey. Do not send anybody up the stairs for this.
    SameLevel,
    Above(u8),
    Below(u8),
}

impl FloorDelta {
    /// A phrase for the UI. Written to be read at a glance while walking.
    pub fn label(self) -> &'static str {
        match self {
            FloorDelta::SameLevel => "same level",
            FloorDelta::Above(1) => "one floor up",
            FloorDelta::Above(2) => "two floors up",
            FloorDelta::Above(_) => "several floors up",
            FloorDelta::Below(1) => "one floor down",
            FloorDelta::Below(2) => "two floors down",
            FloorDelta::Below(_) => "several floors down",
        }
    }

    pub fn is_same_level(self) -> bool {
        matches!(self, FloorDelta::SameLevel)
    }
}

/// Tracks height change from a reference pressure.
///
/// Fed the *user's* pressure readings. It cannot sense the target's altitude —
/// nothing can, remotely — so this answers "how far have I moved vertically
/// since I started", which combined with the signal getting stronger is what
/// tells somebody they are on the right floor.
#[derive(Debug, Clone)]
pub struct Altimeter {
    reference_pa: Option<f64>,
    smoothed_pa: Option<f64>,
    /// Exponential smoothing weight for each new sample.
    alpha: f64,
    samples: u32,
    last_at: Timestamp,
}

impl Default for Altimeter {
    fn default() -> Self {
        Altimeter {
            reference_pa: None,
            smoothed_pa: None,
            // Barometers are noisy at the fraction-of-a-pascal level and the
            // quantity moves slowly, so heavy smoothing costs nothing and buys
            // a reading that does not flicker between floors.
            alpha: 0.08,
            samples: 0,
            last_at: Timestamp::ZERO,
        }
    }
}

impl Altimeter {
    /// Fold in a pressure reading, in pascals.
    ///
    /// Returns false for a value outside anything Earth's surface produces,
    /// which is a broken sensor rather than a remarkable location.
    pub fn observe(&mut self, pascals: f64, at: Timestamp) -> bool {
        // 87–108 kPa spans the Dead Sea to the top of a tall building in a
        // storm. Anything outside is a fault.
        if !(87_000.0..=108_000.0).contains(&pascals) {
            return false;
        }
        self.smoothed_pa = Some(match self.smoothed_pa {
            None => pascals,
            Some(prev) => prev * (1.0 - self.alpha) + pascals * self.alpha,
        });
        self.reference_pa.get_or_insert(pascals);
        self.samples += 1;
        self.last_at = at;
        true
    }

    /// Re-anchor to the current pressure. Called when a hunt begins, so the
    /// answer is "since you started looking" rather than since the app launched.
    pub fn anchor(&mut self) {
        self.reference_pa = self.smoothed_pa;
    }

    /// Metres risen since the reference. Positive is up.
    pub fn climb_m(&self) -> Option<f64> {
        // A couple of samples are all noise; the smoother has not settled.
        if self.samples < 4 {
            return None;
        }
        let (reference, current) = (self.reference_pa?, self.smoothed_pa?);
        Some((reference - current) / PASCALS_PER_METRE)
    }

    /// How many storeys the user has climbed since the reference.
    pub fn floors(&self) -> Option<FloorDelta> {
        let climb = self.climb_m()?;
        let storeys = climb / STOREY_M;
        // Half a storey of deadband. Below it, a desk and the floor beside it
        // would read as different levels, which is worse than saying nothing.
        if storeys.abs() < 0.5 {
            return Some(FloorDelta::SameLevel);
        }
        let count = storeys.abs().round().clamp(1.0, 255.0) as u8;
        Some(if storeys > 0.0 {
            FloorDelta::Above(count)
        } else {
            FloorDelta::Below(count)
        })
    }

    pub fn has_reading(&self) -> bool {
        self.samples >= 4
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed enough samples for the smoother to converge on `pascals`.
    fn settle(alt: &mut Altimeter, pascals: f64) {
        for i in 0..300 {
            alt.observe(pascals, Timestamp(i as f64 * 0.5));
        }
    }

    #[test]
    fn nothing_is_claimed_before_the_smoother_settles() {
        let mut alt = Altimeter::default();
        assert_eq!(alt.climb_m(), None);
        alt.observe(101_325.0, Timestamp(0.0));
        assert_eq!(alt.climb_m(), None, "one sample is noise, not a reading");
        assert!(!alt.has_reading());
    }

    #[test]
    fn standing_still_reads_as_the_same_level() {
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        assert_eq!(alt.floors(), Some(FloorDelta::SameLevel));
    }

    #[test]
    fn climbing_one_storey_is_reported_as_one_floor_up() {
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        alt.anchor();
        // 3.1 m up is 37 Pa less.
        settle(&mut alt, 101_325.0 - STOREY_M * PASCALS_PER_METRE);
        assert_eq!(alt.floors(), Some(FloorDelta::Above(1)));
        assert_eq!(alt.floors().unwrap().label(), "one floor up");
    }

    #[test]
    fn descending_two_storeys_is_reported_below() {
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        alt.anchor();
        settle(&mut alt, 101_325.0 + 2.0 * STOREY_M * PASCALS_PER_METRE);
        assert_eq!(alt.floors(), Some(FloorDelta::Below(2)));
    }

    #[test]
    fn desk_height_is_not_a_floor() {
        // Lifting the phone from the floor to a table must not change the
        // answer, or the feature is unusable indoors.
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        alt.anchor();
        settle(&mut alt, 101_325.0 - 1.0 * PASCALS_PER_METRE);
        assert_eq!(alt.floors(), Some(FloorDelta::SameLevel));
    }

    #[test]
    fn implausible_pressure_is_rejected_as_a_fault() {
        let mut alt = Altimeter::default();
        assert!(!alt.observe(0.0, Timestamp(0.0)));
        assert!(!alt.observe(500_000.0, Timestamp(0.0)));
        assert!(!alt.observe(f64::NAN, Timestamp(0.0)));
        assert!(alt.observe(101_325.0, Timestamp(0.0)));
    }

    #[test]
    fn weather_drift_over_a_hunt_stays_below_one_storey() {
        // A fast-moving front is about 1 hPa per hour; a hunt is minutes. Ten
        // minutes of that is ~17 Pa, well under the 48 Pa a storey costs.
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        alt.anchor();
        settle(&mut alt, 101_325.0 - 17.0);
        assert_eq!(
            alt.floors(),
            Some(FloorDelta::SameLevel),
            "weather must not be mistaken for stairs"
        );
    }

    #[test]
    fn anchoring_resets_the_reference_to_here() {
        let mut alt = Altimeter::default();
        settle(&mut alt, 101_325.0);
        settle(&mut alt, 101_325.0 - 5.0 * STOREY_M * PASCALS_PER_METRE);
        assert!(matches!(alt.floors(), Some(FloorDelta::Above(_))));
        alt.anchor();
        assert_eq!(alt.floors(), Some(FloorDelta::SameLevel));
    }

    #[test]
    fn every_variant_has_something_to_say() {
        for delta in [
            FloorDelta::SameLevel,
            FloorDelta::Above(1),
            FloorDelta::Above(2),
            FloorDelta::Above(9),
            FloorDelta::Below(1),
            FloorDelta::Below(2),
            FloorDelta::Below(9),
        ] {
            assert!(!delta.label().is_empty());
        }
        assert!(FloorDelta::SameLevel.is_same_level());
        assert!(!FloorDelta::Above(1).is_same_level());
    }
}
