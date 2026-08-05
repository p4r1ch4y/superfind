# Superfind — design notes

Combining `findphone` (CLI RSSI hunt) with the radar/trail-map app in the
screenshots, into a cross-platform device finder. Written 2026-08-05.

---

## 1. The constraint that shapes everything: platform asymmetry

localsend is portable because its problem — HTTP over the LAN — is identical on
every OS. This problem is not. Radio access differs enormously per platform, and
a plan that treats iOS and Android as symmetric will collapse at the first demo.

| Capability | Android | iOS | Desktop (macOS) |
|---|---|---|---|
| Scan all BLE devices | yes, with MAC | **opaque per-app UUID, no MAC** | yes, no MAC |
| Background scan, unfiltered | yes (restricted) | **service-UUID filter only** | yes |
| BLE RSSI (advert + connected) | yes | yes | yes |
| BLE Channel Sounding (±20 cm, BT 6.0) | **yes — Android 16 Ranging API** | not exposed | no |
| UWB range **and angle** | yes (Ranging API / UWB API) | NearbyInteraction, *peer must run your app or be an MFi NIAP accessory* | no |
| Wi-Fi RTT (802.11mc FTM) | yes (`WifiRttManager`) | **no** | no |
| Wi-Fi scan list (BSSID + RSSI) | yes (throttled 4/2min) | **no public API** | yes (CoreWLAN) |
| Wi-Fi CSI (DensePose-style sensing) | **no** (root + Nexmon + specific Broadcom only) | **no** | no |
| Classic BT RSSI | limited | no | yes (`system_profiler`) |

**Consequence:** Android is the flagship. iOS is a deliberately reduced
instrument. Say so in the UI rather than shipping an iOS build that silently
underperforms and reads as broken.

### On "DensePose for Wi-Fi"

Cut it. Two independent blockers:

1. It needs raw Channel State Information from a multi-antenna receiver. No
   phone exposes CSI to userspace on either OS. Nexmon-CSI needs root and a
   handful of specific Broadcom chips.
2. It senses **human bodies in a room**, not a specific lost device. Even with
   the data it answers a different question.

The achievable version of the same instinct — "use Wi-Fi, not just Bluetooth" —
is **Wi-Fi RTT/FTM trilateration against APs on Android**, which is real,
shipping, and gives metres of accuracy indoors. Keep the ambition, change the
mechanism.

---

## 2. What is genuinely open

Four levers, in descending order of solidity:

### 2.1 Android 16 unified Ranging API — the single most important one
UWB, BLE Channel Sounding, and Wi-Fi NAN RTT behind one interface, and it
explicitly documents interop with iOS UWB via Apple's Nearby Interaction
Accessory Protocol. This is what turns the app from "warmer/colder" into
sub-metre truth. Build the abstraction layer around this API's shape.

- https://developer.android.com/develop/connectivity/ranging
- https://source.android.com/docs/core/connect/ranging-oob-spec

### 2.2 Google Find Hub Network (FMDN) — public specification
EID beacon format, key hierarchy (account / ephemeral identity / recovery /
ring), ring command, separated-state MAC behaviour, DULT compliance. Publishing
a *certified* tag needs a Google device-proposal form and approval, but the
protocol itself is documented, and `GoogleFindMyTools` already re-implements the
client side: query trackers and Android devices, read E2EE keys, decrypt
locations.

- https://developers.google.com/nearby/fast-pair/specifications/extensions/fmdn
- https://github.com/leonboe1/GoogleFindMyTools

### 2.3 DULT — the standards-blessed cover for scanning everything
IETF working group, Apple and Google co-authored, standardises how trackers
advertise so they can be *detected*. This gives a legitimate reason to enumerate
every tracker nearby, and makes "scan me for unwanted trackers" a headline
feature rather than a footnote. It is also the anti-stalking answer regulators
will ask for.

- https://datatracker.ietf.org/wg/dult/about/

### 2.4 Apple Find My network — reachable, but treat as a plugin
OpenHaystack (P-224 keypair, public key broadcast as BLE advert, private key
stays local) plus macless-haystack / FindMy.py, which now do local Anisette
generation so no Mac is required.

Risks, all real: depends on Apple's GrandSlam auth not changing (it has broken
before), requires the user's Apple ID, and sits in ToS-grey territory. **Ship it
as an optional plugin the user configures with their own credentials.** Do not
put it on the critical path or in the marketing copy.

- https://github.com/seemoo-lab/openhaystack
- https://github.com/dchristl/macless-haystack
- https://github.com/malmeloo/FindMy.py

---

## 3. Reading the screenshots

**Screenshot 1 — "Anchor scan", 16 sectors, hot bearing 79°.** That arrow is not
a measured bearing. One omnidirectional antenna yields distance only; findphone's
README states this correctly. What is actually happening is a *synthetic
aperture*: sample RSSI while the user rotates or walks, bin by compass heading,
point at the hottest bin. That is legitimate and it works — the honesty is in
showing sector coverage (`1 / 16`) and sample count so the user knows how much to
trust it.

**Screenshot 2 — trail map, "16% confidence".** Dead-reckoned path with RSSI
colouring. The confidence number is the best thing on either screen. Keep that
discipline: every derived quantity ships with its uncertainty.

**Real bearing** requires UWB AoA (Apple U1/U2, Samsung, some Android) or BLE AoA
with a multi-antenna array (not in phones). So the design is:

> UWB/Channel-Sounding angle where hardware allows; synthetic-aperture RSSI
> bearing as the universal fallback; never present the fallback as if it were the
> former.

---

## 4. Architecture

```
┌─────────────────────────────────────────────────┐
│  Flutter UI  — radar, trail map, big dBm, list  │  one codebase
├─────────────────────────────────────────────────┤
│  Rust core (flutter_rust_bridge)                │  platform-independent
│   · particle filter / sensor fusion             │  unit-testable on CI
│   · RSSI→distance model + calibration           │  reused by CLI
│   · sector binning, trilateration               │
│   · FMDN + Find My EID crypto                   │
│   · device registry, ranking                    │
├──────────────┬──────────────┬───────────────────┤
│ Kotlin       │ Swift        │ Desktop           │  thin, per-OS
│ · Ranging API│ · CoreBT     │ · CoreWLAN /      │
│ · BLE scan   │ · NearbyInt. │   BlueZ / WinRT   │
│ · WifiRtt    │ · CoreMotion │                   │
│ · WifiManager│              │                   │
└──────────────┴──────────────┴───────────────────┘
```

**Why Rust in the middle rather than pure Flutter:** the fusion filter is the
product. Keeping it in one testable, platform-free place means the macOS CLI
(direct descendant of findphone), the Android app, and the iOS app all steer by
identical maths — and a regression shows up in `cargo test`, not in a hallway.

### The fusion filter

A particle filter over device position, fusing:

- **RSSI** (soft, log-distance path-loss model, wide variance)
- **Pedestrian dead reckoning** (step counter + compass — this is what makes
  synthetic-aperture bearing possible at all)
- **Hard ranges** when available (UWB, Channel Sounding, Wi-Fi RTT) — these
  collapse the posterior fast
- **Angle** when available (UWB AoA)

Output is a posterior, so the UI can draw an honest confidence ellipse instead of
an arrow that implies precision it does not have. Everything findphone already
got right — median over mean, counting only genuine measurements, refusing to
overclaim — carries directly into this as the RSSI likelihood term.

### Carry over from findphone

- Median-not-mean windowing, and one shared reading driving number + arrow +
  sound so they cannot disagree
- "Count a measurement only when the value actually changes" (the
  `system_profiler` caching lesson) — the same trap exists for any cached OS
  radio value
- The `Snapshot` boundary: renderer is a pure function of an immutable struct
- Fix the two real bugs first (advert readings polluting the median while the
  GATT link is live; cached RSSI recorded as fresh after a link drop) — both are
  likelihood-model bugs in disguise and will hurt worse inside a filter

---

## 5. Phasing

| Phase | Scope | Proves |
|---|---|---|
| **0** | Android hunt mode: BLE scan, RSSI, synthetic-aperture bearing, particle filter, proximity sound | The core loop is better than existing apps |
| **1** | Ranging API integration — UWB / Channel Sounding / Wi-Fi RTT where hardware allows | Sub-metre. This is where it stops being a toy |
| **2** | Offline finding: FMDN client in Rust (port GoogleFindMyTools), Apple Find My as opt-in plugin | Finds devices out of Bluetooth range |
| **3** | iOS app, scoped honestly + DULT unwanted-tracker scanner on both platforms | Cross-platform claim becomes true |
| **4** | DIY tag firmware (ESP32 / nRF5x) beaconing on both networks | Own the hardware story |
| **5** | Desktop clients + CLI from the same Rust core | The localsend-style everywhere claim |

Ship Phase 0 standalone. It is a complete, useful product on its own, and it is
the phase that proves the fusion filter before anything expensive depends on it.

---

## 6. Hard truths to design around

1. **You cannot find a powered-off device you did not manufacture.** Apple's and
   Google's powered-off finding is silicon — reserve power routed to the
   Bluetooth controller, keys held in secure hardware. Pixel 8/9/10 and recent
   iPhones only; Samsung's offline finding explicitly stops when the device loses
   power. Your app can *query* those networks. It cannot add the capability to a
   phone that lacks it. Do not promise this.

2. **Apple will not open Precision Finding or the AirTag ring API.** Assume this
   permanently.

3. **The Apple Find My path is brittle and grey.** Plugin, user's own
   credentials, clearly labelled.

4. **Anti-stalking is not a feature, it is a licence to operate.** A great
   universal device-finder is structurally also a great stalking tool. Non-
   negotiables: DULT compliance; never persist another party's rotating
   identifiers beyond a session; no silent background tracking of devices the
   user does not own; the unwanted-tracker scanner is a headline feature, not a
   settings toggle. Both app stores will review this specifically.

5. **Permission cost is brutal on Android** — `BLUETOOTH_SCAN`,
   `BLUETOOTH_CONNECT`, `ACCESS_FINE_LOCATION`, `NEARBY_WIFI_DEVICES`, plus
   foreground-service and motion. Sequence the prompts around the moment each is
   actually needed or install-to-first-use conversion will be terrible.

---

## 7. Open questions

- Calibration: RSSI→distance needs a per-device TX-power prior. Fast Pair /
  FMDN adverts carry calibrated TX power; arbitrary devices do not. Ship a
  30-second "hold it at arm's length" calibration?
- Does the Rust core or the platform layer own the scan loop? Duty cycling is
  very platform-specific and battery-critical.
- iOS background: is a useful background mode achievable at all given the
  service-UUID filter constraint, or is iOS foreground-only by honest necessity?
- Legal review needed before Phase 2 on the Apple Find My plugin.
