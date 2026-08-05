//! Working out what a device *is* when it will not say its name.
//!
//! A BLE advertisement carries a name only if the device chooses to broadcast
//! one, and most do not. Worse, modern devices advertise from a **rotating
//! private address**, so the survey fills with hex strings that mean nothing and
//! are different an hour later. Measured on a Linux laptop and an Android phone
//! in the same room, roughly five of every seven nearby devices were both
//! unnamed and randomised.
//!
//! An advertisement still says more than nothing. Almost every device announces
//! its manufacturer, and many announce which service they implement. That is
//! enough for "Apple · Find My" or "Google Fast Pair" instead of
//! `6D:5E:8A:FE:E4:F0` — not an identity, but a *description*, which is what
//! someone scanning a room actually needs.
//!
//! This lives in the core rather than in either front end because it is pure
//! interpretation of bytes, with no platform in it: BlueZ hands over
//! `ManufacturerData` and `UUIDs`, Android hands over the same fields off a
//! `ScanRecord`, and both deserve the same answer.
//!
//! The tables below are the common cases, not the register. An unrecognised
//! company is reported by number rather than guessed at.

/// What an advertisement implies about the device that sent it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// Manufacturer, from the Bluetooth SIG company identifier.
    pub vendor: Option<String>,
    /// What it appears to do, from a vendor payload or a service UUID.
    pub kind: Option<String>,
}

impl DeviceIdentity {
    /// Interpret an advertisement.
    ///
    /// `manufacturer` is `(company_id, payload)` pairs; `service_uuids` are the
    /// 16-bit assigned numbers, already narrowed from any 128-bit form.
    pub fn from_advert(manufacturer: &[(u16, Vec<u8>)], service_uuids: &[u16]) -> Self {
        let company = manufacturer.first().map(|(id, _)| *id);

        let vendor = company.map(|id| match company_name(id) {
            Some(name) => name.to_string(),
            None => format!("Company 0x{id:04X}"),
        });

        let kind = apple_kind(manufacturer)
            .or_else(|| service_uuids.iter().find_map(|u| service_name(*u)))
            .map(|s| s.to_string());

        DeviceIdentity { vendor, kind }
    }

    /// Best available description, or `None` when the advert said nothing
    /// useful. Callers fall back to the address.
    pub fn label(&self) -> Option<String> {
        match (&self.vendor, &self.kind) {
            // A device advertising Google's company ID *and* a Google service
            // would otherwise read "Google · Google". The service adds nothing
            // once the vendor has said the same thing.
            (Some(v), Some(k)) if v.eq_ignore_ascii_case(k) => Some(format!("{v} device")),
            (Some(v), Some(k)) => Some(format!("{v} · {k}")),
            (None, Some(k)) => Some(k.clone()),
            (Some(v), None) => Some(format!("{v} device")),
            (None, None) => None,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.vendor.is_none() && self.kind.is_none()
    }
}

const APPLE: u16 = 0x004C;

/// Apple's Continuity protocol, whose first payload byte is a message type.
///
/// Inherited from findphone, which needed the same table to tell a phone from a
/// pair of earbuds. These describe an *activity* rather than a model: a device
/// advertising "Nearby" is announcing itself to other Apple devices, no more.
fn apple_kind(manufacturer: &[(u16, Vec<u8>)]) -> Option<&'static str> {
    let payload = manufacturer
        .iter()
        .find(|(id, _)| *id == APPLE)
        .map(|(_, data)| data)?;

    match *payload.first()? {
        0x02 => Some("iBeacon"),
        0x05 => Some("AirDrop"),
        0x07 => Some("AirPods or earbuds"),
        0x09 => Some("AirPlay"),
        0x0A => Some("AirPlay target"),
        0x0C => Some("Handoff"),
        0x0D => Some("Hotspot target"),
        0x0E => Some("Hotspot source"),
        0x0F => Some("Nearby action"),
        0x10 => Some("Nearby"),
        0x12 => Some("Find My"),
        _ => None,
    }
}

fn company_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x0006 => "Microsoft",
        0x000F => "Broadcom",
        0x001D => "Qualcomm",
        0x004C => "Apple",
        0x0075 => "Samsung",
        0x0087 => "Garmin",
        0x009E => "Bose",
        0x00E0 => "Google",
        0x0110 => "Nordic",
        0x0157 => "Huawei",
        0x0171 => "Amazon",
        0x01D7 => "Fitbit",
        0x0201 => "Anker",
        0x02E0 | 0x038F => "Xiaomi",
        0x0499 => "Ruuvi",
        0x05A7 => "Sonos",
        0x0644 => "Tile",
        _ => return None,
    })
}

fn service_name(uuid: u16) -> Option<&'static str> {
    Some(match uuid {
        0x180D => "Heart rate",
        0x180F => "Battery service",
        0x1812 => "Keyboard or mouse",
        0xFD5A => "Samsung",
        0xFD6F => "Exposure notification",
        0xFE2C => "Google Fast Pair",
        0xFE59 => "Nordic firmware update",
        0xFE9F | 0xFDF0 => "Google",
        0xFEAA => "Eddystone beacon",
        0xFEED => "Tile tracker",
        _ => return None,
    })
}

/// Narrow a 128-bit Bluetooth UUID to its 16-bit assigned number.
///
/// Returns `None` for a genuinely custom UUID, which carries no shared meaning
/// and should not be guessed at.
pub fn short_uuid(uuid: &str) -> Option<u16> {
    // The assigned range expands to `0000xxxx-0000-1000-8000-00805f9b34fb`.
    const SUFFIX: &str = "-0000-1000-8000-00805f9b34fb";
    let lower = uuid.to_lowercase();
    let head = lower.strip_suffix(SUFFIX)?;
    if head.len() != 8 || !head.starts_with("0000") {
        return None;
    }
    u16::from_str_radix(&head[4..], 16).ok()
}

/// Whether an address is a randomised private one.
///
/// The top two bits of the most significant octet encode the type: `11` is a
/// static random address and `01` a resolvable private one, both of which rotate
/// and identify nothing across sessions. Saying so is more useful than showing
/// hex as though it were a name.
///
/// Note what cannot be detected: `00` is a *non-resolvable* private address and
/// is indistinguishable from a public one by inspection. So this reports the
/// cases it can prove and stays silent on the rest — which is also why nothing
/// anywhere should filter on address type.
pub fn is_randomised_address(address: &str) -> bool {
    let Some(first) = address.split([':', '-']).next() else {
        return false;
    };
    let Ok(octet) = u8::from_str_radix(first, 16) else {
        return false;
    };
    matches!(octet >> 6, 0b11 | 0b01)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mfg(id: u16, bytes: &[u8]) -> Vec<(u16, Vec<u8>)> {
        vec![(id, bytes.to_vec())]
    }

    #[test]
    fn an_empty_advert_says_nothing_rather_than_guessing() {
        let id = DeviceIdentity::from_advert(&[], &[]);
        assert!(id.is_empty());
        assert_eq!(id.label(), None);
    }

    #[test]
    fn apple_continuity_types_are_named() {
        let id = DeviceIdentity::from_advert(&mfg(APPLE, &[0x12, 0x00]), &[]);
        assert_eq!(id.label().as_deref(), Some("Apple · Find My"));

        let earbuds = DeviceIdentity::from_advert(&mfg(APPLE, &[0x07, 0x19]), &[]);
        assert_eq!(earbuds.label().as_deref(), Some("Apple · AirPods or earbuds"));
    }

    #[test]
    fn a_known_vendor_with_no_payload_still_describes_something() {
        let id = DeviceIdentity::from_advert(&mfg(0x0006, &[]), &[]);
        assert_eq!(id.label().as_deref(), Some("Microsoft device"));
    }

    #[test]
    fn an_unknown_company_is_reported_by_number_not_invented() {
        let id = DeviceIdentity::from_advert(&mfg(0xABCD, &[1, 2]), &[]);
        assert_eq!(id.label().as_deref(), Some("Company 0xABCD device"));
    }

    #[test]
    fn a_vendor_is_not_repeated_as_its_own_service() {
        // Seen in the wild: a device advertising Google's company ID and a
        // Google service UUID rendered as "Google · Google".
        let id = DeviceIdentity::from_advert(&mfg(0x00E0, &[]), &[0xFE9F]);
        assert_eq!(id.label().as_deref(), Some("Google device"));
    }

    #[test]
    fn service_uuids_describe_devices_that_name_no_vendor() {
        let id = DeviceIdentity::from_advert(&[], &[0xFE2C]);
        assert_eq!(id.label().as_deref(), Some("Google Fast Pair"));
    }

    #[test]
    fn short_uuid_narrows_only_the_assigned_range() {
        assert_eq!(short_uuid("0000fe2c-0000-1000-8000-00805f9b34fb"), Some(0xFE2C));
        assert_eq!(short_uuid("0000180d-0000-1000-8000-00805F9B34FB"), Some(0x180D));
        // A vendor's own UUID means nothing to us and must not be coerced.
        assert_eq!(short_uuid("185f3df4-3268-4e3f-9fca-d4d5059915bd"), None);
    }

    #[test]
    fn randomised_addresses_are_detected_where_detectable() {
        // Static random: top bits 11.
        assert!(is_randomised_address("FD:73:35:BF:5C:AB"));
        // Resolvable private: top bits 01.
        assert!(is_randomised_address("6D:5E:8A:FE:E4:F0"));
        // Public OUI.
        assert!(!is_randomised_address("88:D0:39:C8:07:CE"));
        // Non-resolvable private, indistinguishable from public — must not
        // claim knowledge it does not have.
        assert!(!is_randomised_address("13:56:33:75:56:BB"));
        assert!(!is_randomised_address("not-an-address"));
    }

    #[test]
    fn dashes_and_colons_are_both_accepted() {
        assert!(is_randomised_address("FD-73-35-BF-5C-AB"));
    }
}
