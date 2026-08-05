package dev.superfind.radio

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.content.ContextCompat

/**
 * Which permissions this Android version actually needs, and why.
 *
 * The set changed twice in ways that break naive code:
 *
 * - **API 23–30** tie BLE scanning to location. Without `ACCESS_FINE_LOCATION`
 *   the scan callback simply never fires. It does not throw, it does not warn —
 *   it returns zero results forever. That silent failure is the single most
 *   common reason a Bluetooth app looks broken on older phones.
 * - **API 31+** introduced `BLUETOOTH_SCAN` with a `neverForLocation` flag,
 *   which lets us drop the location request entirely. Worth doing on its own
 *   merits: we want signal strength, not whereabouts, and "this app wants your
 *   location" is a far harder prompt to accept than "this app wants to find
 *   nearby devices".
 * - **API 33+** added `NEARBY_WIFI_DEVICES` for Wi-Fi RTT ranging.
 *
 * Permissions are requested at the moment they are needed rather than in a wall
 * at launch, because install-to-first-use conversion dies otherwise.
 */
object Permissions {

    /** The minimum needed to scan at all. Without these, nothing works. */
    fun scanning(): List<String> = when {
        Build.VERSION.SDK_INT >= Build.VERSION_CODES.S -> listOf(
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_CONNECT,
        )
        // Pre-12: BLUETOOTH and BLUETOOTH_ADMIN are install-time, so only the
        // location grant is actually asked for here.
        else -> listOf(Manifest.permission.ACCESS_FINE_LOCATION)
    }

    /**
     * Extra permissions for ranging, requested only once the device is known to
     * have a ranging radio. Asking a phone without UWB for ranging access is
     * noise the user cannot act on.
     */
    fun ranging(capabilities: Capabilities): List<String> {
        if (capabilities.ranging == null) return emptyList()
        val out = mutableListOf<String>()
        if (Build.VERSION.SDK_INT >= 36) {
            out += PERMISSION_RANGING
        }
        if (capabilities.ranging == RangeTech.WIFI_RTT &&
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU
        ) {
            out += Manifest.permission.NEARBY_WIFI_DEVICES
        }
        return out
    }

    /**
     * Step detection needs activity recognition from Android 10. Optional: the
     * accelerometer fallback needs no permission at all, so declining this
     * costs precision rather than the feature.
     */
    fun stepDetection(): List<String> =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            listOf(Manifest.permission.ACTIVITY_RECOGNITION)
        } else {
            emptyList()
        }

    fun granted(context: Context, permission: String): Boolean =
        ContextCompat.checkSelfPermission(context, permission) ==
            PackageManager.PERMISSION_GRANTED

    fun allGranted(context: Context, permissions: List<String>): Boolean =
        permissions.all { granted(context, it) }

    fun missing(context: Context, permissions: List<String>): List<String> =
        permissions.filterNot { granted(context, it) }

    /**
     * A sentence explaining why, shown before the system dialog. A prompt the
     * user understands is a prompt they grant.
     */
    fun rationale(permission: String): String = when (permission) {
        Manifest.permission.ACCESS_FINE_LOCATION ->
            "Android ${Build.VERSION.RELEASE} requires location permission to scan for " +
                "Bluetooth devices. Superfind never reads or stores your location — " +
                "newer Android versions let apps say so explicitly, and this one is too old to."
        Manifest.permission.BLUETOOTH_SCAN ->
            "Needed to hear nearby Bluetooth devices. Declared as never used for " +
                "location, so your whereabouts are not involved."
        Manifest.permission.BLUETOOTH_CONNECT ->
            "Needed to connect to the device you are hunting, which gives a much " +
                "quieter signal reading than passive listening."
        Manifest.permission.NEARBY_WIFI_DEVICES ->
            "Needed for Wi-Fi ranging, which measures true distance rather than " +
                "inferring it from signal strength."
        PERMISSION_RANGING ->
            "Needed for precise ranging using ultra-wideband or Bluetooth Channel Sounding."
        else ->
            "Needed to detect your steps, which is how direction is worked out as you walk."
    }

    /** `Manifest.permission.RANGING`, inlined so this compiles below API 36. */
    const val PERMISSION_RANGING: String = "android.permission.RANGING"
}
