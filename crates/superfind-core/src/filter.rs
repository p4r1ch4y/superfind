//! Particle filter over the target's position.
//!
//! Why particles rather than a Kalman filter: the RSSI likelihood is wildly
//! non-Gaussian in position space. A single signal-strength reading says "the
//! device is somewhere on an annulus around me", and two readings from different
//! places say "somewhere near one of the two intersections". That posterior is
//! ring-shaped and often bimodal, and an extended Kalman filter would average
//! the two intersections and confidently point at the empty space between them.
//! Particles represent it directly, and the bimodality resolves itself as the
//! user walks.
//!
//! The state is just the target's position: a lost phone does not move. Process
//! noise is therefore small and exists to model *our* error, not the device's
//! motion. The mechanism that actually moves information into the filter is the
//! user walking, which is why [`crate::motion`] is a prerequisite rather than a
//! nicety.
//!
//! Weights are kept in log space and normalised with the log-sum-exp shift.
//! Naive multiplication of Gaussian densities underflows to zero within a few
//! hundred updates, at which point every particle has weight zero and the filter
//! silently reports its prior forever.

use crate::geom::{wrap_angle, Covariance2, Ellipse, Point2};
use crate::measurement::{Measurement, RssiSource};
use crate::pathloss::PathLoss;
use crate::rng::Rng;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FilterConfig {
    pub particles: usize,
    /// Radius of the initial uniform disc, metres. Should comfortably exceed
    /// the radio's usable range so the truth is inside the prior.
    pub init_radius_m: f64,
    /// Per-second random walk applied to particles, metres. Small: this models
    /// our own error, not the device moving.
    pub process_noise_m_per_s: f64,
    /// Extra jitter added after a resample, metres. Without it, repeated
    /// resampling collapses every particle onto a handful of identical points
    /// and the filter stops being able to change its mind.
    pub roughening_m: f64,
    /// Resample when effective sample size falls below this fraction of the
    /// population.
    pub resample_threshold: f64,
    /// Below this effective fraction, resampling alone cannot save the filter —
    /// duplicating a handful of survivors does not restore the shape of the
    /// posterior. Inject fresh measurement-consistent particles instead.
    pub rejuvenate_threshold: f64,
    /// Fraction of the population replaced when rejuvenating.
    pub rejuvenate_fraction: f64,
    /// A measurement no particle can explain within this many standard
    /// deviations counts as a bad fit.
    pub bad_fit_sigmas: f64,
    /// Consecutive bad fits before the filter declares itself diverged.
    pub bad_fits_before_diverged: u32,
}

impl Default for FilterConfig {
    fn default() -> Self {
        FilterConfig {
            particles: 2048,
            init_radius_m: 60.0,
            process_noise_m_per_s: 0.05,
            roughening_m: 0.15,
            resample_threshold: 0.5,
            rejuvenate_threshold: 0.05,
            rejuvenate_fraction: 0.3,
            bad_fit_sigmas: 6.0,
            bad_fits_before_diverged: 8,
        }
    }
}

/// The filter's answer, with everything the UI needs to be honest about it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fix {
    /// Weighted mean of the posterior, in the session's local frame.
    pub position: Point2,
    /// 95% confidence ellipse. Draw this, not a dot.
    pub ellipse: Ellipse,
    /// Distance from the user to the estimate, metres.
    pub distance_m: f64,
    /// Compass bearing from the user to the estimate, radians clockwise from
    /// north.
    pub bearing_rad: f64,
    /// Circular standard deviation of the particles' bearings. Large when the
    /// posterior is still an annulus around the user, which is the honest state
    /// of affairs before they have walked anywhere.
    pub bearing_sigma_rad: f64,
    /// `0..=1`, from the ellipse size relative to the range. Knowing a device is
    /// 20 m away give or take 15 m is not the same as 2 m give or take 0.5 m,
    /// and a single number should reflect that.
    pub confidence: f64,
    /// Effective sample size as a fraction of the population. A diagnostic: if
    /// this sits near zero the model and the measurements disagree.
    pub effective_fraction: f64,
    pub observations: u32,
}

#[derive(Debug, Clone)]
pub struct ParticleFilter {
    config: FilterConfig,
    particles: Vec<Point2>,
    log_weights: Vec<f64>,
    rng: Rng,
    observations: u32,
    /// Set when the weights degenerate completely, so the caller can surface a
    /// "lost the trail" state instead of a stale answer.
    diverged: bool,
    /// Consecutive measurements that no particle could explain.
    consecutive_bad_fits: u32,
}

impl ParticleFilter {
    /// Initialise a uniform disc of particles centred on the user.
    pub fn new(config: FilterConfig, centre: Point2, seed: u64) -> Self {
        let mut rng = Rng::seeded(seed);
        let n = config.particles.max(1);
        let particles = (0..n)
            .map(|_| sample_disc(&mut rng, centre, config.init_radius_m))
            .collect();
        ParticleFilter {
            config,
            particles,
            log_weights: vec![0.0; n],
            rng,
            observations: 0,
            diverged: false,
            consecutive_bad_fits: 0,
        }
    }

    pub fn config(&self) -> FilterConfig {
        self.config
    }

    pub fn observations(&self) -> u32 {
        self.observations
    }

    pub fn diverged(&self) -> bool {
        self.diverged
    }

    pub fn particles(&self) -> &[Point2] {
        &self.particles
    }

    /// Restart from a uniform disc around `centre`, discarding all evidence.
    pub fn reset(&mut self, centre: Point2) {
        for p in self.particles.iter_mut() {
            *p = sample_disc(&mut self.rng, centre, self.config.init_radius_m);
        }
        self.log_weights.iter_mut().for_each(|w| *w = 0.0);
        self.observations = 0;
        self.diverged = false;
        self.consecutive_bad_fits = 0;
    }

    /// Advance time. Diffuses the particles slightly so the filter retains the
    /// ability to revise a wrong answer.
    pub fn predict(&mut self, dt_s: f64) {
        if dt_s <= 0.0 {
            return;
        }
        let sigma = self.config.process_noise_m_per_s * dt_s;
        if sigma <= 0.0 {
            return;
        }
        for p in self.particles.iter_mut() {
            p.x += self.rng.normal_with(0.0, sigma);
            p.y += self.rng.normal_with(0.0, sigma);
        }
    }

    /// Fold in one observation, taken from `user` at the time it was made.
    ///
    /// Implausible measurements are dropped rather than fitted — a `-127` dBm
    /// "no reading" sentinel treated as a real sample would push every particle
    /// to the horizon.
    pub fn update(&mut self, m: &Measurement, user: Point2, model: &PathLoss) -> bool {
        if !m.is_plausible() {
            return false;
        }

        // A diffuse prior and a precise measurement barely overlap. Reweighting
        // a 60 m disc against a 10 cm UWB range leaves roughly five particles
        // with non-zero weight, and five points cannot represent a ring — they
        // cluster by chance and the filter reports a confident bearing it has
        // no business having. So the first measurement does not reweight the
        // prior, it *replaces* it, by sampling the exact single-measurement
        // posterior.
        if self.observations == 0 {
            self.seed_from(m, user, model);
            self.observations += 1;
            return true;
        }

        let worst_z = match *m {
            Measurement::Rssi { dbm, source, .. } => self.update_rssi(dbm, source, user, model),
            Measurement::Range { metres, source, .. } => {
                self.update_range(metres, source.sigma_m(), user)
            }
            Measurement::Angle {
                bearing_rad,
                sigma_rad,
                ..
            } => self.update_angle(bearing_rad, sigma_rad, user),
        };

        self.observations += 1;
        self.note_fit(worst_z);
        self.maybe_resample(m, user, model);
        true
    }

    /// Each updater returns the *best* fit achieved by any particle, in standard
    /// deviations. That number is the divergence signal: if not one particle out
    /// of thousands can explain the measurement, the problem is the model or the
    /// data, not the sampling.
    fn update_rssi(&mut self, dbm: f64, source: RssiSource, user: Point2, model: &PathLoss) -> f64 {
        let sigma = source.sigma_db();
        let mut best_z = f64::INFINITY;
        for (p, w) in self.particles.iter().zip(self.log_weights.iter_mut()) {
            let distance = user.distance_to(*p);
            *w += model.log_likelihood(dbm, distance, sigma);
            let z = ((dbm - model.expected_rssi(distance)) / sigma).abs();
            best_z = best_z.min(z);
        }
        best_z
    }

    fn update_range(&mut self, metres: f64, sigma_m: f64, user: Point2) -> f64 {
        let sigma = sigma_m.max(0.01);
        let mut best_z = f64::INFINITY;
        for (p, w) in self.particles.iter().zip(self.log_weights.iter_mut()) {
            let residual = (user.distance_to(*p) - metres) / sigma;
            *w += -0.5 * residual * residual;
            best_z = best_z.min(residual.abs());
        }
        best_z
    }

    fn update_angle(&mut self, bearing_rad: f64, sigma_rad: f64, user: Point2) -> f64 {
        let sigma = sigma_rad.max(1e-3);
        let mut best_z = f64::INFINITY;
        for (p, w) in self.particles.iter().zip(self.log_weights.iter_mut()) {
            // A particle sitting exactly on the user has no defined bearing;
            // leave its weight alone rather than inventing one.
            if user.distance_to(*p) < 1e-9 {
                continue;
            }
            let residual = wrap_angle(user.bearing_to(*p) - bearing_rad) / sigma;
            *w += -0.5 * residual * residual;
            best_z = best_z.min(residual.abs());
        }
        best_z
    }

    fn note_fit(&mut self, best_z: f64) {
        if best_z.is_finite() && best_z > self.config.bad_fit_sigmas {
            self.consecutive_bad_fits += 1;
            if self.consecutive_bad_fits >= self.config.bad_fits_before_diverged {
                self.diverged = true;
            }
        } else {
            self.consecutive_bad_fits = 0;
        }
    }

    /// Draw a position consistent with this measurement alone, treating the
    /// dimensions it does not constrain as uniform. A range says "somewhere on
    /// this ring"; an angle says "somewhere along this line".
    fn propose(&mut self, m: &Measurement, user: Point2, model: &PathLoss) -> Point2 {
        match *m {
            Measurement::Rssi { dbm, source, .. } => {
                // Perturb in dB, then invert — the noise is Gaussian in dB, not
                // in metres, so this gives the annulus its correct thickness.
                let noisy = dbm + self.rng.normal_with(0.0, source.sigma_db());
                let distance = model.distance(noisy);
                let bearing = self.rng.uniform() * core::f64::consts::TAU;
                user.offset(bearing, distance)
            }
            Measurement::Range { metres, source, .. } => {
                let distance = self
                    .rng
                    .normal_with(metres, source.sigma_m())
                    .max(0.0);
                let bearing = self.rng.uniform() * core::f64::consts::TAU;
                user.offset(bearing, distance)
            }
            Measurement::Angle {
                bearing_rad,
                sigma_rad,
                ..
            } => {
                let bearing = self.rng.normal_with(bearing_rad, sigma_rad);
                // Unconstrained in range: uniform over the disc's area.
                let distance = self.config.init_radius_m * self.rng.uniform().sqrt();
                user.offset(bearing, distance)
            }
        }
    }

    /// Replace the entire population with draws from one measurement.
    fn seed_from(&mut self, m: &Measurement, user: Point2, model: &PathLoss) {
        for i in 0..self.particles.len() {
            self.particles[i] = self.propose(m, user, model);
        }
        self.log_weights.iter_mut().for_each(|w| *w = 0.0);
    }

    /// Normalised weights, via the log-sum-exp shift. Returns `None` if the
    /// weights have degenerated beyond recovery.
    fn normalised_weights(&self) -> Option<Vec<f64>> {
        let max = self
            .log_weights
            .iter()
            .copied()
            .fold(f64::NEG_INFINITY, f64::max);
        if !max.is_finite() {
            return None;
        }
        let mut w: Vec<f64> = self.log_weights.iter().map(|l| (l - max).exp()).collect();
        let total: f64 = w.iter().sum();
        // Both guards matter: a zero total means every weight underflowed, and
        // a non-finite total means one went to infinity. NaN fails `is_finite`.
        if !total.is_finite() || total <= 0.0 {
            return None;
        }
        w.iter_mut().for_each(|x| *x /= total);
        Some(w)
    }

    /// Effective sample size as a fraction of the population, `0..=1`.
    pub fn effective_fraction(&self) -> f64 {
        match self.normalised_weights() {
            None => 0.0,
            Some(w) => {
                let sum_sq: f64 = w.iter().map(|x| x * x).sum();
                if sum_sq <= 0.0 {
                    0.0
                } else {
                    (1.0 / sum_sq) / self.particles.len() as f64
                }
            }
        }
    }

    fn maybe_resample(&mut self, m: &Measurement, user: Point2, model: &PathLoss) {
        let Some(weights) = self.normalised_weights() else {
            // Total collapse: every particle is impossible under the model. Keep
            // the positions, flatten the weights, and flag it. Silently carrying
            // on with zeroed weights would report the prior as if it were a fix.
            self.log_weights.iter_mut().for_each(|w| *w = 0.0);
            self.diverged = true;
            return;
        };

        let sum_sq: f64 = weights.iter().map(|x| x * x).sum();
        let ess_fraction = (1.0 / sum_sq) / self.particles.len() as f64;
        if ess_fraction >= self.config.resample_threshold {
            return;
        }

        self.resample(&weights);

        // Resampling redistributes what survived; it cannot recreate what did
        // not. When the effective sample size has fallen this far, the surviving
        // particles are too few to describe the posterior's shape, so top the
        // population back up with fresh draws from the measurement that caused
        // the collapse. They are consistent with the newest evidence by
        // construction, which is the most that can be said for any particle.
        if ess_fraction < self.config.rejuvenate_threshold {
            self.rejuvenate(m, user, model);
        }
    }

    fn rejuvenate(&mut self, m: &Measurement, user: Point2, model: &PathLoss) {
        let n = self.particles.len();
        let replace = ((n as f64) * self.config.rejuvenate_fraction.clamp(0.0, 1.0)) as usize;
        for _ in 0..replace {
            // Replace at random rather than the first k, so rejuvenation does
            // not systematically evict whichever particles resampling happened
            // to place at the front.
            let victim = (self.rng.uniform() * n as f64) as usize % n;
            self.particles[victim] = self.propose(m, user, model);
        }
    }

    /// Systematic resampling: one uniform draw, then evenly spaced strata.
    /// Lower variance than multinomial and O(n) rather than O(n log n).
    fn resample(&mut self, weights: &[f64]) {
        let n = self.particles.len();
        let step = 1.0 / n as f64;
        let mut position = self.rng.uniform() * step;
        let mut cumulative = weights[0];
        let mut source = 0usize;

        let mut resampled = Vec::with_capacity(n);
        for _ in 0..n {
            while position > cumulative && source + 1 < n {
                source += 1;
                cumulative += weights[source];
            }
            resampled.push(self.particles[source]);
            position += step;
        }

        let rough = self.config.roughening_m;
        for p in resampled.iter_mut() {
            p.x += self.rng.normal_with(0.0, rough);
            p.y += self.rng.normal_with(0.0, rough);
        }

        self.particles = resampled;
        self.log_weights.iter_mut().for_each(|w| *w = 0.0);
    }

    /// Current best estimate, or `None` before any evidence has arrived.
    ///
    /// Returning the untouched prior as a "fix" would be the single most
    /// misleading thing this crate could do, so it does not.
    pub fn fix(&self, user: Point2) -> Option<Fix> {
        if self.observations == 0 {
            return None;
        }
        let weights = self.normalised_weights()?;

        let mut mean = Point2::ORIGIN;
        for (p, w) in self.particles.iter().zip(&weights) {
            mean.x += p.x * w;
            mean.y += p.y * w;
        }

        let mut cov = Covariance2::default();
        for (p, w) in self.particles.iter().zip(&weights) {
            let dx = p.x - mean.x;
            let dy = p.y - mean.y;
            cov.xx += w * dx * dx;
            cov.xy += w * dx * dy;
            cov.yy += w * dy * dy;
        }

        // Circular statistics on the bearings, weighted. The resultant length
        // gives an angular spread that stays meaningful across the north wrap,
        // which a plain standard deviation would not.
        let mut sin_sum = 0.0;
        let mut cos_sum = 0.0;
        let mut angular_weight = 0.0;
        for (p, w) in self.particles.iter().zip(&weights) {
            if user.distance_to(*p) < 1e-9 {
                continue;
            }
            let b = user.bearing_to(*p);
            sin_sum += w * b.sin();
            cos_sum += w * b.cos();
            angular_weight += w;
        }

        let (bearing_rad, bearing_sigma_rad) = if angular_weight > 0.0 {
            let r = (sin_sum.hypot(cos_sum) / angular_weight).clamp(1e-9, 1.0);
            (
                wrap_angle(sin_sum.atan2(cos_sum)),
                (-2.0 * r.ln()).sqrt(),
            )
        } else {
            (0.0, core::f64::consts::PI)
        };

        let ellipse = cov.confidence_ellipse(mean);
        let distance_m = user.distance_to(mean);

        // Precision relative to range: a 1 m ellipse means something different
        // at 2 m than at 40 m.
        let relative = ellipse.semi_major / distance_m.max(1.0);
        let confidence = (1.0 / (1.0 + relative)).clamp(0.0, 1.0);

        Some(Fix {
            position: mean,
            ellipse,
            distance_m,
            bearing_rad,
            bearing_sigma_rad,
            confidence,
            effective_fraction: (1.0 / weights.iter().map(|x| x * x).sum::<f64>())
                / self.particles.len() as f64,
            observations: self.observations,
        })
    }
}

/// Uniform over the *area* of a disc. `sqrt` on the radius is essential —
/// sampling radius uniformly clusters particles at the centre and gives the
/// prior a bias towards the user's own position.
fn sample_disc(rng: &mut Rng, centre: Point2, radius: f64) -> Point2 {
    let theta = rng.uniform() * core::f64::consts::TAU;
    let r = radius * rng.uniform().sqrt();
    Point2::new(centre.x + r * theta.cos(), centre.y + r * theta.sin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{angle_diff, to_degrees};
    use crate::measurement::RangeSource;
    use crate::time::Timestamp;

    fn rssi_at(model: &PathLoss, user: Point2, truth: Point2, rng: &mut Rng, sigma: f64) -> f64 {
        model.expected_rssi(user.distance_to(truth)) + rng.normal_with(0.0, sigma)
    }

    #[test]
    fn no_fix_before_any_evidence() {
        let f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 1);
        assert!(f.fix(Point2::ORIGIN).is_none());
    }

    #[test]
    fn initial_particles_fill_the_disc_uniformly_by_area() {
        let config = FilterConfig {
            particles: 40_000,
            init_radius_m: 10.0,
            ..Default::default()
        };
        let f = ParticleFilter::new(config, Point2::ORIGIN, 3);
        // For an area-uniform disc, half the points lie inside r/sqrt(2).
        let inner = 10.0 / 2f64.sqrt();
        let count = f
            .particles()
            .iter()
            .filter(|p| Point2::ORIGIN.distance_to(**p) < inner)
            .count();
        let fraction = count as f64 / 40_000.0;
        assert!(
            (fraction - 0.5).abs() < 0.02,
            "particles are not area-uniform: {fraction}"
        );
        assert!(f
            .particles()
            .iter()
            .all(|p| Point2::ORIGIN.distance_to(*p) <= 10.0 + 1e-9));
    }

    #[test]
    fn a_single_range_produces_an_annulus_not_a_point() {
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 5);
        let m = Measurement::Range {
            metres: 8.0,
            source: RangeSource::Uwb,
            at: Timestamp::ZERO,
        };
        assert!(f.update(&m, Point2::ORIGIN, &PathLoss::default()));

        let fix = f.fix(Point2::ORIGIN).unwrap();
        // The mean of a ring is its centre, so the distance estimate is near
        // zero — and the ellipse must be huge, and the bearing meaningless.
        assert!(
            fix.ellipse.semi_major > 5.0,
            "a single range should stay ambiguous, got {}",
            fix.ellipse.semi_major
        );
        assert!(
            fix.bearing_sigma_rad > 1.0,
            "bearing should be near-useless from one range, got {}",
            fix.bearing_sigma_rad
        );
        assert!(fix.confidence < 0.4);
    }

    #[test]
    fn ranges_from_three_places_trilaterate() {
        let truth = Point2::new(6.0, -3.0);
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 7);
        let model = PathLoss::default();

        for user in [
            Point2::new(0.0, 0.0),
            Point2::new(10.0, 0.0),
            Point2::new(0.0, 10.0),
            Point2::new(10.0, 10.0),
        ] {
            for _ in 0..5 {
                let m = Measurement::Range {
                    metres: user.distance_to(truth),
                    source: RangeSource::Uwb,
                    at: Timestamp::ZERO,
                };
                f.update(&m, user, &model);
            }
        }

        let fix = f.fix(Point2::ORIGIN).unwrap();
        let error = fix.position.distance_to(truth);
        assert!(error < 1.0, "trilateration error was {error} m");
        assert!(fix.confidence > 0.6, "confidence was {}", fix.confidence);
    }

    #[test]
    fn walking_while_reading_rssi_converges_on_the_device() {
        let truth = Point2::new(12.0, 5.0);
        let model = PathLoss::default();
        let mut rng = Rng::seeded(99);
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 21);

        // Walk a short L-shaped path, reading signal all the way.
        let mut user = Point2::ORIGIN;
        for leg in 0..2 {
            for _ in 0..12 {
                user = if leg == 0 {
                    Point2::new(user.x + 1.0, user.y)
                } else {
                    Point2::new(user.x, user.y + 1.0)
                };
                f.predict(1.0);
                for _ in 0..4 {
                    let dbm = rssi_at(&model, user, truth, &mut rng, 4.0);
                    let m = Measurement::Rssi {
                        dbm,
                        source: RssiSource::ConnectedLink,
                        at: Timestamp::ZERO,
                    };
                    f.update(&m, user, &model);
                }
            }
        }

        let fix = f.fix(user).unwrap();
        let error = fix.position.distance_to(truth);
        assert!(
            error < 5.0,
            "RSSI-only error was {error} m from {:?}",
            fix.position
        );
        assert!(!f.diverged());

        // And the bearing it reports should actually point at the device.
        let true_bearing = user.bearing_to(truth);
        let bearing_error = to_degrees(angle_diff(fix.bearing_rad, true_bearing)).abs();
        assert!(bearing_error < 40.0, "bearing error was {bearing_error} deg");
    }

    #[test]
    fn uwb_beats_rssi_for_the_same_number_of_observations() {
        let truth = Point2::new(9.0, 4.0);
        let model = PathLoss::default();
        let path = [
            Point2::new(0.0, 0.0),
            Point2::new(4.0, 0.0),
            Point2::new(8.0, 0.0),
            Point2::new(8.0, 4.0),
        ];

        let mut rssi_filter = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 31);
        let mut uwb_filter = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 31);
        let mut rng = Rng::seeded(5);

        for user in path {
            for _ in 0..8 {
                rssi_filter.update(
                    &Measurement::Rssi {
                        dbm: rssi_at(&model, user, truth, &mut rng, 4.0),
                        source: RssiSource::ConnectedLink,
                        at: Timestamp::ZERO,
                    },
                    user,
                    &model,
                );
                uwb_filter.update(
                    &Measurement::Range {
                        metres: user.distance_to(truth),
                        source: RangeSource::Uwb,
                        at: Timestamp::ZERO,
                    },
                    user,
                    &model,
                );
            }
        }

        let rssi_error = rssi_filter.fix(path[3]).unwrap().position.distance_to(truth);
        let uwb_error = uwb_filter.fix(path[3]).unwrap().position.distance_to(truth);
        assert!(
            uwb_error < rssi_error,
            "UWB error {uwb_error} should beat RSSI error {rssi_error}"
        );
        assert!(uwb_error < 0.5, "UWB error was {uwb_error} m");
    }

    #[test]
    fn an_angle_measurement_collapses_the_annulus() {
        let truth = Point2::new(0.0, 8.0); // due north
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 41);
        let model = PathLoss::default();

        f.update(
            &Measurement::Range {
                metres: 8.0,
                source: RangeSource::Uwb,
                at: Timestamp::ZERO,
            },
            Point2::ORIGIN,
            &model,
        );
        let before = f.fix(Point2::ORIGIN).unwrap().bearing_sigma_rad;

        f.update(
            &Measurement::Angle {
                bearing_rad: 0.0,
                sigma_rad: 0.15,
                at: Timestamp::ZERO,
            },
            Point2::ORIGIN,
            &model,
        );
        let after = f.fix(Point2::ORIGIN).unwrap();

        assert!(
            after.bearing_sigma_rad < before / 2.0,
            "angle should sharpen the bearing: {before} -> {}",
            after.bearing_sigma_rad
        );
        assert!(after.position.distance_to(truth) < 2.0);
    }

    #[test]
    fn implausible_measurements_are_rejected_not_fitted() {
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 13);
        let model = PathLoss::default();
        let sentinel = Measurement::Rssi {
            dbm: -127.0,
            source: RssiSource::ConnectedLink,
            at: Timestamp::ZERO,
        };
        assert!(!f.update(&sentinel, Point2::ORIGIN, &model));
        assert_eq!(f.observations(), 0);
        assert!(f.fix(Point2::ORIGIN).is_none());
    }

    #[test]
    fn weights_survive_thousands_of_updates_without_underflowing() {
        // The failure this guards against: multiplying raw densities collapses
        // every weight to zero within a few hundred updates.
        let truth = Point2::new(5.0, 5.0);
        let model = PathLoss::default();
        let mut rng = Rng::seeded(77);
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 17);

        for i in 0..5000 {
            let user = Point2::new((i % 7) as f64, ((i / 7) % 7) as f64);
            f.update(
                &Measurement::Rssi {
                    dbm: rssi_at(&model, user, truth, &mut rng, 4.0),
                    source: RssiSource::ConnectedLink,
                    at: Timestamp::ZERO,
                },
                user,
                &model,
            );
        }

        assert!(!f.diverged(), "filter degenerated over a long session");
        let fix = f.fix(Point2::new(3.0, 3.0)).expect("should still have a fix");
        assert!(fix.position.x.is_finite() && fix.position.y.is_finite());
        assert!(fix.effective_fraction > 0.0);
        assert!(fix.position.distance_to(truth) < 4.0);
    }

    #[test]
    fn resampling_keeps_diversity() {
        let truth = Point2::new(4.0, 0.0);
        let model = PathLoss::default();
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 23);
        for _ in 0..200 {
            f.update(
                &Measurement::Range {
                    metres: Point2::ORIGIN.distance_to(truth),
                    source: RangeSource::Uwb,
                    at: Timestamp::ZERO,
                },
                Point2::ORIGIN,
                &model,
            );
        }
        // Distinct positions should remain — a collapsed filter cannot recover.
        let distinct = f
            .particles()
            .iter()
            .filter(|p| Point2::ORIGIN.distance_to(**p) > 0.0)
            .count();
        assert!(distinct > 100, "particle diversity collapsed: {distinct}");
        assert!(f.effective_fraction() > 0.1);
    }

    #[test]
    fn reset_discards_all_evidence() {
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 29);
        let model = PathLoss::default();
        f.update(
            &Measurement::Range {
                metres: 5.0,
                source: RangeSource::Uwb,
                at: Timestamp::ZERO,
            },
            Point2::ORIGIN,
            &model,
        );
        assert!(f.fix(Point2::ORIGIN).is_some());
        f.reset(Point2::ORIGIN);
        assert_eq!(f.observations(), 0);
        assert!(f.fix(Point2::ORIGIN).is_none());
    }

    #[test]
    fn contradictory_evidence_is_flagged_rather_than_averaged() {
        // Two incompatible UWB ranges from the same spot. There is no position
        // consistent with both; the filter must not report a confident midpoint.
        let mut f = ParticleFilter::new(FilterConfig::default(), Point2::ORIGIN, 37);
        let model = PathLoss::default();
        for _ in 0..60 {
            f.update(
                &Measurement::Range {
                    metres: 3.0,
                    source: RangeSource::Uwb,
                    at: Timestamp::ZERO,
                },
                Point2::ORIGIN,
                &model,
            );
            f.update(
                &Measurement::Range {
                    metres: 40.0,
                    source: RangeSource::Uwb,
                    at: Timestamp::ZERO,
                },
                Point2::ORIGIN,
                &model,
            );
        }
        assert!(
            f.diverged(),
            "irreconcilable measurements should raise the diverged flag"
        );
    }
}
