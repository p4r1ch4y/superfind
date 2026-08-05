//! Sharing observations with other devices over the local network.
//!
//! The transport for [`superfind_core::peer`]. One UDP socket, multicast, no
//! discovery protocol and no handshake: every peer shouts its readings and
//! listens for everyone else's.
//!
//! ## Why multicast and not something better
//!
//! A hunt lasts minutes among devices already on the same Wi-Fi, and the payload
//! is one short line per reading. A connection-oriented protocol would need
//! discovery, retries and teardown to deliver something that does not matter if
//! a packet is lost — the next reading is a fraction of a second behind. Losing
//! packets is not a failure mode here, it is the normal case handled for free.
//!
//! The cost is honest: **anything on the same network can read these packets**,
//! and they contain the address of the device being hunted and rough positions.
//! That is fine among your own devices on your own network, and it is the reason
//! this is off unless explicitly switched on.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::time::Duration;

use anyhow::{Context, Result};
use superfind_core::{Anchor, PeerReport, Point2};

/// Administratively-scoped multicast, so packets stay on the local network
/// rather than being forwarded by any router that should know better.
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 42, 99);
const PORT: u16 = 47_811;

pub struct PeerLink {
    socket: UdpSocket,
    anchor: Anchor,
    /// This device's name in reports, and what identifies its own packets so
    /// they can be ignored on the way back in.
    name: String,
    /// Where this device is in the shared frame, if it knows.
    position: Option<Point2>,
}

impl PeerLink {
    /// Join the group.
    ///
    /// `position` is this device's location in the anchor's coordinate frame —
    /// the origin for the anchoring device itself. `None` means this peer can
    /// receive and fuse but cannot usefully contribute, which is the honest
    /// state for a device nobody has placed.
    pub fn open(session: &str, name: &str, position: Option<Point2>) -> Result<Self> {
        // SO_REUSEADDR before bind: several receivers share this port by
        // design. Without it a second instance on the same machine fails with
        // EADDRINUSE, which rules out the simplest way to try this out.
        let raw = socket2::Socket::new(
            socket2::Domain::IPV4,
            socket2::Type::DGRAM,
            Some(socket2::Protocol::UDP),
        )
        .context("could not create the peer socket")?;
        raw.set_reuse_address(true)
            .context("could not set SO_REUSEADDR on the peer socket")?;
        raw.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())
            .context("could not bind the peer port")?;
        let socket: UdpSocket = raw.into();
        socket
            .join_multicast_v4(&GROUP, &Ipv4Addr::UNSPECIFIED)
            .context("could not join the multicast group")?;
        // Non-blocking: the hunt loop polls this alongside the radio and the
        // keyboard, and must never stall waiting for a peer that may not exist.
        socket.set_nonblocking(true)?;
        // Loopback ON, deliberately. Two instances on one machine is both the
        // easiest way to try this out and a real configuration — a laptop
        // anchoring while a phone walks. Our own packets do come back, and are
        // discarded by name in `drain`, which is why that name has to be unique
        // per process rather than merely per host.
        socket.set_multicast_loop_v4(true).ok();

        Ok(PeerLink {
            socket,
            anchor: Anchor {
                session: session.to_string(),
            },
            name: name.to_string(),
            position,
        })
    }

    pub fn anchor(&self) -> &Anchor {
        &self.anchor
    }

    pub fn position(&self) -> Option<Point2> {
        self.position
    }

    /// Announce a reading. Silent on failure — a hunt must not stop because the
    /// network did.
    pub fn share(&self, target: &str, rssi_dbm: f64, source_seconds: f64, source: RssiKind) {
        let Some(at) = self.position else {
            return;
        };
        let report = PeerReport {
            session: self.anchor.session.clone(),
            peer: self.name.clone(),
            at: Some(at),
            target: target.to_string(),
            rssi_dbm,
            source: source.into(),
            seconds: source_seconds,
        };
        let destination = SocketAddr::from(SocketAddrV4::new(GROUP, PORT));
        let _ = self.socket.send_to(report.encode().as_bytes(), destination);
    }

    /// Collect whatever has arrived since the last call.
    ///
    /// Reports from this device, from other sessions, about other targets, or
    /// without a position are all dropped here rather than reaching the filter.
    pub fn drain(&self, target: &str) -> Vec<PeerReport> {
        let mut out = Vec::new();
        let mut buffer = [0_u8; 512];
        // Bounded: a flooded network must not starve the hunt loop.
        for _ in 0..64 {
            let Ok((len, _)) = self.socket.recv_from(&mut buffer) else {
                break;
            };
            let Ok(line) = std::str::from_utf8(&buffer[..len]) else {
                continue;
            };
            let Some(report) = PeerReport::decode(line.trim()) else {
                continue;
            };
            if report.peer == self.name {
                continue;
            }
            if !report.is_relevant(&self.anchor, target) || !report.is_locatable() {
                continue;
            }
            out.push(report);
        }
        out
    }

    /// Block briefly waiting for any peer to speak. For a "is anyone else
    /// there?" check before a hunt starts.
    pub fn wait_for_peer(&self, timeout: Duration) -> Result<bool> {
        self.socket.set_nonblocking(false)?;
        self.socket.set_read_timeout(Some(timeout))?;
        let mut buffer = [0_u8; 512];
        let heard = self.socket.recv_from(&mut buffer).is_ok();
        self.socket.set_nonblocking(true)?;
        Ok(heard)
    }
}

/// Mirrors `RssiSource` without dragging the core's enum through the CLI's
/// argument lists.
#[derive(Debug, Clone, Copy)]
pub enum RssiKind {
    Link,
    Advert,
}

impl From<RssiKind> for superfind_core::RssiSource {
    fn from(k: RssiKind) -> Self {
        match k {
            RssiKind::Link => superfind_core::RssiSource::ConnectedLink,
            RssiKind::Advert => superfind_core::RssiSource::Advertisement,
        }
    }
}

/// Parse `--at x,y` into a position.
///
/// Metres east and north of whoever anchored the session. Nothing establishes
/// this frame automatically, so it is typed by a human with a tape measure or a
/// floor plan — which is worth stating plainly rather than hiding behind an
/// automatic-looking default.
pub fn parse_position(text: &str) -> Result<Point2> {
    let (x, y) = text
        .split_once(',')
        .context("expected two numbers separated by a comma, as in --at 4,2.5")?;
    Ok(Point2::new(
        x.trim().parse().context("the x coordinate is not a number")?,
        y.trim().parse().context("the y coordinate is not a number")?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_parse_with_or_without_spaces() {
        assert_eq!(parse_position("4,2.5").unwrap(), Point2::new(4.0, 2.5));
        assert_eq!(parse_position(" -3 , 0 ").unwrap(), Point2::new(-3.0, 0.0));
    }

    #[test]
    fn a_malformed_position_explains_itself() {
        let err = parse_position("over there").unwrap_err().to_string();
        assert!(err.contains("--at"), "error should show the expected form: {err}");
        assert!(parse_position("4,north").is_err());
    }

    /// Two links on one machine must not hear themselves, or a solo hunt would
    /// silently gain a phantom observer standing exactly where it does.
    #[test]
    fn a_link_ignores_its_own_reports() {
        let Ok(link) = PeerLink::open("test-session", "self", Some(Point2::ORIGIN)) else {
            // No multicast in this environment; nothing to assert.
            return;
        };
        link.share("AA:BB:CC:DD:EE:FF", -70.0, 1.0, RssiKind::Advert);
        std::thread::sleep(Duration::from_millis(50));
        assert!(link.drain("AA:BB:CC:DD:EE:FF").is_empty());
    }
}
