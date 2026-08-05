package dev.superfind.ui

import androidx.compose.animation.core.animateFloatAsState
import androidx.compose.animation.core.LinearEasing
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.text.drawText
import androidx.compose.ui.graphics.drawscope.rotate
import androidx.compose.ui.text.TextMeasurer
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.rememberTextMeasurer
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import dev.superfind.core.BearingEstimate
import dev.superfind.core.Proximity
import dev.superfind.core.Snapshot
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.min
import kotlin.math.sin

/**
 * Below this confidence the radar refuses to draw an arrow.
 *
 * This threshold is the whole ethic of the app in one constant. A swept-RSSI
 * bearing is an inference, and at low coverage or low contrast it is worth
 * nothing. Drawing a crisp arrow anyway would send someone walking confidently
 * in a direction we have not earned — so below this the radar shows a sweeping
 * "still listening" animation and says what is missing instead.
 */
private const val BEARING_ARROW_THRESHOLD = 0.30

/**
 * The radar.
 *
 * Reads as a compass rose the user can act on immediately, but every element is
 * tied to something measured:
 *
 * - the **wedge width is the bearing's own sigma**, so an uncertain bearing is
 *   visibly a fan rather than a needle;
 * - the **sector heat** is the raw synthetic-aperture data — where the user has
 *   swept, and what the signal was there, so unswept arcs read as unexplored
 *   rather than as empty;
 * - the **range rings** carry metre labels only when a fix exists to justify
 *   them.
 *
 * The rose counter-rotates with the device heading, so north stays north and
 * the arrow points where the user should physically walk.
 */
@Composable
fun Radar(
    snapshot: Snapshot,
    /**
     * False when the heading comes from a gyroscope with no magnetic reference.
     * The rose is then meaningless and is hidden — labelling an arbitrary zero
     * "N" would be a lie dressed as a compass — while the arrow itself stays
     * correct relative to how the phone is held.
     */
    headingIsAbsolute: Boolean = true,
    modifier: Modifier = Modifier,
) {
    val measurer = rememberTextMeasurer()

    val bearing = snapshot.bearing
    val confident = bearing != null && bearing.confidence >= BEARING_ARROW_THRESHOLD

    // Animate to the new bearing rather than snapping: a jumping arrow reads as
    // broken even when each individual value is correct.
    val arrowDeg by animateFloatAsState(
        targetValue = ((bearing?.bearingRad ?: 0.0) * 180.0 / PI).toFloat(),
        animationSpec = tween(600),
        label = "bearing",
    )
    val headingDeg by animateFloatAsState(
        targetValue = (snapshot.userHeadingRad * 180.0 / PI).toFloat(),
        animationSpec = tween(300),
        label = "heading",
    )

    val sweep = rememberInfiniteTransition(label = "sweep")
    val sweepDeg by sweep.animateFloatWrapped(
        durationMillis = 2600,
        label = "sweepAngle",
    )

    val tone = snapshot.proximity?.let { proximityTone(it) } ?: SuperfindColors.Idle

    Box(modifier = modifier.fillMaxWidth().aspectRatio(1f), contentAlignment = Alignment.Center) {
        Canvas(Modifier.fillMaxWidth().aspectRatio(1f)) {
            val radius = min(size.width, size.height) / 2f * 0.86f
            val centre = Offset(size.width / 2f, size.height / 2f)

            drawSectorHeat(centre, radius, snapshot, tone)
            drawRangeRings(centre, radius, snapshot, measurer)

            // The rose turns opposite the phone so north stays put.
            rotate(degrees = -headingDeg, pivot = centre) {
                if (headingIsAbsolute) drawCompassRose(centre, radius, measurer)

                if (confident && bearing != null) {
                    drawBearingWedge(centre, radius, arrowDeg, bearing, tone)
                } else {
                    drawSearchSweep(centre, radius, sweepDeg, tone)
                }
            }

            drawCentreMarker(centre, tone)
        }
    }
}

/**
 * Where the user has swept, and what the signal was there.
 *
 * Unsampled sectors stay dark. That is deliberate: the gap in the ring *is* the
 * instruction — it shows exactly which way the user has not yet turned, which is
 * more actionable than any text prompt.
 */
private fun DrawScope.drawSectorHeat(
    centre: Offset,
    radius: Float,
    snapshot: Snapshot,
    tone: Color,
) {
    val sectors = snapshot.sectorMeans
    if (sectors.isEmpty()) return

    val present = sectors.filterNotNull()
    if (present.isEmpty()) return
    val weakest = present.min()
    val strongest = present.max()
    val span = (strongest - weakest).coerceAtLeast(1.0)

    val sweepPer = 360f / sectors.size
    val inner = radius * 0.30f
    val thickness = radius - inner

    sectors.forEachIndexed { index, mean ->
        // Sector 0 is centred on north, so the arc starts half a sector before.
        val start = index * sweepPer - 90f - sweepPer / 2f
        val alpha = if (mean == null) 0.05f else {
            (0.12f + 0.62f * ((mean - weakest) / span).toFloat()).coerceIn(0.12f, 0.78f)
        }
        drawArc(
            color = if (mean == null) SuperfindColors.Unswept else tone,
            startAngle = start,
            sweepAngle = sweepPer - 1.2f,
            useCenter = false,
            topLeft = Offset(centre.x - radius + thickness / 2f, centre.y - radius + thickness / 2f),
            size = Size((radius - thickness / 2f) * 2f, (radius - thickness / 2f) * 2f),
            style = Stroke(width = thickness),
            alpha = alpha,
        )
    }
}

private fun DrawScope.drawRangeRings(
    centre: Offset,
    radius: Float,
    snapshot: Snapshot,
    measurer: TextMeasurer,
) {
    val fix = snapshot.fix
    listOf(0.34f, 0.62f, 0.9f).forEachIndexed { i, fraction ->
        drawCircle(
            color = SuperfindColors.Grid,
            radius = radius * fraction,
            center = centre,
            style = Stroke(width = 1.dp.toPx()),
            alpha = 0.35f,
        )
        // Metre labels only once a fix exists to scale them by. Numbers on an
        // unscaled ring would be decoration pretending to be measurement.
        if (fix != null) {
            val metres = fix.distanceM * 1.6 * fraction
            val label = measurer.measure(
                text = if (metres >= 10) "${metres.toInt()}m" else "%.1fm".format(metres),
                style = TextStyle(color = SuperfindColors.Muted, fontSize = 9.sp),
            )
            drawText(
                textLayoutResult = label,
                topLeft = Offset(
                    centre.x + 4.dp.toPx(),
                    centre.y - radius * fraction - label.size.height / 2f,
                ),
            )
        }
    }
}

private fun DrawScope.drawCompassRose(centre: Offset, radius: Float, measurer: TextMeasurer) {
    listOf("N" to 0f, "E" to 90f, "S" to 180f, "W" to 270f).forEach { (letter, deg) ->
        val rad = (deg - 90f) * PI.toFloat() / 180f
        val r = radius * 1.06f
        val point = Offset(centre.x + r * cos(rad), centre.y + r * sin(rad))
        val layout = measurer.measure(
            text = letter,
            style = TextStyle(
                color = if (letter == "N") SuperfindColors.North else SuperfindColors.Muted,
                fontSize = 11.sp,
            ),
        )
        drawText(
            textLayoutResult = layout,
            topLeft = Offset(
                point.x - layout.size.width / 2f,
                point.y - layout.size.height / 2f,
            ),
        )
    }
}

/**
 * The arrow, drawn as a wedge whose half-angle is the bearing's sigma.
 *
 * An uncertain bearing is therefore visibly a fan and a confident one visibly a
 * needle, without the user needing to read a number. The wedge is clamped to a
 * sensible range so it never degenerates into either a line or a full circle.
 */
private fun DrawScope.drawBearingWedge(
    centre: Offset,
    radius: Float,
    bearingDeg: Float,
    bearing: BearingEstimate,
    tone: Color,
) {
    val halfAngle = ((bearing.sigmaRad * 180.0 / PI).toFloat()).coerceIn(8f, 70f)
    val reach = radius * 0.92f

    val path = Path().apply {
        moveTo(centre.x, centre.y)
        arcTo(
            rect = Rect(
                centre.x - reach, centre.y - reach,
                centre.x + reach, centre.y + reach,
            ),
            startAngleDegrees = bearingDeg - 90f - halfAngle,
            sweepAngleDegrees = halfAngle * 2f,
            forceMoveTo = false,
        )
        close()
    }

    drawPath(
        path = path,
        brush = Brush.radialGradient(
            colors = listOf(tone.copy(alpha = 0.42f), tone.copy(alpha = 0.06f)),
            center = centre,
            radius = reach,
        ),
    )

    // A solid spine through the middle of the fan: the best single guess.
    val rad = (bearingDeg - 90f) * PI.toFloat() / 180f
    drawLine(
        color = tone,
        start = centre,
        end = Offset(centre.x + reach * cos(rad), centre.y + reach * sin(rad)),
        strokeWidth = 3.dp.toPx(),
    )
}

/**
 * Shown instead of an arrow when the bearing is not yet worth trusting.
 *
 * A rotating sweep is the honest visual: it says "still listening" rather than
 * pointing somewhere. It is also the familiar radar idiom, so it needs no
 * explanation.
 */
private fun DrawScope.drawSearchSweep(
    centre: Offset,
    radius: Float,
    sweepDeg: Float,
    tone: Color,
) {
    val reach = radius * 0.92f
    val path = Path().apply {
        moveTo(centre.x, centre.y)
        arcTo(
            rect = Rect(centre.x - reach, centre.y - reach, centre.x + reach, centre.y + reach),
            startAngleDegrees = sweepDeg - 90f,
            sweepAngleDegrees = 52f,
            forceMoveTo = false,
        )
        close()
    }
    drawPath(
        path = path,
        brush = Brush.radialGradient(
            colors = listOf(tone.copy(alpha = 0.20f), Color.Transparent),
            center = centre,
            radius = reach,
        ),
    )
}

private fun DrawScope.drawCentreMarker(centre: Offset, tone: Color) {
    drawCircle(color = tone.copy(alpha = 0.18f), radius = 13.dp.toPx(), center = centre)
    drawCircle(color = tone, radius = 4.dp.toPx(), center = centre)
}

@Composable
private fun androidx.compose.animation.core.InfiniteTransition.animateFloatWrapped(
    durationMillis: Int,
    label: String,
) = animateFloat(
    initialValue = 0f,
    targetValue = 360f,
    animationSpec = infiniteRepeatable<Float>(
        animation = tween(durationMillis, easing = LinearEasing)
    ),
    label = label,
)

internal fun proximityTone(p: Proximity): Color = when (p) {
    Proximity.ARMS_REACH -> SuperfindColors.Nearest
    Proximity.SAME_TABLE -> SuperfindColors.Near
    Proximity.SAME_ROOM -> SuperfindColors.Mid
    Proximity.FAR_OR_OBSTRUCTED -> SuperfindColors.Far
    Proximity.VERY_FAR_OR_SHIELDED -> SuperfindColors.Farthest
}
