//! What this device can actually do, and what we may therefore promise.
//!
//! The app has to run on a 2015 phone and on a Pixel 10, and behave honestly on
//! both. The tempting approach is to gate features by Android version and show a
//! UI with things greyed out; that reads as broken rather than as adapted, and
//! it never explains *why*.
//!
//! So capability is modelled as data, here, in the platform-free core. The
//! platform layer reports which sensors exist; this module decides what that
//! adds up to, what accuracy may be claimed, and what the user is told. An old
//! phone is then not a broken new phone — it is a different, fully working
//! [`Tier`] with its own honest description.
//!
//! The consequence that matters: **the floor is low**. Advertisement RSSI plus a
//! compass is enough for [`Tier::Guided`], which recovers both distance and
//! direction, and that combination has existed since Android 4.3. Ranging radios
//! improve the answer; they are not what makes it possible.

use crate::measurement::RangeSource;

/// What the platform layer found. Constructed by the platform, interpreted here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Capabilities {
    /// Best hard-ranging technology available, if any.
    pub ranging: Option<RangeSource>,
    /// True angle of arrival. In practice UWB only — a multi-antenna array is
    /// not something a phone has.
    pub angle_of_arrival: bool,
    /// RSSI from an established GATT connection. Quieter than advertisements,
    /// but it needs the target to accept a connection.
    pub connected_rssi: bool,
    /// RSSI from passively observed advertisements. The floor: without this
    /// there is nothing to work with at all.
    pub advert_rssi: bool,
    /// A usable heading. Without it the synthetic aperture has nothing to bin
    /// against and no bearing is recoverable.
    pub compass: bool,
    /// Step detection, from a hardware step detector or accelerometer peaks.
    /// Turns the user's walk into a measurement baseline.
    pub step_detection: bool,
}

/// How good a bearing we can offer. These are genuinely different things and the
/// UI must not draw them the same way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BearingQuality {
    /// Measured by the radio. Trustworthy immediately, no walking required.
    Measured,
    /// Inferred from signal swept across headings. Needs the user to turn or
    /// walk, and arrives with real error bars.
    Inferred,
    /// No heading sensor, so no direction of any kind. Warmer and colder only.
    None,
}

/// What the device as a whole can do. Ordered worst to best.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Not enough to locate anything.
    Unavailable,
    /// Signal strength only, with no way to sense the user's own movement.
    /// Distance is a guess and there is no direction. Still useful — this is
    /// the classic hot-and-cold hunt — but it must not pretend to be more.
    Proximity,
    /// Signal strength plus motion sensing, so the synthetic aperture works and
    /// direction is recoverable by moving. This is where most phones land, and
    /// it is a complete product rather than a degraded one.
    Guided,
    /// A hard ranging radio is present: metric distance, fast convergence, and
    /// with UWB a measured bearing that needs no walking at all.
    Precision,
}

impl Tier {
    pub fn label(self) -> &'static str {
        match self {
            Tier::Unavailable => "Unavailable",
            Tier::Proximity => "Proximity",
            Tier::Guided => "Guided",
            Tier::Precision => "Precision",
        }
    }
}

impl Capabilities {
    /// The floor: passive advertisement scanning and nothing else.
    pub fn advert_only() -> Self {
        Capabilities {
            advert_rssi: true,
            ..Default::default()
        }
    }

    /// A typical phone without a ranging radio. Note this reaches
    /// [`Tier::Guided`] — a complete directional experience — on hardware from
    /// a decade ago.
    pub fn typical_phone() -> Self {
        Capabilities {
            connected_rssi: true,
            advert_rssi: true,
            compass: true,
            step_detection: true,
            ..Default::default()
        }
    }

    pub fn tier(&self) -> Tier {
        if !self.advert_rssi && !self.connected_rssi && self.ranging.is_none() {
            return Tier::Unavailable;
        }
        if self.ranging.is_some() {
            return Tier::Precision;
        }
        // A compass alone is enough to sweep an aperture by turning on the
        // spot; step detection improves it but is not required.
        if self.compass {
            return Tier::Guided;
        }
        Tier::Proximity
    }

    pub fn bearing_quality(&self) -> BearingQuality {
        if self.angle_of_arrival {
            BearingQuality::Measured
        } else if self.compass {
            BearingQuality::Inferred
        } else {
            BearingQuality::None
        }
    }

    /// Best-case position accuracy in metres, once enough evidence is in.
    ///
    /// Deliberately the optimistic end for each technology, because this sets
    /// expectations *before* a hunt begins. It is not a live figure — that is
    /// [`crate::Fix::ellipse`], which is measured rather than assumed. `None`
    /// when nothing can be promised.
    pub fn expected_accuracy_m(&self) -> Option<f64> {
        match self.tier() {
            Tier::Unavailable => None,
            Tier::Precision => self.ranging.map(|r| match r {
                RangeSource::Uwb => 0.2,
                RangeSource::ChannelSounding => 0.5,
                RangeSource::WifiRtt => 2.0,
            }),
            // Walking a good path with signal strength alone lands here.
            Tier::Guided => Some(3.0),
            // No baseline to triangulate from: the annulus never collapses.
            Tier::Proximity => Some(8.0),
        }
    }

    /// One line for the UI. Says what the user gets, not what they lack.
    pub fn headline(&self) -> &'static str {
        match self.tier() {
            Tier::Unavailable => "Bluetooth unavailable",
            Tier::Precision if self.angle_of_arrival => "Precise distance and direction",
            Tier::Precision => "Precise distance, direction by walking",
            Tier::Guided => "Distance and direction by walking",
            Tier::Proximity => "Warmer and colder only",
        }
    }

    /// What the user should actually do, given what this device has. Usually the
    /// most useful sentence on the screen.
    pub fn instruction(&self) -> &'static str {
        match self.tier() {
            Tier::Unavailable => "Turn Bluetooth on to begin.",
            Tier::Precision if self.angle_of_arrival => "Point the phone around slowly.",
            Tier::Precision => "Walk a few steps; the estimate settles quickly.",
            Tier::Guided => "Turn slowly on the spot, then walk a dogleg — not a straight line.",
            Tier::Proximity => "Walk around and watch the number. Closer to zero is nearer.",
        }
    }

    /// Honest limitations, for a UI that would rather say so up front than
    /// disappoint later. Empty when there is nothing worth warning about.
    pub fn limitations(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.tier() == Tier::Unavailable {
            out.push("No Bluetooth scanning available on this device.");
            return out;
        }
        if !self.compass {
            out.push(
                "No compass, so this device cannot work out a direction — only whether \
                 you are getting closer.",
            );
        } else if !self.step_detection {
            out.push("No step detection, so direction relies on turning rather than walking.");
        }
        if self.ranging.is_none() {
            out.push(
                "No ranging radio, so distances come from signal strength and are \
                 affected by walls, metal and bodies.",
            );
        }
        if self.ranging == Some(RangeSource::WifiRtt) {
            out.push("Wi-Fi ranging is coarser than UWB — expect metres, not centimetres.");
        }
        if !self.connected_rssi && self.advert_rssi {
            out.push("Readings come from advertisements only, which are noisier than a connection.");
        }
        out
    }

    /// Whether a hard range will ever arrive. Drives whether the UI shows metric
    /// distance prominently or keeps signal strength as the headline.
    pub fn has_hard_ranging(&self) -> bool {
        self.ranging.is_some()
    }

    /// Whether the synthetic aperture is worth running at all.
    pub fn can_sweep(&self) -> bool {
        self.compass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_at_all_is_unavailable() {
        let c = Capabilities::default();
        assert_eq!(c.tier(), Tier::Unavailable);
        assert_eq!(c.expected_accuracy_m(), None);
        assert!(!c.limitations().is_empty());
    }

    #[test]
    fn advert_scanning_alone_still_works() {
        // The floor must be genuinely usable, not a stub.
        let c = Capabilities::advert_only();
        assert_eq!(c.tier(), Tier::Proximity);
        assert!(c.expected_accuracy_m().is_some());
        assert_eq!(c.bearing_quality(), BearingQuality::None);
        assert!(!c.instruction().is_empty());
    }

    #[test]
    fn a_decade_old_phone_reaches_the_guided_tier() {
        // The headline compatibility claim: no ranging radio, no UWB, nothing
        // modern — and still a full directional experience.
        let c = Capabilities::typical_phone();
        assert_eq!(c.tier(), Tier::Guided);
        assert_eq!(c.bearing_quality(), BearingQuality::Inferred);
        assert!(c.can_sweep());
        assert!(!c.has_hard_ranging());
    }

    #[test]
    fn a_compass_alone_is_enough_to_be_guided() {
        // Turning on the spot sweeps an aperture; walking is a bonus.
        let c = Capabilities {
            advert_rssi: true,
            compass: true,
            ..Default::default()
        };
        assert_eq!(c.tier(), Tier::Guided);
    }

    #[test]
    fn losing_the_compass_drops_to_proximity() {
        let c = Capabilities {
            step_detection: true,
            ..Capabilities::advert_only()
        };
        assert_eq!(c.tier(), Tier::Proximity);
        assert_eq!(c.bearing_quality(), BearingQuality::None);
        assert!(c.limitations().iter().any(|l| l.contains("direction")));
    }

    #[test]
    fn ranging_promotes_to_precision() {
        for source in [
            RangeSource::WifiRtt,
            RangeSource::ChannelSounding,
            RangeSource::Uwb,
        ] {
            let c = Capabilities {
                ranging: Some(source),
                ..Capabilities::typical_phone()
            };
            assert_eq!(c.tier(), Tier::Precision, "{source:?} should be precise");
            assert!(c.has_hard_ranging());
        }
    }

    #[test]
    fn only_uwb_offers_a_measured_bearing() {
        let uwb = Capabilities {
            ranging: Some(RangeSource::Uwb),
            angle_of_arrival: true,
            ..Capabilities::typical_phone()
        };
        assert_eq!(uwb.bearing_quality(), BearingQuality::Measured);

        // Channel Sounding gives range, not angle — the bearing is still
        // inferred, and the UI must not imply otherwise.
        let cs = Capabilities {
            ranging: Some(RangeSource::ChannelSounding),
            angle_of_arrival: false,
            ..Capabilities::typical_phone()
        };
        assert_eq!(cs.bearing_quality(), BearingQuality::Inferred);
    }

    #[test]
    fn accuracy_improves_monotonically_with_capability() {
        let bare = Capabilities::advert_only().expected_accuracy_m().unwrap();
        let guided = Capabilities::typical_phone().expected_accuracy_m().unwrap();
        let rtt = Capabilities {
            ranging: Some(RangeSource::WifiRtt),
            ..Capabilities::typical_phone()
        }
        .expected_accuracy_m()
        .unwrap();
        let uwb = Capabilities {
            ranging: Some(RangeSource::Uwb),
            ..Capabilities::typical_phone()
        }
        .expected_accuracy_m()
        .unwrap();

        assert!(bare > guided, "{bare} should be worse than {guided}");
        assert!(guided > rtt, "{guided} should be worse than {rtt}");
        assert!(rtt > uwb, "{rtt} should be worse than {uwb}");
    }

    #[test]
    fn tiers_order_worst_to_best() {
        assert!(Tier::Unavailable < Tier::Proximity);
        assert!(Tier::Proximity < Tier::Guided);
        assert!(Tier::Guided < Tier::Precision);
    }

    #[test]
    fn every_tier_has_something_to_say() {
        let cases = [
            Capabilities::default(),
            Capabilities::advert_only(),
            Capabilities::typical_phone(),
            Capabilities {
                ranging: Some(RangeSource::Uwb),
                angle_of_arrival: true,
                ..Capabilities::typical_phone()
            },
        ];
        for c in cases {
            assert!(!c.headline().is_empty(), "{:?} had no headline", c.tier());
            assert!(!c.instruction().is_empty(), "{:?} had no instruction", c.tier());
            assert!(!c.tier().label().is_empty());
        }
    }

    #[test]
    fn a_capable_device_is_not_nagged_with_warnings() {
        let best = Capabilities {
            ranging: Some(RangeSource::Uwb),
            angle_of_arrival: true,
            ..Capabilities::typical_phone()
        };
        assert!(
            best.limitations().is_empty(),
            "top-tier device should have no warnings, got {:?}",
            best.limitations()
        );
    }
}
