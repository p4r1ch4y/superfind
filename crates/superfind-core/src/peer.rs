//! Observations shared between devices hunting the same thing.
//!
//! One observer sees an annulus. Two see its intersection. Three, not in a
//! line, simply see where the device is — no walking, no waiting for a bearing
//! to earn its arrow. [`crate::Tracker::observe_from`] does the fusion; this
//! module is the wire format that gets a peer's reading there.
//!
//! ## The hard part is the shared frame
//!
//! Every peer position here is expressed in *one session's* coordinate frame,
//! and nothing in Bluetooth establishes that frame. Two phones in a room have no
//! idea where they are relative to each other. So a session names an
//! [`Anchor`] — the device that defines the origin — and every other peer states
//! its offset from it, by whatever means the humans have: a tape measure, a
//! floor plan, or standing at agreed corners of a room.
//!
//! That is a genuine limitation and not one to paper over. A peer that cannot
//! say where it is contributes nothing, because a range from an unknown point
//! constrains nothing at all. [`PeerReport::is_locatable`] exists so callers can
//! reject those rather than quietly fusing garbage.
//!
//! ## Why the format is deliberately dull
//!
//! Newline-delimited, self-describing, and parseable with `split`. Peers may be
//! running different versions across a LAN, and a hand-checkable format means a
//! mismatch shows up as a rejected line rather than as a plausible wrong number.

use crate::geom::Point2;
use crate::measurement::{Measurement, RssiSource};
use crate::time::Timestamp;

/// Which device defines the origin of a shared hunt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Anchor {
    /// Stable identifier for the session — every peer must agree on it, or they
    /// are measuring in unrelated frames and must not be fused.
    pub session: String,
}

/// One peer's contribution: a reading, and where it was taken from.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerReport {
    pub session: String,
    /// Who sent it. For display and for spotting a peer that has gone quiet.
    pub peer: String,
    /// Where the peer was, in the anchor's frame, in metres.
    pub at: Option<Point2>,
    pub target: String,
    pub rssi_dbm: f64,
    pub source: RssiSource,
    /// Seconds since the session began, on the anchor's clock.
    pub seconds: f64,
}

impl PeerReport {
    /// Whether this report can contribute anything.
    ///
    /// A range from an unknown position constrains nothing — it is a radius
    /// around a circle whose centre is unknown, which is the whole plane. Such
    /// reports are dropped rather than fused.
    pub fn is_locatable(&self) -> bool {
        self.at.is_some()
    }

    pub fn measurement(&self) -> Measurement {
        Measurement::Rssi {
            dbm: self.rssi_dbm,
            source: self.source,
            at: Timestamp(self.seconds),
        }
    }

    /// Serialise to one line.
    ///
    /// `superfind/1 <session> <peer> <target> <x> <y> <rssi> <source> <seconds>`
    /// with `-` for an unknown position.
    pub fn encode(&self) -> String {
        let (x, y) = match self.at {
            Some(p) => (format!("{:.3}", p.x), format!("{:.3}", p.y)),
            None => ("-".to_string(), "-".to_string()),
        };
        format!(
            "superfind/1 {} {} {} {} {} {:.1} {} {:.3}",
            self.session,
            self.peer,
            self.target,
            x,
            y,
            self.rssi_dbm,
            source_tag(self.source),
            self.seconds,
        )
    }

    /// Parse one line. Returns `None` for anything unrecognised.
    ///
    /// Deliberately strict: a peer running a version we do not understand should
    /// be ignored, not guessed at. Silently mis-parsing a coordinate would move
    /// the fix somewhere confidently wrong.
    pub fn decode(line: &str) -> Option<PeerReport> {
        let mut parts = line.split_whitespace();
        if parts.next()? != "superfind/1" {
            return None;
        }
        let session = parts.next()?.to_string();
        let peer = parts.next()?.to_string();
        let target = parts.next()?.to_string();
        let x = parts.next()?;
        let y = parts.next()?;
        let rssi_dbm: f64 = parts.next()?.parse().ok()?;
        let source = parse_source(parts.next()?)?;
        let seconds: f64 = parts.next()?.parse().ok()?;

        let at = match (x, y) {
            ("-", "-") => None,
            _ => Some(Point2::new(x.parse().ok()?, y.parse().ok()?)),
        };

        // A reading outside the physically possible range is a corrupted line,
        // not a distant device.
        if !(-127.0..0.0).contains(&rssi_dbm) || !seconds.is_finite() {
            return None;
        }

        Some(PeerReport {
            session,
            peer,
            at,
            target,
            rssi_dbm,
            source,
            seconds,
        })
    }

    /// Whether this report belongs to `session` and concerns `target`.
    ///
    /// Both must match. Fusing a reading of a *different device* would drag the
    /// estimate towards something nobody is looking for, and fusing across
    /// sessions would mix incompatible coordinate frames.
    pub fn is_relevant(&self, anchor: &Anchor, target: &str) -> bool {
        self.session == anchor.session && self.target.eq_ignore_ascii_case(target)
    }
}

fn source_tag(s: RssiSource) -> &'static str {
    match s {
        RssiSource::ConnectedLink => "link",
        RssiSource::Advertisement => "advert",
        RssiSource::ClassicPoll => "classic",
    }
}

fn parse_source(tag: &str) -> Option<RssiSource> {
    match tag {
        "link" => Some(RssiSource::ConnectedLink),
        "advert" => Some(RssiSource::Advertisement),
        "classic" => Some(RssiSource::ClassicPoll),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PeerReport {
        PeerReport {
            session: "kitchen-hunt".into(),
            peer: "laptop".into(),
            at: Some(Point2::new(12.0, -3.5)),
            target: "AA:BB:CC:DD:EE:FF".into(),
            rssi_dbm: -73.5,
            source: RssiSource::Advertisement,
            seconds: 41.25,
        }
    }

    #[test]
    fn a_report_survives_a_round_trip() {
        let original = sample();
        let decoded = PeerReport::decode(&original.encode()).expect("decodes");
        assert_eq!(decoded, original);
    }

    #[test]
    fn an_unknown_position_round_trips_as_unknown() {
        let mut report = sample();
        report.at = None;
        let decoded = PeerReport::decode(&report.encode()).expect("decodes");
        assert_eq!(decoded.at, None);
        assert!(!decoded.is_locatable(), "must not be fused");
    }

    #[test]
    fn a_future_version_is_ignored_rather_than_guessed_at() {
        let line = sample().encode().replace("superfind/1", "superfind/2");
        assert_eq!(PeerReport::decode(&line), None);
    }

    #[test]
    fn corrupt_lines_are_rejected() {
        assert_eq!(PeerReport::decode(""), None);
        assert_eq!(PeerReport::decode("hello world"), None);
        // Truncated.
        assert_eq!(PeerReport::decode("superfind/1 s p t 1 2"), None);
        // A positive dBm is not a distant device, it is a broken line.
        let bad = sample().encode().replace("-73.5", "45.0");
        assert_eq!(PeerReport::decode(&bad), None);
        // An unparseable coordinate must not silently become zero.
        let bad = sample().encode().replace("12.000", "east");
        assert_eq!(PeerReport::decode(&bad), None);
    }

    #[test]
    fn relevance_requires_both_session_and_target() {
        let anchor = Anchor {
            session: "kitchen-hunt".into(),
        };
        let report = sample();
        assert!(report.is_relevant(&anchor, "aa:bb:cc:dd:ee:ff"));
        assert!(!report.is_relevant(&anchor, "11:22:33:44:55:66"));

        let elsewhere = Anchor {
            session: "garage-hunt".into(),
        };
        assert!(
            !report.is_relevant(&elsewhere, "AA:BB:CC:DD:EE:FF"),
            "frames from different sessions must never be mixed"
        );
    }

    #[test]
    fn a_decoded_report_produces_a_usable_measurement() {
        let report = sample();
        match report.measurement() {
            Measurement::Rssi { dbm, source, at } => {
                assert_eq!(dbm, -73.5);
                assert_eq!(source, RssiSource::Advertisement);
                assert_eq!(at.0, 41.25);
            }
            other => panic!("expected an RSSI measurement, got {other:?}"),
        }
    }
}
