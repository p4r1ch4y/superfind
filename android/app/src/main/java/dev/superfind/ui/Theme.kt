package dev.superfind.ui

import androidx.compose.foundation.isSystemInDarkTheme
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Typography
import androidx.compose.material3.darkColorScheme
import androidx.compose.material3.lightColorScheme
import androidx.compose.runtime.Composable
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp

/**
 * The palette.
 *
 * The proximity ramp is the load-bearing part and is ordered by *lightness* as
 * well as hue, so it survives being read by someone with red-green colour
 * blindness and by anyone glancing at a phone in sunlight. Colour is never the
 * only channel: every band also carries its own text label, and the radar
 * wedge's width encodes uncertainty independently.
 */
object SuperfindColors {
    // Proximity ramp, nearest to farthest.
    val Nearest = Color(0xFF4ADE80)
    val Near = Color(0xFF86EFAC)
    val Mid = Color(0xFFFDE047)
    val Far = Color(0xFFFB923C)
    val Farthest = Color(0xFFF87171)

    val Idle = Color(0xFF64748B)
    val Grid = Color(0xFF94A3B8)
    val Unswept = Color(0xFF475569)
    val North = Color(0xFFE2E8F0)
    val Muted = Color(0xFF94A3B8)

    val Warmer = Nearest
    val Colder = Farthest

    val SurfaceDark = Color(0xFF0E1116)
    val SurfaceDarkElevated = Color(0xFF171B22)
    val SurfaceLight = Color(0xFFF8FAFC)
    val SurfaceLightElevated = Color(0xFFFFFFFF)
}

private val DarkScheme = darkColorScheme(
    primary = SuperfindColors.Nearest,
    onPrimary = Color(0xFF04120A),
    background = SuperfindColors.SurfaceDark,
    onBackground = Color(0xFFE6EAF2),
    surface = SuperfindColors.SurfaceDark,
    onSurface = Color(0xFFE6EAF2),
    surfaceVariant = SuperfindColors.SurfaceDarkElevated,
    onSurfaceVariant = Color(0xFFA9B4C6),
    error = SuperfindColors.Farthest,
)

private val LightScheme = lightColorScheme(
    primary = Color(0xFF15803D),
    onPrimary = Color.White,
    background = SuperfindColors.SurfaceLight,
    onBackground = Color(0xFF0F172A),
    surface = SuperfindColors.SurfaceLight,
    onSurface = Color(0xFF0F172A),
    surfaceVariant = Color(0xFFE7ECF3),
    onSurfaceVariant = Color(0xFF475569),
    error = Color(0xFFB91C1C),
)

/**
 * The reading is set in a monospaced face at a large size for one practical
 * reason: it is read at arm's length, in motion, while looking at the room
 * rather than the screen. Proportional digits jitter as the value changes and
 * make a steady signal look unsteady.
 */
val ReadoutStyle = TextStyle(
    fontFamily = FontFamily.Monospace,
    fontWeight = FontWeight.Bold,
    fontSize = 76.sp,
    letterSpacing = (-2).sp,
)

val ReadoutUnitStyle = TextStyle(
    fontFamily = FontFamily.Monospace,
    fontWeight = FontWeight.Medium,
    fontSize = 18.sp,
)

@Composable
fun SuperfindTheme(
    darkTheme: Boolean = isSystemInDarkTheme(),
    content: @Composable () -> Unit,
) {
    MaterialTheme(
        colorScheme = if (darkTheme) DarkScheme else LightScheme,
        typography = Typography(),
        content = content,
    )
}
