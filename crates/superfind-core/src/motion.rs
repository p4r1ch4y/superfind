//! Pedestrian dead reckoning: where the *user* is.
//!
//! This is the quiet prerequisite for everything interesting. A single
//! omnidirectional antenna cannot produce a bearing from one sample — the
//! aperture that makes direction recoverable is the user's own movement. So the
//! filter needs to know where the phone was when each reading was taken, and
//! that comes from the step counter and the compass, not from GPS, which is
//! useless at the scale of "which corner of this room".
//!
//! Dead reckoning drifts, and the drift is tracked explicitly rather than
//! ignored: [`DeadReckoner::position_sigma`] grows with distance walked, and the
//! bearing estimate uses it to widen its own error bars honestly.

use crate::geom::{wrap_angle, Point2};
use crate::time::Timestamp;

/// A recorded point on the user's path, with the signal seen there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrailPoint {
    pub position: Point2,
    pub heading: f64,
    pub at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub position: Point2,
    /// Compass heading the user is facing, radians clockwise from north.
    pub heading: f64,
}

impl Default for Pose {
    fn default() -> Self {
        Pose {
            position: Point2::ORIGIN,
            heading: 0.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StrideModel {
    /// Metres per step. 0.72 m is a reasonable adult default; a real build
    /// should refine it from height or from a GPS-calibrated walk outdoors.
    pub length_m: f64,
    /// Fraction of distance travelled that accumulates as position error.
    /// Roughly 5% is typical for step-and-heading dead reckoning indoors.
    pub drift_rate: f64,
    /// Floor on position uncertainty, metres. Even standing still, the origin
    /// is not known perfectly.
    pub base_sigma_m: f64,
}

impl Default for StrideModel {
    fn default() -> Self {
        StrideModel {
            length_m: 0.72,
            drift_rate: 0.05,
            base_sigma_m: 0.5,
        }
    }
}

/// Tracks the user's pose and the path they have walked.
#[derive(Debug, Clone)]
pub struct DeadReckoner {
    stride: StrideModel,
    pose: Pose,
    steps: u32,
    distance_m: f64,
    trail: Vec<TrailPoint>,
    max_trail: usize,
}

impl Default for DeadReckoner {
    fn default() -> Self {
        DeadReckoner::new(StrideModel::default())
    }
}

impl DeadReckoner {
    pub fn new(stride: StrideModel) -> Self {
        DeadReckoner {
            stride,
            pose: Pose::default(),
            steps: 0,
            distance_m: 0.0,
            trail: Vec::new(),
            // Enough to draw a long walk; older points are dropped from the
            // front so a session left running does not grow without bound.
            max_trail: 4096,
        }
    }

    pub fn pose(&self) -> Pose {
        self.pose
    }

    pub fn position(&self) -> Point2 {
        self.pose.position
    }

    pub fn heading(&self) -> f64 {
        self.pose.heading
    }

    pub fn steps(&self) -> u32 {
        self.steps
    }

    pub fn distance_walked(&self) -> f64 {
        self.distance_m
    }

    pub fn trail(&self) -> &[TrailPoint] {
        &self.trail
    }

    /// Update the compass heading without moving. Recording a trail point here
    /// too is what lets a user map a room by turning on the spot.
    pub fn set_heading(&mut self, heading_rad: f64, at: Timestamp) {
        self.pose.heading = wrap_angle(heading_rad);
        self.push_trail(at);
    }

    /// One detected step, in the direction currently faced.
    pub fn step(&mut self, at: Timestamp) {
        self.advance(self.stride.length_m, at);
    }

    /// A step of known length — for platforms whose pedometer reports stride,
    /// or for replaying a measured trace.
    pub fn step_of(&mut self, length_m: f64, at: Timestamp) {
        self.advance(length_m.max(0.0), at);
    }

    /// Teleport the user, resetting accumulated drift. For an external fix
    /// (a GNSS lock outdoors, a scanned anchor QR indoors) that is better than
    /// anything dead reckoning can offer.
    pub fn reset_to(&mut self, pose: Pose, at: Timestamp) {
        self.pose = Pose {
            position: pose.position,
            heading: wrap_angle(pose.heading),
        };
        self.distance_m = 0.0;
        self.push_trail(at);
    }

    fn advance(&mut self, length_m: f64, at: Timestamp) {
        self.pose.position = self.pose.position.offset(self.pose.heading, length_m);
        self.distance_m += length_m;
        self.steps += 1;
        self.push_trail(at);
    }

    fn push_trail(&mut self, at: Timestamp) {
        if self.trail.len() >= self.max_trail {
            self.trail.remove(0);
        }
        self.trail.push(TrailPoint {
            position: self.pose.position,
            heading: self.pose.heading,
            at,
        });
    }

    /// Current one-sigma position uncertainty, metres. Grows with distance
    /// walked, because that is what dead reckoning error actually does.
    pub fn position_sigma(&self) -> f64 {
        self.stride.base_sigma_m + self.stride.drift_rate * self.distance_m
    }

    /// How much of the compass the user has covered, as a fraction in `0..=1`.
    ///
    /// This is the honest measure of whether a synthetic-aperture bearing means
    /// anything: sweep 20 degrees and the answer is noise regardless of how many
    /// samples were taken.
    pub fn heading_coverage(&self, sectors: usize) -> f64 {
        if sectors == 0 || self.trail.is_empty() {
            return 0.0;
        }
        let mut seen = vec![false; sectors];
        for point in &self.trail {
            seen[crate::bearing::sector_of(point.heading, sectors)] = true;
        }
        seen.iter().filter(|s| **s).count() as f64 / sectors as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{to_degrees, to_radians};

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn walking_north_increases_northing() {
        let mut dr = DeadReckoner::default();
        dr.set_heading(0.0, Timestamp::ZERO);
        for i in 0..10 {
            dr.step(Timestamp(i as f64));
        }
        let p = dr.position();
        assert!(close(p.x, 0.0, 1e-9), "should not drift east");
        assert!(close(p.y, 7.2, 1e-9), "ten 0.72 m steps north");
        assert_eq!(dr.steps(), 10);
    }

    #[test]
    fn walking_east_increases_easting() {
        let mut dr = DeadReckoner::default();
        dr.set_heading(to_radians(90.0), Timestamp::ZERO);
        dr.step_of(5.0, Timestamp(1.0));
        let p = dr.position();
        assert!(close(p.x, 5.0, 1e-9));
        assert!(close(p.y, 0.0, 1e-9));
    }

    #[test]
    fn a_square_walk_returns_to_the_origin() {
        let mut dr = DeadReckoner::default();
        for deg in [0.0, 90.0, 180.0, 270.0] {
            dr.set_heading(to_radians(deg), Timestamp::ZERO);
            dr.step_of(4.0, Timestamp::ZERO);
        }
        let p = dr.position();
        assert!(close(p.x, 0.0, 1e-9) && close(p.y, 0.0, 1e-9), "got {p:?}");
        // But the odometer knows it walked 16 m, and uncertainty reflects that.
        assert!(close(dr.distance_walked(), 16.0, 1e-9));
        assert!(dr.position_sigma() > StrideModel::default().base_sigma_m);
    }

    #[test]
    fn uncertainty_grows_with_distance_not_with_time() {
        let mut dr = DeadReckoner::default();
        let standing_still = dr.position_sigma();
        for i in 0..100 {
            dr.set_heading(0.0, Timestamp(i as f64));
        }
        assert!(close(dr.position_sigma(), standing_still, 1e-12));
        dr.step_of(20.0, Timestamp(200.0));
        assert!(dr.position_sigma() > standing_still);
    }

    #[test]
    fn reset_clears_accumulated_drift() {
        let mut dr = DeadReckoner::default();
        dr.step_of(50.0, Timestamp::ZERO);
        assert!(dr.position_sigma() > 2.0);
        dr.reset_to(Pose::default(), Timestamp(1.0));
        assert!(close(dr.position_sigma(), StrideModel::default().base_sigma_m, 1e-12));
    }

    #[test]
    fn heading_is_wrapped() {
        let mut dr = DeadReckoner::default();
        dr.set_heading(to_radians(450.0), Timestamp::ZERO);
        assert!(close(to_degrees(dr.heading()), 90.0, 1e-9));
    }

    #[test]
    fn coverage_reflects_how_much_was_swept() {
        let mut dr = DeadReckoner::default();
        // Facing one way only: one sector out of sixteen.
        dr.set_heading(0.0, Timestamp::ZERO);
        assert!(close(dr.heading_coverage(16), 1.0 / 16.0, 1e-9));

        // A full turn touches every sector.
        for deg in (0..360).step_by(5) {
            dr.set_heading(to_radians(deg as f64), Timestamp::ZERO);
        }
        assert!(close(dr.heading_coverage(16), 1.0, 1e-9));
    }

    #[test]
    fn trail_is_bounded() {
        let mut dr = DeadReckoner::new(StrideModel::default());
        for i in 0..5000 {
            dr.step(Timestamp(i as f64));
        }
        assert!(dr.trail().len() <= 4096);
        // The most recent point is always retained.
        assert_eq!(dr.trail().last().unwrap().position, dr.position());
    }
}
