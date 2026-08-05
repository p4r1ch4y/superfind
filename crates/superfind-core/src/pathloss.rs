//! Log-distance path loss: the bridge from dBm to metres.
//!
//! ```text
//!   rssi(d) = tx_power_1m - 10 * n * log10(d)
//! ```
//!
//! `tx_power_1m` is the RSSI you would read at one metre and `n` is the path
//! loss exponent — 2.0 in free space, 2.7 to 4.0 through a furnished building.
//!
//! The important part is not the distance estimate, which is poor, but
//! [`PathLoss::log_likelihood`]. The filter never converts RSSI to a distance
//! and then treats that distance as a measurement; it asks "given a candidate
//! position, how surprising is this dBm reading?" and evaluates the Gaussian in
//! **dB space**, where the shadowing noise actually is Gaussian. Converting
//! first and applying Gaussian error in metres is the standard mistake, and it
//! makes far-away particles enormously over-penalised relative to near ones.

use crate::measurement::RssiSource;

/// Below this the log-distance model is meaningless — you are in the antenna's
/// near field and RSSI stops varying usefully with distance.
const MIN_DISTANCE_M: f64 = 0.25;

const LN_TAU: f64 = 1.837_877_066_409_345_5; // ln(2*pi)

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PathLoss {
    /// Expected RSSI at one metre, dBm.
    pub tx_power_1m: f64,
    /// Path loss exponent.
    pub exponent: f64,
}

impl Default for PathLoss {
    /// A furnished indoor room and a typical phone-class radio. Deliberately
    /// pessimistic on the exponent: overestimating `n` makes the filter
    /// under-confident, which is the safe direction to be wrong in.
    fn default() -> Self {
        PathLoss {
            tx_power_1m: -59.0,
            exponent: 2.8,
        }
    }
}

impl PathLoss {
    pub fn new(tx_power_1m: f64, exponent: f64) -> Self {
        PathLoss {
            tx_power_1m,
            // A non-positive exponent would invert the model, and anything
            // under 1.5 is below free space, which is not physical indoors.
            exponent: exponent.max(1.5),
        }
    }

    /// Free space. Useful as a lower bound and for outdoor line-of-sight.
    pub fn free_space(tx_power_1m: f64) -> Self {
        PathLoss::new(tx_power_1m, 2.0)
    }

    /// The RSSI this model predicts at `distance_m`.
    pub fn expected_rssi(&self, distance_m: f64) -> f64 {
        let d = distance_m.max(MIN_DISTANCE_M);
        self.tx_power_1m - 10.0 * self.exponent * d.log10()
    }

    /// Point estimate of distance from a single reading. Coarse by nature — a
    /// phone in a metal drawer two metres away reads like one fifteen metres
    /// away in open air. Use it for display, never as a filter input.
    pub fn distance(&self, rssi_dbm: f64) -> f64 {
        let exp = (self.tx_power_1m - rssi_dbm) / (10.0 * self.exponent);
        (10f64.powf(exp)).max(MIN_DISTANCE_M)
    }

    /// Log-likelihood of observing `rssi_dbm` from a target at `distance_m`.
    ///
    /// Evaluated in dB space, which is the whole point. Returns the natural log
    /// of a Gaussian density, so the filter can sum these across measurements
    /// and exponentiate once.
    pub fn log_likelihood(&self, rssi_dbm: f64, distance_m: f64, sigma_db: f64) -> f64 {
        let sigma = sigma_db.max(0.5);
        let residual = (rssi_dbm - self.expected_rssi(distance_m)) / sigma;
        -0.5 * residual * residual - sigma.ln() - 0.5 * LN_TAU
    }

    /// Convenience: log-likelihood using the source's own declared noise.
    pub fn log_likelihood_from(&self, rssi_dbm: f64, distance_m: f64, source: RssiSource) -> f64 {
        self.log_likelihood(rssi_dbm, distance_m, source.sigma_db())
    }

    /// Re-derive `tx_power_1m` from a reading taken at a known distance,
    /// holding the exponent fixed.
    ///
    /// This is the "hold the phone at arm's length for a moment" calibration.
    /// It is worth doing: TX power varies by more than 15 dB across handsets,
    /// and that error maps straight into a multiplicative distance error.
    pub fn calibrated_at(&self, rssi_dbm: f64, known_distance_m: f64) -> PathLoss {
        let d = known_distance_m.max(MIN_DISTANCE_M);
        PathLoss {
            tx_power_1m: rssi_dbm + 10.0 * self.exponent * d.log10(),
            exponent: self.exponent,
        }
    }

    /// Least-squares fit of both parameters from `(distance_m, rssi_dbm)` pairs.
    ///
    /// Linear regression of RSSI on `log10(d)`: the intercept is `tx_power_1m`
    /// and the slope is `-10n`. Needs at least two samples at genuinely
    /// different distances; returns `None` otherwise rather than producing a
    /// confident fit from a degenerate input.
    pub fn fit(samples: &[(f64, f64)]) -> Option<PathLoss> {
        let usable: Vec<(f64, f64)> = samples
            .iter()
            .filter(|(d, r)| d.is_finite() && r.is_finite() && *d >= MIN_DISTANCE_M)
            .map(|(d, r)| (d.log10(), *r))
            .collect();

        if usable.len() < 2 {
            return None;
        }

        let n = usable.len() as f64;
        let mean_x = usable.iter().map(|(x, _)| x).sum::<f64>() / n;
        let mean_y = usable.iter().map(|(_, y)| y).sum::<f64>() / n;

        let sxx: f64 = usable.iter().map(|(x, _)| (x - mean_x).powi(2)).sum();
        if sxx < 1e-9 {
            // Every sample at the same distance: the slope is unidentifiable.
            return None;
        }
        let sxy: f64 = usable
            .iter()
            .map(|(x, y)| (x - mean_x) * (y - mean_y))
            .sum();

        let slope = sxy / sxx;
        Some(PathLoss::new(mean_y - slope * mean_x, -slope / 10.0))
    }

    /// RMS residual of this model against `(distance_m, rssi_dbm)` samples, in dB.
    ///
    /// This is the number that decides whether a calibration is worth keeping.
    /// A tidy indoor fit lands around 3–5 dB. Above roughly 8 dB the environment
    /// is too reflective for a single path-loss exponent to describe — the honest
    /// response is to tell the user to calibrate somewhere less cluttered rather
    /// than to save a fit that will quietly mislead the filter.
    pub fn residual_rms(&self, samples: &[(f64, f64)]) -> Option<f64> {
        let usable: Vec<&(f64, f64)> = samples
            .iter()
            .filter(|(d, r)| d.is_finite() && r.is_finite() && *d >= MIN_DISTANCE_M)
            .collect();
        if usable.is_empty() {
            return None;
        }
        let sum_sq: f64 = usable
            .iter()
            .map(|(d, r)| (r - self.expected_rssi(*d)).powi(2))
            .sum();
        Some((sum_sq / usable.len() as f64).sqrt())
    }

    /// Sanity-check a calibration before trusting it.
    ///
    /// A fit can be numerically fine and still physically absurd — a reflective
    /// corridor can produce a negative exponent, and a single bad distance entry
    /// can throw the intercept 40 dB out. Bounds are generous; they exist to
    /// catch nonsense, not to enforce a prior.
    pub fn is_plausible(&self) -> bool {
        self.tx_power_1m.is_finite()
            && self.exponent.is_finite()
            && (-100.0..=-20.0).contains(&self.tx_power_1m)
            && (1.5..=6.0).contains(&self.exponent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: f64, b: f64, tol: f64) -> bool {
        (a - b).abs() < tol
    }

    #[test]
    fn one_metre_reads_tx_power() {
        let m = PathLoss::default();
        assert!(close(m.expected_rssi(1.0), m.tx_power_1m, 1e-9));
    }

    #[test]
    fn signal_falls_with_distance() {
        let m = PathLoss::default();
        let mut previous = f64::INFINITY;
        for d in [0.5, 1.0, 2.0, 5.0, 10.0, 30.0] {
            let r = m.expected_rssi(d);
            assert!(r < previous, "rssi did not fall at {d} m");
            previous = r;
        }
    }

    #[test]
    fn distance_inverts_expected_rssi() {
        let m = PathLoss::new(-59.0, 2.8);
        for d in [0.5, 1.0, 3.0, 7.5, 20.0] {
            let round_tripped = m.distance(m.expected_rssi(d));
            assert!(close(round_tripped, d, 1e-6), "round trip failed at {d} m");
        }
    }

    #[test]
    fn free_space_loses_twenty_db_per_decade() {
        let m = PathLoss::free_space(-40.0);
        assert!(close(m.expected_rssi(1.0) - m.expected_rssi(10.0), 20.0, 1e-9));
    }

    #[test]
    fn near_field_is_clamped_not_infinite() {
        let m = PathLoss::default();
        assert!(m.expected_rssi(0.0).is_finite());
        assert_eq!(m.expected_rssi(0.0), m.expected_rssi(MIN_DISTANCE_M));
        assert!(m.distance(0.0) >= MIN_DISTANCE_M);
    }

    #[test]
    fn exponent_is_floored_at_something_physical() {
        assert!(PathLoss::new(-59.0, 0.0).exponent >= 1.5);
        assert!(PathLoss::new(-59.0, -3.0).exponent >= 1.5);
    }

    #[test]
    fn likelihood_peaks_at_the_true_distance() {
        let m = PathLoss::default();
        let truth = 4.0;
        let observed = m.expected_rssi(truth);
        let best = m.log_likelihood(observed, truth, 4.0);
        for wrong in [1.0, 2.0, 3.0, 6.0, 10.0, 25.0] {
            assert!(
                m.log_likelihood(observed, wrong, 4.0) < best,
                "{wrong} m scored at least as well as the truth"
            );
        }
    }

    #[test]
    fn a_noisier_source_discriminates_less() {
        let m = PathLoss::default();
        let observed = m.expected_rssi(4.0);
        // Penalty for being wrong by the same amount, under two noise levels.
        let penalty = |sigma: f64| {
            m.log_likelihood(observed, 4.0, sigma) - m.log_likelihood(observed, 12.0, sigma)
        };
        let link = penalty(RssiSource::ConnectedLink.sigma_db());
        let advert = penalty(RssiSource::Advertisement.sigma_db());
        assert!(
            link > advert,
            "the trusted source should separate hypotheses more sharply"
        );
    }

    #[test]
    fn likelihood_is_a_normalised_density() {
        // At the peak, the log density should equal -ln(sigma) - 0.5*ln(2pi).
        let m = PathLoss::default();
        let sigma = 4.0;
        let peak = m.log_likelihood(m.expected_rssi(3.0), 3.0, sigma);
        assert!(close(peak, -sigma.ln() - 0.5 * LN_TAU, 1e-9));
    }

    #[test]
    fn calibration_recovers_a_shifted_tx_power() {
        let truth = PathLoss::new(-45.0, 2.8);
        let assumed = PathLoss::new(-59.0, 2.8);
        // Observe the true radio at a known 2 m and correct our assumption.
        let fixed = assumed.calibrated_at(truth.expected_rssi(2.0), 2.0);
        assert!(close(fixed.tx_power_1m, truth.tx_power_1m, 1e-9));
    }

    #[test]
    fn fit_recovers_both_parameters_from_clean_samples() {
        let truth = PathLoss::new(-52.0, 3.2);
        let samples: Vec<(f64, f64)> = [0.5, 1.0, 2.0, 4.0, 8.0, 16.0]
            .iter()
            .map(|&d| (d, truth.expected_rssi(d)))
            .collect();
        let fitted = PathLoss::fit(&samples).expect("clean samples should fit");
        assert!(close(fitted.tx_power_1m, truth.tx_power_1m, 1e-6));
        assert!(close(fitted.exponent, truth.exponent, 1e-6));
    }

    #[test]
    fn fit_refuses_degenerate_input() {
        assert!(PathLoss::fit(&[]).is_none());
        assert!(PathLoss::fit(&[(2.0, -60.0)]).is_none());
        // Every sample at the same distance cannot identify a slope.
        assert!(PathLoss::fit(&[(2.0, -60.0), (2.0, -63.0), (2.0, -58.0)]).is_none());
    }

    #[test]
    fn residual_rms_is_zero_on_a_perfect_fit() {
        let m = PathLoss::new(-52.0, 3.0);
        let samples: Vec<(f64, f64)> = [1.0, 2.0, 4.0, 8.0]
            .iter()
            .map(|&d| (d, m.expected_rssi(d)))
            .collect();
        assert!(m.residual_rms(&samples).unwrap() < 1e-9);
    }

    #[test]
    fn residual_rms_recovers_the_injected_noise_level() {
        let truth = PathLoss::new(-52.0, 3.0);
        let mut rng = crate::rng::Rng::seeded(8);
        let samples: Vec<(f64, f64)> = (0..4000)
            .map(|i| {
                let d = 1.0 + (i % 20) as f64 * 0.5;
                (d, truth.expected_rssi(d) + rng.normal_with(0.0, 5.0))
            })
            .collect();
        let rms = truth.residual_rms(&samples).unwrap();
        assert!((rms - 5.0).abs() < 0.4, "rms was {rms}, expected about 5 dB");
    }

    #[test]
    fn residual_rms_refuses_empty_input() {
        assert!(PathLoss::default().residual_rms(&[]).is_none());
    }

    #[test]
    fn a_worse_model_scores_a_worse_residual() {
        let truth = PathLoss::new(-52.0, 3.0);
        let samples: Vec<(f64, f64)> = [1.0, 2.0, 4.0, 8.0, 16.0]
            .iter()
            .map(|&d| (d, truth.expected_rssi(d)))
            .collect();
        let wrong = PathLoss::new(-70.0, 2.0);
        assert!(wrong.residual_rms(&samples).unwrap() > truth.residual_rms(&samples).unwrap());
    }

    #[test]
    fn plausibility_rejects_physically_absurd_fits() {
        assert!(PathLoss::default().is_plausible());
        assert!(PathLoss::new(-45.0, 2.2).is_plausible());
        // An exponent above 6 means the fit has absorbed something that is not
        // path loss — a moving obstruction, or a mislabelled distance.
        assert!(!PathLoss::new(-59.0, 9.0).is_plausible());
        // Below free space is caught by the constructor's clamp rather than
        // here, so a too-low input arrives already corrected.
        assert_eq!(PathLoss::new(-59.0, 0.5).exponent, 1.5);
        assert!(!PathLoss::new(-5.0, 2.8).is_plausible());
        assert!(!PathLoss::new(-140.0, 2.8).is_plausible());
        assert!(!PathLoss::new(f64::NAN, 2.8).is_plausible());
    }

    #[test]
    fn fit_tolerates_noise() {
        let truth = PathLoss::new(-52.0, 3.0);
        let mut rng = crate::rng::Rng::seeded(4);
        let samples: Vec<(f64, f64)> = (0..400)
            .map(|i| {
                let d = 0.5 + (i % 40) as f64 * 0.5;
                (d, truth.expected_rssi(d) + rng.normal_with(0.0, 5.0))
            })
            .collect();
        let fitted = PathLoss::fit(&samples).expect("should fit");
        assert!(close(fitted.tx_power_1m, truth.tx_power_1m, 1.5));
        assert!(close(fitted.exponent, truth.exponent, 0.3));
    }
}
