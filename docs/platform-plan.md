# Superfind — cross-platform plan, findings, and library inventory

Companion to [`design.md`](design.md). That document argues the shape of the
product; this one is the engineering ground truth underneath it — what each
platform's radios will actually let us do, which open protocols are genuinely
open, which libraries we can build on, and what each of them costs in licensing
and maintenance risk.

Researched and verified 2026-08-05. Every claim about an API or a repository was
checked against the source that day; where something is inference rather than
verified fact it says so.

---

## 0. Executive summary

Three things are better than expected, one is worse, and one changes the plan.

**Better.** Android 16's unified Ranging API collapses UWB, Bluetooth Channel
Sounding, Wi-Fi NAN RTT *and* BLE RSSI ranging behind one interface with one
permission. Google's Find Hub Network protocol is publicly specified. DULT gives
us a standards-blessed reason to enumerate every tracker in a room.

**Worse.** Ranging requires the *lost* device to be awake, on Android 16, and
actively cooperating in a session. A phone asleep under a sofa cushion will not
respond. Sub-metre accuracy is real, but it applies to a narrower slice of the
problem than the marketing instinct suggests. Section 2 makes this precise.

**Changes the plan.** The Apple Find My open-source lineage splits sharply on
licence and maintenance. OpenHaystack is AGPL-3.0 and has had no commit since
July 2024. macless-haystack is also AGPL-3.0. But **FindMy.py is MIT-licensed
and was last pushed four days ago**. For anything we intend to ship, FindMy.py is
the only viable base; the others are reference reading. See section 5.

---

## 1. The regime framing

The single most useful thing to fix before choosing technology: "find my device"
is three different problems, and they need three different mechanisms. Conflating
them is how these apps end up promising things physics will not deliver.

| | Regime A — cooperative | Regime B — passive | Regime C — offline |
|---|---|---|---|
| **Device state** | on, awake, running our app | on, advertising, not our app | out of Bluetooth range, or off |
| **Typical case** | "it's in this room somewhere" | "which of these 14 BLE devices is my earbud" | "I left it in a taxi" |
| **Mechanism** | Ranging API: UWB / Channel Sounding / Wi-Fi RTT | passive BLE advert RSSI + synthetic aperture | crowd-sourced finding networks |
| **Accuracy** | 10 cm – 1 m, with true bearing | 2 – 8 m, inferred bearing, wide error bars | 10 – 100 m, minutes-to-hours latency |
| **Needs from the target** | Android 16, ranging hardware, awake, consenting | nothing — it just has to advertise | OEM support baked into silicon |
| **Our status** | not started (Phase 1) | **built and working** (`superfind`) | not started (Phase 2) |

Regime B is the one that works on everything, needs nothing from the target
device, and is what we have already shipped. It is also the one everyone
underinvests in because it is unglamorous. Regimes A and C are additive
refinements on top of it, not replacements for it.

**Design consequence:** the fusion core must treat a hard range as an *optional
extra measurement*, never as a required input. `superfind-core` already does —
`Measurement::Range` is one variant among three, and every test that exercises it
has a sibling test that works without it.

---

## 2. Android 16 Ranging API — what it actually gives us

Sources: [developer guide](https://developer.android.com/develop/connectivity/ranging),
[`RangingManager`](https://developer.android.com/reference/android/ranging/RangingManager),
[OOB spec](https://source.android.com/docs/core/connect/ranging-oob-spec).

### The good part

Four technologies, one interface, one permission:

```xml
<uses-permission android:name="android.permission.RANGING" />
```

- **UWB** — ~10 cm, and the only one that yields a true angle of arrival.
- **Bluetooth Channel Sounding** (BT 6.0) — ±20 cm via phase-based ranging plus
  round-trip time. Vastly more available than UWB over time, because it rides on
  ordinary Bluetooth silicon.
- **Wi-Fi NAN RTT** — metres, but longer reach.
- **Bluetooth RSSI ranging** — the fallback, and the same physics `superfind-core`
  already models.

`RangingManager` reports which technologies the local device supports and their
current availability, so capability negotiation is a query rather than a
hardcoded device allowlist.

### The constraints that matter

These are the ones that shape the product, and they are easy to miss:

1. **Both devices need Android 16 and ranging hardware.** Not just ours. For the
   next several years that is a small intersection of the installed base.
2. **The target must be awake and in a session.** Ranging is an active,
   two-way, negotiated protocol. It is not something you can do *to* a device.
   A sleeping phone, a phone with our app swiped away, a phone at 2% battery —
   none of these will range. This is the constraint that keeps Regime B load-bearing.
3. **The module does not do peer discovery.** "Does not handle peer discovery or
   connection establishment — the app must establish the connection first." So we
   still need our own BLE discovery layer to find the device before we can range
   to it. The Ranging API is the last hundred metres, not the search.
4. **Background ranging is UWB-only** on supported devices; the others are
   restricted. So a "keep hunting while the screen is off" feature is UWB-only.
5. **Responders serve one initiator at a time.** No multi-user hunting of the
   same device.
6. **Only UWB delivers data back to the peer**; the others report to the
   initiator only.

### iOS interop

Real, but narrow. It requires the **raw** OOB mode (`RANGING_SESSION_RAW`) rather
than the default, configured to match Apple's Nearby Interaction Accessory
Protocol:

- config id `UwbRangingParams.CONFIG_UNICAST_DS_TWR`
- session key info = vendor ID + static STS IV
- complex channel = channel number and preamble index matching the iOS side

In other words: an Android phone can range to an iPhone *if the iPhone is running
an app that sets up a matching Nearby Interaction session*. It cannot range to an
arbitrary iPhone, and it certainly cannot range to an AirTag. Useful for
"find my partner's phone, we both have the app". Not useful for finding a lost
iPhone that isn't running our software.

---

## 3. Google Find Hub Network (FMDN) — publicly specified

Source: [FMDN specification](https://developers.google.com/nearby/fast-pair/specifications/extensions/fmdn).

The whole protocol is documented: ephemeral identifier (EID) advertising on a
160-bit or 256-bit curve, a four-key hierarchy (account key, ephemeral identity
key, recovery key, ring key), challenge-response over GATT, the ring command with
per-component volume and duration, and a "separated state" with a fixed MAC so
that unwanted-tracker detection can work. Beacons advertise at least every 2
seconds. DULT compliance is mandatory for certification.

**What is open:** the protocol, the crypto, the frame formats. Enough to build
both a tag and a client.

**What is gated:** shipping a *certified* tag requires a Find Hub device proposal
form, registration in Google's Nearby Device Console, and Google's approval.

**What already exists:**
[GoogleFindMyTools](https://github.com/leonboe1/GoogleFindMyTools) re-implements
the client side — query trackers and Android devices, read E2EE keys, decrypt
locations, with experimental ESP32 tag support. Actively maintained (last push
2026-05-05). **GPL-3.0**, which matters: see section 5.

This is the strongest offline-finding position available to us. Unlike the Apple
side, we are not fighting the vendor — we are implementing a spec they published.

---

## 4. DULT — the credibility shield

Source: [IETF DULT working group](https://datatracker.ietf.org/wg/dult/about/).

Apple and Google co-authored a standard for how location trackers advertise so
that they can be *detected* by anyone. Both already implement it; Samsung, Tile
and Chipolo ship compatible behaviour.

Why this matters more than it first appears:

- **It legitimises the scanning.** An app that enumerates every BLE tracker in a
  room looks alarming until it is doing so against an IETF standard whose entire
  purpose is protecting people from covert tracking.
- **It is a headline feature, not a footnote.** "Scan me for unwanted trackers"
  is a thing people actively want, and it shares 100% of its plumbing with the
  find-my-thing feature.
- **It is what app-store review will ask about.** Both stores scrutinise this
  category specifically. Arriving with DULT compliance already built is much
  better than retrofitting it after a rejection.

[AirGuard](https://github.com/seemoo-lab/AirGuard) from SEEMOO is the reference
Android implementation, **Apache-2.0** and actively maintained (2026-07-20). It is
the one major project in this space with a licence we can freely learn from and
borrow patterns from.

---

## 5. Library inventory

This is where the research changed the plan. Licences and maintenance status were
verified against GitHub and the registries on 2026-08-05.

### Tier 1 — safe to depend on and ship

| Layer | Choice | Licence | Last activity | Notes |
|---|---|---|---|---|
| Fusion core | **`superfind-core`** (ours) | MIT | live | Zero dependencies by design |
| Rust↔Dart FFI | [`flutter_rust_bridge`](https://github.com/fzyzcjy/flutter_rust_bridge) | MIT | 2026-07-31 | Android, iOS, Windows, Linux, macOS, Web. Async + Stream support |
| Flutter BLE | [`flutter_blue_plus`](https://pub.dev/packages/flutter_blue_plus) v2.3.11 | BSD-3 (verify) | 2026-07-24 | Central role on Android, iOS, macOS, Linux, Windows, Web |
| Apple Find My | [`FindMy.py`](https://github.com/malmeloo/FindMy.py) | **MIT** | **2026-08-01** | Local Anisette generation, SMS + trusted-device 2FA. The only permissively-licensed, actively-maintained option |
| Linux D-Bus | `zbus` 5.18 | MIT | live | Already in use. No C toolchain, no `libdbus-1-dev` |
| Async / TUI | `tokio`, `crossterm`, `anyhow`, `futures-util` | MIT / Apache-2.0 | live | Already in use |

### Tier 2 — reference only, do not link

| Project | Licence | Last activity | Why it's here |
|---|---|---|---|
| [OpenHaystack](https://github.com/seemoo-lab/openhaystack) | **AGPL-3.0** | **2024-07-09** | The original Find My reverse engineering. Unmaintained for two years. AGPL makes linking it into a product a serious commitment |
| [macless-haystack](https://github.com/dchristl/macless-haystack) | **AGPL-3.0** | 2026-03-15 | Removes the Mac requirement. Same licence problem |
| [GoogleFindMyTools](https://github.com/leonboe1/GoogleFindMyTools) | **GPL-3.0** | 2026-05-05 | Best FMDN client reference. GPL means we read the protocol from it and write our own, or we ship GPL |
| [AirGuard](https://github.com/seemoo-lab/AirGuard) | Apache-2.0 | 2026-07-20 | Permissive — the exception. Genuinely usable as a base for DULT detection |

### The licensing conclusion

- **Apple Find My path: use FindMy.py.** MIT, active as of four days ago. The
  AGPL alternatives are both a legal encumbrance and, in OpenHaystack's case,
  abandoned.
- **FMDN path: read GoogleFindMyTools, write our own.** The spec is public, so a
  clean-room implementation from Google's own documentation is straightforward
  and avoids GPL entirely. This is the *correct* engineering answer anyway — our
  implementation belongs in Rust in the core, not in Python beside it.
- **DULT path: AirGuard is Apache-2.0**, so we can borrow freely.
- **Verify `flutter_blue_plus` and `btleplug` licences before shipping.** GitHub
  could not parse either (`NOASSERTION`); both are believed BSD-3-Clause but that
  needs confirming from the files themselves.

### Deliberately rejected

- **`btleplug`** for the Linux CLI — its Linux backend links `libdbus`, so
  building it needs `libdbus-1-dev` installed as root. Requiring a system package
  and sudo just to try the tool is a bad first impression. `zbus` removes the C
  toolchain entirely. Revisit only if we want one Rust codebase covering Windows
  and macOS BLE too.
- **Wi-Fi CSI / DensePose-from-WiFi** — no phone exposes CSI to userspace on
  either OS; Nexmon-CSI needs root and specific Broadcom chips. And it senses
  human bodies, not devices. Cut, as argued in `DESIGN.md`.

---

## 6. Cross-platform scanning: what each OS permits

| Capability | Linux (BlueZ) | Android | Windows | iOS / macOS |
|---|---|---|---|---|
| Enumerate all BLE devices | yes, with MAC | yes, with MAC | yes (WinRT) | opaque per-app UUID only |
| Continuous advert RSSI | yes, `PropertiesChanged` | yes, scan callbacks | yes | yes |
| **Connected-link RSSI** | **no D-Bus API** — needs raw HCI + `CAP_NET_RAW` | yes, `readRemoteRssi()` | yes | yes, `readRSSI()` |
| Advertised TX power | yes, `Device1.TxPower` — but see below | yes | yes | yes |
| Unfiltered background scan | yes | yes, with restrictions | yes | service-UUID filter only |
| Manufacturer data | yes | yes | yes | yes |
| Unified Ranging API | no | **Android 16+** | no | no (Nearby Interaction instead) |
| Wi-Fi RTT | no | yes, `WifiRttManager` | no | no |
| Wi-Fi scan list | yes | yes, throttled 4/2min | yes | no public API |
| Our status | **shipped** | Phase 1 | Phase 3 | Phase 4, borrowed hardware |

### The trap on every platform

Every one of these stacks caches RSSI and will happily serve the same value
repeatedly. macOS `system_profiler` refreshes every 3–12 seconds. BlueZ updates
the `RSSI` property only when an advertisement arrives. Android's scan callbacks
fire per-advert, but with `CALLBACK_TYPE_ALL_MATCHES` and an aggressive scan mode
you will still see repeats.

**Feeding a duplicate into a particle filter is not harmless.** Each one is
treated as independent evidence, so a hundred re-reads of one advertisement make
the filter roughly a hundred times more certain than the single measurement
justifies. The ellipse shrinks, the confidence climbs, and none of it is real.
This is a much more damaging failure than it was in findphone, where the same
trap merely skewed a median.

Every platform backend must therefore emit *changed* values only. Our Linux
backend does it by using D-Bus signals rather than polling. The Android backend
will need an equivalent guard.

### Two findings from building the Linux backend

Both were expected to go the other way, and both were settled by measurement
rather than by reading documentation.

**BlueZ has no connection RSSI.** `org.bluez.Device1` on BlueZ 5.72 exposes
`RSSI` and `TxPower`, both derived from *advertising*, plus `Connect`/`Disconnect`
— but nothing for the RSSI of an established link. The HCI `Read_RSSI` command
that would provide it requires a raw `AF_BLUETOOTH`/`BTPROTO_HCI` socket and
`CAP_NET_RAW`. So the roadmap item "GATT connect to compare link-RSSI against
advert-RSSI" is not reachable through the D-Bus API at all, and would mean either
shipping a setcap step or a privileged helper. Deferred rather than half-built.
Android exposes `readRemoteRssi()` directly, so the comparison happens there.

**Almost nothing advertises TX power.** The AD type exists, BlueZ surveys it, and
FMDN and Fast Pair beacons carry it — which promised a free 1 m calibration
reference. Measured against 7 devices in an ordinary room: **0 of 7 advertised
it.** Ordinary phones, TVs and cheap tags simply do not. Guided calibration is
therefore the primary mechanism, not a fallback, and survey mode now reports the
count so the assumption stays checkable in whatever environment we are in.

---

## 7. Target architecture

```
┌──────────────────────────────────────────────────────────┐
│  Flutter UI — radar, trail map, big dBm, tracker scanner │
├──────────────────────────────────────────────────────────┤
│  superfind-core (Rust)          ← the product            │
│   · particle filter, path loss, dead reckoning           │
│   · synthetic-aperture bearing                           │
│   · FMDN EID crypto (clean-room from Google's spec)      │
│   · DULT tracker classification                          │
├───────────┬──────────────┬──────────────┬────────────────┤
│ Android   │ Linux        │ Windows      │ iOS (later)    │
│ · Ranging │ · zbus/BlueZ │ · WinRT      │ · CoreBluetooth│
│ · BLE scan│   (shipped)  │              │ · NearbyInter. │
│ · WifiRtt │              │              │                │
└───────────┴──────────────┴──────────────┴────────────────┘
                    │
                    ├── FindMy.py sidecar (MIT) — Apple network, opt-in plugin
                    └── FMDN client in Rust — Google network, clean-room
```

Two things worth calling out:

**The offline-finding clients sit beside the app, not inside it.** They need
network access, account credentials and a server round-trip. They are not part of
the real-time hunt loop, and coupling them to it would make the hunt loop's
failure modes include "your Apple ID session expired".

**FMDN crypto goes in the Rust core, FindMy.py stays a sidecar.** FMDN we can
implement cleanly from a public spec, so it belongs with the rest of our
cryptography. The Apple side depends on a reverse-engineered authentication flow
that breaks periodically; keeping it as a separate process in someone else's
actively-maintained MIT codebase is the right risk boundary.

---

## 8. Roadmap

| Phase | Deliverable | Gate |
|---|---|---|
| **0 — done** | `superfind-core` + Linux CLI, 101 tests, verified on real hardware | ✅ |
| **0.5 — done** | Guided per-device calibration: geometric distances, least-squares fit, plausibility and residual gates, persisted and auto-applied. GATT link-RSSI **deferred** — not reachable through BlueZ's D-Bus API (see section 6) | Distances now come from measured radios, not literature priors |
| **1** | Android app: Flutter UI + core via `flutter_rust_bridge`, BLE scanning, real compass and step counter | The synthetic aperture stops being keyboard-driven |
| **1.5** | Ranging API integration, capability-gated | Sub-metre where hardware allows, graceful fallback where it doesn't |
| **2** | FMDN client in Rust, clean-room from the public spec | Offline finding for Android devices and Find Hub tags |
| **2.5** | DULT scanner as a headline feature, patterns borrowed from AirGuard | Credibility, and app-store defensibility |
| **3** | Windows backend behind the same interface | Cross-platform claim starts being true |
| **4** | Apple Find My via FindMy.py sidecar, opt-in, user's own credentials | Legal review required before starting |
| **5** | DIY tag firmware (ESP32 / nRF5x), FMDN-compliant, DULT-compliant | Own the hardware story |

**Phase 0.5 before Phase 1.** The path-loss defaults (`tx_power_1m = -59 dBm`,
`n = 2.8`) are literature priors. Calibrating them against real hardware is
cheap, and every downstream phase inherits the improvement.

---

## 9. Risks, and things we will not build

### Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Android 16 + ranging hardware intersection is small for years | high | Regime B works on everything; ranging is additive |
| Apple GrandSlam auth breaks, killing the Find My plugin | high | Sidecar process, opt-in, clearly labelled as best-effort |
| AGPL contamination from the Find My ecosystem | high | FindMy.py (MIT) only; OpenHaystack lineage is reference reading |
| Android permission burden (`RANGING`, `BLUETOOTH_SCAN`, `BLUETOOTH_CONNECT`, `ACCESS_FINE_LOCATION`, `NEARBY_WIFI_DEVICES`, motion, foreground service) crushes install-to-first-use | high | Sequence prompts at the moment each is needed, never on launch |
| App-store rejection under anti-stalking policy | medium | DULT compliance from day one, not retrofitted |
| Particle filter overconfidence from duplicate measurements | medium | Already structural: changed-values-only in every backend |

### Explicit non-goals

**We will not build a stealth tracker.** The ESP32 Find My ecosystem contains
firmware explicitly designed to defeat anti-stalking protections — the "Find You"
research demonstrated a clone that rotates keys to stay invisible to Apple's
warnings. That work is legitimate as security research and disclosed as such. It
is not something we ship. Concretely:

- Any tag firmware we produce implements DULT separated-state behaviour and is
  detectable by AirGuard, Apple's and Google's scanners.
- We do not persist another party's rotating identifiers beyond a session.
- No silent background tracking of devices the user does not own.
- The unwanted-tracker scanner is a first-class feature, not a settings toggle.

**We will not promise to find powered-off devices we did not manufacture.**
Apple's and Google's powered-off finding is silicon — reserve power to the
Bluetooth controller, keys in secure hardware, Pixel 8/9/10 and recent iPhones
only. Samsung's offline finding stops outright when the device loses power. We
can *query* those networks. We cannot add the capability to hardware that lacks
it.

---

## 10. Open questions

1. ~~**Calibration UX.**~~ **Answered by measurement, 2026-08-05.** The hope was
   that advertised TX power (`Device1.TxPower` on BlueZ, the standard AD type)
   would give us a free 1 m reference. Measured against 7 devices in a normal
   room: **0 advertised it.** Fast Pair and FMDN beacons carry it; ordinary
   phones, TVs and cheap tags do not. So guided calibration is the load-bearing
   path, not a fallback, and it is now implemented — four geometric distances,
   25 samples each, least-squares fit, rejected unless physically plausible and
   under 8 dB RMS. Survey mode reports how many devices advertise TX power so the
   assumption stays checkable rather than remembered.
2. **Who owns the scan duty cycle** — the Rust core or the platform layer? It is
   battery-critical and very platform-specific, which argues for the platform;
   but it interacts with filter convergence, which argues for the core.
3. **Is a useful iOS background mode achievable at all**, given the service-UUID
   filter constraint, or is iOS honestly foreground-only?
4. **Does Bluetooth Channel Sounding require pairing/bonding?** It rides on a
   connection, which suggests yes, which would materially narrow Regime A. Needs
   testing on Pixel 10 hardware.
5. **Legal review of the Apple Find My plugin** before Phase 4 starts.
6. **Confirm `flutter_blue_plus` and `btleplug` licences** from the files
   themselves — GitHub reported `NOASSERTION` for both.

---

## Sources

- [Android Ranging API guide](https://developer.android.com/develop/connectivity/ranging) ·
  [`RangingManager`](https://developer.android.com/reference/android/ranging/RangingManager) ·
  [OOB spec](https://source.android.com/docs/core/connect/ranging-oob-spec) ·
  [Android UWB](https://developer.android.com/develop/connectivity/uwb)
- [Find Hub Network (FMDN) specification](https://developers.google.com/nearby/fast-pair/specifications/extensions/fmdn)
- [IETF DULT working group](https://datatracker.ietf.org/wg/dult/about/)
- [GoogleFindMyTools](https://github.com/leonboe1/GoogleFindMyTools) ·
  [FindMy.py](https://github.com/malmeloo/FindMy.py) ·
  [OpenHaystack](https://github.com/seemoo-lab/openhaystack) ·
  [macless-haystack](https://github.com/dchristl/macless-haystack) ·
  [AirGuard](https://github.com/seemoo-lab/AirGuard)
- [flutter_rust_bridge](https://github.com/fzyzcjy/flutter_rust_bridge) ·
  [flutter_blue_plus](https://pub.dev/packages/flutter_blue_plus)
- ["Find You" stealth AirTag research, Positive Security](https://positive.security/blog/find-you)
- [Pixel powered-off finding](https://www.androidauthority.com/pixel-9-find-my-switched-off-3473133/) ·
  [SmartThings Find](https://www.samsung.com/uk/support/apps-services/use-smartthings-find-with-the-smartthings-app/)
