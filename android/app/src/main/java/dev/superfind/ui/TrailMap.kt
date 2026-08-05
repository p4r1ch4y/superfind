package dev.superfind.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.aspectRatio
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.drawscope.rotateRad
import androidx.compose.ui.unit.dp
import dev.superfind.core.Snapshot
import kotlin.math.max
import kotlin.math.min

/**
 * The spatial view: where you have walked, and where the device probably is.
 *
 * Complements the radar rather than duplicating it. The radar answers "which way
 * now"; this answers "where have I already looked", which is the question that
 * stops a search going in circles.
 *
 * The estimate is drawn as its **95% confidence ellipse**, never as a dot. A dot
 * would claim a precision the filter has not got, and when the posterior is
 * bimodal — which happens whenever the walk has been close to a straight line —
 * the ellipse honestly sprawls across both possibilities instead of confidently
 * marking the empty space between them.
 */
@Composable
fun TrailMap(
    snapshot: Snapshot,
    modifier: Modifier = Modifier,
) {
    Box(modifier = modifier.fillMaxWidth().aspectRatio(1f), contentAlignment = Alignment.Center) {
        Canvas(Modifier.fillMaxWidth().aspectRatio(1f)) {
            val trail = snapshot.trail
            val fix = snapshot.fix

            // Everything that must stay in frame: the whole walk, plus the
            // estimate and its uncertainty.
            var minX = 0.0; var maxX = 0.0
            var minY = 0.0; var maxY = 0.0
            trail.forEach {
                minX = min(minX, it.x); maxX = max(maxX, it.x)
                minY = min(minY, it.y); maxY = max(maxY, it.y)
            }
            if (fix != null) {
                val reach = fix.semiMajorM
                minX = min(minX, fix.x - reach); maxX = max(maxX, fix.x + reach)
                minY = min(minY, fix.y - reach); maxY = max(maxY, fix.y + reach)
            }

            val spanX = (maxX - minX).coerceAtLeast(6.0)
            val spanY = (maxY - minY).coerceAtLeast(6.0)
            val span = max(spanX, spanY) * 1.25
            val centreX = (minX + maxX) / 2.0
            val centreY = (minY + maxY) / 2.0

            val scale = (min(size.width, size.height) / span).toFloat()

            // World metres to screen pixels. Screen y grows downward while north
            // grows upward, hence the negation.
            fun project(x: Double, y: Double) = Offset(
                size.width / 2f + ((x - centreX) * scale).toFloat(),
                size.height / 2f - ((y - centreY) * scale).toFloat(),
            )

            drawMetreGrid(scale, span)

            if (fix != null) {
                drawConfidenceEllipse(project(fix.x, fix.y), fix.semiMajorM, fix.semiMinorM,
                    fix.orientationRad, scale, snapshot)
            }

            drawTrail(trail.map { project(it.x, it.y) })

            trail.lastOrNull()?.let { last ->
                drawUser(project(last.x, last.y), snapshot.userHeadingRad)
            }
        }
    }
}

private fun DrawScope.drawMetreGrid(scale: Float, span: Double) {
    // Round metre spacing that keeps roughly 6 lines on screen at any zoom.
    val stepM = listOf(1.0, 2.0, 5.0, 10.0, 20.0, 50.0).firstOrNull { span / it <= 8 } ?: 100.0
    val stepPx = (stepM * scale).toFloat()
    if (stepPx < 12f) return

    var x = size.width / 2f % stepPx
    while (x < size.width) {
        drawLine(SuperfindColors.Grid.copy(alpha = 0.10f), Offset(x, 0f), Offset(x, size.height))
        x += stepPx
    }
    var y = size.height / 2f % stepPx
    while (y < size.height) {
        drawLine(SuperfindColors.Grid.copy(alpha = 0.10f), Offset(0f, y), Offset(size.width, y))
        y += stepPx
    }
}

/**
 * The walked path, fading with age so the recent end reads as current.
 */
private fun DrawScope.drawTrail(points: List<Offset>) {
    if (points.size < 2) return
    val path = Path().apply {
        moveTo(points.first().x, points.first().y)
        points.drop(1).forEach { lineTo(it.x, it.y) }
    }
    drawPath(
        path = path,
        color = SuperfindColors.Grid.copy(alpha = 0.55f),
        style = Stroke(width = 2.dp.toPx()),
    )
    // Origin, so the user can see where the session started.
    drawCircle(
        color = SuperfindColors.Muted,
        radius = 3.dp.toPx(),
        center = points.first(),
        style = Stroke(width = 1.5.dp.toPx()),
    )
}

private fun DrawScope.drawUser(position: Offset, headingRad: Double) {
    drawCircle(SuperfindColors.North.copy(alpha = 0.20f), 11.dp.toPx(), position)
    drawCircle(SuperfindColors.North, 5.dp.toPx(), position)
    // A short spur showing which way the phone is pointing.
    rotateRad(radians = headingRad.toFloat(), pivot = position) {
        drawLine(
            color = SuperfindColors.North,
            start = position,
            end = Offset(position.x, position.y - 16.dp.toPx()),
            strokeWidth = 2.dp.toPx(),
        )
    }
}

/**
 * The 95% confidence ellipse, oriented by the posterior's own covariance.
 *
 * Its size is the honest headline: a small tight ellipse means the filter knows
 * where the device is, and a sprawling one means it does not — which the user
 * can read at a glance without parsing a percentage.
 */
private fun DrawScope.drawConfidenceEllipse(
    centre: Offset,
    semiMajorM: Double,
    semiMinorM: Double,
    orientationRad: Double,
    scale: Float,
    snapshot: Snapshot,
) {
    val tone = snapshot.proximity?.let { proximityTone(it) } ?: SuperfindColors.Idle
    val major = (semiMajorM * scale).toFloat().coerceAtLeast(2f)
    val minor = (semiMinorM * scale).toFloat().coerceAtLeast(2f)

    // The stored orientation is a compass bearing (clockwise from north); the
    // canvas rotates counter-clockwise from the x axis, hence the negation.
    rotateRad(radians = (-orientationRad).toFloat(), pivot = centre) {
        drawOval(
            color = tone.copy(alpha = 0.16f),
            topLeft = Offset(centre.x - minor, centre.y - major),
            size = Size(minor * 2f, major * 2f),
        )
        drawOval(
            color = tone.copy(alpha = 0.85f),
            topLeft = Offset(centre.x - minor, centre.y - major),
            size = Size(minor * 2f, major * 2f),
            style = Stroke(width = 1.5.dp.toPx()),
        )
    }
    drawCircle(color = tone, radius = 3.dp.toPx(), center = centre)
}

internal val TransparentGrid = Color.Transparent
