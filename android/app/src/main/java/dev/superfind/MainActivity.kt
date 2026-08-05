package dev.superfind

import android.os.Bundle
import android.os.SystemClock
import android.widget.Toast
import androidx.activity.ComponentActivity
import androidx.activity.compose.BackHandler
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Button
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.lifecycle.viewmodel.compose.viewModel
import dev.superfind.core.NativeCore
import dev.superfind.radio.Permissions
import dev.superfind.radio.Tier
import dev.superfind.ui.HuntScreen
import dev.superfind.ui.HuntViewModel
import dev.superfind.ui.Screen
import dev.superfind.ui.SuperfindTheme
import dev.superfind.ui.SurveyScreen

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContent {
            SuperfindTheme {
                Surface(Modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
                    App()
                }
            }
        }
    }
}

@Composable
private fun App(model: HuntViewModel = viewModel()) {
    val capabilities = model.capabilities
    val context = model.getApplication<android.app.Application>()

    var granted by remember {
        mutableStateOf(Permissions.allGranted(context, Permissions.scanning()))
    }

    val launcher = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestMultiplePermissions()
    ) { result ->
        granted = result.values.all { it }
    }

    // Requested at the moment scanning actually begins rather than in a wall at
    // launch, which is what keeps install-to-first-use conversion alive.
    LaunchedEffect(granted) {
        if (granted) model.startSurvey()
    }

    if (!granted) {
        PermissionGate(
            rationale = Permissions.scanning().joinToString("\n\n") { Permissions.rationale(it) },
            onRequest = { launcher.launch(Permissions.scanning().toTypedArray()) },
        )
        return
    }

    ConfirmExitOnBack(enabled = true)

    val screen by model.screen.collectAsState()
    val sightings by model.sightings.collectAsState()
    val snapshot by model.snapshot.collectAsState()
    val error by model.error.collectAsState()
    val linkSupported by model.linkSupported.collectAsState()
    val floors by model.floors.collectAsState()
    val soundEnabled by model.soundEnabled.collectAsState()
    val hapticsEnabled by model.hapticsEnabled.collectAsState()

    // The native-core warning joins the hardware limitations: from the user's
    // point of view they are the same kind of fact — something this build of
    // this app on this phone cannot do, stated plainly.
    val limitations = buildList {
        addAll(capabilities.limitations())
        if (!NativeCore.available) add(NativeCore.unavailableReason)
    }

    // Back from a hunt returns to the list rather than leaving the app. Losing a
    // search because of a reflexive back-press would be infuriating, and the
    // list is where the user would go next anyway.
    BackHandler(enabled = screen is Screen.Hunt) { model.closeHunt() }

    when (val current = screen) {
        is Screen.Survey -> SurveyScreen(
            sightings = sightings,
            tier = capabilities.tier,
            headline = capabilities.headline,
            instruction = capabilities.instruction,
            limitations = limitations,
            error = error,
            onSelect = { model.startHunt(it.address, it.label) },
            onHuntAddress = { model.huntAddress(it) },
        )

        is Screen.Hunt -> HuntScreen(
            deviceName = current.label,
            snapshot = snapshot,
            fusionAvailable = model.fusionAvailable,
            tierHeadline = capabilities.headline,
            instruction = if (capabilities.tier == Tier.UNAVAILABLE) {
                "Turn Bluetooth on to continue."
            } else {
                capabilities.instruction
            },
            headingIsAbsolute = capabilities.headingIsAbsolute,
            randomisedAddress = current.randomisedAddress,
            linkSupported = linkSupported,
            floors = floors,
            soundEnabled = soundEnabled,
            hapticsEnabled = hapticsEnabled,
            hasHaptics = model.hasHaptics,
            onToggleSound = { model.setSound(it) },
            onToggleHaptics = { model.setHaptics(it) },
            onClose = { model.closeHunt() },
            onReset = { model.reset() },
        )
    }
}

/**
 * Press back twice to leave.
 *
 * A single back-press from the top screen would drop a hunt in progress, and
 * this is an app used one-handed while walking and looking at the room rather
 * than the phone — precisely the situation in which a stray press happens. Two
 * seconds is the conventional window; the toast is what makes the first press
 * legible rather than merely ignored.
 */
@Composable
private fun ConfirmExitOnBack(enabled: Boolean) {
    val context = LocalContext.current
    var armedAt by remember { mutableStateOf(0L) }

    BackHandler(enabled = enabled) {
        val now = SystemClock.elapsedRealtime()
        if (now - armedAt in 1..EXIT_WINDOW_MS) {
            (context as? ComponentActivity)?.finish()
        } else {
            armedAt = now
            Toast.makeText(context, "Press back again to exit", Toast.LENGTH_SHORT).show()
        }
    }
}

private const val EXIT_WINDOW_MS = 2000L

@Composable
private fun PermissionGate(rationale: String, onRequest: () -> Unit) {
    Column(
        Modifier
            .fillMaxSize()
            .background(MaterialTheme.colorScheme.background)
            .padding(28.dp),
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            text = "Superfind needs to hear nearby devices",
            style = MaterialTheme.typography.headlineSmall,
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onBackground,
        )
        Text(
            text = rationale,
            style = MaterialTheme.typography.bodyMedium,
            textAlign = TextAlign.Center,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(vertical = 20.dp),
        )
        Button(onClick = onRequest) { Text("Continue") }
    }
}
