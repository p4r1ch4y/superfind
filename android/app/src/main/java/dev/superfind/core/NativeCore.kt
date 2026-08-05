package dev.superfind.core

/**
 * The bridge to `superfind-core`.
 *
 * The fusion filter is the product, and it lives in Rust so that one tested
 * implementation serves the Linux CLI, this app, and whatever comes next. This
 * file is the only place that knows the boundary exists.
 *
 * ## Why it degrades instead of crashing
 *
 * Building the native library needs the Android NDK. Until that is wired into
 * the build, `System.loadLibrary` will fail, and the honest response is to say
 * so rather than to die on launch or — far worse — to quietly substitute a
 * simpler estimator and present its output as if it were the real thing.
 *
 * So [available] is a fact the UI can read and report. When it is false the app
 * still scans, still shows live signal strength, still ranks nearby devices —
 * everything that does not require fusion. What disappears is the fix, the
 * ellipse and the inferred bearing, and the UI says exactly that.
 */
object NativeCore {

    /** Whether the Rust core is present and usable in this build. */
    val available: Boolean = runCatching {
        System.loadLibrary("superfind_jni")
        true
    }.getOrDefault(false)

    /** Why it is not, for a UI that would rather explain than shrug. */
    val unavailableReason: String =
        "The native fusion core is not bundled in this build. Live signal strength, " +
            "device ranking and proximity all work; position estimates and inferred " +
            "bearing need the Rust core, which requires the Android NDK to compile."

    // ---- Session lifecycle -------------------------------------------------

    /** Create a tracker, returning an opaque handle. Zero means failure. */
    external fun createTracker(seed: Long): Long

    external fun destroyTracker(handle: Long)

    /** Discard all evidence, keeping the user where they are. */
    external fun reset(handle: Long)

    // ---- Observations ------------------------------------------------------

    /**
     * Fold in a signal-strength reading.
     *
     * @param source ordinal of [RssiSource] — the Rust side treats
     *   `ConnectedLink` as roughly half as noisy as `Advertisement`, which is
     *   what stops the faster, noisier source outvoting the better one.
     * @return false if the reading was rejected as implausible.
     */
    external fun observeRssi(handle: Long, dbm: Double, source: Int, atSeconds: Double): Boolean

    /**
     * Fold in a peer's reading, taken from a known position in the shared frame.
     *
     * The counterpart of [observeRssi], and what lets the app locate something
     * without walking: one observer's likelihood is a ring, two intersect.
     */
    external fun observeRssiFrom(
        handle: Long,
        dbm: Double,
        source: Int,
        x: Double,
        y: Double,
        atSeconds: Double,
    ): Boolean

    /** Fold in a true metric range from UWB, Channel Sounding or Wi-Fi RTT. */
    external fun observeRange(handle: Long, metres: Double, source: Int, atSeconds: Double): Boolean

    /** Fold in a measured angle of arrival. UWB only. */
    external fun observeAngle(
        handle: Long,
        bearingRad: Double,
        sigmaRad: Double,
        atSeconds: Double,
    ): Boolean

    // ---- User movement -----------------------------------------------------

    external fun setHeading(handle: Long, radians: Double, atSeconds: Double)

    external fun step(handle: Long, lengthM: Double, atSeconds: Double)

    // ---- Calibration -------------------------------------------------------

    /** Replace the path-loss model, after a calibration walk. */
    external fun setPathLoss(handle: Long, txPower1m: Double, exponent: Double)

    /**
     * Fit a path-loss model to `(distance_m, rssi_dbm)` pairs.
     *
     * @return `[txPower1m, exponent, rmsDb]`, or null if the fit is degenerate,
     *   physically implausible, or too noisy to be worth keeping. Refusing to
     *   answer is deliberate: a least-squares fit always returns *something*,
     *   and in a reflective corridor that something is confidently wrong.
     */
    external fun fitPathLoss(distances: DoubleArray, rssi: DoubleArray): DoubleArray?

    // ---- Readout -----------------------------------------------------------

    /**
     * Sample the tracker.
     *
     * Returned as a flat `DoubleArray` rather than a constructed object: one JNI
     * call per frame instead of thirty field reads. [Snapshots.decode] turns it
     * back into a [Snapshot].
     */
    external fun snapshot(handle: Long, atSeconds: Double): DoubleArray?

    /** Particle cloud as interleaved `x, y` pairs, for the posterior heat map. */
    external fun particles(handle: Long): DoubleArray?

    // ---- Altitude ----------------------------------------------------------
    //
    // A handle of its own rather than a field on the tracker: pressure readings
    // start before a hunt and outlive it, so tying the altimeter to a tracker
    // would discard the settled baseline every time the user picks a different
    // device to look for.

    external fun createAltimeter(): Long

    external fun destroyAltimeter(handle: Long)

    /** Fold in a pressure reading in pascals. False if implausible. */
    external fun observePressure(handle: Long, pascals: Double, atSeconds: Double): Boolean

    /** Re-anchor to here, so the answer is "since you started looking". */
    external fun anchorAltitude(handle: Long)

    /**
     * Storeys climbed since the anchor. Negative is below, `NaN` means not yet
     * known — a couple of samples are noise rather than a reading.
     */
    external fun floorDelta(handle: Long): Double
}
