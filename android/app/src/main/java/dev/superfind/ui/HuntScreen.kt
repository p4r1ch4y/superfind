package dev.superfind.ui

import androidx.compose.animation.AnimatedContent
import androidx.compose.animation.fadeIn
import androidx.compose.animation.fadeOut
import androidx.compose.animation.togetherWith
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.filled.Map
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Radar
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.superfind.core.BearingEstimate
import dev.superfind.core.angleDiff
import dev.superfind.core.Fix
import dev.superfind.core.Proximity
import dev.superfind.core.RssiSource
import dev.superfind.core.Snapshot
import dev.superfind.core.TrailPoint
import dev.superfind.core.Trend
import kotlin.math.PI
import kotlin.math.roundToInt

/**
 * The hunt screen: radar first, map on a toggle.
 *
 * Ordering is the whole design. The signal reading is measured and goes at the
 * top. The radar is next, because "which way now" is the live question. The
 * fused estimate sits below in its own visually distinct block, because it is
 * *inferred* — and a reader skimming the screen should be able to tell measured
 * from inferred without being told.
 */
@Composable
fun HuntScreen(
    deviceName: String,
    snapshot: Snapshot,
    fusionAvailable: Boolean,
    tierHeadline: String,
    instruction: String,
    headingIsAbsolute: Boolean = true,
    /** Target advertises from a rotating private address. See [Screen.Hunt]. */
    randomisedAddress: Boolean = false,
    /** False for Classic-only targets: no GATT link, so no connected reading. */
    linkSupported: Boolean = true,
    onClose: () -> Unit,
    onReset: () -> Unit,
    modifier: Modifier = Modifier,
) {
    var showMap by remember { mutableStateOf(false) }

    Column(
        modifier = modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(horizontal = 20.dp, vertical = 16.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Header(deviceName, tierHeadline, snapshot)

        Spacer(Modifier.height(8.dp))

        AnimatedContent(
            targetState = showMap,
            transitionSpec = { fadeIn().togetherWith(fadeOut()) },
            label = "view",
        ) { map ->
            if (map) TrailMap(snapshot) else Radar(snapshot, headingIsAbsolute)
        }

        Spacer(Modifier.height(4.dp))

        Readout(snapshot)

        Spacer(Modifier.height(12.dp))

        EstimateBlock(snapshot, fusionAvailable, instruction, headingIsAbsolute)

        // A rotating address that has gone quiet is a specific, explicable
        // failure — not the same as a device being out of range, and saying
        // "No contact" for it would be a lie of omission.
        if (!linkSupported && snapshot.totalSamples == 0 && snapshot.elapsedSeconds > 10) {
            Spacer(Modifier.height(8.dp))
            Surface(
                Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(14.dp),
                color = SuperfindColors.Mid.copy(alpha = 0.14f),
            ) {
                Text(
                    text = "This is a Classic Bluetooth device — headphones and speakers " +
                        "usually are. Android offers no way to read its signal strength " +
                        "unless it is advertising, and most stop once paired and idle. " +
                        "Switching it on, or putting it in pairing mode, will make it findable.",
                    modifier = Modifier.padding(14.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = SuperfindColors.Mid,
                )
            }
        }

        if (randomisedAddress && snapshot.totalSamples == 0 && snapshot.elapsedSeconds > 15) {
            Spacer(Modifier.height(8.dp))
            Surface(
                Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(14.dp),
                color = SuperfindColors.Mid.copy(alpha = 0.14f),
            ) {
                Text(
                    text = "This device advertises from a rotating address, and nothing has " +
                        "arrived on it. The address has most likely changed — go back and " +
                        "pick the device again from the list.",
                    modifier = Modifier.padding(14.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = SuperfindColors.Mid,
                )
            }
        }

        Spacer(Modifier.weight(1f))

        Controls(
            showMap = showMap,
            onToggleView = { showMap = !showMap },
            onReset = onReset,
            onClose = onClose,
        )
    }
}

@Composable
private fun Header(deviceName: String, tierHeadline: String, snapshot: Snapshot) {
    Column(horizontalAlignment = Alignment.CenterHorizontally, modifier = Modifier.fillMaxWidth()) {
        Text(
            text = tierHeadline.uppercase(),
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            letterSpacing = 2.sp,
        )
        Text(
            text = deviceName,
            style = MaterialTheme.typography.headlineSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onBackground,
            textAlign = TextAlign.Center,
        )
        Text(
            text = "${snapshot.elapsedSeconds.roundToInt()}s · ${snapshot.totalSamples} readings",
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
    }
}

/**
 * The measured number, as large as the screen allows.
 *
 * Sized to be legible at arm's length while walking and looking at the room
 * rather than the phone — the same reasoning that made findphone draw its
 * reading in a block font.
 */
@Composable
private fun Readout(snapshot: Snapshot) {
    val dbm = snapshot.rssiDbm
    val tone = snapshot.proximity?.let { proximityTone(it) } ?: SuperfindColors.Idle

    Row(verticalAlignment = Alignment.Bottom) {
        Text(
            text = dbm?.roundToInt()?.toString() ?: "––",
            style = ReadoutStyle,
            color = if (snapshot.isFresh) tone else tone.copy(alpha = 0.35f),
        )
        Spacer(Modifier.width(6.dp))
        Text(
            text = "dBm",
            style = ReadoutUnitStyle,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(bottom = 14.dp),
        )
    }

    Row(verticalAlignment = Alignment.CenterVertically) {
        Text(
            text = snapshot.proximity?.label ?: "No contact",
            style = MaterialTheme.typography.titleMedium,
            fontWeight = FontWeight.SemiBold,
            color = tone,
        )
        if (snapshot.trend != Trend.UNKNOWN) {
            Spacer(Modifier.width(12.dp))
            Text(
                text = when (snapshot.trend) {
                    Trend.WARMER -> "▲ warmer"
                    Trend.COLDER -> "▼ colder"
                    else -> "· steady"
                },
                style = MaterialTheme.typography.bodyMedium,
                color = when (snapshot.trend) {
                    Trend.WARMER -> SuperfindColors.Warmer
                    Trend.COLDER -> SuperfindColors.Colder
                    else -> MaterialTheme.colorScheme.onSurfaceVariant
                },
            )
        }
    }

    // Staleness is stated rather than hidden, so a frozen number is never
    // mistaken for a steady one.
    if (!snapshot.isFresh && snapshot.ageSeconds != null) {
        Text(
            text = "No reading for ${snapshot.ageSeconds.roundToInt()}s",
            style = MaterialTheme.typography.bodySmall,
            color = SuperfindColors.Far,
        )
    }
}

/**
 * The inferred half of the screen, deliberately set apart from the measured
 * half above it.
 */
@Composable
private fun EstimateBlock(
    snapshot: Snapshot,
    fusionAvailable: Boolean,
    instruction: String,
    headingIsAbsolute: Boolean,
) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(16.dp)) {
            when {
                !fusionAvailable -> Line("Estimate", "unavailable in this build")

                snapshot.diverged -> Line(
                    "Estimate",
                    "readings disagree — tap reset",
                    tone = SuperfindColors.Farthest,
                )

                snapshot.fix == null -> Line("Estimate", "gathering evidence…")

                else -> {
                    val fix = snapshot.fix
                    Line("Distance", "%.1f m".format(fix.distanceM))
                    // The ellipse is reported as a span, which is far more
                    // legible than a percentage: "give or take 4 m" is
                    // actionable, "62% confident" is not.
                    Line("Give or take", "%.1f m".format(fix.semiMajorM))
                }
            }

            val bearing = snapshot.bearing
            when {
                bearing == null -> Line("Direction", "turn slowly to start mapping")
                bearing.confidence < 0.30 -> Line(
                    "Direction",
                    "keep turning · ${(bearing.coverage * 100).roundToInt()}% swept",
                    tone = SuperfindColors.Mid,
                )
                else -> Line(
                    "Direction",
                    // Without a magnetometer there is no compass point to name,
                    // so the bearing is stated as a turn from where the phone is
                    // pointing — which is the actionable form anyway.
                    if (headingIsAbsolute) {
                        "${compassPoint(bearing.bearingRad)} · ±${
                            (bearing.sigmaRad * 180 / PI).roundToInt()
                        }°"
                    } else {
                        val turn = angleDiff(snapshot.userHeadingRad, bearing.bearingRad)
                        val deg = (turn * 180 / PI).roundToInt()
                        val side = if (deg >= 0) "right" else "left"
                        "turn $side ${kotlin.math.abs(deg)}° · ±${
                            (bearing.sigmaRad * 180 / PI).roundToInt()
                        }°"
                    },
                )
            }

            Spacer(Modifier.height(10.dp))
            Text(
                text = instruction,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun Line(label: String, value: String, tone: androidx.compose.ui.graphics.Color? = null) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 2.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            text = value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium,
            color = tone ?: MaterialTheme.colorScheme.onSurface,
        )
    }
}

@Composable
private fun Controls(
    showMap: Boolean,
    onToggleView: () -> Unit,
    onReset: () -> Unit,
    onClose: () -> Unit,
) {
    Row(
        Modifier.fillMaxWidth().padding(top = 8.dp),
        horizontalArrangement = Arrangement.SpaceEvenly,
    ) {
        ControlButton(if (showMap) "Radar" else "Map", onToggleView) {
            Icon(
                imageVector = if (showMap) Icons.Filled.Radar else Icons.Filled.Map,
                contentDescription = null,
            )
        }
        ControlButton("Reset", onReset) {
            Icon(Icons.Filled.Refresh, contentDescription = null)
        }
        ControlButton("Close", onClose) {
            Icon(Icons.Filled.Close, contentDescription = null)
        }
    }
}

@Composable
private fun ControlButton(label: String, onClick: () -> Unit, icon: @Composable () -> Unit) {
    TextButton(onClick = onClick) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Box(
                Modifier
                    .size(46.dp)
                    .clip(RoundedCornerShape(23.dp))
                    .background(MaterialTheme.colorScheme.surfaceVariant),
                contentAlignment = Alignment.Center,
            ) { icon() }
            Spacer(Modifier.height(4.dp))
            Text(label, style = MaterialTheme.typography.labelSmall)
        }
    }
}

internal fun compassPoint(bearingRad: Double): String {
    val points = listOf("N", "NE", "E", "SE", "S", "SW", "W", "NW")
    var deg = bearingRad * 180.0 / PI % 360.0
    if (deg < 0) deg += 360.0
    return points[(((deg + 22.5) / 45.0).toInt()) % 8]
}

// ---------------------------------------------------------------------------
// Previews. These are how the UI gets reviewed without a device or the native
// core — each one pins a state that is easy to get wrong.
// ---------------------------------------------------------------------------

private fun sampleSnapshot(
    dbm: Double? = -58.0,
    bearingConfidence: Double = 0.62,
    withFix: Boolean = true,
) = Snapshot.empty().copy(
    elapsedSeconds = 47.0,
    rssiDbm = dbm,
    rssiSource = RssiSource.ADVERTISEMENT,
    isFresh = true,
    ageSeconds = 0.4,
    crudeDistanceM = 3.2,
    proximity = dbm?.let { Proximity.of(it) },
    trend = Trend.WARMER,
    userHeadingRad = 0.6,
    steps = 24,
    distanceWalkedM = 17.3,
    headingCoverage = 0.75,
    samplesInWindow = 9,
    totalSamples = 214,
    observations = 214,
    sectorMeans = List(16) { i ->
        if (i in 3..12) -50.0 - (i - 7) * (i - 7).toDouble() else null
    },
    fix = if (withFix) Fix(
        x = 4.2, y = 6.1, distanceM = 3.4,
        bearingRad = 0.95, bearingSigmaRad = 0.28,
        semiMajorM = 2.1, semiMinorM = 1.2, orientationRad = 0.5,
        confidence = 0.62, effectiveFraction = 0.44,
    ) else null,
    bearing = BearingEstimate(
        bearingRad = 0.95, sigmaRad = 0.34,
        confidence = bearingConfidence, coverage = 0.75,
        contrastDb = 7.4, samples = 214,
    ),
    trail = List(18) { i ->
        TrailPoint(x = i * 0.6, y = i * 0.35 + (i % 3) * 0.2, headingRad = 0.6)
    },
)

@Preview(name = "Confident bearing", showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewConfident() = SuperfindTheme(darkTheme = true) {
    HuntScreen(
        deviceName = "Pixel 9 Pro", snapshot = sampleSnapshot(), fusionAvailable = true,
        tierHeadline = "Distance and direction by walking",
        instruction = "Turn slowly on the spot, then walk a dogleg.",
        onClose = {}, onReset = {},
    )
}

/** The state that must never show an arrow. */
@Preview(name = "Low confidence", showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewLowConfidence() = SuperfindTheme(darkTheme = true) {
    HuntScreen(
        deviceName = "Soundcore Life Q20",
        snapshot = sampleSnapshot(dbm = -84.0, bearingConfidence = 0.11, withFix = false),
        fusionAvailable = true, tierHeadline = "Warmer and colder only",
        instruction = "Walk around and watch the number. Closer to zero is nearer.",
        onClose = {}, onReset = {},
    )
}

@Preview(name = "No contact", showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewNoContact() = SuperfindTheme(darkTheme = true) {
    HuntScreen(
        deviceName = "Tile Mate", snapshot = Snapshot.empty(), fusionAvailable = true,
        tierHeadline = "Distance and direction by walking",
        instruction = "Turn slowly on the spot to start mapping direction.",
        onClose = {}, onReset = {},
    )
}

/** The Moto G40 Fusion shape: gyro heading, no magnetometer, no compass rose. */
@Preview(name = "Relative heading", showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewRelativeHeading() = SuperfindTheme(darkTheme = true) {
    HuntScreen(
        deviceName = "JioSTB Bed Room TV", snapshot = sampleSnapshot(dbm = -72.0),
        fusionAvailable = true, tierHeadline = "Distance and relative direction",
        instruction = "Turn slowly on the spot, then walk a dogleg.",
        headingIsAbsolute = false, onClose = {}, onReset = {},
    )
}

@Preview(name = "Light theme", showBackground = true)
@Composable
private fun PreviewLight() = SuperfindTheme(darkTheme = false) {
    HuntScreen(
        deviceName = "Pixel 9 Pro", snapshot = sampleSnapshot(), fusionAvailable = true,
        tierHeadline = "Precise distance and direction",
        instruction = "Point the phone around slowly.",
        onClose = {}, onReset = {},
    )
}
