//! JNI bridge: `superfind-core` as seen from Kotlin.
//!
//! Deliberately thin. Nothing here decides anything — it moves values across the
//! boundary and nothing more, so that the tested Rust behaviour is what the app
//! actually runs rather than something re-derived on the Kotlin side.
//!
//! ## The flat-array encoding
//!
//! [`snapshot`] returns one `double[]` rather than a constructed object. A
//! snapshot has about thirty fields and is read eight times a second; building a
//! Java object graph per frame would mean hundreds of JNI calls a second for
//! data that is a handful of primitives. One array, one crossing.
//!
//! The layout is a contract with `Snapshots.decode` in Kotlin. **Append only,
//! never reorder** — the two sides have no shared schema to check them against,
//! so a reordered field would silently show the wrong number rather than fail.
//!
//! ## Safety
//!
//! The tracker outlives the call as a raw pointer handed back to Kotlin as a
//! `long`. Every entry point re-derives a reference from it. This is sound only
//! while Kotlin holds the discipline its `Tracker` interface enforces: one
//! `createTracker` per session, `destroyTracker` exactly once, and no use after.
//! `NaN` is used as the null marker throughout, since every real field here is a
//! finite measurement.

use jni::objects::JClass;
use jni::sys::{jboolean, jdouble, jdoubleArray, jint, jlong, JNI_FALSE, JNI_TRUE};
use jni::JNIEnv;

use superfind_core::{
    Altimeter, FloorDelta, Measurement, PathLoss, Point2, Proximity, RangeSource, RssiSource,
    Snapshot, Timestamp, Tracker, TrackerConfig, Trend,
};

/// Fixed-size prefix of the snapshot encoding. Must match `Snapshots.HEADER`.
const HEADER: usize = 20;

// ---------------------------------------------------------------------------
// Handle plumbing
// ---------------------------------------------------------------------------

/// Reconstitute a tracker reference from the handle Kotlin holds.
///
/// # Safety
/// `handle` must come from [`createTracker`] and not yet have been destroyed.
unsafe fn tracker<'a>(handle: jlong) -> Option<&'a mut Tracker> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut Tracker))
}

macro_rules! with_tracker {
    ($handle:expr, $default:expr, |$t:ident| $body:expr) => {
        match unsafe { tracker($handle) } {
            None => $default,
            Some($t) => $body,
        }
    };
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_createTracker(
    _env: JNIEnv,
    _class: JClass,
    seed: jlong,
) -> jlong {
    let config = TrackerConfig {
        seed: seed as u64,
        ..Default::default()
    };
    Box::into_raw(Box::new(Tracker::new(config))) as jlong
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_destroyTracker(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        // Reclaim the Box and drop it. Kotlin must not use the handle again.
        drop(unsafe { Box::from_raw(handle as *mut Tracker) });
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_reset(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    with_tracker!(handle, (), |t| t.reset())
}

// ---------------------------------------------------------------------------
// Observations
// ---------------------------------------------------------------------------

fn rssi_source(ordinal: jint) -> RssiSource {
    match ordinal {
        0 => RssiSource::ConnectedLink,
        2 => RssiSource::ClassicPoll,
        // Anything unrecognised is treated as the noisier source. Erring
        // towards less trust is the safe direction for an unknown provenance.
        _ => RssiSource::Advertisement,
    }
}

fn range_source(ordinal: jint) -> RangeSource {
    match ordinal {
        0 => RangeSource::Uwb,
        1 => RangeSource::ChannelSounding,
        _ => RangeSource::WifiRtt,
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_observeRssi(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    dbm: jdouble,
    source: jint,
    at_seconds: jdouble,
) -> jboolean {
    with_tracker!(handle, JNI_FALSE, |t| {
        let accepted = t.observe(Measurement::Rssi {
            dbm,
            source: rssi_source(source),
            at: Timestamp(at_seconds),
        });
        if accepted { JNI_TRUE } else { JNI_FALSE }
    })
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_observeRange(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    metres: jdouble,
    source: jint,
    at_seconds: jdouble,
) -> jboolean {
    with_tracker!(handle, JNI_FALSE, |t| {
        let accepted = t.observe(Measurement::Range {
            metres,
            source: range_source(source),
            at: Timestamp(at_seconds),
        });
        if accepted { JNI_TRUE } else { JNI_FALSE }
    })
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_observeAngle(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    bearing_rad: jdouble,
    sigma_rad: jdouble,
    at_seconds: jdouble,
) -> jboolean {
    with_tracker!(handle, JNI_FALSE, |t| {
        let accepted = t.observe(Measurement::Angle {
            bearing_rad,
            sigma_rad,
            at: Timestamp(at_seconds),
        });
        if accepted { JNI_TRUE } else { JNI_FALSE }
    })
}

/// Fold in a peer's reading, taken from a known position in the shared frame.
///
/// The counterpart of `observeRssi`, and the reason the app can locate anything
/// without walking: one observer's likelihood is an annulus, two intersect.
#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_observeRssiFrom(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    dbm: jdouble,
    source: jint,
    x: jdouble,
    y: jdouble,
    at_seconds: jdouble,
) -> jboolean {
    with_tracker!(handle, JNI_FALSE, |t| {
        let accepted = t.observe_from(
            Measurement::Rssi {
                dbm,
                source: rssi_source(source),
                at: Timestamp(at_seconds),
            },
            Point2::new(x, y),
        );
        if accepted { JNI_TRUE } else { JNI_FALSE }
    })
}

// ---------------------------------------------------------------------------
// Movement
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_setHeading(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    radians: jdouble,
    at_seconds: jdouble,
) {
    with_tracker!(handle, (), |t| t.set_heading(radians, Timestamp(at_seconds)))
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_step(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    length_m: jdouble,
    at_seconds: jdouble,
) {
    with_tracker!(handle, (), |t| t.step_of(length_m, Timestamp(at_seconds)))
}

// ---------------------------------------------------------------------------
// Calibration
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_setPathLoss(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    tx_power_1m: jdouble,
    exponent: jdouble,
) {
    with_tracker!(handle, (), |t| t.set_path_loss(PathLoss::new(
        tx_power_1m,
        exponent
    )))
}

/// Fit a path-loss model, returning `[tx_power_1m, exponent, rms_db]` or null.
///
/// Null is returned for a degenerate, implausible or too-noisy fit — refusing to
/// answer is deliberate, because least squares always returns *something* and in
/// a reflective room that something is confidently wrong.
#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_fitPathLoss(
    mut env: JNIEnv,
    _class: JClass,
    distances: jdoubleArray,
    rssi: jdoubleArray,
) -> jdoubleArray {
    let null = std::ptr::null_mut();
    let (Ok(d), Ok(r)) = (read_doubles(&mut env, distances), read_doubles(&mut env, rssi)) else {
        return null;
    };
    if d.len() != r.len() || d.is_empty() {
        return null;
    }

    let samples: Vec<(f64, f64)> = d.into_iter().zip(r).collect();
    let Some(model) = PathLoss::fit(&samples) else {
        return null;
    };
    if !model.is_plausible() {
        return null;
    }
    let Some(rms) = model.residual_rms(&samples) else {
        return null;
    };

    write_doubles(&env, &[model.tx_power_1m, model.exponent, rms])
}

// ---------------------------------------------------------------------------
// Readout
// ---------------------------------------------------------------------------

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_snapshot(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
    at_seconds: jdouble,
) -> jdoubleArray {
    with_tracker!(handle, std::ptr::null_mut(), |t| {
        let snapshot = t.snapshot(Timestamp(at_seconds));
        let sectors = t.sector_means();
        let trail: Vec<(f64, f64, f64)> = t
            .trail()
            .iter()
            .map(|p| (p.position.x, p.position.y, p.heading))
            .collect();
        write_doubles(&env, &encode(&snapshot, &sectors, &trail))
    })
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_particles(
    env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jdoubleArray {
    with_tracker!(handle, std::ptr::null_mut(), |t| {
        let flat: Vec<f64> = t
            .particles()
            .iter()
            .flat_map(|p: &Point2| [p.x, p.y])
            .collect();
        write_doubles(&env, &flat)
    })
}

/// Flatten a snapshot. **Append only** — see the module comment.
fn encode(s: &Snapshot, sectors: &[Option<f64>], trail: &[(f64, f64, f64)]) -> Vec<f64> {
    let mut out = vec![f64::NAN; HEADER];

    out[0] = s.at.seconds();
    out[1] = s.rssi_dbm.unwrap_or(f64::NAN);
    out[2] = s.rssi_source.map(rssi_ordinal).unwrap_or(f64::NAN);
    out[3] = flag(s.is_fresh);
    out[4] = s.age_s.unwrap_or(f64::NAN);
    out[5] = s.crude_distance_m.unwrap_or(f64::NAN);
    out[6] = s.proximity.map(proximity_ordinal).unwrap_or(f64::NAN);
    out[7] = trend_ordinal(s.trend);
    out[8] = s.user.heading;
    out[9] = flag(s.fix.is_some());
    out[10] = flag(s.bearing.is_some());
    out[11] = s.steps as f64;
    out[12] = s.distance_walked_m;
    out[13] = s.heading_coverage;
    out[14] = s.samples_in_window as f64;
    out[15] = s.total_samples as f64;
    out[16] = s.observations as f64;
    out[17] = flag(s.diverged);
    out[18] = s.remote_observations as f64;
    // 19 stays reserved so another scalar can be added without moving the
    // variable-length tail.
    out[19] = 0.0;

    if let Some(fix) = &s.fix {
        out.extend_from_slice(&[
            fix.position.x,
            fix.position.y,
            fix.distance_m,
            fix.bearing_rad,
            fix.bearing_sigma_rad,
            fix.ellipse.semi_major,
            fix.ellipse.semi_minor,
            fix.ellipse.orientation,
            fix.confidence,
            fix.effective_fraction,
        ]);
    }

    if let Some(b) = &s.bearing {
        out.extend_from_slice(&[
            b.bearing_rad,
            b.sigma_rad,
            b.confidence,
            b.coverage,
            b.contrast_db,
            b.samples as f64,
        ]);
    }

    out.push(sectors.len() as f64);
    out.extend(sectors.iter().map(|m| m.unwrap_or(f64::NAN)));

    out.push(trail.len() as f64);
    for (x, y, heading) in trail {
        out.extend_from_slice(&[*x, *y, *heading]);
    }

    out
}

fn flag(b: bool) -> f64 {
    if b {
        1.0
    } else {
        0.0
    }
}

fn rssi_ordinal(s: RssiSource) -> f64 {
    match s {
        RssiSource::ConnectedLink => 0.0,
        RssiSource::Advertisement => 1.0,
        RssiSource::ClassicPoll => 2.0,
    }
}

fn proximity_ordinal(p: Proximity) -> f64 {
    match p {
        Proximity::ArmsReach => 0.0,
        Proximity::SameTable => 1.0,
        Proximity::SameRoom => 2.0,
        Proximity::FarOrObstructed => 3.0,
        Proximity::VeryFarOrShielded => 4.0,
    }
}

fn trend_ordinal(t: Trend) -> f64 {
    match t {
        Trend::Warmer => 0.0,
        Trend::Colder => 1.0,
        Trend::Steady => 2.0,
        Trend::Unknown => 3.0,
    }
}

// ---------------------------------------------------------------------------
// Altimeter
// ---------------------------------------------------------------------------
//
// A separate handle rather than a field on the tracker: pressure readings begin
// arriving before a hunt starts and outlive it, so tying the altimeter's
// lifetime to a tracker would throw away the settled baseline every time the
// user picks a different device.

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_createAltimeter(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    Box::into_raw(Box::new(Altimeter::default())) as jlong
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_destroyAltimeter(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if handle != 0 {
        drop(unsafe { Box::from_raw(handle as *mut Altimeter) });
    }
}

#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_observePressure(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
    pascals: jdouble,
    at_seconds: jdouble,
) -> jboolean {
    match unsafe { altimeter(handle) } {
        None => JNI_FALSE,
        Some(a) => {
            if a.observe(pascals, Timestamp(at_seconds)) {
                JNI_TRUE
            } else {
                JNI_FALSE
            }
        }
    }
}

/// Re-anchor to here, so the answer is "since you started looking".
#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_anchorAltitude(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) {
    if let Some(a) = unsafe { altimeter(handle) } {
        a.anchor();
    }
}

/// Storeys climbed since the anchor: negative below, `NaN` when not yet known.
///
/// One double rather than an enum plus a count, because the Kotlin side has to
/// rebuild the enum anyway and a signed number cannot be got out of order.
#[no_mangle]
pub extern "system" fn Java_dev_superfind_core_NativeCore_floorDelta(
    _env: JNIEnv,
    _class: JClass,
    handle: jlong,
) -> jdouble {
    let Some(a) = (unsafe { altimeter(handle) }) else {
        return f64::NAN;
    };
    match a.floors() {
        None => f64::NAN,
        Some(FloorDelta::SameLevel) => 0.0,
        Some(FloorDelta::Above(n)) => n as f64,
        Some(FloorDelta::Below(n)) => -(n as f64),
    }
}

/// # Safety
/// `handle` must come from [`createAltimeter`] and not yet be destroyed.
unsafe fn altimeter<'a>(handle: jlong) -> Option<&'a mut Altimeter> {
    if handle == 0 {
        return None;
    }
    Some(&mut *(handle as *mut Altimeter))
}

// ---------------------------------------------------------------------------
// Array helpers
// ---------------------------------------------------------------------------

fn read_doubles(env: &mut JNIEnv, array: jdoubleArray) -> Result<Vec<f64>, ()> {
    if array.is_null() {
        return Err(());
    }
    let array = unsafe { jni::objects::JDoubleArray::from_raw(array) };
    let len = env.get_array_length(&array).map_err(|_| ())? as usize;
    let mut buffer = vec![0.0f64; len];
    env.get_double_array_region(&array, 0, &mut buffer)
        .map_err(|_| ())?;
    Ok(buffer)
}

fn write_doubles(env: &JNIEnv, values: &[f64]) -> jdoubleArray {
    let Ok(array) = env.new_double_array(values.len() as i32) else {
        return std::ptr::null_mut();
    };
    if env.set_double_array_region(&array, 0, values).is_err() {
        return std::ptr::null_mut();
    }
    array.into_raw()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The encoding is a contract with Kotlin that no compiler checks. These
    /// pin the offsets so a reordering fails here rather than silently showing
    /// the wrong number on a phone.
    #[test]
    fn header_offsets_are_stable() {
        let mut tracker = Tracker::default();
        tracker.observe(Measurement::Rssi {
            dbm: -63.0,
            source: RssiSource::Advertisement,
            at: Timestamp(0.0),
        });
        let s = tracker.snapshot(Timestamp(0.5));
        let encoded = encode(&s, &[Some(-60.0), None], &[(1.0, 2.0, 0.3)]);

        assert!(encoded.len() > HEADER);
        assert_eq!(encoded[1], -63.0, "rssi must be at index 1");
        assert_eq!(encoded[2], 1.0, "advertisement source ordinal is 1");
        assert_eq!(encoded[3], 1.0, "freshness flag at index 3");
        assert_eq!(encoded[7], trend_ordinal(s.trend));
        assert_eq!(encoded[15], 1.0, "one sample recorded");
    }

    #[test]
    fn absent_values_encode_as_nan_not_zero() {
        // Zero is a plausible reading; NaN cannot be mistaken for one.
        let tracker = Tracker::default();
        let s = tracker.snapshot(Timestamp(1.0));
        let encoded = encode(&s, &[None; 16], &[]);
        assert!(encoded[1].is_nan(), "absent rssi must be NaN");
        assert!(encoded[4].is_nan(), "absent age must be NaN");
        assert_eq!(encoded[9], 0.0, "no fix");
        assert_eq!(encoded[10], 0.0, "no bearing");
    }

    #[test]
    fn tail_layout_follows_the_flags() {
        let tracker = Tracker::default();
        let s = tracker.snapshot(Timestamp(1.0));
        let sectors = vec![Some(-70.0), None, Some(-80.0)];
        let trail = vec![(1.0, 2.0, 0.5), (3.0, 4.0, 0.6)];
        let encoded = encode(&s, &sectors, &trail);

        // No fix and no bearing, so the tail starts immediately after the header.
        let mut cursor = HEADER;
        assert_eq!(encoded[cursor], 3.0, "sector count");
        cursor += 1 + 3;
        assert_eq!(encoded[cursor], 2.0, "trail length");
        assert_eq!(encoded[cursor + 1], 1.0);
        assert_eq!(encoded[cursor + 2], 2.0);
    }

    #[test]
    fn unknown_source_ordinals_degrade_to_the_least_trusted() {
        assert_eq!(rssi_source(0), RssiSource::ConnectedLink);
        assert_eq!(rssi_source(1), RssiSource::Advertisement);
        assert_eq!(rssi_source(99), RssiSource::Advertisement);
        assert_eq!(range_source(0), RangeSource::Uwb);
        assert_eq!(range_source(42), RangeSource::WifiRtt);
    }
}
