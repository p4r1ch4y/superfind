//! A small deterministic PRNG.
//!
//! The particle filter needs randomness for initialisation, roughening and
//! resampling. Pulling in `rand` for that would cost the crate its
//! dependency-free property, and more importantly a seeded, self-contained
//! generator means a failing filter test reproduces exactly, on every platform,
//! forever.
//!
//! PCG-XSH-RR 64/32, from O'Neill (2014). Statistically far better than a
//! xorshift of the same size and about as short to write.

/// Deterministic, seedable generator. Not cryptographically secure — it is used
/// for Monte Carlo sampling only, never for keys.
#[derive(Debug, Clone)]
pub struct Rng {
    state: u64,
    inc: u64,
}

const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

impl Rng {
    /// `seed` selects the stream position; `stream` selects an independent
    /// sequence, so parallel filters can be decorrelated without reseeding.
    pub fn new(seed: u64, stream: u64) -> Self {
        let mut rng = Rng {
            state: 0,
            // The increment must be odd for the LCG to reach full period.
            inc: (stream << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(seed);
        rng.next_u32();
        rng
    }

    pub fn seeded(seed: u64) -> Self {
        Rng::new(seed, 0xda3e_39cb_94b9_5bdb)
    }

    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Uniform in `[0, 1)`. Uses 32 bits of mantissa, which is ample for
    /// particle weights and well clear of ever returning exactly 1.0.
    #[inline]
    pub fn uniform(&mut self) -> f64 {
        self.next_u32() as f64 / (u32::MAX as f64 + 1.0)
    }

    /// Uniform in `[lo, hi)`.
    #[inline]
    pub fn uniform_range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.uniform() * (hi - lo)
    }

    /// Standard normal via Marsaglia polar method. Generates a pair and caches
    /// nothing — the discarded second value costs less than the branch and
    /// keeps the generator's call pattern deterministic regardless of how
    /// callers interleave requests.
    pub fn normal(&mut self) -> f64 {
        loop {
            let u = self.uniform_range(-1.0, 1.0);
            let v = self.uniform_range(-1.0, 1.0);
            let s = u * u + v * v;
            if s > 0.0 && s < 1.0 {
                return u * (-2.0 * s.ln() / s).sqrt();
            }
        }
    }

    #[inline]
    pub fn normal_with(&mut self, mean: f64, sigma: f64) -> f64 {
        mean + sigma * self.normal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_same_sequence() {
        let mut a = Rng::seeded(42);
        let mut b = Rng::seeded(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::seeded(1);
        let mut b = Rng::seeded(2);
        let differs = (0..64).any(|_| a.next_u32() != b.next_u32());
        assert!(differs);
    }

    #[test]
    fn uniform_stays_in_unit_interval() {
        let mut rng = Rng::seeded(7);
        for _ in 0..100_000 {
            let x = rng.uniform();
            assert!((0.0..1.0).contains(&x), "out of range: {x}");
        }
    }

    #[test]
    fn uniform_mean_is_half() {
        let mut rng = Rng::seeded(9);
        const N: usize = 200_000;
        let mean = (0..N).map(|_| rng.uniform()).sum::<f64>() / N as f64;
        assert!((mean - 0.5).abs() < 0.005, "mean was {mean}");
    }

    #[test]
    fn normal_has_unit_moments() {
        let mut rng = Rng::seeded(11);
        const N: usize = 200_000;
        let samples: Vec<f64> = (0..N).map(|_| rng.normal()).collect();
        let mean = samples.iter().sum::<f64>() / N as f64;
        let var = samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / N as f64;
        assert!(mean.abs() < 0.02, "mean was {mean}");
        assert!((var - 1.0).abs() < 0.02, "variance was {var}");
    }

    #[test]
    fn normal_respects_mean_and_sigma() {
        let mut rng = Rng::seeded(13);
        const N: usize = 100_000;
        let samples: Vec<f64> = (0..N).map(|_| rng.normal_with(5.0, 2.0)).collect();
        let mean = samples.iter().sum::<f64>() / N as f64;
        let sd = (samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / N as f64).sqrt();
        assert!((mean - 5.0).abs() < 0.05, "mean was {mean}");
        assert!((sd - 2.0).abs() < 0.05, "sd was {sd}");
    }
}
