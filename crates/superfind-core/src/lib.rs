//! # superfind-core
//!
//! Sensor fusion for locating a radio device, with no platform in it.
//!
//! Everything here is plain arithmetic over observations that some other layer
//! collected — BlueZ on Linux, the Android Ranging API, CoreBluetooth on a
//! borrowed Mac. That boundary is deliberate. The fusion filter is the product,
//! so it lives somewhere it can be tested exhaustively on a laptop, replayed
//! from a recorded trace, and reused unchanged by every front end.
//!
//! ## What it does
//!
//! Feed it [`Measurement`]s and the user's movement; ask it for a [`Snapshot`].
//!
//! ```
//! use superfind_core::{Measurement, RssiSource, Timestamp, Tracker};
//!
//! let mut tracker = Tracker::default();
//! tracker.observe(Measurement::Rssi {
//!     dbm: -62.0,
//!     source: RssiSource::ConnectedLink,
//!     at: Timestamp(0.0),
//! });
//!
//! let snapshot = tracker.snapshot(Timestamp(0.5));
//! assert_eq!(snapshot.rssi_dbm, Some(-62.0));
//! assert!(snapshot.is_fresh);
//! ```
//!
//! ## The design commitment
//!
//! Every number this crate produces carries its uncertainty, and it declines to
//! answer rather than guess. [`ParticleFilter::fix`] returns `None` before any
//! evidence arrives instead of dressing up its prior; [`SyntheticAperture`]
//! refuses a bearing from a narrow sweep however many samples it holds; a
//! measured [`Measurement::Angle`] and an inferred [`BearingEstimate`] are
//! separate types precisely so a UI cannot draw the same confident arrow for
//! both.
//!
//! That discipline is inherited. This crate began as a port of the reasoning in
//! [findphone](https://github.com/ben-z/findphone), a macOS CLI that counted a
//! measurement only when the underlying value actually changed, and reported a
//! much smaller number than its poll rate as a result — because that number was
//! the true one. Two of its bugs are encoded here as regression tests: the
//! better RSSI source wins a window outright rather than being outvoted by a
//! noisier one that happens to arrive faster, and staleness is surfaced so that
//! silence can mean "no signal" rather than "no device".
//!
//! ## Layout
//!
//! - [`measurement`] — observations, each carrying its own noise term
//! - [`pathloss`] — dBm to metres, and the likelihood the filter actually uses
//! - [`motion`] — pedestrian dead reckoning; where the user is
//! - [`filter`] — the particle filter over target position
//! - [`bearing`] — direction inferred from a swept RSSI aperture
//! - [`capability`] — what a given device can do, and what may be promised
//! - [`tracker`] — the facade, and the immutable snapshot a UI renders
//! - [`geom`], [`rng`], [`time`] — primitives

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod bearing;
pub mod capability;
pub mod filter;
pub mod geom;
pub mod measurement;
pub mod motion;
pub mod pathloss;
pub mod rng;
pub mod time;
pub mod tracker;

pub use bearing::{BearingEstimate, SyntheticAperture};
pub use capability::{BearingQuality, Capabilities, Tier};
pub use filter::{Fix, FilterConfig, ParticleFilter};
pub use geom::{to_degrees, to_radians, Covariance2, Ellipse, Point2};
pub use measurement::{Measurement, RangeSource, RssiSource};
pub use motion::{DeadReckoner, Pose, StrideModel, TrailPoint};
pub use pathloss::PathLoss;
pub use rng::Rng;
pub use time::Timestamp;
pub use tracker::{Proximity, Snapshot, Tracker, TrackerConfig, Trend};

#[cfg(test)]
mod integration {
    //! Cross-module properties that no single module owns.

    use super::*;

    /// Fraction of the particle cloud within `radius` of a point. Ignores
    /// weights, which is fine immediately after a resample and is only ever used
    /// here to check that a posterior lobe exists at all.
    fn tracker_particles_near(tracker: &Tracker, centre: Point2, radius: f64) -> f64 {
        let particles = tracker.particles();
        if particles.is_empty() {
            return 0.0;
        }
        particles
            .iter()
            .filter(|p| centre.distance_to(**p) < radius)
            .count() as f64
            / particles.len() as f64
    }

    /// The headline claim: a walk that sweeps the compass recovers both the
    /// distance and the direction to a device, from signal strength alone, on
    /// hardware with no ranging radio at all. This is the Linux and old-Android
    /// path — the one that has to work everywhere.
    #[test]
    fn rssi_and_a_sweep_alone_locate_a_device() {
        let truth = Point2::new(10.0, 10.0);
        let mut tracker = Tracker::default();
        let model = tracker.config().path_loss;
        let mut rng = Rng::seeded(2024);
        let mut clock = 0.0;

        // Walk a rough spiral, turning as we go, which is what a person
        // actually does when hunting for something.
        let legs = [
            (0.0, 6),
            (90.0, 6),
            (180.0, 4),
            (270.0, 4),
            (45.0, 8),
            (135.0, 3),
            (315.0, 6),
            (225.0, 3),
        ];

        for (heading_deg, steps) in legs {
            tracker.set_heading(to_radians(heading_deg), Timestamp(clock));
            for _ in 0..steps {
                clock += 0.5;
                tracker.step(Timestamp(clock));
                let user = tracker.user_pose().position;

                // Body shadowing: the user's torso attenuates the signal when
                // the device is behind them. This is the effect the synthetic
                // aperture lives on.
                let facing = tracker.user_pose().heading;
                let alignment = geom::angle_diff(facing, user.bearing_to(truth)).cos();
                let shadow = 8.0 * (1.0 - alignment) / 2.0;

                for _ in 0..4 {
                    clock += 0.1;
                    let dbm = model.expected_rssi(user.distance_to(truth)) - shadow
                        + rng.normal_with(0.0, 4.0);
                    tracker.observe(Measurement::Rssi {
                        dbm,
                        source: RssiSource::ConnectedLink,
                        at: Timestamp(clock),
                    });
                }
            }
        }

        let s = tracker.snapshot(Timestamp(clock));

        let fix = s.fix.expect("a full walk should produce a fix");
        let error = fix.position.distance_to(truth);
        assert!(
            error < 7.0,
            "RSSI-only position error was {error} m (estimate {:?}, truth {truth:?})",
            fix.position
        );

        // The sweep covered the compass, so the bearing should be usable and
        // should say so.
        assert!(
            s.heading_coverage > 0.4,
            "coverage was only {}",
            s.heading_coverage
        );
        let bearing = s.bearing.expect("a swept walk should yield a bearing");
        let user = tracker.user_pose().position;
        let bearing_error =
            to_degrees(geom::angle_diff(bearing.bearing_rad, user.bearing_to(truth))).abs();
        assert!(
            bearing_error < 60.0,
            "swept bearing was {bearing_error} deg off"
        );
        assert!(!s.diverged);
    }

    /// Adding a ranging radio must strictly help. If a UWB-equipped phone did
    /// worse than an RSSI-only one, the fusion is wired up backwards.
    #[test]
    fn ranging_hardware_strictly_improves_the_answer() {
        let truth = Point2::new(7.0, -9.0);
        // A dogleg, not a straight line. Ranges taken from collinear points
        // leave a mirror ambiguity that no amount of precision resolves — see
        // `a_straight_line_walk_leaves_a_mirror_ambiguity`.
        let path: Vec<Point2> = (0..8)
            .map(|i| Point2::new(i as f64 * 1.5, 0.0))
            .chain((1..8).map(|i| Point2::new(10.5, -(i as f64) * 1.5)))
            .collect();

        let run = |with_ranging: bool, seed: u64| {
            let mut tracker = Tracker::new(TrackerConfig {
                seed,
                ..Default::default()
            });
            let model = tracker.config().path_loss;
            let mut rng = Rng::seeded(seed);
            let mut clock = 0.0;

            for user in &path {
                tracker.reset_user_to(*user, Timestamp(clock));
                let d = user.distance_to(truth);
                for _ in 0..4 {
                    clock += 0.1;
                    tracker.observe(Measurement::Rssi {
                        dbm: model.expected_rssi(d) + rng.normal_with(0.0, 4.0),
                        source: RssiSource::ConnectedLink,
                        at: Timestamp(clock),
                    });
                }
                if with_ranging {
                    tracker.observe(Measurement::Range {
                        metres: d + rng.normal_with(0.0, RangeSource::Uwb.sigma_m()),
                        source: RangeSource::Uwb,
                        at: Timestamp(clock),
                    });
                }
            }
            tracker
                .snapshot(Timestamp(clock))
                .fix
                .expect("fix")
                .position
                .distance_to(truth)
        };

        // Averaged over several seeds: a single Monte Carlo run proves nothing.
        let seeds = [1u64, 2, 3, 4, 5, 6, 7, 8];
        let plain: f64 = seeds.iter().map(|s| run(false, *s)).sum::<f64>() / seeds.len() as f64;
        let ranged: f64 = seeds.iter().map(|s| run(true, *s)).sum::<f64>() / seeds.len() as f64;

        assert!(
            ranged < plain,
            "ranging made it worse: {ranged} m vs {plain} m without"
        );
        assert!(ranged < 1.0, "ranging error averaged {ranged} m");
    }

    /// A real geometric limitation, pinned down so it cannot regress into a
    /// confident wrong answer.
    ///
    /// Ranges measured from points along a straight line cannot distinguish the
    /// target from its reflection in that line: every range is identical for
    /// both. No amount of measurement precision fixes it, because the ambiguity
    /// is in the geometry, not the noise. The posterior is genuinely bimodal,
    /// and the only honest response is a wide ellipse and low confidence — never
    /// a crisp point midway between the two lobes.
    ///
    /// The product consequence, which belongs in the UI: tell the user to walk a
    /// dogleg. Twenty steps around a corner beat two hundred in a straight line.
    #[test]
    fn a_straight_line_walk_leaves_a_mirror_ambiguity() {
        let truth = Point2::new(7.0, -9.0);
        let mut tracker = Tracker::default();
        let mut clock = 0.0;

        // Walk due east along y = 0, taking near-perfect UWB ranges.
        for i in 0..16 {
            let user = Point2::new(i as f64 * 0.9, 0.0);
            tracker.reset_user_to(user, Timestamp(clock));
            for _ in 0..4 {
                clock += 0.1;
                tracker.observe(Measurement::Range {
                    metres: user.distance_to(truth),
                    source: RangeSource::Uwb,
                    at: Timestamp(clock),
                });
            }
        }

        let fix = tracker.snapshot(Timestamp(clock)).fix.expect("fix");
        let mirrored = Point2::new(truth.x, -truth.y);

        // The east-west coordinate is well determined; it is north-south that
        // the reflection destroys.
        assert!(
            (fix.position.x - truth.x).abs() < 2.0,
            "along-track position should still be recovered, got {:?}",
            fix.position
        );

        // The filter must not have collapsed onto one lobe with confidence, nor
        // reported the midpoint as a precise answer.
        assert!(
            fix.ellipse.semi_major > 3.0,
            "a bimodal posterior must stay wide, got semi-major {}",
            fix.ellipse.semi_major
        );
        assert!(
            fix.confidence < 0.75,
            "an unresolvable ambiguity must not read as confident, got {}",
            fix.confidence
        );

        // Both lobes remain populated: this is a real ambiguity, not a filter
        // that has simply lost the target.
        let near_truth = tracker_particles_near(&tracker, truth, 3.0);
        let near_mirror = tracker_particles_near(&tracker, mirrored, 3.0);
        assert!(
            near_truth > 0.05 && near_mirror > 0.05,
            "expected both lobes populated, got {near_truth:.3} and {near_mirror:.3}"
        );
    }

    /// Uncertainty must shrink monotonically-ish as evidence accumulates. A
    /// filter whose confidence wanders upward on no new information is lying.
    #[test]
    fn confidence_grows_with_evidence_not_with_time() {
        let truth = Point2::new(5.0, 5.0);
        let mut tracker = Tracker::default();
        let mut clock = 0.0;

        let sample_at = |tracker: &mut Tracker, user: Point2, clock: &mut f64| {
            tracker.reset_user_to(user, Timestamp(*clock));
            for _ in 0..6 {
                *clock += 0.1;
                tracker.observe(Measurement::Range {
                    metres: user.distance_to(truth),
                    source: RangeSource::Uwb,
                    at: Timestamp(*clock),
                });
            }
        };

        sample_at(&mut tracker, Point2::new(0.0, 0.0), &mut clock);
        let early = tracker.snapshot(Timestamp(clock)).fix.unwrap();

        // Time passes with no new measurements: confidence must not improve.
        let idle = tracker.snapshot(Timestamp(clock + 60.0)).fix.unwrap();
        assert!(
            idle.confidence <= early.confidence + 1e-9,
            "confidence rose on no evidence: {} -> {}",
            early.confidence,
            idle.confidence
        );

        // New geometry, though, genuinely helps.
        sample_at(&mut tracker, Point2::new(10.0, 0.0), &mut clock);
        sample_at(&mut tracker, Point2::new(0.0, 10.0), &mut clock);
        let late = tracker.snapshot(Timestamp(clock)).fix.unwrap();

        assert!(
            late.ellipse.semi_major < early.ellipse.semi_major,
            "ellipse did not shrink: {} -> {}",
            early.ellipse.semi_major,
            late.ellipse.semi_major
        );
        assert!(late.confidence > early.confidence);
    }
}
