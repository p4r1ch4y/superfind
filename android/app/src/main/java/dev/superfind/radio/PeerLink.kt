package dev.superfind.radio

import android.content.Context
import android.net.wifi.WifiManager
import java.net.DatagramPacket
import java.net.InetAddress
import java.net.MulticastSocket
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.flowOn
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch

/**
 * One peer's reading, and where it was taken from.
 *
 * Mirrors `superfind_core::PeerReport`, and the wire format is the contract
 * between them: `superfind/1 <session> <peer> <target> <x> <y> <rssi> <source>
 * <seconds>`, with `-` for an unknown position. Deliberately dull, so a version
 * mismatch between a phone and a laptop shows up as a rejected line rather than
 * as a plausible wrong number.
 */
data class PeerReport(
    val session: String,
    val peer: String,
    val target: String,
    val x: Double,
    val y: Double,
    val rssiDbm: Double,
    val source: Int,
    val seconds: Double,
)

/**
 * Pooling readings with other devices hunting the same thing.
 *
 * A single observer's signal strength describes an annulus — "somewhere on a
 * ring around me" — and no number of readings from one spot narrows it, which is
 * why the app otherwise has to ask people to walk a dogleg. Two devices a few
 * metres apart intersect two rings, standing still.
 *
 * ## What this cannot do for you
 *
 * Nothing in Bluetooth or Wi-Fi tells two phones where they are relative to each
 * other. Positions here are in one session's frame, established by a human —
 * a tape measure, a floor plan, or two agreed corners of a room. A peer that
 * cannot say where it is is dropped rather than fused, because a range from an
 * unknown centre constrains nothing at all.
 *
 * ## Two Android-specific traps
 *
 * A multicast lock is required or the Wi-Fi chip filters the packets away
 * before Android ever sees them — the socket looks healthy and simply receives
 * nothing. And the peer name must be unique per *process*: a peer discards its
 * own packets by name, so two instances sharing a name would each discard the
 * other's readings while appearing to work.
 */
class PeerLink(
    private val context: Context,
    private val session: String,
    private val name: String,
    /** Where this device is in the shared frame, or null to listen only. */
    private val position: Pair<Double, Double>?,
) {
    private var socket: MulticastSocket? = null
    private var lock: WifiManager.MulticastLock? = null

    /**
     * Reports from other peers concerning [target].
     *
     * Own packets, other sessions, other targets and positionless reports are
     * all filtered here rather than reaching the filter.
     */
    fun reports(target: String): Flow<PeerReport> = callbackFlow {
        val group = InetAddress.getByName(GROUP)
        val sock = runCatching {
            MulticastSocket(PORT).apply {
                reuseAddress = true
                // On, deliberately: a phone anchoring while a laptop walks is a
                // real configuration, and same-device testing needs it. Our own
                // packets are discarded by name below.
                loopbackMode = false
                joinGroup(group)
            }
        }.getOrNull() ?: run { close(); return@callbackFlow }

        // Without this the Wi-Fi hardware drops multicast to save power, and the
        // socket receives nothing while looking perfectly healthy.
        val wifi = context.applicationContext
            .getSystemService(Context.WIFI_SERVICE) as? WifiManager
        val multicastLock = wifi?.createMulticastLock("superfind-peers")?.apply {
            setReferenceCounted(true)
            acquire()
        }

        socket = sock
        lock = multicastLock

        val reader = launch(Dispatchers.IO) {
            val buffer = ByteArray(512)
            while (isActive) {
                val packet = DatagramPacket(buffer, buffer.size)
                val line = runCatching {
                    sock.receive(packet)
                    String(packet.data, 0, packet.length, Charsets.UTF_8)
                }.getOrNull() ?: continue

                val report = decode(line.trim()) ?: continue
                if (report.peer == name) continue
                if (report.session != session) continue
                if (!report.target.equals(target, ignoreCase = true)) continue
                trySend(report)
            }
        }

        awaitClose {
            reader.cancel()
            runCatching { sock.leaveGroup(group) }
            runCatching { sock.close() }
            runCatching { multicastLock?.release() }
            socket = null
            lock = null
        }
    }.flowOn(Dispatchers.IO)

    /** Announce a reading. Silent on failure: a hunt must not stop for the network. */
    fun share(target: String, rssiDbm: Int, seconds: Double, source: Int) {
        val at = position ?: return
        val sock = socket ?: return
        val line = "superfind/1 %s %s %s %.3f %.3f %.1f %s %.3f".format(
            session, name, target, at.first, at.second,
            rssiDbm.toDouble(), sourceTag(source), seconds,
        )
        runCatching {
            val bytes = line.toByteArray(Charsets.UTF_8)
            sock.send(DatagramPacket(bytes, bytes.size, InetAddress.getByName(GROUP), PORT))
        }
    }

    companion object {
        /** Administratively-scoped, so packets stay on the local network. */
        private const val GROUP = "239.255.42.99"
        private const val PORT = 47811

        private fun sourceTag(ordinal: Int) = when (ordinal) {
            0 -> "link"
            2 -> "classic"
            else -> "advert"
        }

        private fun parseSource(tag: String): Int? = when (tag) {
            "link" -> 0
            "advert" -> 1
            "classic" -> 2
            else -> null
        }

        /**
         * Parse one line, strictly.
         *
         * A peer running a version we do not understand should be ignored, not
         * guessed at: silently mis-parsing a coordinate would move the fix
         * somewhere confidently wrong.
         */
        fun decode(line: String): PeerReport? {
            val parts = line.split(" ").filter { it.isNotEmpty() }
            if (parts.size < 9 || parts[0] != "superfind/1") return null

            // A positionless report constrains nothing and must not be fused.
            if (parts[4] == "-" || parts[5] == "-") return null

            val x = parts[4].toDoubleOrNull() ?: return null
            val y = parts[5].toDoubleOrNull() ?: return null
            val rssi = parts[6].toDoubleOrNull() ?: return null
            val source = parseSource(parts[7]) ?: return null
            val seconds = parts[8].toDoubleOrNull() ?: return null

            // Outside this range is a corrupted line, not a distant device.
            if (rssi >= 0.0 || rssi <= -127.0) return null
            if (!seconds.isFinite()) return null

            return PeerReport(
                session = parts[1],
                peer = parts[2],
                target = parts[3],
                x = x,
                y = y,
                rssiDbm = rssi,
                source = source,
                seconds = seconds,
            )
        }

        /**
         * A name for this device, unique per process.
         *
         * Peers discard their own packets by matching this. Two instances
         * sharing a name would each discard the other's readings and the whole
         * feature would silently do nothing, so the pid is not decoration.
         */
        fun deviceName(): String {
            val model = android.os.Build.MODEL.replace(' ', '-').take(16)
            return "$model-${android.os.Process.myPid()}"
        }
    }
}
