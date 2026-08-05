package dev.superfind.ui

import android.content.Context
import androidx.core.content.edit

/**
 * The handful of choices worth remembering between launches.
 *
 * Sound and haptics reset to off every time the process restarts otherwise, and
 * a phone hunting in a pocket is exactly the situation where the app gets killed
 * and relaunched — so the setting a user made deliberately would be lost at the
 * moment it mattered most.
 *
 * `SharedPreferences` rather than DataStore: four scalars, read once at
 * construction, written on a toggle. DataStore would add a dependency and a
 * coroutine boundary to store two booleans.
 *
 * Note what is *not* persisted: the defaults themselves stay off. Remembering a
 * choice is not the same as making one, and a fresh install must still be
 * silent until asked.
 */
class Settings(context: Context) {

    private val prefs =
        context.applicationContext.getSharedPreferences("superfind", Context.MODE_PRIVATE)

    var soundEnabled: Boolean
        get() = prefs.getBoolean(KEY_SOUND, false)
        set(value) = prefs.edit { putBoolean(KEY_SOUND, value) }

    var hapticsEnabled: Boolean
        get() = prefs.getBoolean(KEY_HAPTICS, false)
        set(value) = prefs.edit { putBoolean(KEY_HAPTICS, value) }

    /**
     * Amplitude, 0 to 1.
     *
     * Clamped on read as well as on write: a preferences file is editable by
     * anyone with root or a backup tool, and a value of 40 would arrive as a
     * blast of clipping rather than a loud click.
     */
    var volume: Double
        get() = prefs.getFloat(KEY_VOLUME, 0.9f).toDouble().coerceIn(0.05, 0.98)
        set(value) = prefs.edit { putFloat(KEY_VOLUME, value.coerceIn(0.05, 0.98).toFloat()) }

    /** Session name for pooling readings, when the user has set one up. */
    var peerSession: String?
        get() = prefs.getString(KEY_SESSION, null)?.takeIf { it.isNotBlank() }
        set(value) = prefs.edit { putString(KEY_SESSION, value) }

    private companion object {
        const val KEY_SOUND = "sound_enabled"
        const val KEY_HAPTICS = "haptics_enabled"
        const val KEY_VOLUME = "volume"
        const val KEY_SESSION = "peer_session"
    }
}
