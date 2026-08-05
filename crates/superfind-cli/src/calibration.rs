//! Per-device path-loss calibration, and where it is stored.
//!
//! ## Why bother
//!
//! `PathLoss::default()` carries `tx_power_1m = -59 dBm, n = 2.8` — priors from
//! published indoor studies, not from anything we measured. Transmit power
//! varies by more than 15 dB across handsets and tags, and that error maps
//! straight into a multiplicative distance error: assume a device is 15 dB
//! louder than it is and every distance estimate is roughly a third of the truth.
//!
//! Sixty seconds of standing at known distances fixes it, and every later phase
//! inherits the improvement.
//!
//! ## Why the fit is checked before it is saved
//!
//! A least-squares fit always returns *something*. In a reflective corridor it
//! will happily return an exponent of 1.1 or a 1 m reference of -12 dBm, both
//! physically impossible, and the filter would then be confidently wrong rather
//! than honestly uncertain. So a fit has to be both plausible and tight enough
//! before it is written down; otherwise the honest move is to keep the priors and
//! say so.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use superfind_core::PathLoss;

/// Above this RMS residual the environment is too reflective for a single
/// path-loss exponent to describe, and the fit is not worth keeping.
pub const MAX_ACCEPTABLE_RMS_DB: f64 = 8.0;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    #[serde(default)]
    pub name: Option<String>,
    pub tx_power_1m: f64,
    pub exponent: f64,
    #[serde(default)]
    pub rms_db: f64,
    #[serde(default)]
    pub samples: usize,
}

impl Entry {
    pub fn model(&self) -> PathLoss {
        PathLoss::new(self.tx_power_1m, self.exponent)
    }
}

/// The whole store, keyed by uppercase Bluetooth address.
#[derive(Debug, Clone, Default)]
pub struct Store {
    entries: BTreeMap<String, Entry>,
}

fn key(address: &str) -> String {
    address.to_uppercase()
}

impl Store {
    pub fn path() -> Result<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .context("neither XDG_CONFIG_HOME nor HOME is set")?;
        Ok(base.join("superfind").join("calibration.json"))
    }

    pub fn load() -> Store {
        // A missing or corrupt store is not an error worth stopping for: the
        // priors still work. Silently falling back is right here, where the
        // consequence is slightly worse distance estimates, not a wrong answer.
        let Ok(path) = Store::path() else {
            return Store::default();
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Store::default();
        };
        Store {
            entries: serde_json::from_str(&text).unwrap_or_default(),
        }
    }

    pub fn get(&self, address: &str) -> Option<&Entry> {
        self.entries.get(&key(address))
    }

    pub fn insert(&mut self, address: &str, entry: Entry) {
        self.entries.insert(key(address), entry);
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &Entry)> {
        self.entries.iter()
    }

    pub fn save(&self) -> Result<PathBuf> {
        let path = Store::path()?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)
                .with_context(|| format!("could not create {}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(&self.entries)
            .context("could not serialise the calibration store")?;
        std::fs::write(&path, text + "\n")
            .with_context(|| format!("could not write {}", path.display()))?;
        Ok(path)
    }
}

/// Fit a model to `(distance_m, rssi_dbm)` samples, refusing anything that is
/// not worth trusting. The `Err` string is written for a user, not a log.
pub fn fit(samples: &[(f64, f64)]) -> Result<(PathLoss, f64), String> {
    if samples.len() < 8 {
        return Err(format!(
            "only {} samples — collect more, or move closer so the device is heard",
            samples.len()
        ));
    }

    let distinct = {
        let mut ds: Vec<f64> = samples.iter().map(|(d, _)| *d).collect();
        ds.sort_by(|a, b| a.partial_cmp(b).unwrap());
        ds.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
        ds.len()
    };
    if distinct < 2 {
        return Err("samples came from only one distance — the slope is unidentifiable".into());
    }

    let model = PathLoss::fit(samples).ok_or("the fit did not converge")?;
    if !model.is_plausible() {
        return Err(format!(
            "fit is not physically plausible (1 m ref {:.1} dBm, exponent {:.2}) — \
             likely a reflective spot or a mistyped distance",
            model.tx_power_1m, model.exponent
        ));
    }

    let rms = model.residual_rms(samples).ok_or("no usable samples")?;
    if rms > MAX_ACCEPTABLE_RMS_DB {
        return Err(format!(
            "residual is {rms:.1} dB, above the {MAX_ACCEPTABLE_RMS_DB:.0} dB limit — \
             too much multipath here. Try an emptier room."
        ));
    }

    Ok((model, rms))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples_from(model: &PathLoss, noise: f64, seed: u64) -> Vec<(f64, f64)> {
        let mut rng = superfind_core::Rng::seeded(seed);
        let mut out = Vec::new();
        for d in [1.0, 2.0, 4.0, 8.0] {
            for _ in 0..30 {
                out.push((d, model.expected_rssi(d) + rng.normal_with(0.0, noise)));
            }
        }
        out
    }

    #[test]
    fn fit_recovers_a_known_radio() {
        let truth = PathLoss::new(-48.0, 2.6);
        let (fitted, rms) = fit(&samples_from(&truth, 3.0, 1)).expect("should fit");
        assert!((fitted.tx_power_1m - truth.tx_power_1m).abs() < 2.0);
        assert!((fitted.exponent - truth.exponent).abs() < 0.3);
        assert!(rms < MAX_ACCEPTABLE_RMS_DB);
    }

    #[test]
    fn fit_refuses_too_few_samples() {
        let e = fit(&[(1.0, -50.0), (2.0, -60.0)]).unwrap_err();
        assert!(e.contains("samples"), "got: {e}");
    }

    #[test]
    fn fit_refuses_a_single_distance() {
        let one: Vec<(f64, f64)> = (0..40).map(|i| (2.0, -60.0 + i as f64 * 0.1)).collect();
        let e = fit(&one).unwrap_err();
        assert!(e.contains("one distance"), "got: {e}");
    }

    #[test]
    fn fit_refuses_a_multipath_mess() {
        // Enormous noise: the fit will converge but is worthless.
        let truth = PathLoss::new(-50.0, 2.8);
        let e = fit(&samples_from(&truth, 25.0, 7)).unwrap_err();
        assert!(e.contains("multipath") || e.contains("plausible"), "got: {e}");
    }

    #[test]
    fn round_trips_through_the_file_format() {
        let mut store = Store::default();
        store.insert(
            "aa:bb:cc:dd:ee:ff",
            Entry {
                name: Some("Pixel 9 \"Pro\"".into()),
                tx_power_1m: -48.25,
                exponent: 2.6412,
                rms_db: 3.75,
                samples: 120,
            },
        );
        store.insert(
            "11:22:33:44:55:66",
            Entry {
                name: None,
                tx_power_1m: -61.0,
                exponent: 3.1,
                rms_db: 5.0,
                samples: 90,
            },
        );

        let text = serde_json::to_string_pretty(&store.entries).unwrap();
        let reloaded = Store {
            entries: serde_json::from_str(&text).unwrap(),
        };

        let a = reloaded.get("AA:BB:CC:DD:EE:FF").expect("first entry");
        assert_eq!(a.name.as_deref(), Some("Pixel 9 \"Pro\""));
        assert!((a.tx_power_1m - (-48.25)).abs() < 1e-3);
        assert!((a.exponent - 2.6412).abs() < 1e-4);
        assert_eq!(a.samples, 120);

        let b = reloaded.get("11:22:33:44:55:66").expect("second entry");
        assert_eq!(b.name, None);
        assert!((b.exponent - 3.1).abs() < 1e-4);
    }

    #[test]
    fn lookup_is_case_insensitive() {
        let mut store = Store::default();
        store.insert(
            "AA:BB:CC:DD:EE:FF",
            Entry {
                name: None,
                tx_power_1m: -50.0,
                exponent: 2.5,
                rms_db: 1.0,
                samples: 50,
            },
        );
        assert!(store.get("aa:bb:cc:dd:ee:ff").is_some());
        assert!(store.get("Aa:Bb:Cc:Dd:Ee:Ff").is_some());
    }

    #[test]
    fn a_corrupt_file_yields_an_empty_store_not_a_panic() {
        // A calibration file is not worth crashing over: the priors still work.
        for junk in ["", "not json", "{", "[]", "{\"a\": {}}", "{\"a\": {\"exponent\": }}"] {
            let entries: BTreeMap<String, Entry> =
                serde_json::from_str(junk).unwrap_or_default();
            assert!(entries.is_empty(), "junk parsed as data: {junk}");
        }
    }

    #[test]
    fn awkward_device_names_survive_a_round_trip() {
        // Names come from the device, not from us: quotes, braces, backslashes
        // and emoji all turn up in the wild.
        for name in [
            r#"Pixel 9 "Pro""#,
            r"back\slash",
            "brace{1}",
            "tab\there",
            "Ben's iPhone 📱",
        ] {
            let mut store = Store::default();
            store.insert(
                "AA:BB:CC:DD:EE:FF",
                Entry {
                    name: Some(name.to_string()),
                    tx_power_1m: -50.0,
                    exponent: 2.5,
                    rms_db: 1.0,
                    samples: 10,
                },
            );
            let text = serde_json::to_string(&store.entries).unwrap();
            let back: BTreeMap<String, Entry> = serde_json::from_str(&text).unwrap();
            assert_eq!(
                back["AA:BB:CC:DD:EE:FF"].name.as_deref(),
                Some(name),
                "round trip lost {name:?}"
            );
        }
    }

    #[test]
    fn the_saved_model_is_the_one_that_comes_back() {
        let entry = Entry {
            name: None,
            tx_power_1m: -47.5,
            exponent: 2.9,
            rms_db: 4.0,
            samples: 100,
        };
        let m = entry.model();
        assert!((m.tx_power_1m - (-47.5)).abs() < 1e-9);
        assert!((m.exponent - 2.9).abs() < 1e-9);
    }
}
