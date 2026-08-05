package dev.superfind.ui

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
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import dev.superfind.core.Calibration

/** What the calibration flow is doing right now. */
sealed interface CalibrationState {
    /** Waiting for the user to place the device and start the step. */
    data class Waiting(val distanceM: Double, val step: Int, val steps: Int) : CalibrationState

    data class Collecting(
        val distanceM: Double,
        val step: Int,
        val steps: Int,
        val samples: Int,
        val target: Int,
    ) : CalibrationState

    data class Done(val calibration: Calibration) : CalibrationState

    /** The fit was made and rejected. Says why, rather than blaming the user. */
    data class Rejected(val reason: String) : CalibrationState
}

/**
 * The calibration walk.
 *
 * A minute of a person's time buys the largest accuracy improvement available to
 * a signal-strength finder, because the built-in priors are a published average
 * rather than a measurement of *this* device. Transmit power varies by more than
 * 15 dB across hardware, and that error is multiplicative in distance.
 *
 * The screen therefore does two things carefully: it asks for placements that
 * are easy to get right, and it is explicit that a bad fit will be thrown away.
 * Somebody who has just spent a minute holding a phone at arm's length deserves
 * to know the answer might be "keep the priors" — and why.
 */
@Composable
fun CalibrateScreen(
    deviceLabel: String,
    state: CalibrationState,
    onStartStep: () -> Unit,
    onSave: (Calibration) -> Unit,
    onRetry: () -> Unit,
    onCancel: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(24.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = "CALIBRATE",
            style = MaterialTheme.typography.labelSmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            text = deviceLabel,
            style = MaterialTheme.typography.headlineSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onBackground,
            textAlign = TextAlign.Center,
        )

        Spacer(Modifier.height(28.dp))

        when (state) {
            is CalibrationState.Waiting -> Placement(
                distanceM = state.distanceM,
                step = state.step,
                steps = state.steps,
                progress = null,
                onStart = onStartStep,
            )

            is CalibrationState.Collecting -> Placement(
                distanceM = state.distanceM,
                step = state.step,
                steps = state.steps,
                progress = state.samples.toFloat() / state.target.coerceAtLeast(1),
                onStart = null,
            )

            is CalibrationState.Done -> Result(state.calibration, onSave, onRetry)
            is CalibrationState.Rejected -> Rejection(state.reason, onRetry)
        }

        Spacer(Modifier.weight(1f))
        TextButton(onClick = onCancel) { Text("Cancel") }
    }
}

@Composable
private fun Placement(
    distanceM: Double,
    step: Int,
    steps: Int,
    progress: Float?,
    onStart: (() -> Unit)?,
) {
    Text(
        text = "Step $step of $steps",
        style = MaterialTheme.typography.labelMedium,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
    )
    Spacer(Modifier.height(10.dp))
    Text(
        text = if (distanceM >= 1.0) "%.0f m".format(distanceM) else "%.1f m".format(distanceM),
        style = ReadoutStyle,
        color = SuperfindColors.Nearest,
    )
    Spacer(Modifier.height(6.dp))
    Text(
        text = "Put the device this far away, in clear air, and stand still.",
        style = MaterialTheme.typography.bodyMedium,
        color = MaterialTheme.colorScheme.onSurface,
        textAlign = TextAlign.Center,
    )
    Spacer(Modifier.height(6.dp))
    Text(
        // The two things that ruin a fit, said before it is ruined.
        text = "Keep your body out of the line between them, and away from " +
            "metal — both attenuate the signal and would be fitted as distance.",
        style = MaterialTheme.typography.bodySmall,
        color = MaterialTheme.colorScheme.onSurfaceVariant,
        textAlign = TextAlign.Center,
    )

    Spacer(Modifier.height(26.dp))

    if (progress != null) {
        LinearProgressIndicator(
            progress = { progress.coerceIn(0f, 1f) },
            modifier = Modifier.fillMaxWidth(),
        )
        Spacer(Modifier.height(10.dp))
        Row(verticalAlignment = Alignment.CenterVertically) {
            CircularProgressIndicator(Modifier.size(16.dp), strokeWidth = 2.dp)
            Spacer(Modifier.height(8.dp))
            Text(
                text = "  Collecting readings…",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    } else if (onStart != null) {
        Button(onClick = onStart) { Text("It is in place") }
    }
}

@Composable
private fun Result(
    calibration: Calibration,
    onSave: (Calibration) -> Unit,
    onRetry: () -> Unit,
) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(18.dp)) {
            Line("Reference at 1 m", "%.1f dBm".format(calibration.txPower1m))
            Line("Path-loss exponent", "%.2f".format(calibration.exponent))
            // The residual is the honest headline: it says how much the room
            // disagreed with the model, which is what the fit is worth.
            Line(
                "Residual",
                "%.1f dB · %s".format(calibration.rmsDb, calibration.quality),
            )
            Spacer(Modifier.height(10.dp))
            Text(
                text = when {
                    calibration.exponent < 2.2 ->
                        "An exponent near 2 means the path was close to free space. " +
                            "Expect worse accuracy indoors than this fit suggests."
                    calibration.rmsDb > 5.0 ->
                        "The room disagreed with the model more than usual — likely " +
                            "reflections. Still better than the generic priors."
                    else ->
                        "A tidy fit. Distances for this device should now be worth " +
                            "trusting to a couple of metres."
                },
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    Spacer(Modifier.height(18.dp))
    Row(horizontalArrangement = Arrangement.Center) {
        Button(onClick = { onSave(calibration) }) { Text("Save") }
        Spacer(Modifier.size(12.dp))
        TextButton(onClick = onRetry) { Text("Measure again") }
    }
}

@Composable
private fun Rejection(reason: String, onRetry: () -> Unit) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(16.dp),
        color = SuperfindColors.Mid.copy(alpha = 0.14f),
    ) {
        Column(Modifier.padding(18.dp)) {
            Text(
                text = "Keeping the built-in model",
                style = MaterialTheme.typography.titleSmall,
                fontWeight = FontWeight.SemiBold,
                color = SuperfindColors.Mid,
            )
            Spacer(Modifier.height(8.dp))
            Text(
                text = reason,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
    Spacer(Modifier.height(18.dp))
    Button(onClick = onRetry) { Text("Try again") }
}

@Composable
private fun Line(label: String, value: String) {
    Row(
        Modifier.fillMaxWidth().padding(vertical = 3.dp),
        horizontalArrangement = Arrangement.SpaceBetween,
    ) {
        Text(label, style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            value,
            style = MaterialTheme.typography.bodyMedium,
            fontFamily = FontFamily.Monospace,
            fontWeight = FontWeight.Medium,
            color = MaterialTheme.colorScheme.onSurface,
        )
    }
}

@Preview(showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewCollecting() = SuperfindTheme(darkTheme = true) {
    CalibrateScreen(
        deviceLabel = "Soundcore Life Q20",
        state = CalibrationState.Collecting(2.0, 2, 4, 11, 25),
        onStartStep = {}, onSave = {}, onRetry = {}, onCancel = {},
    )
}

@Preview(showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewResult() = SuperfindTheme(darkTheme = true) {
    CalibrateScreen(
        deviceLabel = "Soundcore Life Q20",
        state = CalibrationState.Done(Calibration(-61.4, 2.93, 3.8, 0)),
        onStartStep = {}, onSave = {}, onRetry = {}, onCancel = {},
    )
}

/** The state that must not be dressed up as success. */
@Preview(showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewRejected() = SuperfindTheme(darkTheme = true) {
    CalibrateScreen(
        deviceLabel = "JioSTB Bed Room TV",
        state = CalibrationState.Rejected(
            "The readings did not fit a path-loss curve closely enough to trust. " +
                "This usually means reflections — try a corridor or a larger room, " +
                "with the device in clear air rather than on a metal surface."
        ),
        onStartStep = {}, onSave = {}, onRetry = {}, onCancel = {},
    )
}
