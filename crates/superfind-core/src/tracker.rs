//! The facade: one object a platform layer drives, one snapshot it renders.
//!
//! Modelled on the boundary findphone got right. The renderer is a pure function
//! of an immutable [`Snapshot`], so the UI cannot reach into mutable tracker
//! state, cannot race it, and cannot accidentally show two numbers that
//! disagree because they were sampled a frame apart.

use crate::bearing::{BearingEstimate, SyntheticAperture};
use crate::filter::{Fix, FilterConfig, ParticleFilter};
use crate::geom::Point2;
use crate::measurement::{Measurement, RssiSource};
use crate::motion::{DeadReckoner, Pose, StrideModel, TrailPoint};
use crate::pathloss::PathLoss;
use crate::time::Timestamp;

/// Coarse distance bands for display. Straight from findphone, which arrived at
/// them empirically, with the caveat it states plainly: signal strength is a
/// poor proxy for distance, and a phone in a filing cabinet two metres away
/// reads like one fifteen metres away in open air. Trust the trend, not the
/// band.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Proximity {
    ArmsReach,
    SameTable,
    SameRoom,
    FarOrObstructed,
    VeryFarOrShielded,
}

impl Proximity {
    pub fn of(rssi_dbm: f64) -> Proximity {
        match rssi_dbm {
            r if r >= -45.0 => Proximity::ArmsReach,
            r if r >= -60.0 => Proximity::SameTable,
            r if r >= -72.0 => Proximity::SameRoom,
            r if r >= -85.0 => Proximity::FarOrObstructed,
            _ => Proximity::VeryFarOrShielded,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Proximity::ArmsReach => "arm's reach",
            Proximity::SameTable => "same table",
            Proximity::SameRoom => "same room",
            Proximity::FarOrObstructed => "far, or behind cover",
            Proximity::VeryFarOrShielded => "very far, or shielded",
        }
    }
}

/// Which way the signal is going. Needs both windows populated before it will
/// commit to an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trend {
    Warmer,
    Colder,
    Steady,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    pub filter: FilterConfig,
    pub stride: StrideModel,
    pub path_loss: PathLoss,
    /// Sectors for the synthetic-aperture bearing.
    pub bearing_sectors: usize,
    /// Window the displayed reading is the median of, seconds.
    pub live_window_s: f64,
    /// Older window the trend compares against, seconds.
    pub trend_window_s: f64,
    /// dB of change before the trend commits to warmer or colder.
    pub trend_threshold_db: f64,
    /// How long a reading remains worth steering by, seconds.
    pub freshness_s: f64,
    /// How much RSSI history to retain, seconds.
    pub history_s: f64,
    pub seed: u64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        TrackerConfig {
            filter: FilterConfig::default(),
            stride: StrideModel::default(),
            path_loss: PathLoss::default(),
            bearing_sectors: 16,
            live_window_s: 4.0,
            trend_window_s: 12.0,
            trend_threshold_db: 3.0,
            freshness_s: 10.0,
            history_s: 600.0,
            seed: 0x5f_1d_9e_2b,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Sample {
    at: Timestamp,
    dbm: f64,
    source: RssiSource,
}

/// Everything the UI needs, sampled at one instant.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub at: Timestamp,
    /// Median of the live window, from the best source available in it.
    pub rssi_dbm: Option<f64>,
    /// Which source that median came from.
    pub rssi_source: Option<RssiSource>,
    /// Whether the newest reading is recent enough to steer by. When false the
    /// UI should say "no signal", and the proximity clicks should fall silent —
    /// silence must mean no signal, never a device that is simply far away.
    pub is_fresh: bool,
    pub age_s: Option<f64>,
    /// Single-reading distance estimate. For display only; the filter does not
    /// consume this.
    pub crude_distance_m: Option<f64>,
    pub proximity: Option<Proximity>,
    pub trend: Trend,
    /// The fused estimate. `None` until evidence exists.
    pub fix: Option<Fix>,
    /// The swept-RSSI bearing inference, distinct from a measured angle.
    pub bearing: Option<BearingEstimate>,
    pub user: Pose,
    pub steps: u32,
    pub distance_walked_m: f64,
    pub heading_coverage: f64,
    pub samples_in_window: usize,
    pub total_samples: usize,
    pub observations: u32,
    /// Model and measurements have become irreconcilable; the honest UI
    /// response is to say so and offer a reset.
    pub diverged: bool,
    /// Readings contributed by peers observing from their own positions. Zero
    /// on a solo hunt. Worth showing: it is the difference between an annulus
    /// and a fix, and the user should know which one they are looking at.
    pub remote_observations: u32,
}

#[derive(Debug, Clone)]
pub struct Tracker {
    config: TrackerConfig,
    filter: ParticleFilter,
    aperture: SyntheticAperture,
    motion: DeadReckoner,
    history: Vec<Sample>,
    last_predict: Timestamp,
    /// How many observations arrived from peers rather than the local user.
    remote_observations: u32,
}

impl Default for Tracker {
    fn default() -> Self {
        Tracker::new(TrackerConfig::default())
    }
}

impl Tracker {
    pub fn new(config: TrackerConfig) -> Self {
        Tracker {
            filter: ParticleFilter::new(config.filter, Point2::ORIGIN, config.seed),
            aperture: SyntheticAperture::new(
                config.bearing_sectors,
                RssiSource::Advertisement.sigma_db(),
            ),
            motion: DeadReckoner::new(config.stride),
            history: Vec::new(),
            last_predict: Timestamp::ZERO,
            remote_observations: 0,
            config,
        }
    }

    pub fn config(&self) -> TrackerConfig {
        self.config
    }

    pub fn user_pose(&self) -> Pose {
        self.motion.pose()
    }

    pub fn trail(&self) -> &[TrailPoint] {
        self.motion.trail()
    }

    /// Mean RSSI per aperture sector, `None` where nothing was sampled. What
    /// the radar draws its heat ring from.
    pub fn sector_means(&self) -> Vec<Option<f64>> {
        self.aperture.sector_means()
    }

    /// The raw particle cloud. For diagnostics, and for a UI that would rather
    /// draw the posterior as a heat map than flatten it to one ellipse — which
    /// is the more honest rendering whenever the posterior is bimodal.
    pub fn particles(&self) -> &[Point2] {
        self.filter.particles()
    }

    /// Replace the path-loss model, e.g. after a calibration walk.
    pub fn set_path_loss(&mut self, model: PathLoss) {
        self.config.path_loss = model;
    }

    pub fn set_heading(&mut self, heading_rad: f64, at: Timestamp) {
        self.motion.set_heading(heading_rad, at);
    }

    pub fn step(&mut self, at: Timestamp) {
        self.motion.step(at);
    }

    pub fn step_of(&mut self, length_m: f64, at: Timestamp) {
        self.motion.step_of(length_m, at);
    }

    /// Place the user at a known position, clearing dead-reckoning drift.
    ///
    /// For an external fix better than anything step counting can offer — a
    /// GNSS lock outdoors, a scanned anchor indoors — and for replaying a trace
    /// whose true positions are known.
    pub fn reset_user_to(&mut self, position: Point2, at: Timestamp) {
        let heading = self.motion.heading();
        self.motion.reset_to(Pose { position, heading }, at);
    }

    /// Fold in an observation. Returns false if it was rejected as implausible.
    pub fn observe(&mut self, m: Measurement) -> bool {
        let here = self.motion.position();
        self.observe_from(m, here)
    }

    /// Fold in an observation taken from a *known* position that is not the
    /// user's.
    ///
    /// This is what turns the whole thing from a hot-and-cold game into
    /// trilateration. A single observer's RSSI likelihood is an annulus — "the
    /// device is somewhere on a ring around me" — and no amount of extra
    /// readings from that one spot will narrow it, which is why the UI has to
    /// ask the user to walk a dogleg. Two observers a few metres apart intersect
    /// their rings and collapse the posterior immediately, standing still.
    ///
    /// The caller owns the hard part: `observer` must be expressed in *this
    /// tracker's* coordinate frame, and establishing that shared frame between
    /// two phones is a real problem this function does not solve. Anchoring one
    /// device at the origin, or placing peers by hand, is the honest starting
    /// point.
    pub fn observe_from(&mut self, m: Measurement, observer: Point2) -> bool {
        if !m.is_plausible() {
            return false;
        }
        let now = m.at();

        let dt = now - self.last_predict;
        if dt > 0.0 {
            self.filter.predict(dt);
            self.last_predict = now;
        }

        if !self.filter.update(&m, observer, &self.config.path_loss) {
            return false;
        }

        // Only the local user's readings feed the display window and the
        // aperture. A peer's signal strength says nothing about which way *this*
        // phone is pointing, and blending it into the on-screen number would
        // make the reading jump for reasons the user cannot see.
        let local = observer == self.motion.position();
        if let Measurement::Rssi { dbm, source, at } = m {
            if local {
                self.history.push(Sample { at, dbm, source });
                // The aperture is fed the raw sample and the heading it was
                // taken at; body shadowing is the signal it works from.
                self.aperture.observe(dbm, self.motion.heading());
            }
        }
        if !local {
            self.remote_observations = self.remote_observations.saturating_add(1);
        }

        self.prune(now);
        true
    }

    /// Discard everything and start over from the user's current position.
    pub fn reset(&mut self) {
        self.filter.reset(self.motion.position());
        self.aperture.clear();
        self.history.clear();
        self.remote_observations = 0;
    }

    /// Observations contributed by peers. Zero means this is a solo hunt.
    pub fn remote_observations(&self) -> u32 {
        self.remote_observations
    }

    fn prune(&mut self, now: Timestamp) {
        let horizon = self.config.history_s;
        // History is appended in time order, so the expired prefix is contiguous.
        let cut = self
            .history
            .iter()
            .position(|s| s.at.age_at(now) <= horizon)
            .unwrap_or(self.history.len());
        if cut > 0 {
            self.history.drain(..cut);
        }
    }

    fn window(&self, seconds: f64, now: Timestamp) -> &[Sample] {
        let start = self
            .history
            .iter()
            .position(|s| s.at.age_at(now) <= seconds)
            .unwrap_or(self.history.len());
        &self.history[start..]
    }

    /// Median RSSI over a window, restricted to the best source present.
    ///
    /// This is findphone's lesson made structural. That tool mixed
    /// connected-link reads with passively observed advertisements in one
    /// median; adverts arrive far faster, so the noisier source outvoted the
    /// better one. Here the best source in the window wins outright and the
    /// others are not consulted at all.
    fn median_of_best(&self, seconds: f64, now: Timestamp) -> Option<(f64, RssiSource, usize)> {
        let window = self.window(seconds, now);
        let best = window.iter().map(|s| s.source).min()?;
        let mut values: Vec<f64> = window
            .iter()
            .filter(|s| s.source == best)
            .map(|s| s.dbm)
            .collect();
        if values.is_empty() {
            return None;
        }
        values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        Some((values[values.len() / 2], best, values.len()))
    }

    fn trend(&self, now: Timestamp) -> Trend {
        let recent = self.median_of_best(self.config.live_window_s, now);
        let older: Vec<f64> = self
            .window(self.config.trend_window_s, now)
            .iter()
            .filter(|s| s.at.age_at(now) > self.config.live_window_s)
            .map(|s| s.dbm)
            .collect();

        let (Some((new, _, new_count)), true) = (recent, older.len() >= 2) else {
            return Trend::Unknown;
        };
        if new_count < 2 {
            return Trend::Unknown;
        }

        let mut older = older;
        older.sort_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
        let old = older[older.len() / 2];

        let delta = new - old;
        if delta > self.config.trend_threshold_db {
            Trend::Warmer
        } else if delta < -self.config.trend_threshold_db {
            Trend::Colder
        } else {
            Trend::Steady
        }
    }

    /// Sample the whole tracker at `now`.
    pub fn snapshot(&self, now: Timestamp) -> Snapshot {
        let live = self.median_of_best(self.config.live_window_s, now);
        let newest = self.history.last();
        let age_s = newest.map(|s| s.at.age_at(now));
        let is_fresh = age_s.is_some_and(|a| a < self.config.freshness_s);

        let rssi_dbm = live.map(|(v, _, _)| v);
        let crude_distance_m = rssi_dbm.map(|v| self.config.path_loss.distance(v));

        Snapshot {
            at: now,
            rssi_dbm,
            rssi_source: live.map(|(_, s, _)| s),
            is_fresh,
            age_s,
            crude_distance_m,
            proximity: rssi_dbm.map(Proximity::of),
            trend: self.trend(now),
            fix: self.filter.fix(self.motion.position()),
            bearing: self.aperture.estimate(),
            user: self.motion.pose(),
            remote_observations: self.remote_observations,
            steps: self.motion.steps(),
            distance_walked_m: self.motion.distance_walked(),
            heading_coverage: self.motion.heading_coverage(self.config.bearing_sectors),
            samples_in_window: live.map(|(_, _, n)| n).unwrap_or(0),
            total_samples: self.history.len(),
            observations: self.filter.observations(),
            diverged: self.filter.diverged(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::to_radians;
    use crate::measurement::RangeSource;
    use crate::rng::Rng;

    fn rssi(dbm: f64, source: RssiSource, t: f64) -> Measurement {
        Measurement::Rssi {
            dbm,
            source,
            at: Timestamp(t),
        }
    }

    #[test]
    fn proximity_bands_are_ordered_and_cover_the_range() {
        assert_eq!(Proximity::of(-30.0), Proximity::ArmsReach);
        assert_eq!(Proximity::of(-45.0), Proximity::ArmsReach);
        assert_eq!(Proximity::of(-50.0), Proximity::SameTable);
        assert_eq!(Proximity::of(-65.0), Proximity::SameRoom);
        assert_eq!(Proximity::of(-80.0), Proximity::FarOrObstructed);
        assert_eq!(Proximity::of(-99.0), Proximity::VeryFarOrShielded);
        assert!(Proximity::ArmsReach < Proximity::VeryFarOrShielded);
    }

    #[test]
    fn empty_tracker_reports_nothing_rather_than_zero() {
        let t = Tracker::default();
        let s = t.snapshot(Timestamp(1.0));
        assert!(s.rssi_dbm.is_none());
        assert!(s.fix.is_none());
        assert!(s.bearing.is_none());
        assert!(!s.is_fresh);
        assert_eq!(s.trend, Trend::Unknown);
    }

    #[test]
    fn the_better_source_wins_the_window_outright() {
        // This is the findphone bug, as a test. Adverts arrive far more often
        // and read several dB lower; a blended median would follow them.
        let mut t = Tracker::default();
        for i in 0..40 {
            t.observe(rssi(-80.0, RssiSource::Advertisement, i as f64 * 0.05));
        }
        for i in 0..3 {
            t.observe(rssi(-50.0, RssiSource::ConnectedLink, 2.0 + i as f64 * 0.1));
        }

        let s = t.snapshot(Timestamp(2.5));
        assert_eq!(s.rssi_source, Some(RssiSource::ConnectedLink));
        assert_eq!(s.rssi_dbm, Some(-50.0));
        assert_eq!(
            s.samples_in_window, 3,
            "only the trusted source should be counted"
        );
    }

    #[test]
    fn falls_back_to_a_weaker_source_when_it_is_all_there_is() {
        let mut t = Tracker::default();
        t.observe(rssi(-70.0, RssiSource::Advertisement, 0.0));
        t.observe(rssi(-72.0, RssiSource::Advertisement, 0.5));
        let s = t.snapshot(Timestamp(1.0));
        assert_eq!(s.rssi_source, Some(RssiSource::Advertisement));
        assert!(s.rssi_dbm.is_some());
    }

    #[test]
    fn median_ignores_a_reflected_spike() {
        let mut t = Tracker::default();
        for (i, v) in [-70.0, -71.0, -30.0, -70.0, -72.0].iter().enumerate() {
            t.observe(rssi(*v, RssiSource::ConnectedLink, i as f64 * 0.2));
        }
        let s = t.snapshot(Timestamp(1.0));
        // The -30 outlier must not drag the reading several metres.
        assert_eq!(s.rssi_dbm, Some(-70.0));
    }

    #[test]
    fn staleness_is_reported_so_silence_can_mean_no_signal() {
        let mut t = Tracker::default();
        t.observe(rssi(-60.0, RssiSource::ConnectedLink, 0.0));
        assert!(t.snapshot(Timestamp(1.0)).is_fresh);
        let stale = t.snapshot(Timestamp(30.0));
        assert!(!stale.is_fresh);
        assert!(stale.age_s.unwrap() >= 30.0);
    }

    #[test]
    fn trend_detects_approach_and_retreat() {
        let mut approaching = Tracker::default();
        for i in 0..10 {
            approaching.observe(rssi(-80.0, RssiSource::ConnectedLink, i as f64 * 0.5));
        }
        for i in 0..10 {
            approaching.observe(rssi(-60.0, RssiSource::ConnectedLink, 6.0 + i as f64 * 0.1));
        }
        assert_eq!(approaching.snapshot(Timestamp(7.0)).trend, Trend::Warmer);

        let mut leaving = Tracker::default();
        for i in 0..10 {
            leaving.observe(rssi(-60.0, RssiSource::ConnectedLink, i as f64 * 0.5));
        }
        for i in 0..10 {
            leaving.observe(rssi(-80.0, RssiSource::ConnectedLink, 6.0 + i as f64 * 0.1));
        }
        assert_eq!(leaving.snapshot(Timestamp(7.0)).trend, Trend::Colder);
    }

    #[test]
    fn trend_stays_unknown_without_both_windows() {
        let mut t = Tracker::default();
        t.observe(rssi(-60.0, RssiSource::ConnectedLink, 0.0));
        t.observe(rssi(-61.0, RssiSource::ConnectedLink, 0.2));
        assert_eq!(t.snapshot(Timestamp(0.5)).trend, Trend::Unknown);
    }

    #[test]
    fn implausible_readings_never_enter_the_history() {
        let mut t = Tracker::default();
        assert!(!t.observe(rssi(-127.0, RssiSource::ConnectedLink, 0.0)));
        assert!(!t.observe(rssi(5.0, RssiSource::ConnectedLink, 0.1)));
        let s = t.snapshot(Timestamp(1.0));
        assert_eq!(s.total_samples, 0);
        assert_eq!(s.observations, 0);
    }

    #[test]
    fn history_is_pruned_to_the_configured_horizon() {
        let mut t = Tracker::new(TrackerConfig {
            history_s: 10.0,
            ..Default::default()
        });
        for i in 0..500 {
            t.observe(rssi(-65.0, RssiSource::ConnectedLink, i as f64 * 0.1));
        }
        let s = t.snapshot(Timestamp(50.0));
        // 10 s of history at 10 Hz, give or take the boundary sample.
        assert!(s.total_samples <= 102, "history grew to {}", s.total_samples);
        assert!(s.total_samples > 0);
    }

    #[test]
    fn a_walk_with_ranging_locates_the_device_end_to_end() {
        let truth = Point2::new(8.0, 6.0);
        let mut t = Tracker::default();
        let model = t.config().path_loss;
        let mut rng = Rng::seeded(4242);
        let mut clock = 0.0;

        // Walk east, then north, feeding RSSI plus occasional UWB ranges.
        for (heading_deg, steps) in [(90.0, 11), (0.0, 8)] {
            t.set_heading(to_radians(heading_deg), Timestamp(clock));
            for _ in 0..steps {
                clock += 0.6;
                t.step(Timestamp(clock));
                let user = t.user_pose().position;
                let d = user.distance_to(truth);
                for _ in 0..3 {
                    clock += 0.1;
                    t.observe(rssi(
                        model.expected_rssi(d) + rng.normal_with(0.0, 4.0),
                        RssiSource::ConnectedLink,
                        clock,
                    ));
                }
                t.observe(Measurement::Range {
                    metres: d,
                    source: RangeSource::Uwb,
                    at: Timestamp(clock),
                });
            }
        }

        let s = t.snapshot(Timestamp(clock));
        let fix = s.fix.expect("should have a fix after a walk with ranging");
        let error = fix.position.distance_to(truth);
        assert!(error < 1.5, "end-to-end error was {error} m");
        assert!(fix.confidence > 0.5, "confidence was {}", fix.confidence);
        assert!(s.is_fresh);
        assert!(!s.diverged);
        assert!(s.steps == 19, "steps were {}", s.steps);
        assert!(s.distance_walked_m > 13.0);
    }

    #[test]
    fn bearing_needs_a_sweep_not_just_samples() {
        let mut t = Tracker::default();
        // Hundreds of readings, all facing one way.
        t.set_heading(0.0, Timestamp::ZERO);
        for i in 0..300 {
            t.observe(rssi(-55.0, RssiSource::ConnectedLink, i as f64 * 0.05));
        }
        let s = t.snapshot(Timestamp(20.0));
        assert!(s.heading_coverage < 0.15);
        // Either no bearing at all, or one that admits it is worthless.
        if let Some(b) = s.bearing {
            assert!(b.confidence < 0.25, "confidence was {}", b.confidence);
        }
    }

    #[test]
    fn reset_clears_evidence_but_keeps_the_user_where_they_are() {
        let mut t = Tracker::default();
        t.set_heading(to_radians(90.0), Timestamp::ZERO);
        t.step(Timestamp(1.0));
        t.observe(rssi(-55.0, RssiSource::ConnectedLink, 1.5));
        let before = t.user_pose().position;

        t.reset();
        let s = t.snapshot(Timestamp(2.0));
        assert!(s.fix.is_none());
        assert!(s.rssi_dbm.is_none());
        assert_eq!(s.total_samples, 0);
        assert_eq!(t.user_pose().position, before, "reset must not teleport the user");
    }

    #[test]
    fn snapshot_is_internally_consistent() {
        // The number, the band and the crude distance must all derive from the
        // same reading — findphone's rule that the display cannot disagree with
        // itself.
        let mut t = Tracker::default();
        for i in 0..10 {
            t.observe(rssi(-64.0, RssiSource::ConnectedLink, i as f64 * 0.1));
        }
        let s = t.snapshot(Timestamp(1.0));
        let dbm = s.rssi_dbm.unwrap();
        assert_eq!(s.proximity, Some(Proximity::of(dbm)));
        let expected = t.config().path_loss.distance(dbm);
        assert!((s.crude_distance_m.unwrap() - expected).abs() < 1e-9);
    }
}
