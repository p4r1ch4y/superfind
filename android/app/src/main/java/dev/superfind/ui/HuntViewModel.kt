package dev.superfind.ui

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.superfind.core.RssiSource
import dev.superfind.core.Snapshot
import dev.superfind.core.Tracker
import dev.superfind.motion.Motion
import dev.superfind.motion.MotionSensors
import dev.superfind.radio.Advert
import dev.superfind.radio.BleScanner
import dev.superfind.radio.Capabilities
import dev.superfind.radio.GattLink
import dev.superfind.radio.KnownDevice
import dev.superfind.radio.KnownDevices
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.launch

/** A device seen during the survey, or known to Android but not currently heard. */
data class Sighting(
    val address: String,
    val label: String,
    /** Null when the device is paired but not currently advertising. */
    val rssi: Int?,
    /**
     * Exponentially smoothed RSSI, used only for sort order.
     *
     * Sorting on the raw value made rows jump under the user's finger — a tap
     * aimed at one device would land on another, which is not a cosmetic
     * problem but a wrong-target bug. Smoothing makes the ordering change at
     * human speed while the displayed number stays live.
     */
    val sortRssi: Double,
    val txPower: Int?,
    val lastSeen: Double,
    /** Paired with this phone, so its name and address are both stable. */
    val bonded: Boolean = false,
    /** Vendor/service description, shown when there is no broadcast name. */
    val descriptor: String? = null,
    /** The advertised address rotates, so it identifies nothing across sessions. */
    val randomisedAddress: Boolean = false,
)

sealed interface Screen {
    data object Survey : Screen
    data class Hunt(
        val address: String,
        val label: String,
        /**
         * The target advertises from a rotating private address.
         *
         * This matters more than it sounds: a scan filtered on such an address
         * goes permanently deaf the moment it rotates, and the UI would sit
         * showing "No contact" forever as though the device had simply gone
         * away. Knowing it lets us say what actually happened.
         */
        val randomisedAddress: Boolean = false,
    ) : Screen
}

/**
 * Owns the scan, the sensors and the tracker, and exposes one immutable
 * [Snapshot] for the UI to render.
 *
 * The single-snapshot boundary is inherited from the CLI and matters for the
 * same reason: the UI cannot reach into mutable tracker state, so two figures on
 * screen can never come from different instants and disagree.
 */
class HuntViewModel(app: Application) : AndroidViewModel(app) {

    val capabilities: Capabilities = Capabilities.detect(app)

    private val scanner = BleScanner(app)
    private val known = KnownDevices(app)
    private val gatt = GattLink(app)
    private val sensors = MotionSensors(app, capabilities.compassSource, capabilities.stepSource)

    private val _screen = MutableStateFlow<Screen>(Screen.Survey)
    val screen: StateFlow<Screen> = _screen.asStateFlow()

    private val _sightings = MutableStateFlow<List<Sighting>>(emptyList())
    val sightings: StateFlow<List<Sighting>> = _sightings.asStateFlow()

    private val _snapshot = MutableStateFlow(Snapshot.empty())
    val snapshot: StateFlow<Snapshot> = _snapshot.asStateFlow()

    private val _error = MutableStateFlow<String?>(null)
    val error: StateFlow<String?> = _error.asStateFlow()

    /** False when the target is Classic-only, so no connected link is possible. */
    private val _linkSupported = MutableStateFlow(true)
    val linkSupported: StateFlow<Boolean> = _linkSupported.asStateFlow()

    private var tracker: Tracker? = null
    private var scanJob: Job? = null
    private var motionJob: Job? = null
    private var tickJob: Job? = null
    private var linkJob: Job? = null

    val fusionAvailable: Boolean get() = tracker?.fusionAvailable ?: false

    fun startSurvey() {
        stopAll()
        _error.value = null
        val seen = LinkedHashMap<String, Sighting>()

        // Names Android already holds for paired devices. A BLE advert carries a
        // name only sometimes, and most devices advertise from an address that
        // rotates — so without this join the survey is a list of meaningless hex.
        val namesByAddress = known.byAddress()

        fun publish() {
            val now = BleScanner.now()
            val live = seen.values.filter { now - it.lastSeen < STALE_AFTER_S }
            val heard = live.map { it.address.uppercase() }.toSet()

            // Paired devices are listed whether or not they are being heard.
            // Classic-only headphones do not appear in an LE scan when idle, and
            // hiding them until they happen to shout is exactly the behaviour
            // that makes these apps feel broken.
            val silentlyPaired = namesByAddress.values
                .filter { it.address !in heard }
                .map { device ->
                    Sighting(
                        address = device.address,
                        label = device.name,
                        rssi = null,
                        sortRssi = Double.NEGATIVE_INFINITY,
                        txPower = null,
                        lastSeen = 0.0,
                        bonded = true,
                    )
                }

            _sightings.value = (live + silentlyPaired)
                .sortedWith(
                    compareByDescending<Sighting> { it.sortRssi }
                        .thenBy { it.address }
                )
        }

        scanJob = viewModelScope.launch {
            scanner.scan()
                .catch { _error.value = it.message }
                .collect { advert ->
                    val address = advert.address.uppercase()
                    val paired: KnownDevice? = namesByAddress[address]
                    seen[advert.address] = Sighting(
                        address = advert.address,
                        // A paired device's stored name beats whatever the
                        // advert happened to carry, which is often nothing.
                        label = paired?.name ?: advert.label,
                        descriptor = advert.identity.takeUnless { it.isEmpty }?.label
                            ?.takeIf { it != advert.label },
                        rssi = advert.rssi,
                        sortRssi = seen[advert.address]?.sortRssi
                            ?.let { it * 0.8 + advert.rssi * 0.2 }
                            ?: advert.rssi.toDouble(),
                        txPower = advert.txPower,
                        lastSeen = advert.timestampSeconds,
                        bonded = paired != null,
                        randomisedAddress = KnownDevices.isRandomised(advert.address),
                    )
                    publish()
                }
        }

        // Show what is already known immediately, rather than an empty list
        // while the first advertisements arrive.
        publish()
    }

    fun startHunt(address: String, label: String) {
        stopAll()
        _error.value = null
        _screen.value = Screen.Hunt(address, label, KnownDevices.isRandomised(address))

        val active = Tracker.create()
        tracker = active

        scanJob = viewModelScope.launch {
            scanner.scan(targetAddress = address)
                .catch { _error.value = it.message }
                .collect { advert ->
                    // Passively observed advertisements. Declared as such so the
                    // filter widens its noise term rather than trusting them like
                    // a connected-link read.
                    active.observeRssi(
                        dbm = advert.rssi.toDouble(),
                        source = RssiSource.ADVERTISEMENT,
                        atSeconds = advert.timestampSeconds,
                    )
                }
        }

        // A connected link in parallel with the scan. This is what makes paired
        // everyday devices findable: many stop advertising once bonded and idle,
        // and Classic-only audio devices never show up in an LE scan at all.
        // Both sources feed the same filter, which already trusts a link read
        // roughly twice as much as an advertisement.
        // Only started where a GATT server can exist. Classic-only devices are
        // reported as unsupported instead, which the UI states plainly.
        _linkSupported.value = gatt.isSupported(address)
        linkJob = viewModelScope.launch {
            gatt.rssi(address).collect { dbm ->
                if (dbm != null) {
                    active.observeRssi(
                        dbm = dbm.toDouble(),
                        source = RssiSource.CONNECTED_LINK,
                        atSeconds = BleScanner.now(),
                    )
                }
            }
        }

        motionJob = viewModelScope.launch {
            sensors.motion().collect { motion ->
                val now = BleScanner.now()
                when (motion) {
                    is Motion.Heading -> active.setHeading(motion.radians, now)
                    is Motion.Step -> active.step(motion.lengthM, now)
                }
            }
        }

        tickJob = viewModelScope.launch {
            var lastLogged = 0.0
            while (true) {
                val now = BleScanner.now()
                val snap = active.snapshot(now)
                _snapshot.value = snap

                // One line a second to logcat, so a walk can be reviewed as a
                // time series rather than a handful of screenshots. Cheap, and
                // it is the only way to tell whether the signal actually tracked
                // the walk or merely looked plausible in a still frame.
                if (now - lastLogged >= 1.0) {
                    lastLogged = now
                    android.util.Log.d(
                        TELEMETRY,
                        "t=%.0f rssi=%s band=%s trend=%s fresh=%b n=%d swept=%.0f%% heading=%.0f steps=%d"
                            .format(
                                snap.elapsedSeconds,
                                snap.rssiDbm?.let { "%.0f".format(it) } ?: "--",
                                snap.proximity?.name ?: "-",
                                snap.trend.name,
                                snap.isFresh,
                                snap.totalSamples,
                                snap.headingCoverage * 100,
                                Math.toDegrees(snap.userHeadingRad),
                                snap.steps,
                            )
                    )
                }
                delay(FRAME_MS)
            }
        }
    }

    /**
     * Hunt an address typed by the user rather than picked from the list.
     *
     * The case that motivates it: you know the MAC of the thing you are looking
     * for — from a label, an earlier session, or another tool — but it is not
     * advertising right now, so there is nothing to tap. Starting the hunt anyway
     * means the app is already listening the moment it wakes up.
     */
    fun huntAddress(rawAddress: String): Boolean {
        val address = KnownDevices.normaliseAddress(rawAddress) ?: return false
        val label = known.byAddress()[address]?.name ?: address
        startHunt(address, label)
        return true
    }

    fun reset() {
        tracker?.reset()
    }

    fun closeHunt() {
        stopAll()
        _snapshot.value = Snapshot.empty()
        _screen.value = Screen.Survey
        startSurvey()
    }

    private fun stopAll() {
        scanJob?.cancel(); scanJob = null
        motionJob?.cancel(); motionJob = null
        linkJob?.cancel(); linkJob = null
        tickJob?.cancel(); tickJob = null
        tracker?.close(); tracker = null
    }

    override fun onCleared() {
        stopAll()
        super.onCleared()
    }

    private companion object {
        // 8 Hz: fast enough that the arrow feels live, slow enough that the
        // radar's animations do the smoothing rather than the data rate.
        const val FRAME_MS = 125L
        const val STALE_AFTER_S = 20.0
        const val TELEMETRY = "SuperfindWalk"
    }
}
