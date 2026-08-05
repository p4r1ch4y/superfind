package dev.superfind.radio

import android.annotation.SuppressLint
import android.bluetooth.BluetoothDevice
import android.bluetooth.BluetoothGatt
import android.bluetooth.BluetoothGattCallback
import android.bluetooth.BluetoothManager
import android.bluetooth.BluetoothProfile
import android.content.Context
import android.os.Build
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * Signal strength from an established connection, rather than from overheard
 * advertisements.
 *
 * ## Why this matters for everyday devices
 *
 * The devices people actually lose are the ones they use daily — earbuds, a
 * watch, a speaker — and those are almost always *paired*. Paired devices are
 * also the worst case for advertisement scanning: once bonded and idle, many
 * stop advertising altogether, and Classic-only audio devices never appear in an
 * LE scan at all. Hunting them by overhearing broadcasts can therefore fail
 * completely on exactly the devices that matter most.
 *
 * Connecting solves it. `readRemoteRssi()` works whenever a GATT link is up,
 * regardless of whether the device advertises, and the reading is quieter than
 * an advertisement: same channel, known transmit power, no rotating address.
 * That is why [RssiSource.CONNECTED_LINK] carries roughly half the noise of
 * [RssiSource.ADVERTISEMENT] in the fusion core.
 *
 * ## The cost, stated plainly
 *
 * Connecting is not free or invisible. It occupies one of the radio's few
 * connection slots, it can interrupt what the device is doing, and on earbuds it
 * may briefly stop audio. So this is offered rather than assumed, and it is
 * always paired with disconnecting cleanly.
 */
class GattLink(private val context: Context) {

    /** How often to ask the link for its signal strength. */
    private val pollInterval = 300L

    /**
     * Whether a GATT link is even possible for this device.
     *
     * Classic-only (BR/EDR) devices — most headphones and speakers, including
     * every one tested here — have no GATT server to connect to. Attempting it
     * anyway produces a poll loop that can never succeed while the UI shows
     * "No contact", which is indistinguishable from the device being out of
     * range and therefore worse than saying nothing.
     */
    @SuppressLint("MissingPermission")
    fun isSupported(address: String): Boolean = runCatching {
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val device = manager?.adapter?.getRemoteDevice(address) ?: return false
        device.type == BluetoothDevice.DEVICE_TYPE_LE ||
            device.type == BluetoothDevice.DEVICE_TYPE_DUAL ||
            // Unknown usually means never seen over Classic; worth a try.
            device.type == BluetoothDevice.DEVICE_TYPE_UNKNOWN
    }.getOrDefault(false)

    /**
     * Connect and emit RSSI until cancelled.
     *
     * Errors are emitted as `null` rather than thrown: a device that refuses a
     * connection is an ordinary event during a hunt, and the caller should fall
     * back to advertisements rather than see the whole flow collapse.
     */
    @SuppressLint("MissingPermission")
    fun rssi(address: String): Flow<Int?> = callbackFlow {
        if (!isSupported(address)) {
            // Classic-only. Nothing to connect to; say so by closing rather
            // than polling forever.
            trySend(null)
            close()
            return@callbackFlow
        }

        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        val adapter = manager?.adapter
        if (adapter == null) {
            trySend(null)
            close()
            return@callbackFlow
        }

        val device: BluetoothDevice = runCatching { adapter.getRemoteDevice(address) }
            .getOrNull() ?: run {
            trySend(null)
            close()
            return@callbackFlow
        }

        var gatt: BluetoothGatt? = null

        val callback = object : BluetoothGattCallback() {
            override fun onConnectionStateChange(g: BluetoothGatt, status: Int, newState: Int) {
                if (newState != BluetoothProfile.STATE_CONNECTED) {
                    // Report the drop rather than silently going quiet, so the
                    // UI can say the link was lost instead of showing a frozen
                    // number that looks like a steady signal.
                    trySend(null)
                }
            }

            override fun onReadRemoteRssi(g: BluetoothGatt, rssi: Int, status: Int) {
                if (status == BluetoothGatt.GATT_SUCCESS && rssi < 0 && rssi > -127) {
                    trySend(rssi)
                }
            }
        }

        gatt = runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
                // LE transport explicitly: without it Android may pick BR/EDR
                // for a dual-mode device, and readRemoteRssi is then unavailable.
                device.connectGatt(context, true, callback, BluetoothDevice.TRANSPORT_LE)
            } else {
                device.connectGatt(context, true, callback)
            }
        }.getOrNull()

        if (gatt == null) {
            trySend(null)
            close()
            return@callbackFlow
        }

        // autoConnect = true above means the connection may take a while to come
        // up and will re-establish itself if it drops. Polling regardless is
        // simplest: a read before the link exists just fails harmlessly.
        val poller = launch {
            while (isActive) {
                runCatching { gatt.readRemoteRssi() }
                delay(pollInterval)
            }
        }

        awaitClose {
            poller.cancel()
            runCatching {
                gatt.disconnect()
                gatt.close()
            }
        }
    }

    /** Whether this device is currently connected to the phone. */
    @SuppressLint("MissingPermission")
    fun isConnected(address: String): Boolean = runCatching {
        val manager = context.getSystemService(Context.BLUETOOTH_SERVICE) as? BluetoothManager
        manager?.getConnectedDevices(BluetoothProfile.GATT)
            ?.any { it.address.equals(address, ignoreCase = true) } == true
    }.getOrDefault(false)
}
