package dev.superfind.radio

import android.Manifest
import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothManager
import android.content.Context
import android.os.Build

/**
 * Names for addresses, from everything Android already knows.
 *
 * ## Why the survey is full of bare MAC addresses
 *
 * A BLE advertisement carries a device name only in *some* packets, and many
 * devices never include one at all. Worse, most modern devices advertise from a
 * **resolvable private address** that rotates every fifteen minutes or so
 * — which is why the survey fills with things like `32:A7:AC:77:21:1C` that mean
 * nothing to a person and are different again an hour later.
 *
 * Android does hold the missing half of that mapping: every *bonded* device has
 * a stable public address and a human name. Joining the two turns "which of
 * these six random hex strings is my headphones" into a list somebody can
 * actually use.
 *
 * ## What this cannot fix
 *
 * A bonded device only appears under its real address while it is advertising
 * from that address. Classic-only devices — most headphones, most speakers —
 * do not appear in an LE scan at all when idle. So bonded devices are listed
 * *whether or not* they are currently being heard, with their last known signal
 * where there is one, rather than being hidden until they happen to shout.
 */
data class KnownDevice(
    val address: String,
    val name: String,
    val bonded: Boolean,
    val type: DeviceKind,
)

enum class DeviceKind(val label: String) {
    CLASSIC("Bluetooth"),
    LOW_ENERGY("BLE"),
    DUAL("dual-mode"),
    UNKNOWN(""),
}

class KnownDevices(private val context: Context) {

    /**
     * Everything Android has a name for, keyed by uppercase address.
     *
     * Failures are swallowed: a missing `BLUETOOTH_CONNECT` grant makes
     * `bondedDevices` throw, and losing names is a much better outcome than
     * losing the scan.
     */
    /**
     * Whether we may read paired device names at all.
     *
     * From Android 12 the bonded-device list needs `BLUETOOTH_CONNECT`. Checking
     * explicitly rather than relying on catching the SecurityException means the
     * UI can *say* why names are missing instead of silently showing a list of
     * hex and leaving the user to wonder.
     */
    fun canReadNames(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.S ||
            Permissions.granted(context, Manifest.permission.BLUETOOTH_CONNECT)

    @SuppressLint("MissingPermission")
    fun byAddress(): Map<String, KnownDevice> = if (!canReadNames()) emptyMap() else runCatching {
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val bonded = manager?.adapter?.bondedDevices.orEmpty()
        bonded.mapNotNull { device ->
            val address = device.address?.uppercase() ?: return@mapNotNull null
            val name = runCatching { device.name }.getOrNull()?.takeIf { it.isNotBlank() }
                ?: return@mapNotNull null
            address to KnownDevice(
                address = address,
                name = name,
                bonded = true,
                type = device.kind(),
            )
        }.toMap()
    }.getOrDefault(emptyMap())

    @SuppressLint("MissingPermission")
    fun bonded(): List<KnownDevice> = byAddress().values.sortedBy { it.name.lowercase() }

    // Guarded by canReadNames() at every call site, and wrapped besides: an
    // OEM that throws here should cost us a type label, not the whole list.
    @SuppressLint("MissingPermission")
    private fun BluetoothDevice.kind(): DeviceKind = runCatching {
        when (type) {
            BluetoothDevice.DEVICE_TYPE_CLASSIC -> DeviceKind.CLASSIC
            BluetoothDevice.DEVICE_TYPE_LE -> DeviceKind.LOW_ENERGY
            BluetoothDevice.DEVICE_TYPE_DUAL -> DeviceKind.DUAL
            else -> DeviceKind.UNKNOWN
        }
    }.getOrDefault(DeviceKind.UNKNOWN)

    companion object {
        /**
         * Whether an address is a randomised private one.
         *
         * The two most significant bits of the top octet encode the address
         * type: `11` is a static random address and `01` a resolvable private
         * address, both of which rotate and are meaningless to a human. Knowing
         * this lets the UI say "randomised address" rather than presenting hex
         * as though it were an identity.
         */
        fun isRandomised(address: String): Boolean = runCatching {
            val top = address.substringBefore(':').toInt(16)
            val kind = (top shr 6) and 0b11
            kind == 0b11 || kind == 0b01
        }.getOrDefault(false)

        /** Whether a string is a usable Bluetooth address. */
        fun isValidAddress(input: String): Boolean =
            normaliseAddress(input) != null

        /**
         * Accept what a person would actually type or paste — with or without
         * separators, in any case, using colons or dashes — and return the
         * canonical form Android expects, or null.
         */
        fun normaliseAddress(input: String): String? {
            val hex = input.trim().uppercase().filter { it.isDigit() || it in 'A'..'F' }
            if (hex.length != 12) return null
            return hex.chunked(2).joinToString(":")
        }
    }
}
