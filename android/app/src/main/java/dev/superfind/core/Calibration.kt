package dev.superfind.core

import android.content.Context
import androidx.core.content.edit
import org.json.JSONObject

/**
 * A path-loss model fitted to one particular device.
 *
 * The built-in priors — `-59 dBm` at 1 m, exponent `2.8` — come from published
 * indoor studies rather than from your hardware. Transmit power varies by more
 * than 15 dB across devices, and because distance is recovered from
 * `10^((tx - rssi) / 10n)`, that error is *multiplicative*: assume a device is
 * 15 dB louder than it really is and every distance reads about a third of the
 * truth. Calibration is the single largest accuracy win available to a
 * signal-strength finder, and it costs a minute.
 */
data class Calibration(
    /** Expected RSSI at one metre, in dBm. */
    val txPower1m: Double,
    /** Path-loss exponent. Free space is 2; cluttered indoor space is 3 to 4. */
    val exponent: Double,
    /** Root-mean-square residual of the fit, in dB. Lower is a better fit. */
    val rmsDb: Double,
    /** Seconds since the epoch, so a stale fit can be spotted. */
    val fittedAt: Long,
) {
    /**
     * A tidy indoor fit lands around 3 to 5 dB. Much above that and the room was
     * reflective enough that the model is describing echoes.
     */
    val quality: String
        get() = when {
            rmsDb <= 3.0 -> "excellent"
            rmsDb <= 5.0 -> "good"
            rmsDb <= 8.0 -> "usable"
            else -> "poor"
        }
}

/**
 * Calibrations, kept per device address.
 *
 * Keyed by address rather than by name because names are absent on most
 * advertisements and change on the ones that have them, while a *public*
 * address is stable. A randomised address will rotate out from under this, and
 * that is an accepted limit: the devices worth calibrating — your own, paired,
 * public-addressed — are exactly the ones this works for.
 */
class CalibrationStore(context: Context) {

    private val prefs = context.applicationContext
        .getSharedPreferences("superfind_calibration", Context.MODE_PRIVATE)

    operator fun get(address: String): Calibration? {
        val raw = prefs.getString(address.uppercase(), null) ?: return null
        return runCatching {
            val json = JSONObject(raw)
            Calibration(
                txPower1m = json.getDouble("tx"),
                exponent = json.getDouble("n"),
                rmsDb = json.getDouble("rms"),
                fittedAt = json.optLong("at", 0L),
            )
        }.getOrNull()
    }

    fun put(address: String, calibration: Calibration) {
        val json = JSONObject()
            .put("tx", calibration.txPower1m)
            .put("n", calibration.exponent)
            .put("rms", calibration.rmsDb)
            .put("at", calibration.fittedAt)
        prefs.edit { putString(address.uppercase(), json.toString()) }
    }

    fun remove(address: String) {
        prefs.edit { remove(address.uppercase()) }
    }

    fun addresses(): Set<String> = prefs.all.keys
}

/**
 * Collects readings at known distances and fits a model to them.
 *
 * The distances are geometric — 1, 2, 4, 8 m — because path loss is linear in
 * `log10(d)`. Doubling spaces the samples evenly along the axis the regression
 * actually fits; taking 1, 2, 3, 4 m instead would crowd three of four points
 * into the first third of the useful range and let the far end be determined by
 * a single reading.
 */
class CalibrationRun(val samplesPerStep: Int = 25) {

    /** Metres. See the class comment for why these and not 1, 2, 3, 4. */
    val steps: List<Double> = listOf(1.0, 2.0, 4.0, 8.0)

    private val collected = mutableMapOf<Int, MutableList<Double>>()

    var stepIndex: Int = 0
        private set

    val currentDistance: Double get() = steps[stepIndex.coerceIn(0, steps.lastIndex)]

    val samplesInStep: Int get() = collected[stepIndex]?.size ?: 0

    val isStepComplete: Boolean get() = samplesInStep >= samplesPerStep

    val isComplete: Boolean get() = stepIndex >= steps.size

    fun record(dbm: Double) {
        if (isComplete || isStepComplete) return
        // A reading outside this range is a fault, not a distant device, and one
        // of them in a four-point fit would drag the whole model.
        if (dbm >= 0.0 || dbm <= -127.0) return
        collected.getOrPut(stepIndex) { mutableListOf() }.add(dbm)
    }

    fun advance() {
        if (isStepComplete) stepIndex++
    }

    fun reset() {
        collected.clear()
        stepIndex = 0
    }

    /**
     * Fit, or `null` if the result should not be trusted.
     *
     * The rejection is the important part, and it lives in the Rust core rather
     * than here. Least squares always returns *something*: in a reflective
     * corridor it will happily produce an exponent of 1.1 or a one-metre
     * reference of -12 dBm, and the filter would then be confidently wrong
     * rather than honestly uncertain. Keeping the priors beats keeping a bad
     * fit.
     */
    fun fit(): Calibration? {
        if (!NativeCore.available) return null

        val distances = mutableListOf<Double>()
        val readings = mutableListOf<Double>()
        collected.forEach { (index, samples) ->
            val distance = steps.getOrNull(index) ?: return@forEach
            samples.forEach {
                distances += distance
                readings += it
            }
        }
        if (distances.size < 8) return null

        val result = runCatching {
            NativeCore.fitPathLoss(distances.toDoubleArray(), readings.toDoubleArray())
        }.getOrNull() ?: return null
        if (result.size < 3) return null

        return Calibration(
            txPower1m = result[0],
            exponent = result[1],
            rmsDb = result[2],
            fittedAt = System.currentTimeMillis() / 1000,
        )
    }
}
