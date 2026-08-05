package dev.superfind.core

/**
 * The Kotlin mirror of `superfind_core::Snapshot`.
 *
 * Deliberately a plain immutable value type with no behaviour. The UI is a pure
 * function of one of these, exactly as the CLI's renderer is — so two numbers on
 * screen can never come from different instants and disagree with each other.
 *
 * Everything that is inferred rather than measured carries its uncertainty
 * alongside it. That is not decoration: [bearing] is a guess derived from
 * sweeping signal strength across headings, and a UI that draws it identically
 * to a UWB angle-of-arrival reading has lied to the person holding the phone.
 */
data class Snapshot(
    val elapsedSeconds: Double,
    /** Median of the live window, from the best source available in it. */
    val rssiDbm: Double?,
    val rssiSource: RssiSource?,
    /** False when the newest reading is too old to steer by. */
    val isFresh: Boolean,
    val ageSeconds: Double?,
    /** Single-reading distance estimate. For display only. */
    val crudeDistanceM: Double?,
    val proximity: Proximity?,
    val trend: Trend,
    val fix: Fix?,
    val bearing: BearingEstimate?,
    val userHeadingRad: Double,
    val steps: Int,
    val distanceWalkedM: Double,
    /** Fraction of the compass swept, 0..1. Below ~0.4 no bearing is credible. */
    val headingCoverage: Double,
    val samplesInWindow: Int,
    val totalSamples: Int,
    val observations: Int,
    /** Model and measurements have become irreconcilable. */
    val diverged: Boolean,
    /**
     * Readings contributed by peers observing from their own positions.
     *
     * Zero on a solo hunt. Worth showing: it is the difference between an
     * annulus and a fix, and the user should know which they are looking at.
     */
    val remoteObservations: Int = 0,
    /** Per-sector mean RSSI for the radar, null where nothing was sampled. */
    val sectorMeans: List<Double?>,
    /** The user's walked path, oldest first, in metres from the session origin. */
    val trail: List<TrailPoint>,
) {
    companion object {
        /** The state before anything has been heard. */
        fun empty() = Snapshot(
            elapsedSeconds = 0.0,
            rssiDbm = null,
            rssiSource = null,
            isFresh = false,
            ageSeconds = null,
            crudeDistanceM = null,
            proximity = null,
            trend = Trend.UNKNOWN,
            fix = null,
            bearing = null,
            userHeadingRad = 0.0,
            steps = 0,
            distanceWalkedM = 0.0,
            headingCoverage = 0.0,
            samplesInWindow = 0,
            totalSamples = 0,
            observations = 0,
            diverged = false,
            remoteObservations = 0,
            sectorMeans = List(16) { null },
            trail = emptyList(),
        )
    }
}

/** The fused position estimate. */
data class Fix(
    val x: Double,
    val y: Double,
    val distanceM: Double,
    val bearingRad: Double,
    /** Circular standard deviation of the particles' bearings. */
    val bearingSigmaRad: Double,
    /** 95% confidence ellipse — draw this, never a bare dot. */
    val semiMajorM: Double,
    val semiMinorM: Double,
    val orientationRad: Double,
    val confidence: Double,
    val effectiveFraction: Double,
)

/**
 * A bearing *inferred* from swept signal strength. Distinct from a measured
 * angle by design; see the note on [Snapshot].
 */
data class BearingEstimate(
    val bearingRad: Double,
    val sigmaRad: Double,
    val confidence: Double,
    val coverage: Double,
    val contrastDb: Double,
    val samples: Int,
)

data class TrailPoint(val x: Double, val y: Double, val headingRad: Double)

enum class RssiSource(val label: String) {
    CONNECTED_LINK("link"),
    ADVERTISEMENT("advert"),
    CLASSIC_POLL("classic"),
}

enum class Trend { WARMER, COLDER, STEADY, UNKNOWN }

/**
 * Coarse distance bands. Inherited from findphone, with its caveat intact:
 * signal strength is a poor proxy for distance, and a phone in a metal drawer
 * two metres away reads like one fifteen metres away in open air.
 */
enum class Proximity(val label: String) {
    ARMS_REACH("Arm's reach"),
    SAME_TABLE("Same table"),
    SAME_ROOM("Same room"),
    FAR_OR_OBSTRUCTED("Far, or behind cover"),
    VERY_FAR_OR_SHIELDED("Very far, or shielded");

    companion object {
        fun of(dbm: Double): Proximity = when {
            dbm >= -45.0 -> ARMS_REACH
            dbm >= -60.0 -> SAME_TABLE
            dbm >= -72.0 -> SAME_ROOM
            dbm >= -85.0 -> FAR_OR_OBSTRUCTED
            else -> VERY_FAR_OR_SHIELDED
        }
    }
}
