//! superfind — locate a Bluetooth device by signal strength.
//!
//! A validation harness as much as a tool. The interesting code lives in
//! `superfind-core`, which is platform-free and heavily tested against
//! synthetic traces; this binary is how that code first meets a real radio.
//!
//! ## The honest caveat about hunt mode
//!
//! A laptop has no compass and no step counter, so the movement a phone would
//! sense automatically has to be typed. `w/a/s/d` steps north, west, south or
//! east; `q`/`e` turn on the spot. That sounds like a toy, and for everyday use
//! it is — but it makes the *ground truth exact*, which a phone never can. Walk
//! a known path and any error in the estimate is the filter's, not the
//! pedometer's. That is exactly what you want when deciding whether the fusion
//! is right before porting it to Android.

mod ble;
mod peers;
mod sound;
mod calibration;
mod ui;

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures_util::StreamExt;
use superfind_core::{to_radians, Measurement, RssiSource, Timestamp, Tracker, TrackerConfig};

const USAGE: &str = "\
superfind — locate a Bluetooth device by signal strength

  superfind                    survey every nearby BLE device
  superfind <name|address>     hunt one device
  superfind --list             devices BlueZ already knows about
  superfind --calibrate <name> fit this device's path loss at known distances

  --adapter <hciN>   use a specific adapter (default: the first powered one)
  --step <metres>    distance per keypress in hunt mode (default: 1.0)
  --no-calibration   ignore any saved calibration, use the built-in priors
  --sound            click faster as you get closer (off by default)
  --sound-fastest <ms>  cadence at arm's reach (default 70)
  --sound-slowest <ms>  cadence at the edge of range (default 1400)
  --sound-volume <0-1>  loudness (default 0.9)
  --share <session>  pool readings with other devices hunting the same thing
  --at <x,y>         where this device is, in metres, in the shared frame
  -h, --help         this message

Calibration is stored in ~/.config/superfind/calibration.json and is used
automatically when hunting a device that has one.
";

#[derive(Debug)]
struct Args {
    query: Option<String>,
    adapter: Option<String>,
    list: bool,
    calibrate: bool,
    no_calibration: bool,
    step_m: f64,
    /// Session name for sharing observations with other devices.
    share: Option<String>,
    /// Audible proximity cadence, and how fast it may get at close range.
    sound: bool,
    sound_min_ms: Option<u32>,
    sound_max_ms: Option<u32>,
    sound_volume: Option<f64>,
    /// This device's position in the shared frame, in metres.
    at: Option<superfind_core::Point2>,
}

/// A name for this device in peer reports.
///
/// The hostname is what a person recognises on their own network, but it is not
/// unique *per process* — and two instances on one machine is a configuration
/// worth supporting. Since a peer discards its own packets by matching this
/// name, a shared name would make each instance discard the other's readings
/// too, and the whole thing would silently do nothing. Hence the pid.
fn peer_name() -> String {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "peer".to_string());
    format!("{host}-{}", std::process::id())
}

fn parse_args<I: Iterator<Item = String>>(it: I) -> Result<Args> {
    let mut args = Args {
        query: None,
        adapter: None,
        list: false,
        calibrate: false,
        no_calibration: false,
        step_m: 1.0,
        share: None,
        at: None,
        sound: false,
        sound_min_ms: None,
        sound_max_ms: None,
        sound_volume: None,
    };
    let mut it = it.peekable();

    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--list" => args.list = true,
            "--calibrate" => args.calibrate = true,
            "--no-calibration" => args.no_calibration = true,
            "--sound" => args.sound = true,
            "--sound-volume" => {
                let raw = it.next().context("--sound-volume needs a value from 0 to 1")?;
                args.sound_volume = Some(
                    raw.parse()
                        .with_context(|| format!("--sound-volume: '{raw}' is not a number"))?,
                );
            }
            "--sound-fastest" => {
                let raw = it.next().context("--sound-fastest needs milliseconds")?;
                args.sound_min_ms = Some(
                    raw.parse()
                        .with_context(|| format!("--sound-fastest: '{raw}' is not a number"))?,
                );
            }
            "--sound-slowest" => {
                let raw = it.next().context("--sound-slowest needs milliseconds")?;
                args.sound_max_ms = Some(
                    raw.parse()
                        .with_context(|| format!("--sound-slowest: '{raw}' is not a number"))?,
                );
            }
            "--share" => {
                args.share = Some(it.next().context("--share needs a session name")?)
            }
            "--at" => {
                let raw = it.next().context("--at needs a position, e.g. --at 4,2.5")?;
                args.at = Some(peers::parse_position(&raw)?);
            }
            "--adapter" => {
                args.adapter = Some(it.next().context("--adapter needs a value, e.g. hci0")?)
            }
            "--step" => {
                let raw = it.next().context("--step needs a value in metres")?;
                args.step_m = raw
                    .parse::<f64>()
                    .with_context(|| format!("--step: '{raw}' is not a number"))?;
                anyhow::ensure!(args.step_m > 0.0, "--step must be positive");
            }
            // Reject unknown flags rather than silently treating one as a device
            // name — a typo should not start a long hunt for nothing.
            other if other.starts_with('-') => {
                anyhow::bail!("unknown option '{other}'\n\n{USAGE}")
            }
            other => {
                anyhow::ensure!(
                    args.query.is_none(),
                    "expected one device name, got '{}' and '{other}'",
                    args.query.as_deref().unwrap_or_default()
                );
                args.query = Some(other.to_string());
            }
        }
    }
    Ok(args)
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        let _ = disable_raw_mode();
        eprintln!("superfind: {e:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = parse_args(std::env::args().skip(1))?;
    let style = ui::Style::detect();

    let scanner = ble::Scanner::open(args.adapter.as_deref()).await?;

    if args.list {
        return list(&scanner).await;
    }

    scanner.start().await?;

    let result = match (args.calibrate, args.query.clone()) {
        (true, Some(query)) => calibrate(&scanner, &query).await,
        (true, None) => Err(anyhow::anyhow!(
            "--calibrate needs a device name or address to calibrate against"
        )),
        (false, Some(query)) => {
            // Sharing is opt-in: these packets carry the hunted address and
            // rough positions, and anything on the network can read them.
            let link = match &args.share {
                Some(session) => {
                    let name = peer_name();
                    let link = peers::PeerLink::open(session, &name, args.at)?;
                    match args.at {
                        Some(at) => eprintln!(
                            "sharing session '{session}' as '{name}' at {:.1},{:.1}",
                            at.x, at.y
                        ),
                        None => eprintln!(
                            "sharing session '{session}' as '{name}' — no --at given, so this \n\
                             device will fuse peers' readings but contribute none of its own"
                        ),
                    }
                    // A mistyped session name otherwise produces a hunt that
                    // looks like it is pooling readings and is not.
                    match link.wait_for_peer(Duration::from_millis(700)) {
                        Ok(true) => eprintln!("heard another peer on '{}'", link.anchor().session),
                        Ok(false) => eprintln!(
                            "no other peer on '{}' yet — readings will be shared \
                             as soon as one joins",
                            link.anchor().session
                        ),
                        Err(_) => {}
                    }
                    Some(link)
                }
                None => None,
            };
            let mut feedback = superfind_core::FeedbackConfig {
                enabled: args.sound,
                ..Default::default()
            };
            if let Some(ms) = args.sound_min_ms {
                feedback.min_interval_ms = ms;
            }
            if let Some(ms) = args.sound_max_ms {
                feedback.max_interval_ms = ms;
            }
            if let Some(v) = args.sound_volume {
                feedback.volume = v;
            }
            let feedback = feedback.sanitised();

            hunt(
                &scanner,
                &style,
                &query,
                args.step_m,
                args.no_calibration,
                link,
                feedback,
            )
            .await
        }
        (false, None) => survey(&scanner, &style).await,
    };

    scanner.stop().await;
    let _ = disable_raw_mode();
    result
}

async fn list(scanner: &ble::Scanner) -> Result<()> {
    let mut devices = scanner.devices().await?;
    devices.sort_by_key(|d| std::cmp::Reverse(d.rssi.unwrap_or(i16::MIN)));

    if devices.is_empty() {
        println!("No devices known to BlueZ on {}.", scanner.adapter_name());
        println!("Run `superfind` with no arguments to scan for nearby ones.");
        return Ok(());
    }

    let store = calibration::Store::load();

    println!(
        "{:<28} {:<20} {:>6}  {:<11} CALIBRATION",
        "NAME", "ADDRESS", "RSSI", "STATE"
    );
    for d in devices {
        let state = match (d.connected, d.paired) {
            (true, _) => "connected",
            (false, true) => "paired",
            _ => "",
        };
        let calibration = match store.get(&d.address) {
            Some(e) => format!(
                "{:.0} dBm @1m, n={:.2} (±{:.1} dB)",
                e.tx_power_1m, e.exponent, e.rms_db
            ),
            None => "-".to_string(),
        };
        println!(
            "{:<28} {:<20} {:>6}  {:<11} {}",
            d.name.as_deref().unwrap_or("-"),
            d.address,
            d.rssi.map(|r| r.to_string()).unwrap_or_else(|| "-".into()),
            state,
            calibration
        );
    }

    if store.is_empty() {
        println!(
            "\nNo devices calibrated yet. Distances will use the built-in priors\n\
             (-59 dBm at 1 m, exponent 2.8), which can be out by a factor of three.\n\
             Fit a real one with:  superfind --calibrate <name or address>"
        );
    } else {
        println!("\n{} device(s) calibrated:", store.iter().count());
        for (address, e) in store.iter() {
            println!(
                "  {address}  {}  {} samples",
                e.name.as_deref().unwrap_or("-"),
                e.samples
            );
        }
    }

    println!("\nTrack one with:  superfind <name or address>");
    Ok(())
}

async fn survey(scanner: &ble::Scanner, style: &ui::Style) -> Result<()> {
    let mut adverts = scanner.adverts().await?;
    let mut seen: HashMap<String, (ui::SurveyRow, Instant)> = HashMap::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(500));

    loop {
        tokio::select! {
            Some(a) = adverts.recv() => {
                seen.insert(
                    a.path.clone(),
                    (
                        ui::SurveyRow {
                            label: a.label(),
                            address: a.address.clone(),
                            rssi: a.rssi,
                            tx_power: a.tx_power,
                            randomised: a.randomised_address(),
                        },
                        a.at,
                    ),
                );
            }
            _ = ticker.tick() => {
                // Devices go quiet; drop them rather than show a stale list.
                seen.retain(|_, (_, at)| at.elapsed() < Duration::from_secs(20));
                let mut rows: Vec<ui::SurveyRow> =
                    seen.values().map(|(row, _)| row.clone()).collect();
                rows.sort_by_key(|row| std::cmp::Reverse(row.rssi));
                let adapter = format!("{} ({})", scanner.adapter_name(), scanner.adapter_address());
                print!("{}", ui::render_survey(style, &adapter, &rows));
                std::io::stdout().flush().ok();
            }
            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

/// Distances the guided calibration walks through. Geometric rather than linear:
/// path loss is linear in `log10(d)`, so doubling each step spaces the samples
/// evenly along the axis the regression actually fits.
const CALIBRATION_DISTANCES_M: [f64; 4] = [1.0, 2.0, 4.0, 8.0];
const SAMPLES_PER_DISTANCE: usize = 25;
const CALIBRATION_TIMEOUT_S: u64 = 45;

async fn calibrate(scanner: &ble::Scanner, query: &str) -> Result<()> {
    let mut adverts = scanner.adverts().await?;

    println!("Calibrating '{query}'.\n");
    println!("You will be asked to place the device at four distances. Accuracy of");
    println!("the distances is what matters — pace them out or use a tape measure.");
    println!("Stand still while sampling, and keep your body out of the line between");
    println!("the two, because you attenuate the signal by 5-15 dB.\n");
    println!("An open room works far better than a corridor or a desk against a wall.\n");

    let mut samples: Vec<(f64, f64)> = Vec::new();
    let mut name: Option<String> = None;
    let mut address: Option<String> = None;
    let mut advertised_tx: Option<i16> = None;

    for distance in CALIBRATION_DISTANCES_M {
        print!("Place the device {distance:.0} m away, then press Enter (or 's' to skip): ");
        std::io::stdout().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim().eq_ignore_ascii_case("s") {
            println!("  skipped\n");
            continue;
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(CALIBRATION_TIMEOUT_S);
        let mut collected = 0usize;

        while collected < SAMPLES_PER_DISTANCE {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, adverts.recv()).await {
                Ok(Some(a)) if a.matches(query) => {
                    if name.is_none() {
                        name = a.name.clone();
                    }
                    address = Some(a.address.clone());
                    if let Some(tx) = a.tx_power {
                        advertised_tx = Some(tx);
                    }
                    samples.push((distance, a.rssi as f64));
                    collected += 1;
                    print!("\r  {collected}/{SAMPLES_PER_DISTANCE} samples   ");
                    std::io::stdout().flush().ok();
                }
                Ok(Some(_)) => continue,
                Ok(None) => break,
                Err(_) => break,
            }
        }

        if collected == 0 {
            println!("\r  no advertisements heard in {CALIBRATION_TIMEOUT_S}s — is it awake?");
        } else {
            println!("\r  {collected} samples collected            ");
        }
        println!();
    }

    let Some(address) = address else {
        anyhow::bail!("never heard from '{query}' — nothing to calibrate");
    };

    match calibration::fit(&samples) {
        Ok((model, rms)) => {
            println!("Fit:");
            println!("  1 m reference   {:.1} dBm", model.tx_power_1m);
            println!("  path exponent   {:.2}", model.exponent);
            println!("  residual        {rms:.1} dB RMS over {} samples", samples.len());
            println!();
            println!("For reference, the built-in priors are -59.0 dBm and 2.80.");
            if let Some(tx) = advertised_tx {
                // Devices that advertise TX power are stating their output at
                // the antenna, not the RSSI a receiver sees at 1 m. The gap
                // between the two is the 1 m path loss plus receiver gain, so
                // this is a cross-check, not a substitute.
                println!(
                    "\nThe device also advertises {tx} dBm TX power; our fitted 1 m\n\
                     reference of {:.1} dBm implies {:.1} dB of loss and receiver gain.",
                    model.tx_power_1m,
                    tx as f64 - model.tx_power_1m
                );
            }
            if rms > 5.0 {
                println!(
                    "\nA residual above 5 dB means a lot of multipath. Usable, but a\n\
                     tidier room would give a better fit."
                );
            }

            let mut store = calibration::Store::load();
            store.insert(
                &address,
                calibration::Entry {
                    name: name.clone(),
                    tx_power_1m: model.tx_power_1m,
                    exponent: model.exponent,
                    rms_db: rms,
                    samples: samples.len(),
                },
            );
            let path = store.save()?;
            println!("\nSaved to {}", path.display());
            println!("Hunting this device will now use it automatically.");
            Ok(())
        }
        Err(reason) => {
            println!("Not saving this calibration: {reason}");
            println!("\nThe built-in priors remain in use. Nothing has been changed.");
            Ok(())
        }
    }
}

async fn hunt(
    scanner: &ble::Scanner,
    style: &ui::Style,
    query: &str,
    step_m: f64,
    ignore_calibration: bool,
    link: Option<peers::PeerLink>,
    feedback: superfind_core::FeedbackConfig,
) -> Result<()> {
    let mut adverts = scanner.adverts().await?;
    let mut config = TrackerConfig::default();
    let store = calibration::Store::load();

    // The calibration is keyed by address, but the user may have typed a name,
    // so it can only be applied once we have heard from the device. Resolved
    // lazily below on the first matching advert.
    let mut calibration_applied = ignore_calibration;
    let mut model_source = ui::ModelSource::Priors;

    let mut tracker = Tracker::new(config);
    let started = Instant::now();
    let mut address = String::from("(searching…)");

    // Started before raw mode so any player noise on startup is not painted
    // over the alternate screen.
    let speaker = if feedback.enabled {
        let speaker = sound::Speaker::open(feedback.volume);
        // Which mode is in use, rather than leaving somebody wondering why it
        // is quiet: with no player available this degrades to terminal bells,
        // which many terminals render as a flash or drop entirely.
        if !speaker.is_audible() {
            eprintln!(
                "no audio player found (tried aplay, pw-play) — falling back to \
                 terminal bells, which some terminals ignore"
            );
        }
        Some(speaker)
    } else {
        None
    };

    // Raw mode so single keypresses drive movement without waiting for Enter.
    enable_raw_mode().context("could not put the terminal into raw mode")?;
    let mut keys = crossterm::event::EventStream::new();
    let mut ticker = tokio::time::interval(Duration::from_millis(250));

    let elapsed = |started: &Instant| Timestamp(started.elapsed().as_secs_f64());

    loop {
        tokio::select! {
            Some(a) = adverts.recv() => {
                if !a.matches(query) {
                    continue;
                }
                address = a.address.clone();

                // First sighting resolves the address, which is what the
                // calibration store is keyed by. Rebuilding the tracker here is
                // safe: no evidence has accumulated yet.
                if !calibration_applied {
                    calibration_applied = true;
                    if let Some(entry) = store.get(&a.address) {
                        config.path_loss = entry.model();
                        tracker = Tracker::new(config);
                        model_source = ui::ModelSource::Calibrated;
                    }
                }

                // Passively observed advertisements — not a connected link, and
                // labelled as such so the filter widens its noise accordingly.
                let now = elapsed(&started);
                tracker.observe(Measurement::Rssi {
                    dbm: a.rssi as f64,
                    source: RssiSource::Advertisement,
                    at: now,
                });

                // Pass it on, so peers hunting the same device can intersect
                // their ring with ours.
                if let Some(link) = &link {
                    link.share(&a.address, a.rssi as f64, now.0, peers::RssiKind::Advert);
                }
            }

            Some(Ok(event)) = keys.next() => {
                if let Event::Key(KeyEvent { code, modifiers, .. }) = event {
                    if modifiers.contains(KeyModifiers::CONTROL) && code == KeyCode::Char('c') {
                        return Ok(());
                    }
                    let t = elapsed(&started);
                    match code {
                        KeyCode::Char('w') => {
                            tracker.set_heading(0.0, t);
                            tracker.step_of(step_m, t);
                        }
                        KeyCode::Char('d') => {
                            tracker.set_heading(to_radians(90.0), t);
                            tracker.step_of(step_m, t);
                        }
                        KeyCode::Char('s') => {
                            tracker.set_heading(to_radians(180.0), t);
                            tracker.step_of(step_m, t);
                        }
                        KeyCode::Char('a') => {
                            tracker.set_heading(to_radians(270.0), t);
                            tracker.step_of(step_m, t);
                        }
                        KeyCode::Char('q') => {
                            let h = tracker.user_pose().heading - to_radians(22.5);
                            tracker.set_heading(h, t);
                        }
                        KeyCode::Char('e') => {
                            let h = tracker.user_pose().heading + to_radians(22.5);
                            tracker.set_heading(h, t);
                        }
                        KeyCode::Char('r') => tracker.reset(),
                        KeyCode::Esc => return Ok(()),
                        _ => {}
                    }
                }
            }

            _ = ticker.tick() => {
                // Peers' readings, folded in at *their* positions. This is what
                // collapses the annulus without anyone walking.
                if let Some(link) = &link {
                    for report in link.drain(&address) {
                        if let Some(at) = report.at {
                            tracker.observe_from(report.measurement(), at);
                        }
                    }
                }
                let snapshot = tracker.snapshot(elapsed(&started));

                // Silence here means the signal has gone stale, never that the
                // device is merely far away — a distant one still clicks slowly.
                if let Some(speaker) = &speaker {
                    speaker.play(superfind_core::ProximityCue::for_snapshot(
                        &snapshot, &feedback,
                    ));
                }
                // Raw mode means a bare \n does not imply a carriage return.
                let frame = ui::render_hunt(style, query, &address, &snapshot, model_source)
                    .replace('\n', "\r\n");
                print!("{frame}");
                std::io::stdout().flush().ok();
            }

            _ = tokio::signal::ctrl_c() => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Args> {
        parse_args(args.iter().map(|s| s.to_string()))
    }

    #[test]
    fn bare_invocation_is_survey_mode() {
        let a = parse(&[]).unwrap();
        assert!(a.query.is_none());
        assert!(!a.list);
    }

    #[test]
    fn a_name_selects_hunt_mode() {
        assert_eq!(parse(&["pixel"]).unwrap().query.as_deref(), Some("pixel"));
    }

    #[test]
    fn unknown_flags_are_rejected() {
        let e = parse(&["--bogus"]).unwrap_err().to_string();
        assert!(e.contains("unknown option '--bogus'"), "got: {e}");
    }

    #[test]
    fn two_names_are_rejected() {
        assert!(parse(&["one", "two"]).is_err());
    }

    #[test]
    fn adapter_and_step_are_parsed() {
        let a = parse(&["--adapter", "hci1", "--step", "0.5", "tag"]).unwrap();
        assert_eq!(a.adapter.as_deref(), Some("hci1"));
        assert_eq!(a.step_m, 0.5);
        assert_eq!(a.query.as_deref(), Some("tag"));
    }

    #[test]
    fn a_missing_or_bad_flag_value_is_an_error_not_a_default() {
        assert!(parse(&["--adapter"]).is_err());
        assert!(parse(&["--step"]).is_err());
        assert!(parse(&["--step", "banana"]).is_err());
        assert!(parse(&["--step", "0"]).is_err());
        assert!(parse(&["--step", "-2"]).is_err());
    }
}
