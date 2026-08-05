//! Synthetic-aperture bearing: recovering direction from one antenna.
//!
//! A single omnidirectional antenna yields distance only. The trick that makes
//! direction recoverable is that a human body is a poor microwave window —
//! roughly 5 to 15 dB of attenuation at 2.4 GHz. Turn on the spot and the signal
//! peaks when the device is in front of you and dips when your own torso is
//! between you and it. Binning RSSI by compass heading turns the user into a
//! crude directional antenna whose aperture is their own rotation.
//!
//! This is an *inference*, not a measurement, and the difference matters. A UWB
//! angle-of-arrival reading is a measurement — it belongs in
//! [`crate::measurement::Measurement::Angle`]. What comes out of this module is
//! a guess with error bars, and the error bars are the honest part. An app that
//! draws the same confident arrow for both has lied to its user.
//!
//! Hence [`BearingEstimate::confidence`], which is the product of three things
//! that must *all* hold for the answer to mean anything:
//!
//! 1. **Coverage** — you cannot infer direction from a 20-degree sweep.
//! 2. **Concentration** — the excess signal must point one way, not several.
//! 3. **Significance** — the peak-to-mean contrast must beat the radio's noise.
//!
//! Any one of them near zero drives the confidence to zero, which is why they
//! multiply rather than average. Low numbers here are the module working.

use crate::geom::{wrap_angle, wrap_positive};
use core::f64::consts::TAU;

/// Which sector a compass heading falls in.
#[inline]
pub fn sector_of(heading_rad: f64, sectors: usize) -> usize {
    if sectors == 0 {
        return 0;
    }
    let width = TAU / sectors as f64;
    let idx = (wrap_positive(heading_rad) / width).floor() as usize;
    // `wrap_positive` can return a value that rounds up to exactly TAU.
    idx.min(sectors - 1)
}

/// The compass heading at the centre of a sector.
#[inline]
pub fn sector_centre(index: usize, sectors: usize) -> f64 {
    if sectors == 0 {
        return 0.0;
    }
    let width = TAU / sectors as f64;
    wrap_angle((index as f64 + 0.5) * width)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct Sector {
    sum: f64,
    count: u32,
}

impl Sector {
    fn mean(&self) -> Option<f64> {
        (self.count > 0).then(|| self.sum / self.count as f64)
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BearingEstimate {
    /// Best guess at the compass bearing to the device, radians clockwise from
    /// north.
    pub bearing_rad: f64,
    /// One-sigma angular uncertainty, radians. Never smaller than half a
    /// sector — you cannot resolve finer than you binned.
    pub sigma_rad: f64,
    /// `0..=1`. Treat anything under about 0.3 as "keep sweeping".
    pub confidence: f64,
    /// Fraction of sectors that hold at least one sample, `0..=1`.
    pub coverage: f64,
    /// Peak-to-mean signal contrast in dB. The raw evidence behind the guess.
    pub contrast_db: f64,
    pub samples: u32,
}

/// Accumulates RSSI by the heading the user was facing when it arrived.
#[derive(Debug, Clone)]
pub struct SyntheticAperture {
    sectors: Vec<Sector>,
    samples: u32,
    /// Expected dB noise of the incoming samples, used to judge significance.
    noise_db: f64,
}

impl Default for SyntheticAperture {
    fn default() -> Self {
        SyntheticAperture::new(16, 7.0)
    }
}

impl SyntheticAperture {
    /// `sectors` is the angular resolution — 16 gives 22.5-degree bins, which
    /// is about the practical limit given body-shadowing is a broad lobe rather
    /// than a sharp null.
    pub fn new(sectors: usize, noise_db: f64) -> Self {
        SyntheticAperture {
            sectors: vec![Sector::default(); sectors.max(1)],
            samples: 0,
            noise_db: noise_db.max(0.5),
        }
    }

    pub fn sector_count(&self) -> usize {
        self.sectors.len()
    }

    pub fn samples(&self) -> u32 {
        self.samples
    }

    pub fn clear(&mut self) {
        self.sectors.iter_mut().for_each(|s| *s = Sector::default());
        self.samples = 0;
    }

    /// Record a reading taken while facing `heading_rad`.
    pub fn observe(&mut self, rssi_dbm: f64, heading_rad: f64) {
        if !rssi_dbm.is_finite() || !heading_rad.is_finite() {
            return;
        }
        let idx = sector_of(heading_rad, self.sectors.len());
        self.sectors[idx].sum += rssi_dbm;
        self.sectors[idx].count += 1;
        self.samples += 1;
    }

    /// Mean RSSI per sector, `None` where nothing was sampled. Indexed by
    /// sector, for drawing the radar.
    pub fn sector_means(&self) -> Vec<Option<f64>> {
        self.sectors.iter().map(Sector::mean).collect()
    }

    pub fn coverage(&self) -> f64 {
        let filled = self.sectors.iter().filter(|s| s.count > 0).count();
        filled as f64 / self.sectors.len() as f64
    }

    /// Best available bearing, or `None` before two distinct sectors hold data —
    /// with one sector there is no contrast to reason from, and returning a
    /// bearing anyway would be exactly the overclaim this module exists to
    /// avoid.
    pub fn estimate(&self) -> Option<BearingEstimate> {
        let means: Vec<Option<f64>> = self.sector_means();
        let filled: Vec<(usize, f64)> = means
            .iter()
            .enumerate()
            .filter_map(|(i, m)| m.map(|v| (i, v)))
            .collect();

        if filled.len() < 2 {
            return None;
        }

        let overall = filled.iter().map(|(_, v)| v).sum::<f64>() / filled.len() as f64;
        let peak = filled.iter().fold(f64::NEG_INFINITY, |a, (_, v)| a.max(*v));
        let contrast_db = peak - overall;

        // Weight each sector by how far above the mean it sits. Sectors at or
        // below the mean carry no evidence for the device being that way.
        let mut sin_sum = 0.0;
        let mut cos_sum = 0.0;
        let mut weight_sum = 0.0;
        for (i, mean) in &filled {
            let excess = (mean - overall).max(0.0);
            if excess <= 0.0 {
                continue;
            }
            let theta = sector_centre(*i, self.sectors.len());
            sin_sum += excess * theta.sin();
            cos_sum += excess * theta.cos();
            weight_sum += excess;
        }

        if weight_sum <= 0.0 {
            return None;
        }

        // Circular weighted mean, which interpolates between adjacent sectors
        // instead of snapping to the argmax bin.
        let bearing_rad = wrap_angle(sin_sum.atan2(cos_sum));

        // Resultant length in 0..=1: 1 when all the excess points one way, near
        // 0 when it is spread around the circle or split between opposite
        // sides. This is the standard circular concentration statistic.
        let concentration = (sin_sum.hypot(cos_sum) / weight_sum).clamp(0.0, 1.0);

        let coverage = self.coverage();

        // Is the peak bigger than the noise would produce on its own? The
        // standard error of a sector mean falls as sqrt(n), so more samples make
        // a given contrast more meaningful.
        let mean_samples = (self.samples as f64 / filled.len() as f64).max(1.0);
        let standard_error = self.noise_db / mean_samples.sqrt();
        let significance = (contrast_db / (2.0 * standard_error)).clamp(0.0, 1.0);

        // All three must hold, so they multiply. Coverage is the harshest term
        // by design: a bearing from a narrow sweep is not a bearing.
        let confidence = (concentration * coverage * significance).clamp(0.0, 1.0);

        // Uncertainty from the circular spread, floored at half a sector.
        let half_sector = TAU / self.sectors.len() as f64 / 2.0;
        let spread = (-2.0 * concentration.clamp(1e-6, 1.0).ln()).sqrt();
        let sigma_rad = spread.max(half_sector);

        Some(BearingEstimate {
            bearing_rad,
            sigma_rad,
            confidence,
            coverage,
            contrast_db,
            samples: self.samples,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{angle_diff, to_degrees, to_radians};

    /// Synthesise a sweep: the device sits at `device_deg`, the user turns all
    /// the way round, and their body attenuates the signal when it is in the
    /// way.
    fn sweep(aperture: &mut SyntheticAperture, device_deg: f64, shadow_db: f64, step_deg: usize) {
        let device = to_radians(device_deg);
        let mut deg = 0;
        while deg < 360 {
            let facing = to_radians(deg as f64);
            // Full signal facing the device, `shadow_db` down facing away.
            let alignment = angle_diff(facing, device).cos();
            let rssi = -60.0 - shadow_db * (1.0 - alignment) / 2.0;
            for _ in 0..8 {
                aperture.observe(rssi, facing);
            }
            deg += step_deg;
        }
    }

    #[test]
    fn sectors_tile_the_compass_without_gaps_or_overlap() {
        for deg in 0..360 {
            let idx = sector_of(to_radians(deg as f64), 16);
            assert!(idx < 16, "{deg} deg produced sector {idx}");
        }
        assert_eq!(sector_of(to_radians(0.0), 16), 0);
        assert_eq!(sector_of(to_radians(359.9), 16), 15);
        // Wrapped input lands in the same bin as its unwrapped equivalent.
        assert_eq!(
            sector_of(to_radians(-10.0), 16),
            sector_of(to_radians(350.0), 16)
        );
    }

    #[test]
    fn sector_centre_round_trips_through_sector_of() {
        for i in 0..16 {
            assert_eq!(sector_of(sector_centre(i, 16), 16), i);
        }
    }

    #[test]
    fn a_full_sweep_finds_the_device() {
        let mut a = SyntheticAperture::default();
        for truth_deg in [0.0, 45.0, 90.0, 137.0, 180.0, 250.0, 315.0] {
            a.clear();
            sweep(&mut a, truth_deg, 12.0, 5);
            let e = a.estimate().expect("a full sweep should yield a bearing");
            let error = to_degrees(angle_diff(e.bearing_rad, to_radians(truth_deg))).abs();
            assert!(error < 25.0, "at {truth_deg} deg the error was {error} deg");
            assert!(
                e.confidence > 0.35,
                "at {truth_deg} deg confidence was only {}",
                e.confidence
            );
        }
    }

    #[test]
    fn refuses_to_answer_before_there_is_evidence() {
        let mut a = SyntheticAperture::default();
        assert!(a.estimate().is_none(), "nothing observed");
        a.observe(-60.0, 0.0);
        assert!(a.estimate().is_none(), "one sector is not a bearing");
    }

    #[test]
    fn a_flat_signal_yields_low_confidence() {
        let mut a = SyntheticAperture::default();
        // No directionality at all: same RSSI whichever way the user faces.
        for deg in (0..360).step_by(5) {
            for _ in 0..8 {
                a.observe(-60.0, to_radians(deg as f64));
            }
        }
        match a.estimate() {
            None => {}
            Some(e) => assert!(
                e.confidence < 0.15,
                "flat signal reported confidence {}",
                e.confidence
            ),
        }
    }

    #[test]
    fn a_narrow_sweep_is_not_trusted_however_strong_the_contrast() {
        let mut narrow = SyntheticAperture::default();
        // Only two adjacent sectors sampled, with a big difference between them.
        for _ in 0..200 {
            narrow.observe(-40.0, to_radians(10.0));
            narrow.observe(-80.0, to_radians(35.0));
        }
        let e = narrow.estimate().expect("two sectors is enough to try");
        assert!(
            e.coverage < 0.2,
            "expected poor coverage, got {}",
            e.coverage
        );
        assert!(
            e.confidence < 0.25,
            "a 25-degree sweep must not read as confident; got {}",
            e.confidence
        );
    }

    #[test]
    fn confidence_rises_as_the_sweep_widens() {
        let mut partial = SyntheticAperture::default();
        let mut full = SyntheticAperture::default();
        for deg in (0..120).step_by(5) {
            let facing = to_radians(deg as f64);
            let alignment = angle_diff(facing, to_radians(60.0)).cos();
            for _ in 0..8 {
                partial.observe(-60.0 - 12.0 * (1.0 - alignment) / 2.0, facing);
            }
        }
        sweep(&mut full, 60.0, 12.0, 5);

        let p = partial.estimate().unwrap();
        let f = full.estimate().unwrap();
        assert!(
            f.confidence > p.confidence,
            "full sweep {} should beat partial {}",
            f.confidence,
            p.confidence
        );
    }

    #[test]
    fn weak_contrast_against_a_noisy_radio_is_not_significant() {
        // The same 1 dB of contrast, judged against a quiet and a noisy source.
        let build = |noise_db: f64| {
            let mut a = SyntheticAperture::new(16, noise_db);
            sweep(&mut a, 90.0, 1.0, 5);
            a.estimate().unwrap()
        };
        let quiet = build(1.0);
        let noisy = build(12.0);
        assert!(
            quiet.confidence > noisy.confidence,
            "quiet {} should beat noisy {}",
            quiet.confidence,
            noisy.confidence
        );
    }

    #[test]
    fn sigma_never_claims_sub_sector_resolution() {
        let mut a = SyntheticAperture::new(16, 7.0);
        sweep(&mut a, 90.0, 30.0, 5);
        let e = a.estimate().unwrap();
        let half_sector = TAU / 16.0 / 2.0;
        assert!(e.sigma_rad >= half_sector - 1e-12);
    }

    #[test]
    fn garbage_input_is_ignored_rather_than_poisoning_the_bins() {
        let mut a = SyntheticAperture::default();
        sweep(&mut a, 90.0, 12.0, 5);
        let before = a.estimate().unwrap();
        a.observe(f64::NAN, 0.0);
        a.observe(-60.0, f64::INFINITY);
        let after = a.estimate().unwrap();
        assert_eq!(before.samples, after.samples);
        assert!(after.bearing_rad.is_finite());
    }
}
