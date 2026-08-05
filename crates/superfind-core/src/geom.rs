//! Planar geometry in a local ENU frame.
//!
//! The session origin is wherever the user was when tracking began. `+x` is
//! east, `+y` is north, and bearings are compass-style: radians clockwise from
//! north, which is what a phone's rotation vector reports and what the radar UI
//! draws. Converting once, here, keeps the trigonometric sign errors in a single
//! tested file instead of scattered through the filter.

use core::f64::consts::{PI, TAU};

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point2 {
    pub x: f64,
    pub y: f64,
}

impl Point2 {
    pub const ORIGIN: Point2 = Point2 { x: 0.0, y: 0.0 };

    #[inline]
    pub fn new(x: f64, y: f64) -> Self {
        Point2 { x, y }
    }

    /// `hypot` rather than `sqrt(dx*dx + dy*dy)`: it will not overflow or lose
    /// precision on the extreme values a diverged filter can briefly produce.
    #[inline]
    pub fn distance_to(self, other: Point2) -> f64 {
        (other.x - self.x).hypot(other.y - self.y)
    }

    /// Compass bearing from `self` to `other`, radians clockwise from north.
    #[inline]
    pub fn bearing_to(self, other: Point2) -> f64 {
        wrap_angle((other.x - self.x).atan2(other.y - self.y))
    }

    /// The point `distance` away along compass `bearing`.
    #[inline]
    pub fn offset(self, bearing: f64, distance: f64) -> Point2 {
        Point2 {
            x: self.x + distance * bearing.sin(),
            y: self.y + distance * bearing.cos(),
        }
    }
}

/// Normalise an angle to `(-PI, PI]`.
#[inline]
pub fn wrap_angle(a: f64) -> f64 {
    let mut a = a % TAU;
    if a > PI {
        a -= TAU;
    } else if a <= -PI {
        a += TAU;
    }
    a
}

/// Signed smallest rotation carrying `from` onto `to`, in `(-PI, PI]`.
#[inline]
pub fn angle_diff(from: f64, to: f64) -> f64 {
    wrap_angle(to - from)
}

/// Normalise to `[0, TAU)`, which is what a UI wants for a compass readout.
#[inline]
pub fn wrap_positive(a: f64) -> f64 {
    let a = a % TAU;
    if a < 0.0 {
        a + TAU
    } else {
        a
    }
}

#[inline]
pub fn to_degrees(rad: f64) -> f64 {
    rad * 180.0 / PI
}

#[inline]
pub fn to_radians(deg: f64) -> f64 {
    deg * PI / 180.0
}

/// A 2x2 symmetric covariance matrix.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Covariance2 {
    pub xx: f64,
    pub xy: f64,
    pub yy: f64,
}

/// The 95% confidence ellipse of a bivariate normal. Chi-square with 2 degrees
/// of freedom: the 0.95 quantile is 5.991, so the axes are `sqrt(5.991 * λ)`.
const CHI2_95_DOF2: f64 = 5.991_464_547_107_98;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ellipse {
    pub centre: Point2,
    /// Longer semi-axis, metres.
    pub semi_major: f64,
    /// Shorter semi-axis, metres.
    pub semi_minor: f64,
    /// Compass bearing of the major axis, radians clockwise from north.
    pub orientation: f64,
}

impl Ellipse {
    pub fn area(&self) -> f64 {
        PI * self.semi_major * self.semi_minor
    }
}

impl Covariance2 {
    /// Eigen-decomposition of the 2x2, scaled to a 95% confidence ellipse.
    ///
    /// Closed form rather than a general solver: for 2x2 the eigenvalues are the
    /// roots of the characteristic quadratic, and doing it by hand avoids both a
    /// dependency and the iteration-count nondeterminism a general routine would
    /// introduce into the tests.
    pub fn confidence_ellipse(&self, centre: Point2) -> Ellipse {
        let trace = self.xx + self.yy;
        let det = self.xx * self.yy - self.xy * self.xy;
        // Clamped at zero: floating-point error on a near-degenerate covariance
        // can push the discriminant very slightly negative.
        let disc = ((trace * trace) / 4.0 - det).max(0.0).sqrt();
        let l1 = (trace / 2.0 + disc).max(0.0);
        let l2 = (trace / 2.0 - disc).max(0.0);

        // Eigenvector for the larger eigenvalue, in (x=east, y=north).
        let (ex, ey) = if self.xy.abs() > 1e-12 {
            (self.xy, l1 - self.xx)
        } else if self.xx >= self.yy {
            (1.0, 0.0)
        } else {
            (0.0, 1.0)
        };

        Ellipse {
            centre,
            semi_major: (CHI2_95_DOF2 * l1).sqrt(),
            semi_minor: (CHI2_95_DOF2 * l2).sqrt(),
            orientation: wrap_angle(ex.atan2(ey)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn bearings_follow_the_compass() {
        let o = Point2::ORIGIN;
        assert!(close(to_degrees(o.bearing_to(Point2::new(0.0, 1.0))), 0.0, 1e-9)); // north
        assert!(close(to_degrees(o.bearing_to(Point2::new(1.0, 0.0))), 90.0, 1e-9)); // east
        assert!(close(to_degrees(o.bearing_to(Point2::new(0.0, -1.0))), 180.0, 1e-9)); // south
        assert!(close(to_degrees(o.bearing_to(Point2::new(-1.0, 0.0))), -90.0, 1e-9)); // west
    }

    #[test]
    fn offset_inverts_bearing_and_distance() {
        let start = Point2::new(3.0, -4.0);
        for deg in [0.0, 37.0, 90.0, 175.0, -120.0, 359.0] {
            let b = to_radians(deg);
            let p = start.offset(b, 12.5);
            assert!(close(start.distance_to(p), 12.5, 1e-9), "distance at {deg}");
            assert!(
                close(angle_diff(start.bearing_to(p), b), 0.0, 1e-9),
                "bearing at {deg}"
            );
        }
    }

    #[test]
    fn wrap_angle_lands_in_half_open_interval() {
        for k in -8..=8 {
            for base in [0.0, 0.7, -0.7, 3.0, -3.0] {
                let a = wrap_angle(base + k as f64 * TAU);
                assert!(a > -PI - 1e-12 && a <= PI + 1e-12, "{a} out of range");
                assert!(close(a, wrap_angle(base), 1e-9));
            }
        }
    }

    #[test]
    fn angle_diff_takes_the_short_way_round() {
        assert!(close(to_degrees(angle_diff(to_radians(350.0), to_radians(10.0))), 20.0, 1e-9));
        assert!(close(to_degrees(angle_diff(to_radians(10.0), to_radians(350.0))), -20.0, 1e-9));
    }

    #[test]
    fn wrap_positive_is_a_compass_reading() {
        assert!(close(to_degrees(wrap_positive(to_radians(-90.0))), 270.0, 1e-9));
        assert!(close(to_degrees(wrap_positive(to_radians(90.0))), 90.0, 1e-9));
    }

    #[test]
    fn isotropic_covariance_gives_a_circle() {
        let cov = Covariance2 { xx: 4.0, xy: 0.0, yy: 4.0 };
        let e = cov.confidence_ellipse(Point2::ORIGIN);
        let expected = (CHI2_95_DOF2 * 4.0).sqrt();
        assert!(close(e.semi_major, expected, 1e-9));
        assert!(close(e.semi_minor, expected, 1e-9));
    }

    #[test]
    fn elongated_covariance_orients_along_its_spread() {
        // Wide in x (east), narrow in y (north): the major axis points east,
        // which on a compass is 90 degrees.
        let cov = Covariance2 { xx: 25.0, xy: 0.0, yy: 1.0 };
        let e = cov.confidence_ellipse(Point2::ORIGIN);
        assert!(e.semi_major > e.semi_minor);
        assert!(close(to_degrees(e.orientation).abs(), 90.0, 1e-6));
    }

    #[test]
    fn correlated_covariance_orients_diagonally() {
        let cov = Covariance2 { xx: 10.0, xy: 9.0, yy: 10.0 };
        let e = cov.confidence_ellipse(Point2::ORIGIN);
        // Equal variances with positive correlation: spread along the x=y
        // diagonal, which is compass 45 degrees.
        assert!(close(to_degrees(e.orientation).abs(), 45.0, 1e-6));
        assert!(e.semi_major > e.semi_minor);
    }

    #[test]
    fn degenerate_covariance_does_not_produce_nan() {
        let e = Covariance2::default().confidence_ellipse(Point2::ORIGIN);
        assert!(e.semi_major.is_finite() && e.semi_minor.is_finite());
        assert_eq!(e.semi_major, 0.0);
    }
}
