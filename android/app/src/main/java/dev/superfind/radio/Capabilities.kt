package dev.superfind.radio

import android.bluetooth.BluetoothAdapter
import android.bluetooth.BluetoothManager
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.Sensor
import android.hardware.SensorManager
import android.os.Build

/**
 * What this particular phone can do, discovered at runtime.
 *
 * The whole compatibility story lives here. The tempting design is to gate
 * features on `Build.VERSION.SDK_INT` and grey out what an old device lacks;
 * that reads as broken rather than as adapted, and never explains why.
 *
 * Instead every capability is probed independently, and the *combination*
 * decides which [Tier] the user is in. Each tier is a complete, working
 * experience with its own honest description — an old phone is not a broken new
 * phone.
 *
 * The point worth internalising: **a compass alone reaches [Tier.GUIDED]**,
 * which recovers both distance and direction. `TYPE_ROTATION_VECTOR` has existed
 * since API 9 and BLE scanning since API 18. Ranging radios improve the answer;
 * they are not what makes it possible.
 */
data class Capabilities(
    val ranging: RangeTech?,
    val angleOfArrival: Boolean,
    val connectedRssi: Boolean,
    val advertRssi: Boolean,
    val compass: Boolean,
    val compassSource: CompassSource,
    /**
     * Whether the heading is referenced to magnetic north.
     *
     * False on devices with a gyroscope but no magnetometer, which turn out to
     * be common in the budget tier — the Moto G40 Fusion this was first tested
     * on has exactly that shape. Their `TYPE_GAME_ROTATION_VECTOR` gives a
     * *relative* heading: stable over a session, but with an arbitrary zero.
     *
     * That is still enough for the synthetic aperture, which only needs turning
     * 90 degrees to register as 90 degrees. What is lost is the ability to name
     * a direction "north-east" — so the radar hides its compass rose and the
     * arrow is read relative to how the phone is being held, which is what the
     * user acts on anyway.
     */
    val headingIsAbsolute: Boolean,
    val stepDetection: Boolean,
    val stepSource: StepSource,
    val apiLevel: Int,
) {
    val tier: Tier
        get() = when {
            !advertRssi && !connectedRssi && ranging == null -> Tier.UNAVAILABLE
            ranging != null -> Tier.PRECISION
            compass -> Tier.GUIDED
            else -> Tier.PROXIMITY
        }

    val bearingQuality: BearingQuality
        get() = when {
            angleOfArrival -> BearingQuality.MEASURED
            compass -> BearingQuality.INFERRED
            else -> BearingQuality.NONE
        }

    /** Best-case accuracy once enough evidence is in, for setting expectations. */
    val expectedAccuracyM: Double?
        get() = when (tier) {
            Tier.UNAVAILABLE -> null
            Tier.PRECISION -> ranging?.typicalAccuracyM
            Tier.GUIDED -> 3.0
            Tier.PROXIMITY -> 8.0
        }

    /** Says what the user gets, not what they lack. */
    val headline: String
        get() = when {
            tier == Tier.UNAVAILABLE -> "Bluetooth unavailable"
            tier == Tier.PRECISION && angleOfArrival -> "Precise distance and direction"
            tier == Tier.PRECISION -> "Precise distance, direction by walking"
            tier == Tier.GUIDED && !headingIsAbsolute -> "Distance and relative direction"
            tier == Tier.GUIDED -> "Distance and direction by walking"
            else -> "Warmer and colder only"
        }

    /** Usually the most useful sentence on the screen. */
    val instruction: String
        get() = when {
            tier == Tier.UNAVAILABLE -> "Turn Bluetooth on to begin."
            tier == Tier.PRECISION && angleOfArrival -> "Point the phone around slowly."
            tier == Tier.PRECISION -> "Walk a few steps; the estimate settles quickly."
            tier == Tier.GUIDED -> "Turn slowly on the spot, then walk a dogleg — not a straight line."
            else -> "Walk around and watch the number. Closer to zero is nearer."
        }

    /** Said up front rather than discovered as disappointment later. */
    fun limitations(): List<String> {
        if (tier == Tier.UNAVAILABLE) {
            return listOf("This device has no usable Bluetooth LE scanning.")
        }
        val out = mutableListOf<String>()
        if (!compass) {
            out += "No compass, so this phone cannot work out a direction — only whether you are getting closer."
        } else if (!stepDetection) {
            out += "No step detection, so direction relies on turning rather than walking."
        }
        if (compass && !headingIsAbsolute) {
            out += "No magnetometer, so direction is shown relative to how you are holding " +
                "the phone rather than as a compass bearing. Keep it pointed the same way."
        }
        if (ranging == null) {
            out += "No ranging radio, so distances come from signal strength and are affected by walls, metal and bodies."
        }
        if (ranging == RangeTech.WIFI_RTT) {
            out += "Wi-Fi ranging is coarser than UWB — expect metres, not centimetres."
        }
        return out
    }

    companion object {
        /**
         * Probe the device. Every check is independent and defensive: a missing
         * system service or a manufacturer that lies about a feature flag must
         * degrade the tier, never crash the app.
         */
        fun detect(context: Context): Capabilities {
            val api = Build.VERSION.SDK_INT
            val pm = context.packageManager
            val sensors = context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager

            val hasBle = pm.hasSystemFeature(PackageManager.FEATURE_BLUETOOTH_LE)
            val adapter = runCatching {
                (context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager)?.adapter
            }.getOrNull()
            val scanner = runCatching { adapter?.bluetoothLeScanner }.getOrNull()
            val bleUsable = hasBle && adapter != null && scanner != null && adapter.isEnabled

            val compassSource = detectCompass(sensors)
            val stepSource = detectSteps(sensors)

            return Capabilities(
                ranging = detectRanging(context, pm, api),
                // Angle of arrival needs UWB. Nothing else in a phone produces
                // it, and Channel Sounding explicitly does not.
                angleOfArrival = api >= 31 && pm.hasSystemFeature(FEATURE_UWB),
                // Any device that can scan can also connect and read link RSSI.
                connectedRssi = bleUsable,
                advertRssi = bleUsable,
                compass = compassSource != CompassSource.NONE,
                compassSource = compassSource,
                headingIsAbsolute = compassSource.isAbsolute,
                stepDetection = stepSource != StepSource.NONE,
                stepSource = stepSource,
                apiLevel = api,
            )
        }

        /**
         * Heading, in descending order of quality.
         *
         * `TYPE_ROTATION_VECTOR` fuses gyroscope, accelerometer and
         * magnetometer and is what should be used when present. The fallback
         * composes a rotation matrix from raw accelerometer and magnetometer
         * readings — noticeably noisier and more affected by nearby metal, but
         * it works on hardware with no gyroscope at all.
         */
        private fun detectCompass(sensors: SensorManager?): CompassSource {
            if (sensors == null) return CompassSource.NONE
            if (sensors.getDefaultSensor(Sensor.TYPE_ROTATION_VECTOR) != null) {
                return CompassSource.ROTATION_VECTOR
            }
            if (sensors.getDefaultSensor(Sensor.TYPE_GEOMAGNETIC_ROTATION_VECTOR) != null) {
                return CompassSource.GEOMAGNETIC_ROTATION_VECTOR
            }
            val hasAccel = sensors.getDefaultSensor(Sensor.TYPE_ACCELEROMETER) != null
            val hasMag = sensors.getDefaultSensor(Sensor.TYPE_MAGNETIC_FIELD) != null
            if (hasAccel && hasMag) return CompassSource.ACCELEROMETER_MAGNETOMETER
            // No magnetometer, but a gyroscope-derived orientation. Relative
            // rather than absolute, and entirely sufficient to sweep an
            // aperture — so this is a Guided device, not a Proximity one.
            if (sensors.getDefaultSensor(Sensor.TYPE_GAME_ROTATION_VECTOR) != null) {
                return CompassSource.GAME_ROTATION_VECTOR
            }
            return CompassSource.NONE
        }

        /**
         * Steps, in descending order of quality.
         *
         * `TYPE_STEP_DETECTOR` is a hardware detector, cheap and accurate, but
         * it is optional hardware — plenty of budget phones omit it. The
         * fallback detects accelerometer magnitude peaks, which is less precise
         * but available anywhere there is an accelerometer, which is everywhere.
         */
        private fun detectSteps(sensors: SensorManager?): StepSource {
            if (sensors == null) return StepSource.NONE
            if (sensors.getDefaultSensor(Sensor.TYPE_STEP_DETECTOR) != null) {
                return StepSource.HARDWARE_DETECTOR
            }
            if (sensors.getDefaultSensor(Sensor.TYPE_ACCELEROMETER) != null) {
                return StepSource.ACCELEROMETER_PEAKS
            }
            return StepSource.NONE
        }

        /**
         * The ranging ladder, best first.
         *
         * Note what is *not* checked here: whether the target device can range.
         * It cannot be known until a session is attempted, because ranging is a
         * negotiated two-way protocol — the lost phone must be awake, on a
         * recent Android, and cooperating. This function reports only what *we*
         * could offer, which is why the whole app is built so that the answer
         * being "nothing" is a working configuration rather than a failure.
         */
        private fun detectRanging(
            context: Context,
            pm: PackageManager,
            api: Int,
        ): RangeTech? {
            // Android 16's unified Ranging module. Reflection rather than a
            // direct reference: it keeps this file compiling against older SDKs
            // and costs one lookup at startup.
            if (api >= 36 && hasRangingService(context)) {
                if (pm.hasSystemFeature(FEATURE_UWB)) return RangeTech.UWB
                return RangeTech.CHANNEL_SOUNDING
            }
            // Jetpack UWB, available from Android 12 on devices with the radio.
            if (api >= 31 && pm.hasSystemFeature(FEATURE_UWB)) return RangeTech.UWB
            // 802.11mc Fine Timing Measurement, from Android 9.
            if (api >= 28 && pm.hasSystemFeature(PackageManager.FEATURE_WIFI_RTT)) {
                return RangeTech.WIFI_RTT
            }
            return null
        }

        private fun hasRangingService(context: Context): Boolean = runCatching {
            context.getSystemService("ranging") != null
        }.getOrDefault(false)

        /** `PackageManager.FEATURE_UWB`, inlined so this compiles below API 31. */
        private const val FEATURE_UWB = "android.hardware.uwb"
    }
}

/** Ordered worst to best. */
enum class Tier(val label: String) {
    UNAVAILABLE("Unavailable"),
    PROXIMITY("Proximity"),
    GUIDED("Guided"),
    PRECISION("Precision"),
}

enum class BearingQuality { MEASURED, INFERRED, NONE }

enum class RangeTech(val label: String, val typicalAccuracyM: Double) {
    UWB("Ultra-wideband", 0.2),
    CHANNEL_SOUNDING("Bluetooth Channel Sounding", 0.5),
    WIFI_RTT("Wi-Fi RTT", 2.0),
}

enum class CompassSource(val label: String, val isAbsolute: Boolean) {
    ROTATION_VECTOR("fused rotation vector", true),
    GEOMAGNETIC_ROTATION_VECTOR("geomagnetic rotation vector", true),
    ACCELEROMETER_MAGNETOMETER("accelerometer + magnetometer", true),
    /** Gyroscope-derived. Stable, but with an arbitrary zero. */
    GAME_ROTATION_VECTOR("game rotation vector (relative)", false),
    NONE("none", false),
}

enum class StepSource(val label: String) {
    HARDWARE_DETECTOR("hardware step detector"),
    ACCELEROMETER_PEAKS("accelerometer peaks"),
    NONE("none"),
}
