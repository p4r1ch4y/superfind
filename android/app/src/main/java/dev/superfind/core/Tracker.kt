package dev.superfind.core

import kotlin.math.abs

/**
 * What the app talks to. Two implementations, chosen at runtime.
 *
 * [NativeTracker] delegates to the Rust fusion core. [DegradedTracker] is what
 * runs when that library is not in the build, and it is deliberately *not* a
 * reimplementation: it computes only what is trivially defensible from a window
 * of readings — a median, a trend, a proximity band — and returns `null` for the
 * fix and the bearing, because those genuinely require the filter.
 *
 * That asymmetry is the honest one. Substituting a cruder estimator and
 * presenting its output in the same place, with the same styling, as a real
 * fused fix would be the single most misleading thing this app could do.
 */
interface Tracker : AutoCloseable {
    val fusionAvailable: Boolean

    fun observeRssi(dbm: Double, source: RssiSource, atSeconds: Double)
    fun observeRange(metres: Double, sourceOrdinal: Int, atSeconds: Double)
    fun observeAngle(bearingRad: Double, sigmaRad: Double, atSeconds: Double)
    fun setHeading(radians: Double, atSeconds: Double)
    fun step(lengthM: Double, atSeconds: Double)
    fun reset()
    fun snapshot(atSeconds: Double): Snapshot

    companion object {
        fun create(seed: Long = 0x5F1D9E2B): Tracker =
            if (NativeCore.available) {
                val handle = runCatching { NativeCore.createTracker(seed) }.getOrDefault(0L)
                if (handle != 0L) NativeTracker(handle) else DegradedTracker()
            } else {
                DegradedTracker()
            }
    }
}

private class NativeTracker(private val handle: Long) : Tracker {
    override val fusionAvailable = true
    private var startedAt: Double? = null

    private fun elapsed(atSeconds: Double): Double {
        val origin = startedAt ?: atSeconds.also { startedAt = it }
        return (atSeconds - origin).coerceAtLeast(0.0)
    }

    override fun observeRssi(dbm: Double, source: RssiSource, atSeconds: Double) {
        NativeCore.observeRssi(handle, dbm, source.ordinal, elapsed(atSeconds))
    }

    override fun observeRange(metres: Double, sourceOrdinal: Int, atSeconds: Double) {
        NativeCore.observeRange(handle, metres, sourceOrdinal, elapsed(atSeconds))
    }

    override fun observeAngle(bearingRad: Double, sigmaRad: Double, atSeconds: Double) {
        NativeCore.observeAngle(handle, bearingRad, sigmaRad, elapsed(atSeconds))
    }

    override fun setHeading(radians: Double, atSeconds: Double) {
        NativeCore.setHeading(handle, radians, elapsed(atSeconds))
    }

    override fun step(lengthM: Double, atSeconds: Double) {
        NativeCore.step(handle, lengthM, elapsed(atSeconds))
    }

    override fun reset() = NativeCore.reset(handle)

    override fun snapshot(atSeconds: Double): Snapshot =
        NativeCore.snapshot(handle, elapsed(atSeconds))
            ?.let { Snapshots.decode(it) }
            ?: Snapshot.empty()

    override fun close() = NativeCore.destroyTracker(handle)
}

/**
 * No fusion. Live signal strength only, and honest about the rest.
 *
 * Everything here operates on a short window of recent readings, mirroring the
 * core's own display rules so the two agree on what the user sees:
 *
 * - The **best source present wins the window outright** rather than being
 *   averaged with worse ones. This is findphone's bug, encoded as behaviour:
 *   advertisements arrive far faster than connected-link reads, so a blended
 *   median follows the noisier source.
 * - The reading is a **median**, so one reflected spike cannot move it.
 */
private class DegradedTracker : Tracker {
    override val fusionAvailable = false

    private data class Sample(val at: Double, val dbm: Double, val source: RssiSource)

    private val history = ArrayDeque<Sample>()
    private var heading = 0.0
    private var steps = 0
    private var walked = 0.0
    private var startedAt: Double? = null
    private val sectorSums = DoubleArray(SECTORS)
    private val sectorCounts = IntArray(SECTORS)

    override fun observeRssi(dbm: Double, source: RssiSource, atSeconds: Double) {
        if (dbm >= 0.0 || dbm <= -127.0) return
        if (startedAt == null) startedAt = atSeconds
        history.addLast(Sample(atSeconds, dbm, source))
        while (history.isNotEmpty() && atSeconds - history.first().at > HISTORY_S) {
            history.removeFirst()
        }
        val sector = sectorOf(heading)
        sectorSums[sector] += dbm
        sectorCounts[sector]++
    }

    // Without the filter there is nothing meaningful to do with these.
    override fun observeRange(metres: Double, sourceOrdinal: Int, atSeconds: Double) = Unit
    override fun observeAngle(bearingRad: Double, sigmaRad: Double, atSeconds: Double) = Unit

    override fun setHeading(radians: Double, atSeconds: Double) {
        heading = radians
    }

    override fun step(lengthM: Double, atSeconds: Double) {
        steps++
        walked += lengthM
    }

    override fun reset() {
        history.clear()
        steps = 0
        walked = 0.0
        startedAt = null
        sectorSums.fill(0.0)
        sectorCounts.fill(0)
    }

    override fun snapshot(atSeconds: Double): Snapshot {
        val origin = startedAt ?: atSeconds
        val window = history.filter { atSeconds - it.at <= LIVE_WINDOW_S }
        val best = window.minByOrNull { it.source.ordinal }?.source
        val values = window.filter { it.source == best }.map { it.dbm }.sorted()
        val median = values.getOrNull(values.size / 2)
        val newest = history.lastOrNull()
        val age = newest?.let { atSeconds - it.at }

        return Snapshot(
            elapsedSeconds = atSeconds - origin,
            rssiDbm = median,
            rssiSource = best,
            isFresh = age != null && age < FRESH_S,
            ageSeconds = age,
            crudeDistanceM = median?.let { crudeDistance(it) },
            proximity = median?.let { Proximity.of(it) },
            trend = trend(atSeconds, median),
            // The two things that genuinely need the filter.
            fix = null,
            bearing = null,
            userHeadingRad = heading,
            steps = steps,
            distanceWalkedM = walked,
            headingCoverage = sectorCounts.count { it > 0 } / SECTORS.toDouble(),
            samplesInWindow = values.size,
            totalSamples = history.size,
            observations = history.size,
            diverged = false,
            sectorMeans = List(SECTORS) { i ->
                if (sectorCounts[i] > 0) sectorSums[i] / sectorCounts[i] else null
            },
            trail = emptyList(),
        )
    }

    private fun trend(now: Double, recent: Double?): Trend {
        if (recent == null) return Trend.UNKNOWN
        val older = history
            .filter { now - it.at in LIVE_WINDOW_S..TREND_WINDOW_S }
            .map { it.dbm }
            .sorted()
        if (older.size < 2) return Trend.UNKNOWN
        val delta = recent - older[older.size / 2]
        return when {
            delta > TREND_THRESHOLD_DB -> Trend.WARMER
            delta < -TREND_THRESHOLD_DB -> Trend.COLDER
            else -> Trend.STEADY
        }
    }

    /** Log-distance path loss with the built-in priors. Display only. */
    private fun crudeDistance(dbm: Double): Double =
        Math.pow(10.0, (TX_POWER_1M - dbm) / (10.0 * PATH_EXPONENT)).coerceAtLeast(0.25)

    private fun sectorOf(radians: Double): Int {
        val tau = 2.0 * Math.PI
        var a = radians % tau
        if (a < 0) a += tau
        return ((a / tau) * SECTORS).toInt().coerceIn(0, SECTORS - 1)
    }

    override fun close() = Unit

    private companion object {
        const val SECTORS = 16
        const val HISTORY_S = 600.0
        const val LIVE_WINDOW_S = 4.0
        const val TREND_WINDOW_S = 12.0
        const val TREND_THRESHOLD_DB = 3.0
        const val FRESH_S = 10.0
        const val TX_POWER_1M = -59.0
        const val PATH_EXPONENT = 2.8
    }
}

/** Flat-array encoding shared with the Rust side. */
object Snapshots {
    // Field order is a contract with the JNI layer; append only, never reorder.
    private const val HEADER = 20

    fun decode(raw: DoubleArray): Snapshot {
        if (raw.size < HEADER) return Snapshot.empty()
        fun v(i: Int) = raw[i]
        fun opt(i: Int) = raw[i].takeUnless { it.isNaN() }
        fun flag(i: Int) = raw[i] != 0.0

        val hasFix = flag(9)
        val hasBearing = flag(10)
        var cursor = HEADER

        val fix = if (hasFix) {
            Fix(
                x = raw[cursor++], y = raw[cursor++],
                distanceM = raw[cursor++],
                bearingRad = raw[cursor++], bearingSigmaRad = raw[cursor++],
                semiMajorM = raw[cursor++], semiMinorM = raw[cursor++],
                orientationRad = raw[cursor++],
                confidence = raw[cursor++], effectiveFraction = raw[cursor++],
            )
        } else null

        val bearing = if (hasBearing) {
            BearingEstimate(
                bearingRad = raw[cursor++], sigmaRad = raw[cursor++],
                confidence = raw[cursor++], coverage = raw[cursor++],
                contrastDb = raw[cursor++], samples = raw[cursor++].toInt(),
            )
        } else null

        val sectorCount = raw.getOrNull(cursor)?.toInt()?.coerceAtLeast(0) ?: 0
        cursor++
        val sectors = (0 until sectorCount).mapNotNull { i ->
            if (cursor + i < raw.size) opt(cursor + i) else null
        }
        cursor += sectorCount

        // The walked path, three doubles per point. Bounds-checked because a
        // truncated array must degrade to a short trail, never index past the
        // end — this is the one place where a mismatched encoding would crash
        // rather than merely mislead.
        val trailCount = raw.getOrNull(cursor)?.toInt()?.coerceAtLeast(0) ?: 0
        cursor++
        val trail = (0 until trailCount).mapNotNull { i ->
            val base = cursor + i * 3
            if (base + 2 < raw.size) {
                TrailPoint(raw[base], raw[base + 1], raw[base + 2])
            } else {
                null
            }
        }

        return Snapshot(
            elapsedSeconds = v(0),
            rssiDbm = opt(1),
            rssiSource = opt(2)?.let { RssiSource.entries.getOrNull(it.toInt()) },
            isFresh = flag(3),
            ageSeconds = opt(4),
            crudeDistanceM = opt(5),
            proximity = opt(6)?.let { Proximity.entries.getOrNull(it.toInt()) },
            trend = Trend.entries.getOrNull(v(7).toInt()) ?: Trend.UNKNOWN,
            userHeadingRad = v(8),
            fix = fix,
            bearing = bearing,
            steps = v(11).toInt(),
            distanceWalkedM = v(12),
            headingCoverage = v(13),
            samplesInWindow = v(14).toInt(),
            totalSamples = v(15).toInt(),
            observations = v(16).toInt(),
            diverged = flag(17),
            sectorMeans = sectors.ifEmpty { List(16) { null } },
            trail = trail,
        )
    }
}

/** Smallest signed rotation from [from] to [to], in (-PI, PI]. */
fun angleDiff(from: Double, to: Double): Double {
    val tau = 2.0 * Math.PI
    var d = (to - from) % tau
    if (d > Math.PI) d -= tau
    if (d <= -Math.PI) d += tau
    return if (abs(d) < 1e-12) 0.0 else d
}
