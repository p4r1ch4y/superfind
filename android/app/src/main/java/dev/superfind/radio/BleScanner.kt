package dev.superfind.radio

import android.annotation.SuppressLint
import android.bluetooth.BluetoothManager
import android.bluetooth.le.ScanCallback
import android.bluetooth.le.ScanFilter
import android.bluetooth.le.ScanResult
import android.bluetooth.le.ScanSettings
import android.content.Context
import android.os.Build
import android.os.SystemClock
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow

/** One observation of a device, with the radio's own timestamp. */
data class Advert(
    val address: String,
    val name: String?,
    val rssi: Int,
    /** Advertised TX power, when the device includes it. Rarely present. */
    val txPower: Int?,
    /** What the advertisement says the device is, when it will not say a name. */
    val identity: DeviceIdentity,
    /** Seconds since boot, from the radio rather than the wall clock. */
    val timestampSeconds: Double,
    val connectable: Boolean,
) {
    /**
     * What to show a person. Falls back through the broadcast name, then what
     * the advertisement implies (vendor and service), and only then the address.
     */
    val label: String get() = name ?: identity.label ?: address
}

/**
 * BLE scanning, as a flow of genuine observations.
 *
 * Two details matter more than they look.
 *
 * **Timestamps come from the radio.** `ScanResult.timestampNanos` is when the
 * packet actually arrived, measured against `elapsedRealtimeNanos`. Using
 * `System.currentTimeMillis()` instead would fold scheduler jitter and delivery
 * latency into the measurement times, and the fusion filter reasons about
 * intervals between readings.
 *
 * **Duplicates are dropped.** Android delivers a callback per advertisement, so
 * repeats are less endemic than on BlueZ — but aggressive scan modes and some
 * chipsets re-report an unchanged value. Each such duplicate is treated by a
 * particle filter as independent evidence, so a hundred re-reads of one packet
 * make it roughly a hundred times more certain than one measurement justifies.
 * The ellipse shrinks, the confidence climbs, and none of it is real. The guard
 * below costs nothing and removes the whole failure mode.
 */
class BleScanner(private val context: Context) {

    @SuppressLint("MissingPermission")
    fun scan(targetAddress: String? = null): Flow<Advert> = callbackFlow {
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val scanner = manager?.adapter?.bluetoothLeScanner
        if (scanner == null) {
            close(IllegalStateException("Bluetooth LE scanning is unavailable"))
            return@callbackFlow
        }

        // address -> last RSSI emitted, for the duplicate guard.
        val lastEmitted = HashMap<String, Int>()

        // Always matched in software. See the note on `filters` below.
        val softwareTarget = targetAddress?.uppercase()

        val callback = object : ScanCallback() {
            override fun onScanResult(callbackType: Int, result: ScanResult) {
                val advert = result.toAdvert() ?: return
                if (softwareTarget != null && advert.address.uppercase() != softwareTarget) return
                if (lastEmitted.put(advert.address, advert.rssi) == advert.rssi) return
                trySend(advert)
            }

            override fun onBatchScanResults(results: MutableList<ScanResult>) {
                results.forEach { onScanResult(ScanSettings.CALLBACK_TYPE_ALL_MATCHES, it) }
            }

            override fun onScanFailed(errorCode: Int) {
                close(IllegalStateException(scanFailureReason(errorCode)))
            }
        }

        val settings = ScanSettings.Builder()
            // Signal strength changes as the user walks, so latency is the
            // whole point; a power-saving mode would smear the very gradient
            // being hunted.
            .setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY)
            // Deliver every packet rather than only the first sighting.
            .setCallbackType(ScanSettings.CALLBACK_TYPE_ALL_MATCHES)
            .setReportDelay(0)
            .apply {
                // Match tuning arrived in API 23; the PHY controls only in 26.
                // Guarding them together would crash on Android 6 and 7, which
                // is precisely the hardware this app exists to still support.
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                    setMatchMode(ScanSettings.MATCH_MODE_AGGRESSIVE)
                    setNumOfMatches(ScanSettings.MATCH_NUM_MAX_ADVERTISEMENT)
                }
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                    // Some devices report only a legacy PHY unless asked otherwise.
                    setLegacy(false)
                }
            }
            .build()

        // No hardware address filter, deliberately.
        //
        // `ScanFilter.setDeviceAddress(String)` assumes a **public** address
        // type, and a filter built that way against a random address silently
        // matches nothing: the scan runs, the callback never fires, and the UI
        // reports "No contact" as though the device were out of range. The
        // typed overload arrived only in API 31.
        //
        // The obvious repair — filter in hardware only for public addresses —
        // does not work either, because a public address cannot be told apart
        // from a non-resolvable private one. Both begin with the same two bits.
        // There are three random forms (static `11`, resolvable private `01`,
        // non-resolvable private `00`) and only the first two are identifiable.
        //
        // Since the classification is undecidable, matching moves to software
        // unconditionally. It costs radio wake-ups during a hunt; it has the
        // compensating advantage of working.
        val filters = emptyList<ScanFilter>()

        runCatching { scanner.startScan(filters, settings, callback) }
            .onFailure { close(it); return@callbackFlow }

        awaitClose {
            runCatching { scanner.stopScan(callback) }
        }
    }

    @SuppressLint("MissingPermission")
    private fun ScanResult.toAdvert(): Advert? {
        // -127 is the "no measurement" sentinel; a positive dBm from a handheld
        // radio is never real. Both reach the callback in practice.
        if (rssi >= 0 || rssi <= -127) return null

        val name = runCatching { scanRecord?.deviceName ?: device.name }.getOrNull()
        val tx = scanRecord?.txPowerLevel?.takeIf { it != Int.MIN_VALUE && it in -100..20 }

        return Advert(
            address = device.address,
            name = name?.takeIf { it.isNotBlank() },
            rssi = rssi,
            txPower = tx,
            identity = DeviceIdentity.of(scanRecord),
            timestampSeconds = timestampNanos / 1_000_000_000.0,
            connectable = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) isConnectable else true,
        )
    }

    private fun scanFailureReason(code: Int): String = when (code) {
        ScanCallback.SCAN_FAILED_ALREADY_STARTED -> "A scan is already running."
        ScanCallback.SCAN_FAILED_APPLICATION_REGISTRATION_FAILED ->
            "Android refused to register the scan. Toggling Bluetooth off and on usually clears this."
        ScanCallback.SCAN_FAILED_FEATURE_UNSUPPORTED -> "This device does not support the requested scan mode."
        ScanCallback.SCAN_FAILED_INTERNAL_ERROR -> "The Bluetooth stack reported an internal error."
        // Undocumented but common: the app has started too many scans too
        // quickly and Android has rate-limited it for 30 seconds.
        6 -> "Too many scans started recently. Android limits this — wait about 30 seconds."
        else -> "Bluetooth scan failed (code $code)."
    }

    companion object {
        /** Seconds since boot on the same clock as [Advert.timestampSeconds]. */
        fun now(): Double = SystemClock.elapsedRealtimeNanos() / 1_000_000_000.0
    }
}
