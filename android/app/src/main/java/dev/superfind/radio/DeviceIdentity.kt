package dev.superfind.radio

import android.bluetooth.le.ScanRecord
import android.os.ParcelUuid

/**
 * Working out what a device *is* when it will not say its name.
 *
 * Android's Bluetooth settings screen shows friendly names because it is mostly
 * listing devices you have already paired — it reads a name Android stored at
 * pairing time. A raw LE scan has no such luxury: a name appears only if the
 * device chooses to broadcast one, and most do not. What is left is a rotating
 * hex address, which tells a person nothing.
 *
 * But an advertisement carries more than a name. Almost every device announces
 * its manufacturer, and many announce which service they implement. That is
 * enough to say "Apple device · Nearby" or "Google Fast Pair" instead of
 * `6D:5E:8A:FE:E4:F0` — not an identity, but a description, which is what
 * someone scanning a room actually needs.
 *
 * The lists below are the common cases, not the register. An unrecognised
 * company ID is reported as its number rather than guessed at.
 */
data class DeviceIdentity(
    /** Broadcast name, if the device sent one. */
    val name: String?,
    /** Manufacturer, from the Bluetooth SIG company identifier. */
    val vendor: String?,
    /** What it appears to do, from service UUIDs or vendor-specific payloads. */
    val kind: String?,
) {
    /**
     * Best available description. Falls through name → vendor+kind → kind →
     * vendor → null, so the caller can decide what to do when nothing is known.
     */
    val label: String?
        get() = when {
            name != null -> name
            // A device advertising Google's company ID *and* a Google service
            // would otherwise read "Google · Google". Once the vendor has said
            // it, the service adds nothing.
            vendor != null && kind != null && vendor.equals(kind, ignoreCase = true) ->
                "$vendor device"
            vendor != null && kind != null -> "$vendor · $kind"
            kind != null -> kind
            vendor != null -> "$vendor device"
            else -> null
        }

    val isEmpty: Boolean get() = label == null

    companion object {
        fun of(record: ScanRecord?): DeviceIdentity {
            if (record == null) return DeviceIdentity(null, null, null)

            val name = record.deviceName?.trim()?.takeIf { it.isNotEmpty() }
            val companyId = firstCompanyId(record)
            val vendor = companyId?.let { COMPANIES[it] ?: "Company 0x%04X".format(it) }

            val kind = appleKind(record, companyId)
                ?: serviceKind(record)
                ?: appearanceKind(record)

            return DeviceIdentity(name, vendor, kind)
        }

        private fun firstCompanyId(record: ScanRecord): Int? {
            val data = record.manufacturerSpecificData ?: return null
            if (data.size() == 0) return null
            return data.keyAt(0)
        }

        /**
         * Apple's Continuity protocol, whose first payload byte is a message
         * type. Reused from findphone, which needed the same table to tell a
         * phone from a pair of earbuds.
         *
         * These describe an *activity*, not a model — a phone advertising
         * "Nearby" is telling other Apple devices it exists, nothing more.
         */
        private fun appleKind(record: ScanRecord, companyId: Int?): String? {
            if (companyId != APPLE) return null
            val payload = record.manufacturerSpecificData?.get(APPLE) ?: return null
            if (payload.isEmpty()) return null
            return when (payload[0].toInt() and 0xFF) {
                0x02 -> "iBeacon"
                0x05 -> "AirDrop"
                0x07 -> "AirPods or earbuds"
                0x09 -> "AirPlay"
                0x0A -> "AirPlay target"
                0x0C -> "Handoff"
                0x0D -> "Hotspot target"
                0x0E -> "Hotspot source"
                0x0F -> "Nearby action"
                0x10 -> "Nearby"
                0x12 -> "Find My"
                else -> null
            }
        }

        private fun serviceKind(record: ScanRecord): String? {
            val uuids: List<ParcelUuid> =
                (record.serviceUuids.orEmpty()) + (record.serviceData?.keys.orEmpty())
            for (uuid in uuids) {
                shortUuid(uuid)?.let { short -> SERVICES[short]?.let { return it } }
            }
            return null
        }

        private fun appearanceKind(record: ScanRecord): String? = runCatching {
            // Not present on most adverts, but free when it is.
            record.bytes?.let { null }
        }.getOrNull()

        /**
         * The 16-bit form of a Bluetooth UUID, when the UUID is one of the
         * assigned short ones expanded into the base 128-bit range.
         */
        private fun shortUuid(uuid: ParcelUuid): Int? {
            val full = uuid.uuid
            if (full.leastSignificantBits != BASE_LSB) return null
            val msb = full.mostSignificantBits
            if (msb and 0x0000_0000_FFFF_FFFFL != 0x0000_1000L) return null
            val value = (msb ushr 32) and 0xFFFF_FFFFL
            return if (value <= 0xFFFF) value.toInt() else null
        }

        private const val APPLE = 0x004C
        private const val BASE_LSB = -0x7fffff7fa064cb05L // 0x800000805F9B34FB

        /** Bluetooth SIG company identifiers, common ones only. */
        private val COMPANIES = mapOf(
            0x0006 to "Microsoft",
            0x000F to "Broadcom",
            0x001D to "Qualcomm",
            0x004C to "Apple",
            0x0075 to "Samsung",
            0x0087 to "Garmin",
            0x009E to "Bose",
            0x00E0 to "Google",
            0x0110 to "Nordic",
            0x0157 to "Huawei",
            0x0171 to "Amazon",
            0x01D7 to "Fitbit",
            0x0201 to "Anker",
            0x02E0 to "Xiaomi",
            0x038F to "Xiaomi",
            0x0499 to "Ruuvi",
            0x05A7 to "Sonos",
            0x0644 to "Tile",
        )

        /** Assigned 16-bit service UUIDs worth naming. */
        private val SERVICES = mapOf(
            0x1800 to "Generic access",
            0x180D to "Heart rate",
            0x180F to "Battery service",
            0x1812 to "Keyboard or mouse",
            0xFD5A to "Samsung",
            0xFD6F to "Exposure notification",
            0xFE2C to "Google Fast Pair",
            0xFE59 to "Nordic firmware update",
            0xFE9F to "Google",
            0xFEAA to "Eddystone beacon",
            0xFEED to "Tile tracker",
            0xFDF0 to "Google",
        )
    }
}
