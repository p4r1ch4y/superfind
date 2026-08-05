//! Terminal rendering.
//!
//! A pure function of a [`Snapshot`] — the renderer cannot reach into tracker
//! state, so it cannot show two numbers sampled a frame apart that disagree with
//! each other. One write per frame, because twenty prints to a line-buffered TTY
//! tears visibly.

use std::fmt::Write as _;

use superfind_core::{to_degrees, Proximity, RssiSource, Snapshot, Trend};

const CLEAR: &str = "\u{1B}[2J\u{1B}[H";
const WIDTH: usize = 64;
const MARGIN: &str = "    ";

pub struct Style {
    enabled: bool,
}

impl Style {
    pub fn detect() -> Self {
        // Honour the de facto standard for "do not colour this".
        Style {
            enabled: std::env::var_os("NO_COLOR").is_none(),
        }
    }

    fn wrap(&self, s: &str, code: &str) -> String {
        if self.enabled {
            format!("\u{1B}[{code}m{s}\u{1B}[0m")
        } else {
            s.to_string()
        }
    }

    fn dim(&self, s: &str) -> String {
        self.wrap(s, "2")
    }
}

const DIM: &str = "2";
const BRIGHT_GREEN: &str = "1;92";
const GREEN: &str = "92";
const YELLOW: &str = "93";
const AMBER: &str = "33";
const RED: &str = "91";

fn band_tone(p: Proximity) -> &'static str {
    match p {
        Proximity::ArmsReach => BRIGHT_GREEN,
        Proximity::SameTable => GREEN,
        Proximity::SameRoom => YELLOW,
        Proximity::FarOrObstructed => AMBER,
        Proximity::VeryFarOrShielded => RED,
    }
}

/// A 3x5 block font, doubled horizontally so the reading stays legible at arm's
/// length while walking. Lifted from findphone, which is where the idea of
/// making the number huge came from: you are looking at the room, not the
/// screen.
fn big_digits(text: &str) -> Vec<String> {
    const ROWS: usize = 5;
    let glyph = |c: char| -> Option<[&'static str; ROWS]> {
        Some(match c {
            '0' => ["###", "# #", "# #", "# #", "###"],
            '1' => ["  #", "  #", "  #", "  #", "  #"],
            '2' => ["###", "  #", "###", "#  ", "###"],
            '3' => ["###", "  #", "###", "  #", "###"],
            '4' => ["# #", "# #", "###", "  #", "  #"],
            '5' => ["###", "#  ", "###", "  #", "###"],
            '6' => ["###", "#  ", "###", "# #", "###"],
            '7' => ["###", "  #", "  #", "  #", "  #"],
            '8' => ["###", "# #", "###", "# #", "###"],
            '9' => ["###", "# #", "###", "  #", "###"],
            '-' => ["   ", "   ", "###", "   ", "   "],
            _ => return None,
        })
    };

    let picked: Vec<[&str; ROWS]> = text.chars().filter_map(glyph).collect();
    (0..ROWS)
        .map(|row| {
            picked
                .iter()
                .map(|g| {
                    g[row]
                        .chars()
                        .map(|c| if c == '#' { "██" } else { "  " })
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

fn bar(fraction: f64, width: usize) -> String {
    let filled = ((fraction.clamp(0.0, 1.0)) * width as f64).round() as usize;
    "█".repeat(filled) + &"░".repeat(width - filled)
}

/// Fraction of the useful -100..-30 dBm span.
fn signal_fraction(dbm: f64) -> f64 {
    ((dbm.clamp(-100.0, -30.0)) + 100.0) / 70.0
}

fn source_label(s: RssiSource) -> &'static str {
    match s {
        RssiSource::ConnectedLink => "link",
        RssiSource::Advertisement => "advert",
        RssiSource::ClassicPoll => "classic",
    }
}

fn trend_label(style: &Style, t: Trend) -> String {
    match t {
        Trend::Warmer => style.wrap("▲ WARMER", BRIGHT_GREEN),
        Trend::Colder => style.wrap("▼ colder", RED),
        Trend::Steady => style.wrap("· steady", DIM),
        Trend::Unknown => String::new(),
    }
}

/// Compass point for a bearing, which is far easier to act on than degrees.
fn compass(bearing_rad: f64) -> &'static str {
    const POINTS: [&str; 8] = ["N", "NE", "E", "SE", "S", "SW", "W", "NW"];
    let mut deg = to_degrees(bearing_rad) % 360.0;
    if deg < 0.0 {
        deg += 360.0;
    }
    POINTS[(((deg + 22.5) / 45.0) as usize) % 8]
}

/// Where the path-loss model in use came from. Shown in the hunt view because a
/// distance derived from a fitted model and one derived from a literature prior
/// can differ by a factor of three, and the user is entitled to know which they
/// are looking at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelSource {
    Calibrated,
    Priors,
}

pub fn render_hunt(
    style: &Style,
    target: &str,
    address: &str,
    s: &Snapshot,
    model: ModelSource,
) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(CLEAR);

    let status = format!("{:.0}s", s.at.seconds());
    let title = format!(" {target}   {address}");
    let gap = WIDTH.saturating_sub(title.chars().count() + status.chars().count()).max(1);
    let _ = writeln!(
        out,
        "{}",
        style.dim(&format!("{title}{}{status}", " ".repeat(gap)))
    );
    let _ = writeln!(out, "{}", style.dim(&"─".repeat(WIDTH)));

    let Some(dbm) = s.rssi_dbm else {
        let _ = writeln!(out, "\n  No contact yet.\n");
        let _ = writeln!(
            out,
            "  If this persists the device is powered off, out of range,"
        );
        let _ = writeln!(out, "  or shut inside something metal.");
        let _ = writeln!(out, "\n{}", style.dim(&"─".repeat(WIDTH)));
        let _ = writeln!(out, "{}", style.dim(CONTROLS));
        return out;
    };

    let band = Proximity::of(dbm);
    let tone = band_tone(band);

    out.push('\n');
    for (i, row) in big_digits(&format!("{:.0}", dbm)).iter().enumerate() {
        let rider = if i == 2 {
            style.dim("   dBm")
        } else {
            String::new()
        };
        let _ = writeln!(out, "{MARGIN}{}{rider}", style.wrap(row, tone));
    }

    let _ = writeln!(
        out,
        "\n{MARGIN}{}     {}",
        style.wrap(&band.label().to_uppercase(), tone),
        trend_label(style, s.trend)
    );
    let _ = writeln!(
        out,
        "\n{MARGIN}{}",
        style.wrap(&bar(signal_fraction(dbm), 44), tone)
    );

    // Everything below is the fused estimate, and it is kept visually separate
    // from the raw reading above. The reading is measured; the estimate is
    // inferred, and the user is entitled to know which is which.
    out.push('\n');
    match &s.fix {
        // Before the user has walked, the posterior is an annulus centred on
        // them — and the mean of a ring is its centre, so the distance comes out
        // near zero however far the device actually is. It is arithmetically
        // right and completely misleading, so it is withheld until the
        // uncertainty is smaller than the distance it qualifies.
        Some(fix) if s.observations > 4 && fix.ellipse.semi_major < fix.distance_m => {
            let _ = writeln!(
                out,
                "{MARGIN}{}  {:.1} m  {} {:.1} m",
                style.dim("estimate"),
                fix.distance_m,
                // A span, not a percentage: "give or take 4 m" is actionable,
                // "62% confident" is not.
                style.dim("give or take"),
                fix.ellipse.semi_major
            );
            let _ = writeln!(
                out,
                "{MARGIN}{}  {:.1} m across, {:.1} m deep",
                style.dim("        "),
                fix.ellipse.semi_major * 2.0,
                fix.ellipse.semi_minor * 2.0
            );
        }
        Some(fix) if s.observations > 4 => {
            let _ = writeln!(
                out,
                "{MARGIN}{}  {}",
                style.dim("estimate"),
                style.dim("not yet — walk a dogleg to pin it down")
            );
            let _ = writeln!(
                out,
                "{MARGIN}{}  {}",
                style.dim("        "),
                style.dim(&format!("uncertainty ± {:.0} m", fix.ellipse.semi_major))
            );
        }
        _ => {
            let _ = writeln!(
                out,
                "{MARGIN}{}",
                style.dim("estimate  gathering evidence…")
            );
        }
    }

    match &s.bearing {
        Some(b) if b.confidence >= 0.25 => {
            let _ = writeln!(
                out,
                "{MARGIN}{}   {} {:>3.0}° ±{:.0}°  {:.0}% confident",
                style.dim("bearing "),
                compass(b.bearing_rad),
                to_degrees(b.bearing_rad).rem_euclid(360.0),
                to_degrees(b.sigma_rad),
                b.confidence * 100.0
            );
        }
        Some(b) => {
            let _ = writeln!(
                out,
                "{MARGIN}{}   {} ({:.0}% swept)",
                style.dim("bearing "),
                style.wrap("keep turning", AMBER),
                b.coverage * 100.0
            );
        }
        None => {
            let _ = writeln!(
                out,
                "{MARGIN}{}   {}",
                style.dim("bearing "),
                style.dim("turn on the spot to start mapping direction")
            );
        }
    }

    out.push('\n');
    let _ = writeln!(
        out,
        "{}",
        style.dim(&format!(
            "{MARGIN}{} in window via {} · {} total · {} steps · {:.0}% swept{}",
            s.samples_in_window,
            s.rssi_source.map(source_label).unwrap_or("-"),
            s.total_samples,
            s.steps,
            s.heading_coverage * 100.0,
            // Only shown when peers are actually contributing, so a solo hunt
            // is not cluttered with a permanent zero.
            match s.remote_observations {
                0 => String::new(),
                n => format!(" · {n} from peers"),
            }
        ))
    );
    let _ = writeln!(
        out,
        "{}",
        match model {
            ModelSource::Calibrated => style.dim(&format!("{MARGIN}distances from this device's own calibration")),
            ModelSource::Priors => style.wrap(
                &format!("{MARGIN}distances from generic priors — run --calibrate for real ones"),
                AMBER
            ),
        }
    );

    if let Some(age) = s.age_s {
        if !s.is_fresh {
            let _ = writeln!(
                out,
                "{}",
                style.wrap(
                    &format!("{MARGIN}stale — no reading for {age:.0}s"),
                    AMBER
                )
            );
        }
    }
    if s.diverged {
        let _ = writeln!(
            out,
            "{}",
            style.wrap(
                &format!("{MARGIN}readings disagree with the model — press r to reset"),
                RED
            )
        );
    }

    let _ = writeln!(out, "{}", style.dim(&"─".repeat(WIDTH)));
    let _ = writeln!(out, "{}", style.dim(CONTROLS));
    out
}

pub const CONTROLS: &str = concat!(
    " w/a/s/d step N/W/S/E · q/e turn left/right · r reset · Ctrl-C quit\n",
    " Walk a dogleg, not a straight line — a corner resolves direction."
);

/// One row in the survey: label, RSSI, and the TX power it advertises if any.
/// One line of the survey.
#[derive(Debug, Clone)]
pub struct SurveyRow {
    /// Name, or what the advertisement implies, or the address.
    pub label: String,
    pub address: String,
    pub rssi: i16,
    pub tx_power: Option<i16>,
    /// The address rotates, so it identifies nothing across sessions.
    pub randomised: bool,
}

pub fn render_survey(style: &Style, adapter: &str, devices: &[SurveyRow]) -> String {
    let mut out = String::with_capacity(2048);
    out.push_str(CLEAR);
    let _ = writeln!(
        out,
        "Nearby Bluetooth LE devices on {adapter}   [{} tracked]",
        devices.len()
    );
    let _ = writeln!(
        out,
        "{}",
        style.dim("Walk slowly. The one that climbs as you approach is yours.")
    );
    let _ = writeln!(out, "{}", "-".repeat(78));

    if devices.is_empty() {
        let _ = writeln!(out, "  (nothing yet — give it a few seconds)");
    }

    for (i, row) in devices.iter().enumerate().take(16) {
        let band = Proximity::of(row.rssi as f64);
        let _ = writeln!(
            out,
            "{:>2}. {} {:>4} dBm  {:<22} {}",
            i + 1,
            style.wrap(&bar(signal_fraction(row.rssi as f64), 24), band_tone(band)),
            row.rssi,
            band.label(),
            // A device that advertises TX power hands us the calibration
            // reference for free; worth flagging where it happens.
            match row.tx_power {
                Some(tx) => format!("tx {tx} dBm"),
                None => String::new(),
            }
        );
        // The address always appears under the label: it is what identifies the
        // device to every other tool, even when it is the label itself.
        let mut detail = row.address.clone();
        if row.randomised {
            // Saying so beats presenting rotating hex as though it were a name.
            detail.push_str("  (randomised address)");
        }
        let _ = writeln!(out, "    {}  {}", row.label, style.dim(&detail));
    }

    let _ = writeln!(out, "{}", "-".repeat(78));
    let _ = writeln!(
        out,
        "{}",
        style.dim(&format!(
            " {} of {} advertise TX power.  Track one with: superfind <name>  ·  Ctrl-C",
            devices.iter().filter(|r| r.tx_power.is_some()).count(),
            devices.len()
        ))
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_digits_rows_are_equal_width() {
        let rows = big_digits("-72");
        assert_eq!(rows.len(), 5);
        let width = rows[0].chars().count();
        assert!(rows.iter().all(|r| r.chars().count() == width));
    }

    #[test]
    fn big_digits_skips_unrenderable_characters() {
        // Must not panic or produce ragged rows on unexpected input.
        let rows = big_digits("-7x2");
        assert_eq!(rows.len(), 5);
        let width = rows[0].chars().count();
        assert!(rows.iter().all(|r| r.chars().count() == width));
    }

    #[test]
    fn bar_is_always_the_requested_width() {
        for f in [-1.0, 0.0, 0.3, 0.5, 1.0, 2.0, f64::NAN] {
            assert_eq!(bar(f, 20).chars().count(), 20, "failed at {f}");
        }
    }

    #[test]
    fn signal_fraction_clamps_to_the_useful_span() {
        assert_eq!(signal_fraction(-30.0), 1.0);
        assert_eq!(signal_fraction(-100.0), 0.0);
        assert_eq!(signal_fraction(-10.0), 1.0);
        assert_eq!(signal_fraction(-200.0), 0.0);
        assert!((signal_fraction(-65.0) - 0.5).abs() < 0.01);
    }

    #[test]
    fn compass_points_the_right_way() {
        use superfind_core::to_radians;
        assert_eq!(compass(to_radians(0.0)), "N");
        assert_eq!(compass(to_radians(90.0)), "E");
        assert_eq!(compass(to_radians(180.0)), "S");
        assert_eq!(compass(to_radians(270.0)), "W");
        assert_eq!(compass(to_radians(-90.0)), "W");
        assert_eq!(compass(to_radians(45.0)), "NE");
        assert_eq!(compass(to_radians(359.0)), "N");
    }

    #[test]
    fn style_can_be_disabled_for_piping() {
        let plain = Style { enabled: false };
        assert_eq!(plain.dim("hello"), "hello");
        let coloured = Style { enabled: true };
        assert!(coloured.dim("hello").contains('\u{1B}'));
    }
}
