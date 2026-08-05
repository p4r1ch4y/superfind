package dev.superfind.ui

import android.app.Application
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import dev.superfind.core.Calibration
import dev.superfind.core.CalibrationRun
import dev.superfind.core.CalibrationStore
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
import dev.superfind.core.NativeCore
import dev.superfind.radio.KnownDevices
import dev.superfind.radio.PeerLink
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
    data class Calibrate(val address: String, val label: String) : Screen
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
    private val feedback = ProximityFeedback(app)
    private val settings = Settings(app)
    private val calibrations = CalibrationStore(app)

    init {
        // Apply the restored choices to the player, not merely to the UI state —
        // otherwise the chips would read "on" while nothing made a sound.
        feedback.soundEnabled = settings.soundEnabled
        feedback.hapticsEnabled = settings.hapticsEnabled
        feedback.volume = settings.volume
    }

    /**
     * Lives for the whole app, not for one hunt.
     *
     * Pressure readings need time to settle, and start arriving long before the
     * user picks something to look for. Recreating this per hunt would throw the
     * settled baseline away each time and report "same level" for a minute.
     */
    private val altimeter: Long =
        if (NativeCore.available) runCatching { NativeCore.createAltimeter() }.getOrDefault(0L)
        else 0L

    /** Where this device is in a shared frame, when hunting with others. */
    /**
     * Devices that have been travelling with the user.
     *
     * Lives as long as the app: the whole signal is persistence across places
     * you have moved between, so a watch recreated per hunt could never see one.
     */
    private val followWatch: Long =
        if (NativeCore.available) runCatching { NativeCore.createFollowWatch() }.getOrDefault(0L)
        else 0L

    private val _followers = MutableStateFlow<List<String>>(emptyList())
    val followers: StateFlow<List<String>> = _followers.asStateFlow()

    private var calibrationRun: CalibrationRun? = null
    private var calibrationJob: Job? = null

    private val _calibration = MutableStateFlow<CalibrationState?>(null)
    val calibration: StateFlow<CalibrationState?> = _calibration.asStateFlow()

    /** Addresses with a saved fit, so the survey can mark them. */
    val calibratedAddresses: Set<String> get() = calibrations.addresses()

    private var peerLink: PeerLink? = null
    private var peerPosition: Pair<Double, Double>? = null
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

    /**
     * Storeys climbed since the hunt began. Null when this device has no
     * barometer, or has not gathered enough samples to say.
     */
    private val _floors = MutableStateFlow<Int?>(null)
    val floors: StateFlow<Int?> = _floors.asStateFlow()

    val hasBarometer: Boolean get() = sensors.hasBarometer
    val hasHaptics: Boolean get() = feedback.hasHaptics

    // Restored from the last session. A phone hunting in a pocket is exactly
    // where the process gets killed and relaunched, so losing the choice would
    // happen at the moment it mattered most.
    private val _soundEnabled = MutableStateFlow(settings.soundEnabled)
    val soundEnabled: StateFlow<Boolean> = _soundEnabled.asStateFlow()

    private val _hapticsEnabled = MutableStateFlow(settings.hapticsEnabled)
    val hapticsEnabled: StateFlow<Boolean> = _hapticsEnabled.asStateFlow()

    /** Amplitude, 0 to 1. Defaults loud; see [ProximityFeedback.volume]. */
    fun setVolume(volume: Double) {
        feedback.volume = volume
        settings.volume = volume
    }

    fun setSound(on: Boolean) {
        _soundEnabled.value = on
        feedback.soundEnabled = on
        settings.soundEnabled = on
    }

    fun setHaptics(on: Boolean) {
        _hapticsEnabled.value = on
        feedback.hapticsEnabled = on
        settings.hapticsEnabled = on
    }

    private var tracker: Tracker? = null
    private var scanJob: Job? = null
    private var motionJob: Job? = null
    private var tickJob: Job? = null
    private var linkJob: Job? = null
    private var peerJob: Job? = null

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

        // Refresh the co-travel list alongside the survey, and forget anything
        // older than a day so a device you have simply owned for a long time is
        // not incriminated by yesterday's journey.
        if (followWatch != 0L) {
            viewModelScope.launch {
                while (true) {
                    val now = BleScanner.now()
                    NativeCore.pruneSightings(followWatch, now, 24.0 * 3600.0)
                    _followers.value = runCatching { NativeCore.followers(followWatch) }
                        .getOrNull()
                        ?.split("\n")
                        ?.filter { it.isNotBlank() }
                        .orEmpty()
                    delay(15_000)
                }
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

        // A fitted model beats the published priors by a wide margin, and the
        // difference is multiplicative in distance rather than additive.
        calibrations[address]?.let {
            active.setPathLoss(it.txPower1m, it.exponent)
        }

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
                    // Pass it on so peers can intersect their ring with ours.
                    peerLink?.share(
                        target = address,
                        rssiDbm = advert.rssi,
                        seconds = advert.timestampSeconds,
                        source = RssiSource.ADVERTISEMENT.ordinal,
                    )
                }
        }

        // A connected link in parallel with the scan. This is what makes paired
        // everyday devices findable: many stop advertising once bonded and idle,
        // and Classic-only audio devices never show up in an LE scan at all.
        // Both sources feed the same filter, which already trusts a link read
        // roughly twice as much as an advertisement.
        feedback.start(viewModelScope)

        // The floor readout is relative to where the hunt started, not to where
        // the app launched.
        if (altimeter != 0L) NativeCore.anchorAltitude(altimeter)

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

        // Peers observing the same device from their own positions. This is what
        // collapses the annulus that one observer, standing still, never can.
        peerLink?.let { link ->
            peerJob = viewModelScope.launch {
                link.reports(address).collect { report ->
                    active.observeRssiFrom(
                        dbm = report.rssiDbm,
                        sourceOrdinal = report.source,
                        x = report.x,
                        y = report.y,
                        atSeconds = report.seconds,
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
                    is Motion.Pressure ->
                        if (altimeter != 0L) NativeCore.observePressure(altimeter, motion.pascals, now)
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
                // Silence here means the reading has gone stale, never that the
                // device is merely far away — a distant one still clicks slowly.
                val dbm = snap.rssiDbm
                if (dbm != null && snap.isFresh && NativeCore.available) {
                    val cue = runCatching {
                        NativeCore.proximityCue(dbm, 70, 1400, 440, 1320, -45.0, -95.0)
                    }.getOrNull()
                    if (cue != null && cue.size >= 2) {
                        feedback.update(cue[0].toInt(), cue[1].toInt())
                    }
                } else {
                    feedback.silence()
                }

                if (altimeter != 0L) {
                    val delta = NativeCore.floorDelta(altimeter)
                    _floors.value = if (delta.isNaN()) null else delta.toInt()
                }

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

    // ---- Calibration -------------------------------------------------------

    fun startCalibration(address: String, label: String) {
        stopAll()
        val run = CalibrationRun()
        calibrationRun = run
        _screen.value = Screen.Calibrate(address, label)
        _calibration.value = CalibrationState.Waiting(run.currentDistance, 1, run.steps.size)
    }

    /**
     * Begin collecting at the current distance.
     *
     * Readings are taken from advertisements rather than a connected link, for
     * a reason worth stating: the model being fitted is the one the *hunt* will
     * use, and the hunt reads advertisements. Calibrating against a quieter
     * source would produce a model that is accurate for measurements the app
     * never makes.
     */
    fun collectCalibrationStep() {
        val run = calibrationRun ?: return
        val screen = _screen.value as? Screen.Calibrate ?: return

        _calibration.value = CalibrationState.Collecting(
            run.currentDistance, run.stepIndex + 1, run.steps.size, 0, run.samplesPerStep,
        )

        calibrationJob?.cancel()
        calibrationJob = viewModelScope.launch {
            scanner.scan(targetAddress = screen.address)
                .catch { _error.value = it.message }
                .collect { advert ->
                    if (!advert.address.equals(screen.address, ignoreCase = true)) return@collect
                    run.record(advert.rssi.toDouble())
                    _calibration.value = CalibrationState.Collecting(
                        run.currentDistance,
                        run.stepIndex + 1,
                        run.steps.size,
                        run.samplesInStep,
                        run.samplesPerStep,
                    )
                    if (run.isStepComplete) {
                        run.advance()
                        calibrationJob?.cancel()
                        _calibration.value = if (run.isComplete) {
                            finishCalibration(run)
                        } else {
                            CalibrationState.Waiting(
                                run.currentDistance, run.stepIndex + 1, run.steps.size,
                            )
                        }
                    }
                }
        }
    }

    private fun finishCalibration(run: CalibrationRun): CalibrationState {
        val fit = run.fit()
        return if (fit == null) {
            // The core refused it. Least squares always returns something, and
            // in a reflective room that something is confidently wrong.
            CalibrationState.Rejected(
                "The readings did not fit a path-loss curve closely enough to " +
                    "trust. That usually means reflections — try a corridor or a " +
                    "larger room, with the device in clear air rather than on a " +
                    "metal surface. The built-in model is being kept."
            )
        } else {
            CalibrationState.Done(fit)
        }
    }

    fun saveCalibration(calibration: Calibration) {
        val screen = _screen.value as? Screen.Calibrate ?: return
        calibrations.put(screen.address, calibration)
        cancelCalibration()
    }

    fun retryCalibration() {
        val screen = _screen.value as? Screen.Calibrate ?: return
        startCalibration(screen.address, screen.label)
    }

    fun cancelCalibration() {
        calibrationJob?.cancel()
        calibrationJob = null
        calibrationRun = null
        _calibration.value = null
        _screen.value = Screen.Survey
        startSurvey()
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
        feedback.silence()
        scanJob?.cancel(); scanJob = null
        motionJob?.cancel(); motionJob = null
        linkJob?.cancel(); linkJob = null
        peerJob?.cancel(); peerJob = null
        tickJob?.cancel(); tickJob = null
        tracker?.close(); tracker = null
    }

    /**
     * Pool readings with other devices hunting the same thing.
     *
     * `position` is metres east and north of whoever anchored the session, and
     * nothing establishes that frame automatically — it is measured by a person.
     * Null means this device listens and fuses but contributes nothing, which is
     * the honest state for a device nobody has placed.
     */
    fun shareWith(session: String, position: Pair<Double, Double>?) {
        peerPosition = position
        peerLink = PeerLink(getApplication(), session, PeerLink.deviceName(), position)
        // Take effect on the next hunt rather than mid-flight: swapping the
        // observer set under a converged filter would move the estimate for
        // reasons the user did not cause.
    }

    fun stopSharing() {
        peerLink = null
        peerPosition = null
    }

    val sharing: Boolean get() = peerLink != null

    override fun onCleared() {
        stopAll()
        feedback.stop()
        if (altimeter != 0L) runCatching { NativeCore.destroyAltimeter(altimeter) }
        if (followWatch != 0L) runCatching { NativeCore.destroyFollowWatch(followWatch) }
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
