//! BlueZ over D-Bus, in pure Rust.
//!
//! ## Why signals rather than polling
//!
//! The obvious implementation calls `GetManagedObjects` on a timer and reads
//! each device's `RSSI` property. It is also wrong, and wrong in a way that is
//! invisible until you look at the filter's confidence.
//!
//! BlueZ updates `RSSI` when an advertisement arrives. Poll faster than the
//! device advertises and you read the same cached number repeatedly. Feed those
//! duplicates into a particle filter and each one is treated as independent
//! evidence, so a hundred re-reads of one advertisement make the filter roughly
//! a hundred times more certain than the single measurement justifies. The
//! ellipse shrinks, the confidence climbs, and none of it is real.
//!
//! findphone hit the same trap on macOS, where `system_profiler` serves a cached
//! RSSI between refreshes three to twelve seconds apart, and solved it by
//! counting a measurement only when the value actually changed.
//!
//! `PropertiesChanged` gives us that for free: BlueZ emits it when the value
//! moves, so every event this module yields is a genuine observation. The
//! `changed_only` guard below is belt and braces for adapters that re-emit.

use std::collections::HashMap;
use std::time::Instant;

use anyhow::{anyhow, Context, Result};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use superfind_core::{short_uuid, DeviceIdentity};
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::{Connection, MatchRule, MessageStream};

const BLUEZ: &str = "org.bluez";

/// One genuine observation of a device.
#[derive(Debug, Clone)]
pub struct Advert {
    /// D-Bus object path. Stable for the lifetime of the device object, and the
    /// right key to track by — a BLE advertising address rotates.
    pub path: String,
    pub address: String,
    pub name: Option<String>,
    pub rssi: i16,
    /// TX power the device advertises, in dBm, when it includes the AD type.
    ///
    /// This is the calibrated 1 m reference handed to us for free — exactly the
    /// parameter `superfind --calibrate` spends a minute measuring. Fast Pair
    /// and FMDN beacons carry it; most phones and cheap tags do not.
    pub tx_power: Option<i16>,
    /// What the advertisement implies the device is, when it broadcasts no name.
    pub identity: DeviceIdentity,
    pub at: Instant,
}

impl Advert {
    /// What to show the user.
    ///
    /// Falls through the broadcast name, then what the advertisement implies —
    /// vendor and service — and only then the address, which for a rotating
    /// private address identifies nothing anyway.
    pub fn label(&self) -> String {
        if let Some(name) = &self.name {
            return name.clone();
        }
        self.identity
            .label()
            .unwrap_or_else(|| self.address.clone())
    }

    /// True when the address rotates, so it is not an identity.
    pub fn randomised_address(&self) -> bool {
        superfind_core::is_randomised_address(&self.address)
    }

    pub fn matches(&self, query: &str) -> bool {
        let q = query.to_lowercase();
        self.address.to_lowercase().replace(':', "") == q.replace(':', "")
            || self.address.to_lowercase().contains(&q)
            || self
                .name
                .as_deref()
                .is_some_and(|n| n.to_lowercase().contains(&q))
    }
}

/// A device as currently known, for survey listings.
#[derive(Debug, Clone)]
pub struct DeviceRecord {
    pub path: String,
    pub address: String,
    pub name: Option<String>,
    /// What the advertisement implies, for devices that broadcast no name.
    pub identity: DeviceIdentity,
    pub rssi: Option<i16>,
    pub connected: bool,
    pub paired: bool,
}

#[zbus::proxy(interface = "org.bluez.Adapter1", default_service = "org.bluez")]
trait Adapter1 {
    fn start_discovery(&self) -> zbus::Result<()>;
    fn stop_discovery(&self) -> zbus::Result<()>;
    fn set_discovery_filter(&self, filter: HashMap<&str, Value<'_>>) -> zbus::Result<()>;

    #[zbus(property)]
    fn powered(&self) -> zbus::Result<bool>;
    #[zbus(property)]
    fn address(&self) -> zbus::Result<String>;
}

pub struct Scanner {
    connection: Connection,
    adapter_path: OwnedObjectPath,
    adapter_address: String,
}

impl Scanner {
    /// Connect to BlueZ and select an adapter. `wanted` is an adapter name such
    /// as `hci0`; the first powered adapter is used when it is `None`.
    pub async fn open(wanted: Option<&str>) -> Result<Self> {
        let connection = Connection::system().await.context(
            "could not reach the system D-Bus. Is dbus running, and is this user allowed on it?",
        )?;

        let objects = managed_objects(&connection).await?;

        let mut candidates: Vec<OwnedObjectPath> = objects
            .iter()
            .filter(|(_, ifaces)| interface(ifaces, "org.bluez.Adapter1").is_some())
            .map(|(path, _)| path.clone())
            .collect();
        candidates.sort_by(|a, b| a.as_str().cmp(b.as_str()));

        if candidates.is_empty() {
            return Err(anyhow!(
                "no Bluetooth adapter found. Check `bluetoothctl list` and that the \
                 bluetooth service is running."
            ));
        }

        let adapter_path = match wanted {
            Some(name) => candidates
                .iter()
                .find(|p| p.as_str().ends_with(&format!("/{name}")))
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "adapter '{name}' not found. Available: {}",
                        adapter_names(&candidates).join(", ")
                    )
                })?,
            None => candidates[0].clone(),
        };

        let proxy = adapter_proxy(&connection, &adapter_path).await?;
        if !proxy.powered().await.unwrap_or(false) {
            return Err(anyhow!(
                "adapter {} is powered off. Try `bluetoothctl power on`.",
                short_name(&adapter_path)
            ));
        }
        let adapter_address = proxy.address().await.unwrap_or_default();

        Ok(Scanner {
            connection,
            adapter_path,
            adapter_address,
        })
    }

    pub fn adapter_name(&self) -> String {
        short_name(&self.adapter_path)
    }

    pub fn adapter_address(&self) -> &str {
        &self.adapter_address
    }

    /// Begin LE discovery.
    ///
    /// `DuplicateData` is the important flag: without it BlueZ reports each
    /// device once and then goes quiet, which is fine for pairing and useless
    /// for tracking signal strength over time.
    pub async fn start(&self) -> Result<()> {
        let proxy = adapter_proxy(&self.connection, &self.adapter_path).await?;

        let mut filter: HashMap<&str, Value<'_>> = HashMap::new();
        filter.insert("Transport", Value::from("le"));
        filter.insert("DuplicateData", Value::from(true));
        // Report everything the radio hears. Filtering by RSSI here would hide
        // exactly the weak, distant devices a search starts from.
        filter.insert("RSSI", Value::from(-127i16));

        proxy
            .set_discovery_filter(filter)
            .await
            .context("SetDiscoveryFilter failed")?;

        match proxy.start_discovery().await {
            Ok(()) => Ok(()),
            // Already scanning, e.g. a bluetoothctl session is open. Harmless.
            Err(zbus::Error::MethodError(name, _, _))
                if name.as_str() == "org.bluez.Error.InProgress" =>
            {
                Ok(())
            }
            Err(e) => Err(anyhow!("StartDiscovery failed: {e}")),
        }
    }

    pub async fn stop(&self) {
        if let Ok(proxy) = adapter_proxy(&self.connection, &self.adapter_path).await {
            let _ = proxy.stop_discovery().await;
        }
    }

    /// Snapshot of every device BlueZ currently knows about on this adapter.
    pub async fn devices(&self) -> Result<Vec<DeviceRecord>> {
        let objects = managed_objects(&self.connection).await?;
        let prefix = format!("{}/", self.adapter_path.as_str());

        let mut out = Vec::new();
        for (path, ifaces) in objects {
            if !path.as_str().starts_with(&prefix) {
                continue;
            }
            let Some(props) = interface(&ifaces, "org.bluez.Device1") else {
                continue;
            };
            let Some(address) = prop_string(props, "Address") else {
                continue;
            };
            let name = broadcast_name(props, &address);
            let identity = identity_from(props);
            out.push(DeviceRecord {
                path: path.as_str().to_string(),
                address,
                name,
                identity,
                rssi: prop_i16(props, "RSSI"),
                connected: prop_bool(props, "Connected").unwrap_or(false),
                paired: prop_bool(props, "Paired").unwrap_or(false),
            });
        }
        Ok(out)
    }

    /// Stream of genuine observations.
    ///
    /// Spawns a task that owns the D-Bus message streams and forwards adverts
    /// over a channel, so callers can select over this alongside keyboard input
    /// without holding a borrow on the connection.
    pub async fn adverts(&self) -> Result<mpsc::Receiver<Advert>> {
        let (tx, rx) = mpsc::channel(1024);

        // Seed the identity cache so a PropertiesChanged carrying only RSSI can
        // still be attributed to an address and name.
        let mut known: HashMap<String, CachedDevice> = HashMap::new();
        for d in self.devices().await? {
            known.insert(
                d.path.clone(),
                CachedDevice {
                    address: d.address.clone(),
                    name: d.name.clone(),
                    // Seeded from BlueZ's accumulated properties, not left
                    // empty. Most nearby devices are already known to BlueZ, so
                    // InterfacesAdded never fires for them and the subsequent
                    // PropertiesChanged carries only RSSI — leaving identity to
                    // arrive from a live packet would mean it never arrives.
                    identity: d.identity.clone(),
                },
            );
        }

        let added_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(BLUEZ)?
            .interface("org.freedesktop.DBus.ObjectManager")?
            .member("InterfacesAdded")?
            .build();

        let changed_rule = MatchRule::builder()
            .msg_type(zbus::message::Type::Signal)
            .sender(BLUEZ)?
            .interface("org.freedesktop.DBus.Properties")?
            .member("PropertiesChanged")?
            .build();

        let mut added = MessageStream::for_match_rule(added_rule, &self.connection, Some(64))
            .await
            .context("could not subscribe to InterfacesAdded")?;
        let mut changed = MessageStream::for_match_rule(changed_rule, &self.connection, Some(256))
            .await
            .context("could not subscribe to PropertiesChanged")?;

        let prefix = format!("{}/", self.adapter_path.as_str());

        tokio::spawn(async move {
            // Last RSSI actually emitted per device. Guards against an adapter
            // that re-signals an unchanged value; see the module comment.
            let mut last_emitted: HashMap<String, i16> = HashMap::new();

            loop {
                let advert = tokio::select! {
                    Some(Ok(msg)) = added.next() => {
                        parse_interfaces_added(&msg, &prefix, &mut known)
                    }
                    Some(Ok(msg)) = changed.next() => {
                        parse_properties_changed(&msg, &prefix, &mut known)
                    }
                    else => break,
                };

                let Some(advert) = advert else { continue };

                let changed_only = last_emitted
                    .insert(advert.path.clone(), advert.rssi)
                    .is_none_or(|previous| previous != advert.rssi);

                if changed_only && tx.send(advert).await.is_err() {
                    break;
                }
            }
        });

        Ok(rx)
    }
}

fn parse_interfaces_added(
    msg: &zbus::Message,
    prefix: &str,
    known: &mut HashMap<String, CachedDevice>,
) -> Option<Advert> {
    let body = msg.body();
    let (path, ifaces): (OwnedObjectPath, HashMap<String, HashMap<String, OwnedValue>>) =
        body.deserialize().ok()?;
    if !path.as_str().starts_with(prefix) {
        return None;
    }
    let props = ifaces.get("org.bluez.Device1")?;
    let address = prop_string(props, "Address")?;
    let name = broadcast_name(props, &address);
    let identity = identity_from(props);
    known.insert(
        path.as_str().to_string(),
        CachedDevice {
            address: address.clone(),
            name: name.clone(),
            identity: identity.clone(),
        },
    );

    Some(Advert {
        path: path.as_str().to_string(),
        address,
        name,
        rssi: prop_i16(props, "RSSI")?,
        tx_power: prop_i16(props, "TxPower"),
        identity,
        at: Instant::now(),
    })
}

fn parse_properties_changed(
    msg: &zbus::Message,
    prefix: &str,
    known: &mut HashMap<String, CachedDevice>,
) -> Option<Advert> {
    let path = msg.header().path()?.as_str().to_string();
    if !path.starts_with(prefix) {
        return None;
    }

    let body = msg.body();
    let (interface, changed, _invalidated): (String, HashMap<String, OwnedValue>, Vec<String>) =
        body.deserialize().ok()?;
    if interface != "org.bluez.Device1" {
        return None;
    }

    // Keep the identity cache current: names often arrive in a later packet
    // than the first RSSI.
    let entry = known.entry(path.clone()).or_default();
    if let Some(address) = prop_string(&changed, "Address") {
        entry.address = address;
    }
    if let Some(name) = broadcast_name(&changed, &entry.address) {
        entry.name = Some(name);
    }
    // Manufacturer data and service UUIDs arrive in their own packets, often
    // later than the first RSSI, so they are merged in rather than replacing
    // what is already known.
    let fresh = identity_from(&changed);
    if !fresh.is_empty() {
        entry.identity = fresh;
    }

    let rssi = prop_i16(&changed, "RSSI")?;
    let tx_power = prop_i16(&changed, "TxPower");
    let cached = entry.clone();
    if cached.address.is_empty() {
        return None;
    }

    Some(Advert {
        path,
        address: cached.address,
        name: cached.name,
        rssi,
        tx_power,
        identity: cached.identity,
        at: Instant::now(),
    })
}

/// What `GetManagedObjects` returns: path -> interface -> property -> value.
type ManagedObjects =
    HashMap<OwnedObjectPath, HashMap<zbus::names::OwnedInterfaceName, Properties>>;
type Properties = HashMap<String, OwnedValue>;

/// Look up an interface's properties by name. The map is keyed by
/// `OwnedInterfaceName`, which does not borrow as `str`, so a plain `get` will
/// not do.
fn interface<'a>(
    ifaces: &'a HashMap<zbus::names::OwnedInterfaceName, Properties>,
    name: &str,
) -> Option<&'a Properties> {
    ifaces
        .iter()
        .find(|(iface, _)| iface.as_str() == name)
        .map(|(_, props)| props)
}

async fn managed_objects(connection: &Connection) -> Result<ManagedObjects> {
    let proxy = zbus::fdo::ObjectManagerProxy::builder(connection)
        .destination(BLUEZ)?
        .path("/")?
        .build()
        .await
        .context("could not reach org.bluez. Is bluetoothd running?")?;
    Ok(proxy.get_managed_objects().await?)
}

async fn adapter_proxy<'a>(
    connection: &Connection,
    path: &OwnedObjectPath,
) -> Result<Adapter1Proxy<'a>> {
    Ok(Adapter1Proxy::builder(connection)
        .path(ObjectPath::try_from(path.as_str().to_owned())?)?
        .build()
        .await?)
}

fn adapter_names(paths: &[OwnedObjectPath]) -> Vec<String> {
    paths.iter().map(short_name).collect()
}

fn short_name(path: &OwnedObjectPath) -> String {
    path.as_str()
        .rsplit('/')
        .next()
        .unwrap_or(path.as_str())
        .to_string()
}

/// What BlueZ has told us about one device so far, accumulated across packets.
#[derive(Debug, Clone, Default)]
struct CachedDevice {
    address: String,
    name: Option<String>,
    identity: DeviceIdentity,
}

/// Read `ManufacturerData` and `UUIDs` and interpret them.
///
/// Everything here is best-effort: a device that sends malformed data should
/// cost us a label, never an advertisement.
fn identity_from(props: &HashMap<String, OwnedValue>) -> DeviceIdentity {
    let manufacturer: Vec<(u16, Vec<u8>)> = props
        .get("ManufacturerData")
        .and_then(|v| HashMap::<u16, OwnedValue>::try_from(v.clone()).ok())
        .map(|map| {
            map.into_iter()
                .filter_map(|(id, payload)| {
                    Vec::<u8>::try_from(payload).ok().map(|bytes| (id, bytes))
                })
                .collect()
        })
        .unwrap_or_default();

    let uuids: Vec<u16> = props
        .get("UUIDs")
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .map(|list| list.iter().filter_map(|u| short_uuid(u)).collect())
        .unwrap_or_default();

    DeviceIdentity::from_advert(&manufacturer, &uuids)
}

/// The device's real broadcast name, if it has one.
///
/// BlueZ falls back to setting `Alias` to the address with dashes when a device
/// has never announced a name — so `Alias` alone would hand back
/// `6A-26-E3-DF-DD-E7` and, being non-empty, it would beat anything the
/// advertisement implies. A name that is merely the address is worse than no
/// name, because it displaces a real description.
fn broadcast_name(props: &HashMap<String, OwnedValue>, address: &str) -> Option<String> {
    let candidate = prop_string(props, "Name").or_else(|| prop_string(props, "Alias"))?;
    let normalise = |s: &str| s.replace(['-', ':'], "").to_lowercase();
    if normalise(&candidate) == normalise(address) {
        return None;
    }
    Some(candidate)
}

fn prop_string(props: &HashMap<String, OwnedValue>, key: &str) -> Option<String> {
    match props.get(key)?.downcast_ref::<zbus::zvariant::Str>() {
        Ok(s) => Some(s.as_str().to_string()),
        Err(_) => None,
    }
}

fn prop_i16(props: &HashMap<String, OwnedValue>, key: &str) -> Option<i16> {
    props.get(key)?.downcast_ref::<i16>().ok()
}

fn prop_bool(props: &HashMap<String, OwnedValue>, key: &str) -> Option<bool> {
    props.get(key)?.downcast_ref::<bool>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn advert(name: Option<&str>, address: &str) -> Advert {
        Advert {
            path: "/org/bluez/hci0/dev_X".into(),
            address: address.into(),
            name: name.map(str::to_string),
            rssi: -60,
            tx_power: None,
            identity: DeviceIdentity::default(),
            at: Instant::now(),
        }
    }

    #[test]
    fn matches_on_name_case_insensitively() {
        let a = advert(Some("Pixel 9 Pro"), "AA:BB:CC:DD:EE:FF");
        assert!(a.matches("pixel"));
        assert!(a.matches("PIXEL 9"));
        assert!(!a.matches("galaxy"));
    }

    #[test]
    fn matches_on_address_with_or_without_colons() {
        let a = advert(None, "AA:BB:CC:DD:EE:FF");
        assert!(a.matches("aa:bb:cc:dd:ee:ff"));
        assert!(a.matches("aabbccddeeff"));
        assert!(a.matches("dd:ee"));
    }

    #[test]
    fn falls_back_to_the_address_when_unnamed() {
        assert_eq!(advert(None, "AA:BB:CC:DD:EE:FF").label(), "AA:BB:CC:DD:EE:FF");
        assert_eq!(advert(Some("Tag"), "AA:BB:CC:DD:EE:FF").label(), "Tag");
    }
}
