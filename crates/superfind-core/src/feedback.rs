//! Hearing the signal instead of watching it.
//!
//! Searching means looking at the room, under cushions, behind furniture — not
//! at a screen. A Geiger-counter cadence that quickens as you approach frees the
//! eyes entirely, and it is the reason metal detectors have sounded the same way
//! for sixty years.
//!
//! ## The rule everything here obeys
//!
//! **Silence means no signal. It never means "far away".**
//!
//! A distant device still clicks, slowly. A device that has gone quiet produces
//! nothing at all. If both were silent, somebody sweeping a room would have no
//! way to tell a dead link from a cold corner, and would keep searching a place
//! the device had already left. [`ProximityCue::for_snapshot`] returns `None`
//! only for staleness, and that is the whole contract.
//!
//! ## Why cadence rather than volume
//!
//! Loudness is a poor channel: it competes with ambient noise, it is unpleasant
//! at close range, and people are bad at judging it. Interval is judged
//! extremely well — the ear notices a rhythm changing long before it notices a
//! level changing — and it keeps working through a pocket, as vibration.
//!
//! Pitch rises alongside cadence because the two together read as one gesture,
//! and because a rising pitch is legible even when clicks are so fast they blur.

use crate::tracker::{Proximity, Snapshot};

/// What to play, and how often.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProximityCue {
    /// Milliseconds between clicks. Smaller is nearer.
    pub interval_ms: u32,
    /// Tone frequency in hertz. Higher is nearer.
    pub pitch_hz: u32,
    /// 0 at the far end of the useful range, 1 at arm's reach. For a platform
    /// that would rather modulate haptic strength than pitch.
    pub intensity: f64,
}

/// Tunable limits. The defaults are the interesting part; see each field.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FeedbackConfig {
    pub enabled: bool,
    /// Fastest cadence, at arm's reach.
    ///
    /// Not lower than about 60 ms: below that the clicks fuse into a buzz and
    /// the rhythm — the thing actually carrying the information — disappears.
    pub min_interval_ms: u32,
    /// Slowest cadence, at the edge of usable signal. Long enough not to nag,
    /// short enough that the user knows the app is still listening.
    pub max_interval_ms: u32,
    pub min_pitch_hz: u32,
    pub max_pitch_hz: u32,
    /// dBm mapped to the fastest cadence. Anything stronger is also fastest.
    pub near_dbm: f64,
    /// dBm mapped to the slowest cadence.
    pub far_dbm: f64,
    /// Amplitude, 0 to 1.
    ///
    /// Loud by default. This competes with a room being searched — drawers,
    /// rustling, someone talking — and a click nobody can hear over that is a
    /// feature that does not exist. The system volume control is the right place
    /// to be quiet.
    pub volume: f64,
}

impl Default for FeedbackConfig {
    fn default() -> Self {
        FeedbackConfig {
            // Off unless asked for. A finder that starts beeping the moment it
            // opens gets muted permanently, and then helps nobody.
            enabled: false,
            min_interval_ms: 70,
            max_interval_ms: 1_400,
            // A comfortable speech-range span. High enough to cut through room
            // noise, low enough not to be piercing held near the ear.
            min_pitch_hz: 440,
            max_pitch_hz: 1_320,
            near_dbm: -45.0,
            far_dbm: -95.0,
            volume: 0.9,
        }
    }
}

impl FeedbackConfig {
    /// Everything at once, for a caller that only wants on or off.
    pub fn on() -> Self {
        FeedbackConfig {
            enabled: true,
            ..Default::default()
        }
    }

    /// Clamp to a usable range, so a configuration file cannot produce a
    /// continuous tone or a silence indistinguishable from a fault.
    pub fn sanitised(mut self) -> Self {
        self.min_interval_ms = self.min_interval_ms.clamp(50, 5_000);
        self.max_interval_ms = self.max_interval_ms.clamp(self.min_interval_ms + 10, 10_000);
        self.min_pitch_hz = self.min_pitch_hz.clamp(80, 8_000);
        self.max_pitch_hz = self.max_pitch_hz.clamp(self.min_pitch_hz + 10, 12_000);
        // Explicit about NaN: an unordered pair is as broken as an inverted
        // one, and both must fall back rather than produce a silent mapping.
        if !matches!(
            self.near_dbm.partial_cmp(&self.far_dbm),
            Some(core::cmp::Ordering::Greater)
        ) {
            self.near_dbm = -45.0;
            self.far_dbm = -95.0;
        }
        // Below 1.0 to leave headroom: the click is a sine burst and clipping it
        // turns a clean tick into a rasp.
        self.volume = if self.volume.is_finite() {
            self.volume.clamp(0.05, 0.98)
        } else {
            0.9
        };
        self
    }
}

impl ProximityCue {
    /// What to play for this snapshot, or `None` for silence.
    ///
    /// `None` is returned only when feedback is off or the signal is stale. A
    /// weak-but-live signal always produces a cue, however slow — see the module
    /// comment for why that distinction is the whole design.
    pub fn for_snapshot(snapshot: &Snapshot, config: &FeedbackConfig) -> Option<ProximityCue> {
        if !config.enabled {
            return None;
        }
        // Staleness, and only staleness, is silent.
        if !snapshot.is_fresh {
            return None;
        }
        let dbm = snapshot.rssi_dbm?;
        Some(ProximityCue::for_rssi(dbm, config))
    }

    /// The mapping itself, independent of any snapshot.
    pub fn for_rssi(dbm: f64, config: &FeedbackConfig) -> ProximityCue {
        let config = config.sanitised();
        let span = config.near_dbm - config.far_dbm;
        // 0 at the far end, 1 at the near end.
        let t = ((dbm - config.far_dbm) / span).clamp(0.0, 1.0);

        // Interpolate geometrically rather than linearly. Perceived tempo and
        // pitch are both roughly logarithmic, so a linear ramp spends most of
        // its range on changes nobody can hear.
        let interval = geometric(
            config.max_interval_ms as f64,
            config.min_interval_ms as f64,
            t,
        );
        let pitch = geometric(config.min_pitch_hz as f64, config.max_pitch_hz as f64, t);

        ProximityCue {
            interval_ms: interval.round() as u32,
            pitch_hz: pitch.round() as u32,
            intensity: t,
        }
    }

    /// A coarse band, for a platform whose only control is which of a few
    /// built-in tones to play.
    pub fn band(&self) -> Proximity {
        match self.intensity {
            i if i >= 0.85 => Proximity::ArmsReach,
            i if i >= 0.65 => Proximity::SameTable,
            i if i >= 0.40 => Proximity::SameRoom,
            i if i >= 0.15 => Proximity::FarOrObstructed,
            _ => Proximity::VeryFarOrShielded,
        }
    }
}

/// Geometric interpolation from `a` to `b`. Both must be positive.
fn geometric(a: f64, b: f64, t: f64) -> f64 {
    a * (b / a).powf(t.clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::measurement::RssiSource;
    use crate::time::Timestamp;
    use crate::tracker::Tracker;

    fn tracked(dbm: f64, at: f64, sampled_at: f64) -> Snapshot {
        let mut t = Tracker::default();
        t.observe(crate::measurement::Measurement::Rssi {
            dbm,
            source: RssiSource::ConnectedLink,
            at: Timestamp(at),
        });
        t.snapshot(Timestamp(sampled_at))
    }

    #[test]
    fn feedback_is_off_until_asked_for() {
        let s = tracked(-50.0, 0.0, 0.2);
        assert_eq!(
            ProximityCue::for_snapshot(&s, &FeedbackConfig::default()),
            None,
            "a finder that beeps on launch gets muted forever"
        );
    }

    /// The rule the whole module exists to enforce.
    #[test]
    fn silence_means_no_signal_and_never_means_far_away() {
        let config = FeedbackConfig::on();

        // Very weak, but live: must still click.
        let weak = tracked(-96.0, 0.0, 0.2);
        assert!(
            ProximityCue::for_snapshot(&weak, &config).is_some(),
            "a distant device must stay audible, or silence becomes ambiguous"
        );

        // Strong, but stale: must be silent.
        let stale = tracked(-40.0, 0.0, 600.0);
        assert!(!stale.is_fresh);
        assert_eq!(ProximityCue::for_snapshot(&stale, &config), None);
    }

    #[test]
    fn closer_clicks_faster_and_higher() {
        let config = FeedbackConfig::on();
        let far = ProximityCue::for_rssi(-90.0, &config);
        let near = ProximityCue::for_rssi(-50.0, &config);

        assert!(near.interval_ms < far.interval_ms, "nearer must be quicker");
        assert!(near.pitch_hz > far.pitch_hz, "nearer must be higher");
        assert!(near.intensity > far.intensity);
    }

    #[test]
    fn the_cadence_stays_inside_its_configured_limits() {
        let config = FeedbackConfig::on();
        for dbm in [-127.0, -100.0, -80.0, -60.0, -30.0, -1.0] {
            let cue = ProximityCue::for_rssi(dbm, &config);
            assert!(
                cue.interval_ms >= config.min_interval_ms
                    && cue.interval_ms <= config.max_interval_ms,
                "{dbm} dBm produced {} ms",
                cue.interval_ms
            );
            assert!(cue.pitch_hz >= config.min_pitch_hz && cue.pitch_hz <= config.max_pitch_hz);
            assert!((0.0..=1.0).contains(&cue.intensity));
        }
    }

    #[test]
    fn the_fastest_cadence_stays_a_rhythm_rather_than_a_buzz() {
        // Below roughly 60 ms the clicks fuse and the information carried by the
        // rhythm is lost, so the floor is not merely cosmetic.
        let cue = ProximityCue::for_rssi(-20.0, &FeedbackConfig::on());
        assert!(cue.interval_ms >= 60, "got {} ms", cue.interval_ms);
    }

    #[test]
    fn absurd_configuration_is_clamped_rather_than_obeyed() {
        let broken = FeedbackConfig {
            enabled: true,
            min_interval_ms: 0,
            max_interval_ms: 0,
            min_pitch_hz: 0,
            max_pitch_hz: 0,
            // Inverted: near quieter than far.
            near_dbm: -95.0,
            far_dbm: -45.0,
            volume: f64::NAN,
        };
        let cue = ProximityCue::for_rssi(-60.0, &broken);
        assert!(cue.interval_ms >= 50, "a zero interval is a continuous tone");
        assert!(cue.pitch_hz >= 80);

        let fixed = broken.sanitised();
        assert!(fixed.near_dbm > fixed.far_dbm, "inverted range must be repaired");
        assert!(fixed.volume.is_finite(), "a NaN volume must not reach the synthesiser");
    }

    #[test]
    fn volume_defaults_loud_and_is_clamped_short_of_clipping() {
        assert!(
            FeedbackConfig::default().volume >= 0.8,
            "a click nobody can hear over a room being searched does not exist"
        );
        let shouted = FeedbackConfig {
            volume: 50.0,
            ..FeedbackConfig::on()
        }
        .sanitised();
        assert!(shouted.volume <= 0.98, "clipping turns a tick into a rasp");

        let silent = FeedbackConfig {
            volume: -3.0,
            ..FeedbackConfig::on()
        }
        .sanitised();
        assert!(silent.volume >= 0.05, "zero volume is indistinguishable from a fault");
    }

    #[test]
    fn bands_run_from_far_to_near() {
        let config = FeedbackConfig::on();
        assert_eq!(
            ProximityCue::for_rssi(-40.0, &config).band(),
            Proximity::ArmsReach
        );
        assert_eq!(
            ProximityCue::for_rssi(-99.0, &config).band(),
            Proximity::VeryFarOrShielded
        );
    }

    #[test]
    fn the_mapping_is_monotonic_across_the_whole_range() {
        // Any inversion would make the tool actively mislead: quickening as the
        // user walks the wrong way.
        let config = FeedbackConfig::on();
        let mut previous = u32::MAX;
        let mut dbm = -100.0;
        while dbm <= -30.0 {
            let cue = ProximityCue::for_rssi(dbm, &config);
            assert!(
                cue.interval_ms <= previous,
                "interval rose at {dbm} dBm: {} after {previous}",
                cue.interval_ms
            );
            previous = cue.interval_ms;
            dbm += 1.0;
        }
    }
}
