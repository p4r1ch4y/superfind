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
//! - [`identity`] — what an advertisement says a device is, when it has no name
//! - [`peer`] — observations shared between devices hunting the same thing
//! - [`following`] — devices that have been travelling with you, per DULT
//! - [`altitude`] — which floor it is on, from a barometer
//! - [`feedback`] — hearing the signal instead of watching it
//! - [`tracker`] — the facade, and the immutable snapshot a UI renders
//! - [`geom`], [`rng`], [`time`] — primitives

#![forbid(unsafe_code)]
#![warn(missing_debug_implementations)]

pub mod altitude;
pub mod bearing;
pub mod capability;
pub mod feedback;
pub mod filter;
pub mod geom;
pub mod following;
pub mod identity;
pub mod measurement;
pub mod motion;
pub mod pathloss;
pub mod peer;
pub mod rng;
pub mod time;
pub mod tracker;

pub use altitude::{Altimeter, FloorDelta};
pub use bearing::{BearingEstimate, SyntheticAperture};
pub use capability::{BearingQuality, Capabilities, Tier};
pub use feedback::{FeedbackConfig, ProximityCue};
pub use filter::{Fix, FilterConfig, ParticleFilter};
pub use geom::{to_degrees, to_radians, Covariance2, Ellipse, Point2};
pub use following::{FollowPolicy, FollowVerdict, FollowWatch};
pub use identity::{is_randomised_address, short_uuid, DeviceIdentity};
pub use measurement::{Measurement, RangeSource, RssiSource};
pub use motion::{DeadReckoner, Pose, StrideModel, TrailPoint};
pub use pathloss::PathLoss;
pub use peer::{Anchor, PeerReport};
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

    /// Two observers collapse the annulus that one, standing still, never can.
    ///
    /// A lone observer feeding the filter a hundred readings from one spot
    /// learns the *radius* precisely and the *direction* not at all, so its
    /// ellipse stays as wide as the ring is across. A second pair of eyes a few
    /// metres away intersects two rings and the uncertainty collapses — with
    /// nobody walking anywhere.
    ///
    /// What two observers do *not* buy is a unique answer: see
    /// [`two_observers_still_leave_a_mirror_ambiguity`].
    #[test]
    fn a_second_observer_shrinks_the_posterior() {
        let truth = Point2::new(9.0, 5.0);
        let model = PathLoss::default();
        let alice = Point2::new(0.0, 0.0);
        let bob = Point2::new(12.0, 0.0);

        let reading = |from: Point2, rng: &mut Rng, clock: f64| Measurement::Rssi {
            dbm: model.expected_rssi(from.distance_to(truth)) + rng.normal_with(0.0, 3.0),
            source: RssiSource::ConnectedLink,
            at: Timestamp(clock),
        };

        let mut solo = Tracker::default();
        let mut rng = Rng::seeded(77);
        let mut clock = 0.0;
        for _ in 0..240 {
            clock += 0.1;
            solo.observe(reading(alice, &mut rng, clock));
        }

        let mut paired = Tracker::default();
        let mut rng = Rng::seeded(77);
        let mut clock = 0.0;
        for _ in 0..240 {
            clock += 0.1;
            paired.observe(reading(alice, &mut rng, clock));
            paired.observe_from(reading(bob, &mut rng, clock), bob);
        }

        let solo_fix = solo.snapshot(Timestamp(clock)).fix.expect("solo fix");
        let paired_fix = paired.snapshot(Timestamp(clock)).fix.expect("paired fix");

        // The ellipse is the honest measure here, not the point estimate: it is
        // what the UI draws and what tells the user how much to trust it.
        // Measured, this is roughly 19 m down to 12 m.
        assert!(
            paired_fix.ellipse.semi_major < solo_fix.ellipse.semi_major * 0.75,
            "uncertainty should shrink: {:.1} m vs {:.1} m",
            paired_fix.ellipse.semi_major,
            solo_fix.ellipse.semi_major
        );
        // But it cannot shrink far, and the reason is worth pinning down: the
        // posterior is now two lobes ten metres apart, and any ellipse covering
        // both is at least that wide. Precision in the ranges cannot help — only
        // breaking the symmetry can, which is what the next two tests are about.
        assert!(
            paired_fix.ellipse.semi_major > 5.0,
            "the mirror lobes floor the ellipse; got {:.1} m",
            paired_fix.ellipse.semi_major
        );
        assert_eq!(paired.remote_observations(), 240);
        assert_eq!(solo.remote_observations(), 0);
    }

    /// Two observers leave the target indistinguishable from its reflection in
    /// the line joining them.
    ///
    /// Two ranges intersect two circles, and two circles meet at two points. The
    /// ambiguity is geometric, so no amount of precision or averaging removes
    /// it — and the filter's honest response is a posterior straddling both
    /// lobes, whose *mean* sits in the empty space between them. That is why the
    /// point estimate can be metres out while the ellipse is small.
    ///
    /// The cure is a third observer off the line, or one observer moving off it.
    /// This is the same trap as `a_straight_line_walk_leaves_a_mirror_ambiguity`,
    /// arriving by a different route.
    #[test]
    fn two_observers_still_leave_a_mirror_ambiguity() {
        let truth = Point2::new(9.0, 5.0);
        let mirrored = Point2::new(9.0, -5.0);
        let model = PathLoss::default();
        let alice = Point2::new(0.0, 0.0);
        let bob = Point2::new(12.0, 0.0);

        // The two candidates are equidistant from both observers, which is
        // precisely why signal strength cannot separate them.
        assert!((alice.distance_to(truth) - alice.distance_to(mirrored)).abs() < 1e-9);
        assert!((bob.distance_to(truth) - bob.distance_to(mirrored)).abs() < 1e-9);

        let mut t = Tracker::default();
        let mut rng = Rng::seeded(77);
        let mut clock = 0.0;
        for _ in 0..240 {
            clock += 0.1;
            let reading = |from: Point2, rng: &mut Rng| Measurement::Rssi {
                dbm: model.expected_rssi(from.distance_to(truth)) + rng.normal_with(0.0, 3.0),
                source: RssiSource::ConnectedLink,
                at: Timestamp(clock),
            };
            t.observe(reading(alice, &mut rng));
            t.observe_from(reading(bob, &mut rng), bob);
        }

        let particles = t.particles();
        let near = |p: Point2| particles.iter().filter(|q| q.distance_to(p) < 4.0).count();
        assert!(
            near(truth) > particles.len() / 10 && near(mirrored) > particles.len() / 10,
            "both lobes should survive: {} at truth, {} at the mirror, of {}",
            near(truth),
            near(mirrored),
            particles.len()
        );
    }

    /// Three observers off a common line resolve it outright.
    ///
    /// This is the configuration worth telling users about: two phones and a
    /// laptop, or two phones and one of them moved a few steps sideways, and the
    /// device is simply located — no walking a dogleg, no waiting for a bearing
    /// to earn its arrow.
    #[test]
    fn three_observers_resolve_the_position_outright() {
        let truth = Point2::new(9.0, 5.0);
        let model = PathLoss::default();
        let observers = [
            Point2::new(0.0, 0.0),
            Point2::new(12.0, 0.0),
            // Off the line joining the other two — that is what breaks the tie.
            Point2::new(4.0, 9.0),
        ];

        let mut t = Tracker::default();
        let mut rng = Rng::seeded(2027);
        let mut clock = 0.0;
        for _ in 0..200 {
            clock += 0.1;
            for from in observers {
                let m = Measurement::Rssi {
                    dbm: model.expected_rssi(from.distance_to(truth))
                        + rng.normal_with(0.0, 3.0),
                    source: RssiSource::ConnectedLink,
                    at: Timestamp(clock),
                };
                t.observe_from(m, from);
            }
        }

        let fix = t.snapshot(Timestamp(clock)).fix.expect("a fix");
        let error = fix.position.distance_to(truth);
        assert!(
            error < 2.5,
            "three observers should locate to a couple of metres, got {error:.1} m \
             (estimate {:?}, truth {truth:?})",
            fix.position
        );
        assert!(
            fix.ellipse.semi_major < 4.0,
            "and say so: ellipse was {:.1} m",
            fix.ellipse.semi_major
        );
    }

    /// A peer's readings must not disturb what the local user sees. The number
    /// on screen is *this* phone's signal, and blending in a peer's would make
    /// it jump for reasons the person holding it cannot observe.
    #[test]
    fn a_peers_readings_do_not_move_the_local_display() {
        let mut t = Tracker::default();
        t.observe(Measurement::Rssi {
            dbm: -55.0,
            source: RssiSource::ConnectedLink,
            at: Timestamp(0.0),
        });
        t.observe_from(
            Measurement::Rssi {
                dbm: -95.0,
                source: RssiSource::ConnectedLink,
                at: Timestamp(0.1),
            },
            Point2::new(20.0, 0.0),
        );

        let s = t.snapshot(Timestamp(0.2));
        assert_eq!(s.rssi_dbm, Some(-55.0), "display follows the local reading");
        assert_eq!(s.total_samples, 1, "peer samples stay out of the window");
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
