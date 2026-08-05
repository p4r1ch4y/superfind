package dev.superfind.ui

import android.content.Context
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Build
import android.os.VibrationEffect
import android.os.Vibrator
import android.os.VibratorManager
import kotlin.math.exp
import kotlin.math.min
import kotlin.math.sin
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * What the user hears and feels, rather than reads.
 *
 * Searching means looking at the room — under cushions, behind furniture — not
 * at a screen. A cadence that quickens as you approach frees the eyes, which is
 * why metal detectors have sounded the same way for sixty years.
 *
 * ## Silence means no signal, never "far away"
 *
 * A distant device still clicks, slowly. A device that has gone quiet produces
 * nothing. If both were silent, somebody sweeping a room could not tell a dead
 * link from a cold corner and would keep searching a place the device had
 * already left. The core enforces this in `ProximityCue::for_snapshot`; this
 * class must not add a second, subtler way to be silent.
 *
 * ## Haptics are not a lesser channel
 *
 * A phone hunted from a pocket, or in a noisy room, is felt rather than heard —
 * and vibration carries cadence perfectly well even though it carries pitch not
 * at all. So the two are independently switchable, and haptics deliberately
 * survive silent mode, where an alarm would be unwelcome but a search you asked
 * for is not.
 */
class ProximityFeedback(private val context: Context) {

    /**
     * Amplitude, 0 to 1.
     *
     * Loud by default. This competes with a room being searched — drawers,
     * rustling, someone talking — and a click nobody can hear over that is a
     * feature that does not exist. Short of 1.0 because the click is a sine
     * burst and clipping turns a clean tick into a rasp; the system volume
     * control is the right place to be quieter.
     */
    @Volatile
    var volume: Double = 0.9

    private var track: AudioTrack? = null
    private var job: Job? = null

    @Volatile
    private var intervalMs: Int = 0

    @Volatile
    private var pitchHz: Int = 0

    @Volatile
    var soundEnabled: Boolean = false

    @Volatile
    var hapticsEnabled: Boolean = false

    private val vibrator: Vibrator? by lazy {
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val manager =
                    context.getSystemService(Context.VIBRATOR_MANAGER_SERVICE) as? VibratorManager
                manager?.defaultVibrator
            } else {
                @Suppress("DEPRECATION")
                context.getSystemService(Context.VIBRATOR_SERVICE) as? Vibrator
            }
        }.getOrNull()?.takeIf { it.hasVibrator() }
    }

    val hasHaptics: Boolean get() = vibrator != null

    /**
     * Set what should be playing.
     *
     * `intervalMs` of zero means silence, which per the rule above means the
     * signal has gone stale.
     */
    fun update(intervalMs: Int, pitchHz: Int) {
        this.intervalMs = intervalMs
        this.pitchHz = pitchHz
    }

    fun silence() {
        intervalMs = 0
    }

    fun start(scope: CoroutineScope) {
        if (job != null) return
        job = scope.launch(Dispatchers.Default) {
            while (isActive) {
                val interval = intervalMs
                if (interval <= 0 || (!soundEnabled && !hapticsEnabled)) {
                    // Poll rather than block, so switching sound on mid-hunt
                    // takes effect immediately.
                    delay(60)
                    continue
                }
                if (soundEnabled) click(pitchHz)
                if (hapticsEnabled) buzz(interval)
                delay(interval.toLong())
            }
            release()
        }
    }

    fun stop() {
        job?.cancel()
        job = null
        release()
    }

    /**
     * One click, synthesised rather than taken from a resource.
     *
     * `ToneGenerator` would be less code but offers a fixed set of DTMF tones,
     * so the pitch could not track the signal — and pitch rising alongside
     * cadence is what makes the two read as a single gesture.
     */
    private fun click(hz: Int) {
        val player = track ?: createTrack().also { track = it } ?: return
        val samples = SAMPLE_RATE * CLICK_MS / 1000
        val buffer = ShortArray(samples)
        for (i in 0 until samples) {
            val t = i.toDouble() / SAMPLE_RATE
            // Exponential decay makes it a tick rather than a beep, and stops
            // the tail colliding with the next click at close range.
            val envelope = exp(-t * 90.0)
            val amplitude = volume.coerceIn(0.05, 0.98)
            buffer[i] = (sin(t * hz * 2.0 * Math.PI) * envelope * amplitude * Short.MAX_VALUE)
                .toInt()
                .coerceIn(Short.MIN_VALUE.toInt(), Short.MAX_VALUE.toInt())
                .toShort()
        }
        runCatching {
            // Max out the track's own gain; the stream slider stays the user's
            // control. Without this the track sits at whatever default the
            // platform picked, which on some devices is well under unity.
            player.setVolume(AudioTrack.getMaxVolume())
            player.write(buffer, 0, buffer.size)
            if (player.playState != AudioTrack.PLAYSTATE_PLAYING) player.play()
        }
    }

    /**
     * One pulse.
     *
     * ## Duration is the only control here
     *
     * No `VibrationAttributes` is passed deliberately, so the pulse is filed as
     * `USAGE_TOUCH` and obeys the system's haptic-feedback setting. Declaring
     * `USAGE_ALARM` would make it fire regardless — but that setting is the user
     * saying they do not want the phone buzzing at them, and a finder is not
     * entitled to overrule it. The UI explains the dependency instead.
     *
     * That suppression is otherwise invisible: Android dispatches the
     * vibration, logs it, and discards it with
     * `status: ignored_for_settings, scale: 0.00`, so the feature looks broken
     * while working perfectly.
     *
     * Length matters more than it looks. Devices reporting no
     * `AMPLITUDE_CONTROL` — including the one this was tested on — ignore the
     * amplitude entirely, and a rotary motor needs tens of milliseconds merely
     * to spin up. At 22 ms it never got moving. The pulse now scales with the
     * gap so it stays a pulse rather than a drone, except at arm's reach where
     * near-continuous is exactly the right sensation.
     */
    private fun buzz(intervalMs: Int) {
        val v = vibrator ?: return
        val duration = (intervalMs * 0.6).toLong().coerceIn(40L, 90L)
        runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                v.vibrate(
                    VibrationEffect.createOneShot(duration, VibrationEffect.DEFAULT_AMPLITUDE)
                )
            } else {
                @Suppress("DEPRECATION")
                v.vibrate(duration)
            }
        }
    }

    private fun createTrack(): AudioTrack? = runCatching {
        val minBytes = AudioTrack.getMinBufferSize(
            SAMPLE_RATE,
            AudioFormat.CHANNEL_OUT_MONO,
            AudioFormat.ENCODING_PCM_16BIT,
        )
        val size = min(minBytes.coerceAtLeast(2048), SAMPLE_RATE)
        AudioTrack.Builder()
            .setAudioAttributes(
                AudioAttributes.Builder()
                    // USAGE_MEDIA rather than SONIFICATION, chosen for a
                    // practical reason: sonification follows the notification
                    // stream, which many people keep low or silenced, and the
                    // result was a finder that appeared not to work. Media is
                    // the slider people actually keep up, and this is a tool
                    // being used deliberately rather than an unsolicited alert.
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build()
            )
            .setAudioFormat(
                AudioFormat.Builder()
                    .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                    .setSampleRate(SAMPLE_RATE)
                    .setChannelMask(AudioFormat.CHANNEL_OUT_MONO)
                    .build()
            )
            .setBufferSizeInBytes(size)
            .setTransferMode(AudioTrack.MODE_STREAM)
            .build()
    }.getOrNull()

    private fun release() {
        runCatching {
            track?.stop()
            track?.release()
        }
        track = null
    }

    private companion object {
        const val SAMPLE_RATE = 22_050
        const val CLICK_MS = 22
    }
}
