package dev.superfind.ui

import androidx.compose.foundation.background
import androidx.compose.foundation.ExperimentalFoundationApi
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.tooling.preview.Preview
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Search
import androidx.compose.material3.Icon
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.TextButton
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardCapitalization
import dev.superfind.core.Proximity
import dev.superfind.radio.KnownDevices
import dev.superfind.radio.Tier

/**
 * The survey: everything nearby, strongest first.
 *
 * The device the user wants is almost always the one that climbs as they walk
 * towards it, so the list is built to make change legible — a signal bar per
 * row, sorted live, with the band named in words as well as coloured.
 */
@Composable
fun SurveyScreen(
    sightings: List<Sighting>,
    tier: Tier,
    headline: String,
    instruction: String,
    limitations: List<String>,
    error: String?,
    /** Addresses that have been with the user across places they have moved. */
    followers: List<String> = emptyList(),
    onSelect: (Sighting) -> Unit,
    /** Long-press: fit a path-loss model to this particular device. */
    onCalibrate: (Sighting) -> Unit = {},
    /** Addresses that already have a saved fit. */
    calibrated: Set<String> = emptySet(),
    onHuntAddress: (String) -> Boolean,
    modifier: Modifier = Modifier,
) {
    Column(
        modifier = modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(horizontal = 20.dp, vertical = 16.dp),
    ) {
        Text(
            text = "Nearby devices",
            style = MaterialTheme.typography.headlineSmall,
            fontWeight = FontWeight.Bold,
            color = MaterialTheme.colorScheme.onBackground,
        )
        Text(
            text = "$headline · ${tier.label} mode",
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )

        Spacer(Modifier.height(12.dp))

        // What this phone can do is stated up front rather than discovered as
        // disappointment three minutes into a search.
        CapabilityCard(instruction, limitations)

        if (error != null) {
            Spacer(Modifier.height(8.dp))
            Surface(
                Modifier.fillMaxWidth(),
                shape = RoundedCornerShape(12.dp),
                color = SuperfindColors.Farthest.copy(alpha = 0.15f),
            ) {
                Text(
                    text = error,
                    modifier = Modifier.padding(12.dp),
                    style = MaterialTheme.typography.bodySmall,
                    color = SuperfindColors.Farthest,
                )
            }
        }

        if (followers.isNotEmpty()) {
            Spacer(Modifier.height(8.dp))
            TravelledWithYou(followers)
        }

        Spacer(Modifier.height(12.dp))

        AddressEntry(onHuntAddress)

        Spacer(Modifier.height(12.dp))

        if (sightings.isEmpty()) {
            Box(Modifier.fillMaxWidth().weight(1f), contentAlignment = Alignment.Center) {
                Text(
                    text = "Listening…",
                    style = MaterialTheme.typography.bodyLarge,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        } else {
            LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                items(sightings, key = { it.address }) { sighting ->
                    SightingRow(
                        sighting = sighting,
                        calibrated = sighting.address.uppercase() in calibrated,
                        onClick = { onSelect(sighting) },
                        onLongClick = { onCalibrate(sighting) },
                    )
                }
            }
        }
    }
}

/**
 * Hunt a device by typing its address.
 *
 * For the case the list cannot serve: you know the MAC — off a label, from an
 * earlier session, from another tool — but the device is not advertising right
 * now, so there is nothing to tap. Starting the hunt anyway means the app is
 * already listening the moment it wakes up.
 *
 * Input is deliberately forgiving: colons, dashes or nothing at all, any case.
 * Rejecting `aabbccddeeff` because it lacks colons would be pedantry.
 */
@Composable
private fun AddressEntry(onHuntAddress: (String) -> Boolean) {
    var text by remember { mutableStateOf("") }
    var expanded by remember { mutableStateOf(false) }
    val valid = KnownDevices.isValidAddress(text)

    if (!expanded) {
        TextButton(onClick = { expanded = true }) {
            Icon(Icons.Filled.Search, contentDescription = null)
            Spacer(Modifier.width(8.dp))
            Text("Find by address")
        }
        return
    }

    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(14.dp)) {
            OutlinedTextField(
                value = text,
                onValueChange = { text = it },
                label = { Text("Bluetooth address") },
                placeholder = { Text("AA:BB:CC:DD:EE:FF") },
                singleLine = true,
                isError = text.isNotBlank() && !valid,
                supportingText = {
                    Text(
                        when {
                            text.isBlank() -> "Colons optional. 12 hex digits."
                            valid -> "Ready — ${KnownDevices.normaliseAddress(text)}"
                            else -> "Needs 12 hex digits; ${text.count { it.isLetterOrDigit() }} so far."
                        },
                        style = MaterialTheme.typography.bodySmall,
                    )
                },
                keyboardOptions = KeyboardOptions(
                    capitalization = KeyboardCapitalization.Characters,
                    imeAction = ImeAction.Go,
                ),
                modifier = Modifier.fillMaxWidth(),
            )
            Row(horizontalArrangement = Arrangement.End, modifier = Modifier.fillMaxWidth()) {
                TextButton(onClick = { expanded = false; text = "" }) { Text("Cancel") }
                TextButton(enabled = valid, onClick = { onHuntAddress(text) }) { Text("Hunt") }
            }
        }
    }
}

/**
 * Devices that have kept pace with you.
 *
 * The wording is the careful part. This detects **co-travel**, which is not
 * proof of anything: a partner's phone, a colleague on the same train and a
 * tracker in your bag are indistinguishable by radio. Saying "you are being
 * tracked" would be a claim the evidence cannot support, and one that invites
 * somebody to search their own belongings and distrust the people around them.
 *
 * So it states the observation and stops. What to make of it is the user's.
 */
@Composable
private fun TravelledWithYou(followers: List<String>) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = SuperfindColors.Mid.copy(alpha = 0.13f),
    ) {
        Column(Modifier.padding(14.dp)) {
            Text(
                text = if (followers.size == 1) {
                    "A device has been with you across places"
                } else {
                    "${followers.size} devices have been with you across places"
                },
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Medium,
                color = SuperfindColors.Mid,
            )
            Spacer(Modifier.height(4.dp))
            followers.take(4).forEach {
                Text(
                    text = it,
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            Spacer(Modifier.height(6.dp))
            Text(
                text = "This means it stayed nearby as you moved — not that it is " +
                    "following you. Your own devices, and anyone travelling with " +
                    "you, look exactly the same from here.",
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
    }
}

@Composable
private fun CapabilityCard(instruction: String, limitations: List<String>) {
    Surface(
        Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Column(Modifier.padding(14.dp)) {
            Text(
                text = instruction,
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurface,
            )
            Spacer(Modifier.height(6.dp))
            Text(
                text = "Hold a device to calibrate it — a minute of measuring makes " +
                    "its distances worth trusting.",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            limitations.forEach {
                Spacer(Modifier.height(6.dp))
                Text(
                    text = "· $it",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

@OptIn(ExperimentalFoundationApi::class)
@Composable
private fun SightingRow(
    sighting: Sighting,
    calibrated: Boolean,
    onClick: () -> Unit,
    onLongClick: () -> Unit,
) {
    val band = sighting.rssi?.let { Proximity.of(it.toDouble()) }
    val tone = band?.let { proximityTone(it) } ?: SuperfindColors.Idle

    Surface(
        modifier = Modifier
            .fillMaxWidth()
            .combinedClickable(onClick = onClick, onLongClick = onLongClick),
        shape = RoundedCornerShape(14.dp),
        color = MaterialTheme.colorScheme.surfaceVariant,
    ) {
        Row(
            Modifier.padding(14.dp),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Column(Modifier.weight(1f)) {
                Text(
                    text = sighting.label,
                    style = MaterialTheme.typography.bodyLarge,
                    fontWeight = FontWeight.Medium,
                    color = MaterialTheme.colorScheme.onSurface,
                    maxLines = 1,
                )
                // The address is always shown, even under a name: it is what
                // identifies the device to every other tool, and it is what the
                // user would type into "find by address" next time.
                Text(
                    text = sighting.address,
                    style = MaterialTheme.typography.labelSmall,
                    fontFamily = FontFamily.Monospace,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Text(
                    text = band?.label ?: "Paired · not heard right now",
                    style = MaterialTheme.typography.bodySmall,
                    color = if (band != null) tone else MaterialTheme.colorScheme.onSurfaceVariant,
                )

                // What the advert implies, when the row's title is only an
                // address. Not an identity, but far more use than hex.
                sighting.descriptor?.let {
                    Text(
                        text = it,
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurface,
                    )
                }

                val notes = buildList {
                    if (sighting.bonded) add("paired")
                    // Worth showing: a fitted model and a generic prior can
                    // disagree about distance by a factor of three.
                    if (calibrated) add("calibrated")
                    // Saying so is more useful than showing hex as if it were an
                    // identity: this address will be different in ten minutes.
                    if (sighting.randomisedAddress) add("randomised address")
                    if (sighting.txPower != null) add("${sighting.txPower} dBm TX")
                }
                if (notes.isNotEmpty()) {
                    Text(
                        text = notes.joinToString(" · "),
                        style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }

            Spacer(Modifier.width(12.dp))
            SignalBar(sighting.rssi, tone)
            Spacer(Modifier.width(12.dp))

            Text(
                text = sighting.rssi?.toString() ?: "--",
                style = MaterialTheme.typography.titleMedium,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Bold,
                color = tone,
            )
        }
    }
}

/** Five bars, so the row is readable without relying on colour alone. */
@Composable
private fun SignalBar(rssi: Int?, tone: androidx.compose.ui.graphics.Color) {
    val filled = rssi?.let { (((it.coerceIn(-100, -30) + 100) / 70f) * 5f).toInt().coerceIn(0, 5) } ?: 0
    Row(verticalAlignment = Alignment.Bottom) {
        repeat(5) { i ->
            Box(
                Modifier
                    .width(4.dp)
                    .height((6 + i * 4).dp)
                    .padding(end = 1.dp)
                    .clip(RoundedCornerShape(1.dp))
                    .background(
                        if (i < filled) tone
                        else MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.20f)
                    ),
            )
            Spacer(Modifier.width(2.dp))
        }
    }
}

@Preview(showBackground = true, backgroundColor = 0xFF0E1116)
@Composable
private fun PreviewSurvey() = SuperfindTheme(darkTheme = true) {
    SurveyScreen(
        sightings = listOf(
            Sighting("AA:BB:CC:DD:EE:01", "Pixel 9 Pro", -47, -47.0, -4, 0.0, bonded = true),
            Sighting("32:A7:AC:77:21:1C", "32:A7:AC:77:21:1C", -68, -68.0, 12, 0.0,
                randomisedAddress = true, descriptor = "Apple · Nearby"),
            Sighting("AA:BB:CC:DD:EE:03", "JioSTB Bed Room TV", -84, -84.0, null, 0.0),
            Sighting("88:D0:39:C8:07:CE", "Soundcore Life Q20", null,
                Double.NEGATIVE_INFINITY, null, 0.0, bonded = true),
        ),
        tier = Tier.GUIDED,
        headline = "Distance and direction by walking",
        instruction = "Turn slowly on the spot, then walk a dogleg — not a straight line.",
        limitations = listOf(
            "No ranging radio, so distances come from signal strength and are affected by walls, metal and bodies.",
        ),
        error = null,
        onSelect = {},
        calibrated = setOf("AA:BB:CC:DD:EE:01"),
        onHuntAddress = { true },
    )
}
