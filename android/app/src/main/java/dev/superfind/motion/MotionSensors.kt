package dev.superfind.motion

import android.content.Context
import android.hardware.Sensor
import android.hardware.SensorEvent
import android.hardware.SensorEventListener
import android.hardware.SensorManager
import android.hardware.display.DisplayManager
import android.view.Display
import android.view.Surface
import dev.superfind.radio.CompassSource
import dev.superfind.radio.StepSource
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlin.math.abs
import kotlin.math.sqrt

/** What the user's body is doing. The aperture that makes direction recoverable. */
sealed interface Motion {
    /** Compass heading in radians clockwise from north. */
    data class Heading(val radians: Double) : Motion
    /** One detected step, of an estimated length in metres. */
    data class Step(val lengthM: Double) : Motion
    /**
     * Ambient pressure in pascals.
     *
     * Optional hardware: plenty of phones have no barometer at all, including
     * the one this was first tested on. Its absence costs the floor readout and
     * nothing else.
     */
    data class Pressure(val pascals: Double) : Motion
}

/**
 * Heading and steps, with a fallback at every layer.
 *
 * This class is why a 2015 phone still gets a direction. Both signals degrade
 * independently rather than failing:
 *
 * - Heading prefers the fused `TYPE_ROTATION_VECTOR`, falls back to the
 *   magnetometer-only geomagnetic variant, then to composing a rotation matrix
 *   from raw accelerometer and magnetometer readings. The last works on hardware
 *   with no gyroscope at all — noisier and more upset by nearby metal, but
 *   present on essentially every Android device ever shipped.
 * - Steps prefer the hardware `TYPE_STEP_DETECTOR`, which many budget phones
 *   omit, and fall back to detecting peaks in accelerometer magnitude.
 */
class MotionSensors(
    private val context: Context,
    private val compassSource: CompassSource,
    private val stepSource: StepSource,
    /** Metres per step. 0.72 m is a reasonable adult default. */
    private val strideM: Double = 0.72,
) {
    private val sensors =
        context.getSystemService(Context.SENSOR_SERVICE) as? SensorManager

    /** Whether this device can tell you which floor something is on. */
    val hasBarometer: Boolean =
        sensors?.getDefaultSensor(Sensor.TYPE_PRESSURE) != null

    fun motion(): Flow<Motion> = callbackFlow {
        val manager = sensors
        if (manager == null) {
            close()
            return@callbackFlow
        }

        val rotation = FloatArray(9)
        val orientation = FloatArray(3)
        var latestAccel: FloatArray? = null
        var latestMag: FloatArray? = null

        val stepDetector = AccelerometerStepDetector()

        val listener = object : SensorEventListener {
            override fun onAccuracyChanged(sensor: Sensor?, accuracy: Int) = Unit

            override fun onSensorChanged(event: SensorEvent) {
                when (event.sensor.type) {
                    Sensor.TYPE_ROTATION_VECTOR,
                    Sensor.TYPE_GEOMAGNETIC_ROTATION_VECTOR,
                    // Gyro-derived, so the zero is arbitrary — but the aperture
                    // only needs turning 90 degrees to read as 90 degrees.
                    Sensor.TYPE_GAME_ROTATION_VECTOR -> {
                        SensorManager.getRotationMatrixFromVector(rotation, event.values)
                        emitHeading(rotation, orientation)
                    }

                    Sensor.TYPE_MAGNETIC_FIELD -> {
                        latestMag = event.values.copyOf()
                        val accel = latestAccel ?: return
                        if (SensorManager.getRotationMatrix(rotation, null, accel, event.values)) {
                            emitHeading(rotation, orientation)
                        }
                    }

                    Sensor.TYPE_ACCELEROMETER -> {
                        latestAccel = event.values.copyOf()
                        if (compassSource == CompassSource.ACCELEROMETER_MAGNETOMETER) {
                            val mag = latestMag
                            if (mag != null &&
                                SensorManager.getRotationMatrix(rotation, null, event.values, mag)
                            ) {
                                emitHeading(rotation, orientation)
                            }
                        }
                        if (stepSource == StepSource.ACCELEROMETER_PEAKS &&
                            stepDetector.accept(event.values, event.timestamp)
                        ) {
                            trySend(Motion.Step(strideM))
                        }
                    }

                    Sensor.TYPE_STEP_DETECTOR -> trySend(Motion.Step(strideM))

                    // Reported in hectopascals; the core works in pascals
                    // because a storey is 37 Pa and hPa would quantise it away.
                    Sensor.TYPE_PRESSURE ->
                        trySend(Motion.Pressure(event.values[0].toDouble() * 100.0))
                }
            }

            private fun emitHeading(rotation: FloatArray, orientation: FloatArray) {
                val adjusted = remapForDisplay(rotation)
                SensorManager.getOrientation(adjusted, orientation)
                trySend(Motion.Heading(orientation[0].toDouble()))
            }
        }

        val registered = mutableListOf<Sensor>()
        fun register(type: Int, rate: Int = SensorManager.SENSOR_DELAY_GAME) {
            manager.getDefaultSensor(type)?.let {
                manager.registerListener(listener, it, rate)
                registered += it
            }
        }

        when (compassSource) {
            CompassSource.ROTATION_VECTOR -> register(Sensor.TYPE_ROTATION_VECTOR)
            CompassSource.GEOMAGNETIC_ROTATION_VECTOR ->
                register(Sensor.TYPE_GEOMAGNETIC_ROTATION_VECTOR)
            CompassSource.ACCELEROMETER_MAGNETOMETER -> {
                register(Sensor.TYPE_ACCELEROMETER)
                register(Sensor.TYPE_MAGNETIC_FIELD)
            }
            CompassSource.GAME_ROTATION_VECTOR -> register(Sensor.TYPE_GAME_ROTATION_VECTOR)
            CompassSource.NONE -> Unit
        }

        // Slow rate: pressure moves at the speed of weather and stairs, and the
        // core smooths heavily anyway.
        if (hasBarometer) register(Sensor.TYPE_PRESSURE, SensorManager.SENSOR_DELAY_NORMAL)

        when (stepSource) {
            StepSource.HARDWARE_DETECTOR -> register(Sensor.TYPE_STEP_DETECTOR)
            // Already registered above if the compass needs it.
            StepSource.ACCELEROMETER_PEAKS ->
                if (compassSource != CompassSource.ACCELEROMETER_MAGNETOMETER) {
                    register(Sensor.TYPE_ACCELEROMETER)
                }
            StepSource.NONE -> Unit
        }

        awaitClose { manager.unregisterListener(listener) }
    }

    /**
     * Correct for how the device is held.
     *
     * `getOrientation` assumes the natural orientation. A phone in landscape,
     * or a tablet whose natural orientation *is* landscape, would otherwise
     * report a heading 90 degrees out — and a 90-degree error in the sweep bins
     * every sample into the wrong sector, which quietly points the arrow the
     * wrong way rather than failing visibly.
     */
    private fun remapForDisplay(rotation: FloatArray): FloatArray {
        // Via DisplayManager, not Context.getDisplay(). This class is
        // constructed with the application context, and from API 30
        // Context.getDisplay() throws UnsupportedOperationException on any
        // non-visual context — which crashed the app on the first sensor event
        // rather than at construction, so nothing but a device with a live
        // compass would ever have caught it. DisplayManager is safe from any
        // context, and the whole thing is wrapped because a rotation we cannot
        // read should degrade the heading, never take down the process.
        val display = runCatching {
            val manager = context.getSystemService(Context.DISPLAY_SERVICE) as? DisplayManager
            manager?.getDisplay(Display.DEFAULT_DISPLAY)?.rotation
        }.getOrNull() ?: Surface.ROTATION_0

        val (axisX, axisY) = when (display) {
            Surface.ROTATION_90 -> SensorManager.AXIS_Y to SensorManager.AXIS_MINUS_X
            Surface.ROTATION_180 -> SensorManager.AXIS_MINUS_X to SensorManager.AXIS_MINUS_Y
            Surface.ROTATION_270 -> SensorManager.AXIS_MINUS_Y to SensorManager.AXIS_X
            else -> SensorManager.AXIS_X to SensorManager.AXIS_Y
        }
        val out = FloatArray(9)
        return if (SensorManager.remapCoordinateSystem(rotation, axisX, axisY, out)) out
        else rotation
    }
}

/**
 * Step detection from accelerometer magnitude, for devices with no hardware
 * step detector.
 *
 * A walking step produces a magnitude peak well above gravity. The two guards
 * are what keep it from counting noise: the magnitude must cross a threshold
 * *upward* (so one peak is one step, not several), and steps closer together
 * than 250 ms are rejected, because nobody walks at four steps a second.
 */
private class AccelerometerStepDetector(
    private val thresholdMs2: Double = 11.5,
    private val minIntervalNanos: Long = 250_000_000L,
) {
    private var wasAbove = false
    private var lastStepNanos = 0L

    fun accept(values: FloatArray, timestampNanos: Long): Boolean {
        val magnitude = sqrt(
            values[0].toDouble() * values[0] +
                values[1].toDouble() * values[1] +
                values[2].toDouble() * values[2]
        )
        val above = magnitude > thresholdMs2
        val rising = above && !wasAbove
        wasAbove = above

        if (!rising) return false
        if (abs(timestampNanos - lastStepNanos) < minIntervalNanos) return false
        lastStepNanos = timestampNanos
        return true
    }
}
