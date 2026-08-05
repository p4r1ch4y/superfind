//! What the radios hand us, and how much each one deserves to be believed.
//!
//! The central design claim of this crate: every observation carries its own
//! noise term, so the filter never has to know which platform or radio produced
//! it. Adding Bluetooth Channel Sounding later is a new `RangeSource` variant
//! and a sigma, not a change to the filter.
//!
//! This is also where findphone's hard-won lesson lives. It fed connected-link
//! RSSI and passively-observed advertisements into one median, and the noisier
//! source outvoted the good one. Here that cannot happen silently: an advert is
//! declared to be roughly twice as noisy as a link read, and the filter weighs
//! it accordingly.

use crate::time::Timestamp;

/// Where an RSSI sample came from. The ordering is deliberate — variants are
/// listed best-first, and `Ord` follows, so `max()` picks the best source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RssiSource {
    /// `readRSSI` on an established GATT connection. Fresh, frequent, and
    /// measured on the connection's own channel with known TX power.
    ConnectedLink,
    /// A passively observed BLE advertisement. Different channel and TX power
    /// to the connected link, and with duplicate reporting enabled these arrive
    /// far faster than link reads — which is exactly why they need a wider
    /// sigma rather than an equal vote.
    Advertisement,
    /// Classic Bluetooth RSSI scraped from an OS tool. Coarse, quantised, and
    /// often served from a cache several seconds stale.
    ClassicPoll,
}

impl RssiSource {
    /// Standard deviation of the log-normal shadowing term, in dB.
    ///
    /// These are starting priors from published indoor path-loss studies, not
    /// measurements from our own hardware. Recalibrate against a real trace
    /// before trusting the absolute numbers; the *ratios* are the load-bearing
    /// part and they are conservative.
    pub fn sigma_db(self) -> f64 {
        match self {
            RssiSource::ConnectedLink => 4.0,
            RssiSource::Advertisement => 7.0,
            RssiSource::ClassicPoll => 9.0,
        }
    }

    /// How long a sample from this source stays worth steering by, in seconds.
    ///
    /// `ClassicPoll` gets a long window because macOS only refreshes the
    /// underlying value every 3–12 seconds; polling faster returns a cached
    /// number, so treating it as stale after 2s would discard the only reading
    /// there is.
    pub fn staleness_horizon(self) -> f64 {
        match self {
            RssiSource::ConnectedLink => 5.0,
            RssiSource::Advertisement => 12.0,
            RssiSource::ClassicPoll => 20.0,
        }
    }
}

/// A true time-of-flight or phase-based range. Unlike RSSI these are metric
/// measurements with tight, roughly Gaussian error, and they collapse the
/// filter's posterior far faster than any amount of signal strength.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RangeSource {
    /// Ultra-wideband two-way ranging. Android UWB API, Apple Nearby
    /// Interaction. The best of the three when both ends have the radio.
    Uwb,
    /// Bluetooth 6.0 Channel Sounding — phase-based ranging plus round-trip
    /// time. Exposed through the Android 16 Ranging API.
    ChannelSounding,
    /// 802.11mc Fine Timing Measurement, via Android's `WifiRttManager`.
    /// Longer range than the other two and works against infrastructure APs,
    /// at coarser precision.
    WifiRtt,
}

impl RangeSource {
    /// One-sigma ranging error in metres, before any distance-dependent term.
    pub fn sigma_m(self) -> f64 {
        match self {
            RangeSource::Uwb => 0.10,
            RangeSource::ChannelSounding => 0.30,
            RangeSource::WifiRtt => 1.50,
        }
    }
}

/// A single observation, timestamped in session time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Measurement {
    /// Received signal strength, in dBm. Always negative in practice.
    Rssi {
        dbm: f64,
        source: RssiSource,
        at: Timestamp,
    },
    /// A metric range to the target, in metres.
    Range {
        metres: f64,
        source: RangeSource,
        at: Timestamp,
    },
    /// A measured angle of arrival: compass bearing from the user to the
    /// target, radians clockwise from north, with its own uncertainty.
    ///
    /// Only UWB AoA and multi-antenna BLE arrays produce this. A bearing
    /// derived by sweeping RSSI is *not* this — that is
    /// [`crate::bearing`] output, which is an inference, and conflating the two
    /// is how an app ends up drawing a confident arrow it has not earned.
    Angle {
        bearing_rad: f64,
        sigma_rad: f64,
        at: Timestamp,
    },
}

impl Measurement {
    pub fn at(&self) -> Timestamp {
        match *self {
            Measurement::Rssi { at, .. } => at,
            Measurement::Range { at, .. } => at,
            Measurement::Angle { at, .. } => at,
        }
    }

    /// Whether this observation is physically plausible. Bad values reach us
    /// routinely: a disconnected radio reports RSSI 127, a failed ranging
    /// attempt reports a negative distance.
    pub fn is_plausible(&self) -> bool {
        match *self {
            // -127 is the "no measurement" sentinel in the BLE HCI spec, and a
            // positive dBm from a handheld radio is never real.
            Measurement::Rssi { dbm, .. } => dbm > -127.0 && dbm < 0.0,
            Measurement::Range { metres, .. } => metres.is_finite() && metres >= 0.0,
            Measurement::Angle {
                bearing_rad,
                sigma_rad,
                ..
            } => bearing_rad.is_finite() && sigma_rad.is_finite() && sigma_rad > 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn link_is_trusted_more_than_advert_more_than_classic() {
        assert!(RssiSource::ConnectedLink.sigma_db() < RssiSource::Advertisement.sigma_db());
        assert!(RssiSource::Advertisement.sigma_db() < RssiSource::ClassicPoll.sigma_db());
    }

    #[test]
    fn source_ordering_is_best_first() {
        let mut sources = [
            RssiSource::ClassicPoll,
            RssiSource::ConnectedLink,
            RssiSource::Advertisement,
        ];
        sources.sort();
        assert_eq!(sources[0], RssiSource::ConnectedLink);
        assert_eq!(sources[2], RssiSource::ClassicPoll);
    }

    #[test]
    fn ranging_sources_are_ordered_by_precision() {
        assert!(RangeSource::Uwb.sigma_m() < RangeSource::ChannelSounding.sigma_m());
        assert!(RangeSource::ChannelSounding.sigma_m() < RangeSource::WifiRtt.sigma_m());
    }

    #[test]
    fn rejects_the_hci_no_measurement_sentinel() {
        let bad = Measurement::Rssi {
            dbm: -127.0,
            source: RssiSource::ConnectedLink,
            at: Timestamp::ZERO,
        };
        assert!(!bad.is_plausible());
    }

    #[test]
    fn rejects_positive_rssi() {
        let bad = Measurement::Rssi {
            dbm: 127.0,
            source: RssiSource::ConnectedLink,
            at: Timestamp::ZERO,
        };
        assert!(!bad.is_plausible());
    }

    #[test]
    fn accepts_an_ordinary_reading() {
        let good = Measurement::Rssi {
            dbm: -62.0,
            source: RssiSource::Advertisement,
            at: Timestamp(3.0),
        };
        assert!(good.is_plausible());
        assert_eq!(good.at(), Timestamp(3.0));
    }

    #[test]
    fn rejects_negative_range_and_zero_sigma_angle() {
        assert!(!Measurement::Range {
            metres: -1.0,
            source: RangeSource::Uwb,
            at: Timestamp::ZERO
        }
        .is_plausible());
        assert!(!Measurement::Angle {
            bearing_rad: 0.0,
            sigma_rad: 0.0,
            at: Timestamp::ZERO
        }
        .is_plausible());
    }
}
