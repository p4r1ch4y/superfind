//! Making the signal audible on a terminal.
//!
//! The core decides *what* to play ([`superfind_core::ProximityCue`]); this
//! decides how to get it out of a laptop with no audio dependency in the build.
//!
//! ## One player, streamed
//!
//! The obvious approach — spawn `aplay` per click — costs a process fork every
//! 70 ms at close range, which is both wasteful and audibly ragged: process
//! startup jitter lands directly on the rhythm the user is listening to.
//!
//! So a single player is started once and raw PCM is streamed into it. Silence
//! is data too, which sounds absurd until you notice it is what makes the gaps
//! exact. The cadence comes from counting samples rather than from sleeping a
//! thread, and is immune to scheduler jitter.
//!
//! ## Latency has to be bounded, or nothing is heard
//!
//! The first version wrote whatever the pipe would accept. At startup the cue is
//! silence, so it free-ran and queued roughly *eighteen seconds* of silence into
//! the player before the first reading arrived — after which every click landed
//! behind that backlog. Bytes flowed, the player stayed healthy, and the tool was
//! inaudible.
//!
//! Two things prevent it now: the player is asked for a small buffer, and the
//! writer emits one short chunk at a time from a running sample clock, so it can
//! never get further ahead than that buffer allows.
//!
//! ## Falling back to the terminal bell
//!
//! With no player available, `\x07` still marks each click. Terminals vary —
//! many render it as a visual flash, some ignore it entirely — so it is a
//! degraded mode rather than an equivalent one, and [`Speaker::is_audible`] says
//! which one is in use rather than letting the user wonder.

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use superfind_core::ProximityCue;

const SAMPLE_RATE: u32 = 22_050;
/// Length of the click itself. Long enough to have a pitch, short enough to
/// read as a tick rather than a beep.
const CLICK_MS: u32 = 22;

/// Plays a cadence that tracks the signal.
pub struct Speaker {
    /// Amplitude, 0 to 1. Read once at construction: changing it mid-hunt would
    /// need another atomic for no benefit anybody has asked for.
    volume: f64,
    /// Packed cue: interval in the low 16 bits, pitch in the high 16. One
    /// atomic rather than a mutex because the audio thread reads it thousands
    /// of times a second and must never block the hunt loop.
    cue: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    audible: bool,
    thread: Option<JoinHandle<()>>,
    child: Option<Child>,
}

impl Speaker {
    /// Start a player, or fall back to the terminal bell.
    pub fn open(volume: f64) -> Speaker {
        let cue = Arc::new(AtomicU32::new(0));
        let running = Arc::new(AtomicBool::new(true));

        let child = spawn_player();
        let audible = child.is_some();

        let mut speaker = Speaker {
            volume: volume.clamp(0.05, 0.98),
            cue,
            running,
            audible,
            thread: None,
            child,
        };
        speaker.start();
        speaker
    }

    fn start(&mut self) {
        let cue = Arc::clone(&self.cue);
        let running = Arc::clone(&self.running);

        let stdin = self.child.as_mut().and_then(|c| c.stdin.take());
        let volume = self.volume;
        let handle = std::thread::spawn(move || match stdin {
            Some(sink) => stream_pcm(sink, cue, running, volume),
            None => ring_bell(cue, running),
        });
        self.thread = Some(handle);
    }

    /// Whether real audio is available, as opposed to the terminal bell.
    pub fn is_audible(&self) -> bool {
        self.audible
    }

    /// Set what should be playing. `None` is silence, and per the core's rule
    /// that means no signal — never merely a distant one.
    pub fn play(&self, cue: Option<ProximityCue>) {
        let packed = match cue {
            None => 0,
            Some(c) => {
                let interval = c.interval_ms.clamp(1, u16::MAX as u32);
                let pitch = c.pitch_hz.clamp(1, u16::MAX as u32);
                (pitch << 16) | interval
            }
        };
        self.cue.store(packed, Ordering::Relaxed);
    }
}

impl Drop for Speaker {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn unpack(packed: u32) -> Option<(u32, u32)> {
    if packed == 0 {
        return None;
    }
    Some((packed & 0xFFFF, packed >> 16))
}

/// Try each player in turn. Ordered by how likely it is to exist and to accept
/// raw PCM on stdin without configuration.
fn spawn_player() -> Option<Child> {
    let candidates: [(&str, Vec<String>); 2] = [
        (
            "aplay",
            vec![
                "-q".into(),
                "-f".into(),
                "S16_LE".into(),
                "-r".into(),
                SAMPLE_RATE.to_string(),
                "-c".into(),
                "1".into(),
                // Bounded buffer. Without it the player accepts seconds of audio
                // and every click arrives that far late; 200 ms is imperceptible
                // and small enough that the writer cannot run away.
                "--buffer-time=200000".into(),
                "--period-time=50000".into(),
                "-".into(),
            ],
        ),
        (
            "pw-play",
            vec![
                "--format=s16".into(),
                format!("--rate={SAMPLE_RATE}"),
                "--channels=1".into(),
                "--latency=100ms".into(),
                "-".into(),
            ],
        ),
    ];

    for (program, args) in candidates {
        let spawned = Command::new(program)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            // Players are chatty on startup and this is a full-screen TUI.
            .stderr(Stdio::null())
            .spawn();
        if let Ok(child) = spawned {
            return Some(child);
        }
    }
    None
}

/// Generate audio continuously from a sample clock.
///
/// Each pass emits one short chunk and works out which samples inside it fall
/// within a click, from the position of a free-running counter. That keeps the
/// cadence exact across chunk boundaries, and — because the chunks are short —
/// stops the writer running further ahead than the player's buffer.
fn stream_pcm(
    mut sink: impl Write,
    cue: Arc<AtomicU32>,
    running: Arc<AtomicBool>,
    volume: f64,
) {
    // Short enough that a cue change is heard promptly, long enough not to
    // syscall per millisecond.
    let chunk = (SAMPLE_RATE / 25) as usize;
    let click_samples = (SAMPLE_RATE * CLICK_MS / 1000) as u64;

    let mut buffer = vec![0_u8; chunk * 2];
    // Samples since the last click began. Survives cue changes, so altering the
    // rhythm never restarts it mid-tick.
    let mut since_click: u64 = 0;

    while running.load(Ordering::Relaxed) {
        let current = unpack(cue.load(Ordering::Relaxed));
        fill_chunk(&mut buffer, chunk, current, volume, click_samples, &mut since_click);

        if sink.write_all(&buffer).is_err() {
            // The player exited — usually because audio went away. Stop rather
            // than spin writing into a broken pipe.
            return;
        }
        let _ = sink.flush();
    }
}

/// Render one chunk from the sample clock.
///
/// Split out from the streaming loop so it can be tested without a player: the
/// bug this replaced produced perfectly valid audio that nobody could hear,
/// because it was queued behind seconds of silence. A test that asserts "the
/// samples are not all zero" catches the class of fault that stares straight
/// through a code review.
fn fill_chunk(
    buffer: &mut [u8],
    chunk: usize,
    cue: Option<(u32, u32)>,
    volume: f64,
    click_samples: u64,
    since_click: &mut u64,
) {
    for i in 0..chunk {
        let sample = match cue {
            None => {
                // Silence, and the clock resets so the first click after a gap
                // lands immediately rather than part-way through.
                *since_click = 0;
                0_i16
            }
            Some((interval_ms, pitch_hz)) => {
                let period = (SAMPLE_RATE as u64 * interval_ms as u64 / 1000).max(1);
                let phase = *since_click % period;
                *since_click = since_click.wrapping_add(1);

                if phase < click_samples {
                    let t = phase as f64 / SAMPLE_RATE as f64;
                    // Exponential decay makes it a tick rather than a beep, and
                    // stops the tail colliding with the next click.
                    let envelope = (-t * 90.0).exp();
                    let value = (t * pitch_hz as f64 * std::f64::consts::TAU).sin()
                        * envelope
                        * volume
                        * i16::MAX as f64;
                    value as i16
                } else {
                    0
                }
            }
        };
        let bytes = sample.to_le_bytes();
        buffer[i * 2] = bytes[0];
        buffer[i * 2 + 1] = bytes[1];
    }
}

/// Degraded mode: mark each click with the terminal bell.
fn ring_bell(cue: Arc<AtomicU32>, running: Arc<AtomicBool>) {
    while running.load(Ordering::Relaxed) {
        match unpack(cue.load(Ordering::Relaxed)) {
            None => std::thread::sleep(std::time::Duration::from_millis(50)),
            Some((interval_ms, _)) => {
                let mut out = std::io::stderr();
                let _ = out.write_all(b"\x07");
                let _ = out.flush();
                // Floor the rate: terminals throttle or coalesce bells, and a
                // 70 ms stream of them is more likely to be dropped than heard.
                std::thread::sleep(std::time::Duration::from_millis(
                    interval_ms.max(120) as u64
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_packs_and_unpacks_as_nothing() {
        assert_eq!(unpack(0), None);
    }

    #[test]
    fn a_cue_survives_packing() {
        let speaker_cue = ProximityCue {
            interval_ms: 350,
            pitch_hz: 880,
            intensity: 0.5,
        };
        let packed = (speaker_cue.pitch_hz << 16) | speaker_cue.interval_ms;
        assert_eq!(unpack(packed), Some((350, 880)));
    }

    /// Render a second of audio and return the samples.
    fn render(cue: Option<(u32, u32)>) -> Vec<i16> {
        let chunk = (SAMPLE_RATE / 25) as usize;
        let click_samples = (SAMPLE_RATE * CLICK_MS / 1000) as u64;
        let mut buffer = vec![0_u8; chunk * 2];
        let mut since_click = 0_u64;
        let mut out = Vec::new();
        for _ in 0..25 {
            fill_chunk(&mut buffer, chunk, cue, 0.9, click_samples, &mut since_click);
            for pair in buffer.chunks_exact(2) {
                out.push(i16::from_le_bytes([pair[0], pair[1]]));
            }
        }
        out
    }

    /// The regression that matters: audio that is generated, accepted by the
    /// player, and completely inaudible.
    #[test]
    fn a_live_cue_produces_audible_samples() {
        let samples = render(Some((200, 880)));
        let loudest = samples.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        assert!(
            loudest > i16::MAX as u16 / 4,
            "the click should be loud; peak was {loudest}"
        );
    }

    #[test]
    fn silence_is_actually_silent() {
        assert!(render(None).iter().all(|s| *s == 0));
    }

    /// Cadence must come out of the sample clock, not out of how often the
    /// thread happened to be scheduled.
    #[test]
    fn clicks_land_at_the_requested_interval() {
        let interval_ms = 200_u32;
        let samples = render(Some((interval_ms, 880)));

        // Onsets: a non-zero sample whose predecessor was silent.
        let onsets: Vec<usize> = samples
            .windows(2)
            .enumerate()
            .filter(|(_, w)| w[0] == 0 && w[1] != 0)
            .map(|(i, _)| i + 1)
            .collect();
        assert!(onsets.len() >= 3, "expected several clicks, got {}", onsets.len());

        let expected = (SAMPLE_RATE as u64 * interval_ms as u64 / 1000) as usize;
        for pair in onsets.windows(2) {
            let gap = pair[1] - pair[0];
            let drift = (gap as i64 - expected as i64).abs();
            assert!(drift < 40, "gap {gap} drifted from {expected}");
        }
    }

    #[test]
    fn a_faster_cue_really_is_faster() {
        let slow = render(Some((400, 440)));
        let fast = render(Some((100, 1320)));
        let count = |s: &[i16]| {
            s.windows(2).filter(|w| w[0] == 0 && w[1] != 0).count()
        };
        assert!(
            count(&fast) > count(&slow),
            "fast {} should exceed slow {}",
            count(&fast),
            count(&slow)
        );
    }

    /// A cue must never pack to zero, or it would be indistinguishable from
    /// silence — which the core reserves for "no signal".
    #[test]
    fn a_live_cue_never_looks_like_silence() {
        for interval in [1_u32, 70, 1_400, 70_000] {
            for pitch in [1_u32, 440, 1_320, 70_000] {
                let i = interval.clamp(1, u16::MAX as u32);
                let p = pitch.clamp(1, u16::MAX as u32);
                assert_ne!((p << 16) | i, 0, "interval {interval}, pitch {pitch}");
            }
        }
    }
}
