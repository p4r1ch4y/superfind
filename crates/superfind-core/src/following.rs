//! Detecting a device that is travelling with you.
//!
//! The same scan that finds your keys will also notice a tracker someone has
//! slipped into your bag. That is not a coincidence — it is the same
//! measurement read the other way round, and the [DULT specification][dult] that
//! Google and Apple now both implement exists precisely because the capability
//! is unavoidable and had better be pointed at the victim's benefit.
//!
//! [dult]: https://developers.google.com/nearby/fast-pair/specifications/extensions/fmdn
//!
//! ## What actually distinguishes a follower
//!
//! Not signal strength: a device can be loud because it is close, and plenty of
//! close devices are innocent. Not duration either — a laptop on the same desk
//! for an hour is not following anyone.
//!
//! What matters is **persistence across places you have moved between**. A
//! stranger's phone is loud in a café and gone by the time you reach the bus. A
//! tracker in your bag is present in both, and in the walk between them. So the
//! signal is: seen over a long span, *and* seen after you have travelled a
//! meaningful distance, *and* seen repeatedly rather than twice by chance.
//!
//! ## Deliberately reluctant
//!
//! Every threshold here errs towards silence. Telling somebody they are being
//! followed when they are not is its own harm — it invites them to search their
//! own belongings, distrust a colleague, or panic — and an alert that fires in
//! every café is one nobody reads. A device must clear all the criteria, not
//! merely look suspicious.
//!
//! This detects *co-travel*, and co-travel is not proof of malice. The wording
//! callers should use is "this has been with you", never "you are being
//! tracked".

use crate::geom::Point2;
use crate::time::Timestamp;

/// One sighting of a device, tied to where the user was at the time.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Sighting {
    at: Timestamp,
    /// The *user's* position, not the device's. We rarely know the latter, and
    /// co-travel is a fact about the observer's journey anyway.
    user: Point2,
}

/// How suspicious a device's persistence is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FollowVerdict {
    /// Nothing worth saying.
    Unremarkable,
    /// Present across a long span and some distance, but not enough of either
    /// to be worth interrupting somebody over. Callers should record it and stay
    /// quiet.
    Noted,
    /// Present throughout a journey that covered real ground. Worth telling the
    /// user about, in those words.
    TravelledWithYou,
}

/// Thresholds, all of which must be cleared together.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FollowPolicy {
    /// How long between first and last sighting before it counts.
    pub min_span_s: f64,
    /// How far the user must have moved since first seeing it. Without this, a
    /// device on the same desk all afternoon would qualify.
    pub min_travel_m: f64,
    /// How many separate sightings. Two can be coincidence.
    pub min_sightings: usize,
    /// A gap longer than this breaks the chain: the device was left behind and
    /// something else was seen later at the same address.
    pub max_gap_s: f64,
}

impl Default for FollowPolicy {
    fn default() -> Self {
        FollowPolicy {
            // Roughly the DULT guidance: long enough that a shared bus ride does
            // not trigger it, short enough to matter on the day.
            min_span_s: 20.0 * 60.0,
            min_travel_m: 300.0,
            min_sightings: 8,
            max_gap_s: 10.0 * 60.0,
        }
    }
}

/// Tracks how long each nearby device has been in your company.
#[derive(Debug, Clone)]
pub struct FollowWatch {
    policy: FollowPolicy,
    devices: Vec<(String, Vec<Sighting>)>,
}

impl Default for FollowWatch {
    fn default() -> Self {
        FollowWatch::new(FollowPolicy::default())
    }
}

impl FollowWatch {
    pub fn new(policy: FollowPolicy) -> Self {
        FollowWatch {
            policy,
            devices: Vec::new(),
        }
    }

    /// Record that `key` was heard while the user was at `user`.
    ///
    /// `key` should be whatever identifies the device *across* sightings. For a
    /// rotating address that is the problem in miniature: a well-behaved tracker
    /// changes address every fifteen minutes precisely so this cannot be done,
    /// which is why real implementations follow the DULT payload rather than the
    /// address. Passing an address here still catches the badly-behaved ones,
    /// which in practice are most of the cheap ones.
    pub fn observe(&mut self, key: &str, at: Timestamp, user: Point2) {
        let sighting = Sighting { at, user };
        if let Some((_, history)) = self.devices.iter_mut().find(|(k, _)| k == key) {
            // A long silence means this is a new encounter, not a continuation.
            if let Some(last) = history.last() {
                if at.0 - last.at.0 > self.policy.max_gap_s {
                    history.clear();
                }
            }
            history.push(sighting);
        } else {
            self.devices.push((key.to_string(), vec![sighting]));
        }
    }

    pub fn verdict(&self, key: &str) -> FollowVerdict {
        let Some((_, history)) = self.devices.iter().find(|(k, _)| k == key) else {
            return FollowVerdict::Unremarkable;
        };
        let (Some(first), Some(last)) = (history.first(), history.last()) else {
            return FollowVerdict::Unremarkable;
        };

        let span = last.at.0 - first.at.0;
        // Straight-line displacement from where it was first heard, not distance
        // walked: pacing a room for an hour is not a journey, and a device that
        // sat through it has not followed anybody anywhere.
        let travel = history
            .iter()
            .map(|s| first.user.distance_to(s.user))
            .fold(0.0_f64, f64::max);

        let long_enough = span >= self.policy.min_span_s;
        let far_enough = travel >= self.policy.min_travel_m;
        let often_enough = history.len() >= self.policy.min_sightings;

        match (long_enough, far_enough, often_enough) {
            (true, true, true) => FollowVerdict::TravelledWithYou,
            // Two of three is worth remembering and not worth saying.
            (a, b, c) if [a, b, c].iter().filter(|x| **x).count() >= 2 => FollowVerdict::Noted,
            _ => FollowVerdict::Unremarkable,
        }
    }

    /// Everything that has travelled with the user, worst first.
    pub fn followers(&self) -> Vec<(&str, FollowVerdict)> {
        let mut out: Vec<(&str, FollowVerdict)> = self
            .devices
            .iter()
            .map(|(k, _)| (k.as_str(), self.verdict(k)))
            .filter(|(_, v)| *v == FollowVerdict::TravelledWithYou)
            .collect();
        out.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        out
    }

    /// Forget sightings older than `horizon` seconds.
    ///
    /// Called periodically so yesterday's journey does not incriminate a device
    /// you have simply owned for a long time.
    pub fn prune(&mut self, now: Timestamp, horizon: f64) {
        for (_, history) in &mut self.devices {
            history.retain(|s| now.0 - s.at.0 <= horizon);
        }
        self.devices.retain(|(_, history)| !history.is_empty());
    }

    pub fn tracked_count(&self) -> usize {
        self.devices.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Walk a kilometre over half an hour with one device present throughout.
    fn journey(watch: &mut FollowWatch, key: &str, sightings: usize, span_s: f64, metres: f64) {
        for i in 0..sightings {
            let f = i as f64 / (sightings.max(2) - 1) as f64;
            watch.observe(key, Timestamp(f * span_s), Point2::new(f * metres, 0.0));
        }
    }

    #[test]
    fn a_device_that_travels_with_you_is_reported() {
        let mut watch = FollowWatch::default();
        journey(&mut watch, "tag", 12, 30.0 * 60.0, 1200.0);
        assert_eq!(watch.verdict("tag"), FollowVerdict::TravelledWithYou);
        assert_eq!(watch.followers().len(), 1);
    }

    #[test]
    fn a_device_you_sat_next_to_all_afternoon_is_not_a_follower() {
        // Long span, many sightings, but the user never went anywhere. A desk
        // lamp is not stalking you.
        let mut watch = FollowWatch::default();
        journey(&mut watch, "desk-speaker", 40, 3.0 * 3600.0, 2.0);
        assert_ne!(
            watch.verdict("desk-speaker"),
            FollowVerdict::TravelledWithYou
        );
        assert!(watch.followers().is_empty());
    }

    #[test]
    fn a_brief_encounter_on_a_long_walk_is_not_a_follower() {
        // Someone walking the other way: seen twice, far apart, over seconds.
        let mut watch = FollowWatch::default();
        watch.observe("passer-by", Timestamp(0.0), Point2::new(0.0, 0.0));
        watch.observe("passer-by", Timestamp(4.0), Point2::new(6.0, 0.0));
        assert_eq!(watch.verdict("passer-by"), FollowVerdict::Unremarkable);
    }

    #[test]
    fn a_long_silence_starts_a_fresh_encounter() {
        // Home in the morning, home again at night. The device did not come
        // along; it was waiting.
        let mut watch = FollowWatch::default();
        journey(&mut watch, "home-tv", 10, 20.0 * 60.0, 400.0);
        assert_eq!(watch.verdict("home-tv"), FollowVerdict::TravelledWithYou);

        watch.observe("home-tv", Timestamp(8.0 * 3600.0), Point2::new(0.0, 0.0));
        assert_eq!(
            watch.verdict("home-tv"),
            FollowVerdict::Unremarkable,
            "the gap should have reset the chain"
        );
    }

    #[test]
    fn two_of_three_criteria_is_noted_but_never_announced() {
        // Long and frequent, but barely any travel.
        let mut watch = FollowWatch::default();
        journey(&mut watch, "cafe-beacon", 20, 40.0 * 60.0, 5.0);
        assert_eq!(watch.verdict("cafe-beacon"), FollowVerdict::Noted);
        assert!(
            watch.followers().is_empty(),
            "Noted must not reach the user: a false alarm is its own harm"
        );
    }

    #[test]
    fn an_unseen_device_is_unremarkable() {
        let watch = FollowWatch::default();
        assert_eq!(watch.verdict("never-heard-of-it"), FollowVerdict::Unremarkable);
    }

    #[test]
    fn pruning_forgets_yesterdays_journey() {
        let mut watch = FollowWatch::default();
        journey(&mut watch, "tag", 12, 30.0 * 60.0, 1200.0);
        assert_eq!(watch.tracked_count(), 1);

        watch.prune(Timestamp(48.0 * 3600.0), 24.0 * 3600.0);
        assert_eq!(watch.tracked_count(), 0);
        assert_eq!(watch.verdict("tag"), FollowVerdict::Unremarkable);
    }

    #[test]
    fn thresholds_are_configurable_for_stricter_or_looser_callers() {
        let strict = FollowPolicy {
            min_travel_m: 5_000.0,
            ..FollowPolicy::default()
        };
        let mut watch = FollowWatch::new(strict);
        journey(&mut watch, "tag", 12, 30.0 * 60.0, 1200.0);
        assert_ne!(watch.verdict("tag"), FollowVerdict::TravelledWithYou);
    }
}
