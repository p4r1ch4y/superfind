# Superfind for Android

Radar-first device finder. Runs on Android 6 through Android 16, and is honest on
all of them about what it can and cannot do.

## Build

```sh
export JAVA_HOME=~/.local/toolchains/jdk-17.0.20+8
~/.local/toolchains/gradle-8.13/bin/gradle :app:assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Needs JDK 17+ and the Android SDK (compileSdk 36). No NDK yet — see
[The native core](#the-native-core).

Verified state: `assembleDebug` and `lintDebug` pass; APK reports
`sdkVersion:'23'`, `targetSdkVersion:'36'`. **Run on hardware** — Moto G40
Fusion, Android 12 (API 31) — surveying, hunting, radar and sensors all working,
no crashes.

## Compatibility, and what it actually means

The app does not gate features on Android version. It probes each capability
independently and the *combination* decides a **tier** — each one a complete
experience with its own headline, instruction and stated limits, rather than a
version-locked set of greyed-out buttons.

| Tier | Needs | Gives | Typical device |
|---|---|---|---|
| **Precision** | any ranging radio | 0.2–2 m; measured bearing with UWB | Pixel 10, recent flagships |
| **Guided** | advert RSSI **+ any heading** | ~3 m; bearing recovered by walking | almost every phone since 2013 |
| **Proximity** | advert RSSI only | ~8 m; warmer/colder, no direction | phones with no compass |
| **Unavailable** | — | — | no usable Bluetooth LE |

**The floor is low, and that is the point.** A compass alone reaches Guided —
a full directional experience. `TYPE_ROTATION_VECTOR` has existed since API 9 and
BLE scanning since API 18. Ranging radios improve the answer; they are not what
makes it possible.

Every sensor degrades independently rather than failing:

| Signal | Preferred | Fallback | Last resort |
|---|---|---|---|
| Heading | `TYPE_ROTATION_VECTOR` | geomagnetic RV → accel + magnetometer | `TYPE_GAME_ROTATION_VECTOR` (relative) |
| Steps | `TYPE_STEP_DETECTOR` | accelerometer magnitude peaks | — |
| Ranging | Android 16 Ranging API | Jetpack UWB (API 31) | Wi-Fi RTT (API 28) |

### Permissions

The set changes twice in ways that break naive code, so it is computed per
version and requested at the moment it is needed, never in a wall at launch.

- **API 23–30** tie BLE scanning to `ACCESS_FINE_LOCATION`. Without it the scan
  callback never fires — it does not throw, it returns zero results forever.
  That silent failure is the most common reason a BLE app looks broken on older
  phones.
- **API 31+** use `BLUETOOTH_SCAN` with `neverForLocation`, so location is never
  requested. Honest and much easier to grant.
- **API 33+** add `NEARBY_WIFI_DEVICES` for Wi-Fi RTT.
- **API 36** adds `RANGING`, requested only on devices that have a ranging radio.

Old permissions carry `maxSdkVersion` so they are not requested forever, and
every radio is `uses-feature required="false"` so the APK installs anywhere.

## The UI

Radar primary, map on a toggle. The ordering encodes the distinction the whole
project rests on: **measured things at the top, inferred things below**, visually
separated so a glance tells them apart.

- **Readout** — the dBm figure, monospaced and large enough to read at arm's
  length while walking. Measured. Dims when stale, and says how stale.
- **Radar** — sector heat is the raw swept-aperture data, so unswept arcs read as
  *unexplored* rather than empty; the gap in the ring is the instruction.
- **The wedge width is the bearing's own sigma.** An uncertain bearing is
  visibly a fan, a confident one visibly a needle, with no number to parse.
- **Below 30% confidence there is no arrow at all** — a rotating "still
  listening" sweep instead. Drawing a crisp arrow from a bad inference would send
  someone walking confidently in a direction we have not earned. That constant
  (`BEARING_ARROW_THRESHOLD` in `Radar.kt`) is the app's ethic in one line.
- **Map** — the 95% confidence ellipse, never a dot. When the walk has been near
  a straight line the posterior is genuinely bimodal, and the ellipse sprawls
  across both possibilities instead of confidently marking the empty space
  between them.
- Uncertainty is reported as **"give or take 4 m"**, not "62% confident" — one
  is actionable, the other is not.

Colour is never the only channel: every band carries a text label, the survey
rows use five-bar meters, and the proximity ramp is ordered by lightness as well
as hue.

### Icon and navigation

The launcher icon is the radar mark from the hunt screen, drawn as a vector
rather than shipped as bitmaps — it scales to every density, adds nothing to the
APK, and stays legible at 48dp. Everything meaningful sits inside the adaptive
icon's centre 66 of 108dp, because launchers mask to shapes the app does not
choose. A `monochrome` layer is supplied so themed icons work on Android 13+.

Back behaves as it should for a tool used one-handed while walking:

- **From a hunt**, back returns to the device list. Dropping a search to a
  reflexive back-press would be infuriating, and the list is where the user was
  heading anyway.
- **From the list**, back must be pressed twice within two seconds, with a toast
  making the first press legible rather than merely ignored.

### Previews

`HuntScreen.kt` and `SurveyScreen.kt` carry `@Preview` composables for the states
that are easy to get wrong — confident bearing, low confidence (the one that must
*not* show an arrow), no contact, and light theme. Open either file in Android
Studio to review the UI without a device or the native core.

## Identifying devices

Verified on hardware: a list that was seven rows of hex now reads **Microsoft
device**, **Apple · AirPlay**, **Apple · Find My**, **Apple device**, plus the
paired **Soundcore Life Q20** and **GoPro 9745** by name.


A BLE advertisement carries a device name only in *some* packets, and most modern
devices advertise from a **resolvable private address that rotates** every few
minutes. Left alone, the survey is a list of hex strings that mean nothing and
are different an hour later — measured here, five of seven nearby devices were
randomised and unnamed.

Four things fix it:

- **Identity is inferred from the advertisement itself.** Almost every device
  announces its manufacturer, and many announce a service. That is enough for
  "Apple · Find My" or "Google Fast Pair" instead of `6D:5E:8A:FE:E4:F0` — not
  an identity, but a description, which is what someone scanning a room needs.
  Apple's Continuity message types are decoded too, reusing the same table
  findphone needed to tell a phone from a pair of earbuds.

- **Paired devices are joined in by name.** Android holds a stable
  address-to-name mapping for every bonded device; the survey uses it, and a
  paired device's stored name beats whatever the advert carried.
- **Paired devices are listed even when not heard.** Classic-only headphones and
  speakers do not appear in an LE scan while idle. Hiding them until they happen
  to shout is exactly what makes these apps feel broken, so they appear with
  "Paired · not heard right now" instead.
- **Randomised addresses are labelled as such**, rather than presented as though
  hex were an identity.

Every row shows the address under the name, because that is what identifies the
device to every other tool and what you would type into:

### Find by address

For the case the list cannot serve: you know the MAC — off a label, from an
earlier session, from another tool — but the device is not advertising right now,
so there is nothing to tap. Entering it starts the hunt anyway, so the app is
already listening the moment the device wakes.

Input is forgiving: colons, dashes or nothing at all, any case. `88D039C807CE`
normalises to `88:D0:39:C8:07:CE` and resolves to its paired name if there is
one.

Reading paired names needs `BLUETOOTH_CONNECT` from Android 12. That is checked
explicitly rather than by catching the SecurityException, so the UI can say why
names are missing instead of silently showing hex.

## Everyday devices: what works, and what cannot

The devices people actually lose are the ones they use daily, and those are
almost always paired — which is the *worst* case for advertisement scanning. Many
stop advertising once bonded and idle.

- **Paired LE devices** get a GATT connection and `readRemoteRssi()`, which works
  whether or not the device advertises and is roughly half as noisy as an
  advertisement: same channel, known transmit power, no rotating address. This is
  the `CONNECTED_LINK` source the fusion core trusts most.
- **Paired Classic (BR/EDR) devices cannot be tracked by signal strength at
  all.** They have no GATT server to connect to, and Android exposes no public
  API for the RSSI of a connected Classic link. Both headphones tested here are
  in this category — `88:D0:39:C8:07:CE [BR/EDR] Soundcore Life Q20`.

The app detects the device type and says which case applies, rather than running
a connection loop that can never succeed while the UI shows "No contact" —
indistinguishable from being out of range, and therefore worse than silence.

Getting Classic RSSI would need either the device to be discoverable (inquiry
reports RSSI) or a raw HCI socket with `CAP_NET_RAW`, which an ordinary app
cannot have. Not a gap we can close; one to be honest about.

## The native core

The fusion filter lives in Rust (`../crates/superfind-core`) so one tested
implementation serves the Linux CLI and this app. Building it for Android needs
the NDK, which is not yet wired in.

Until then `NativeCore.available` is false and the app degrades honestly:
scanning, live signal strength, device ranking, proximity bands and trend all
work; the position fix and inferred bearing do not, and the UI says so in the
same list as hardware limitations.

`DegradedTracker` is deliberately **not** a reimplementation of the filter. It
computes only what is trivially defensible from a window of readings — a median,
a trend, a band — and returns `null` for the fix and bearing. Substituting a
cruder estimator and presenting its output in the same place, with the same
styling, as a real fused fix would be the most misleading thing this app could
do.

To finish the wiring: install the NDK, add the Android Rust targets, build
`libsuperfind_jni.so` per ABI into `app/src/main/jniLibs/`, and implement the
`external fun` signatures in `core/NativeCore.kt`.

## Layout

```
app/src/main/java/dev/superfind/
├── MainActivity.kt          permission gate and top-level routing
├── core/
│   ├── Snapshot.kt          immutable value the UI renders
│   ├── NativeCore.kt        JNI boundary, and what to do when it is absent
│   └── Tracker.kt           native and degraded implementations
├── radio/
│   ├── Capabilities.kt      the tier ladder — the compatibility story
│   ├── Permissions.kt       per-version permission sets, with rationales
│   └── BleScanner.kt        scanning, radio timestamps, duplicate guard
├── motion/MotionSensors.kt  heading and steps, with fallbacks at every layer
└── ui/                      Theme, Radar, TrailMap, HuntScreen, SurveyScreen
```

## What testing on hardware changed

Two things, neither of which any amount of desk reasoning would have produced.

### A budget phone with no magnetometer

The Moto G40 Fusion has an accelerometer, a gyroscope and a hardware step
detector — but **no magnetometer at all**. No `TYPE_ROTATION_VECTOR`, no
geomagnetic rotation vector, and `android.hardware.sensor.compass` absent from
`pm list features`. The first build classified it as "no compass" and dropped it
to Proximity: warmer/colder only, no direction.

That was too pessimistic. It does have `TYPE_GAME_ROTATION_VECTOR` — gyroscope
and accelerometer, no magnetic reference — which gives a *relative* heading:
stable across a session, but with an arbitrary zero. **The synthetic aperture
does not need absolute north.** It bins signal by heading and finds which
direction is strongest; all it requires is that turning 90 degrees registers as
90 degrees.

So the device is now Guided rather than Proximity, and the UI adapts rather than
pretending:

- the radar **hides its compass rose**, because labelling an arbitrary zero "N"
  would be a lie dressed as a compass;
- the direction line reads **"turn right 40°"** instead of "NE", which is the
  actionable form anyway;
- the headline becomes "Distance and relative direction", and the limitations
  list says to keep the phone pointed the same way.

Budget hardware omitting the magnetometer appears to be common, so this is
likely a large slice of real users moved from "no direction" to "direction".

### A crash only a real sensor could find

```
UnsupportedOperationException: Tried to obtain display from a Context not
associated with one
  at MotionSensors.remapForDisplay(MotionSensors.kt:158)
```

`MotionSensors` is constructed with the application context, and from API 30
`Context.getDisplay()` throws on any non-visual context. It threw on the *first
sensor event*, not at construction — so the app launched fine, surveyed fine, and
died a second after entering hunt mode. Compilation, lint and Compose previews
all passed it. Fixed by going through `DisplayManager.getDisplay()`, which is
valid from any context, wrapped so an unreadable rotation degrades the heading
rather than taking down the process.

It also explains an earlier red herring: a screenshot that appeared to show a
transparent window was not a rendering bug, it was the process being torn down
mid-frame.

### The walk

Recorded as a one-line-per-second telemetry stream (`adb logcat -s SuperfindWalk`)
rather than screenshots, which is the only way to tell whether the signal tracked
the walk or merely looked plausible in a still frame.

| Measure | Result |
|---|---|
| RSSI dynamic range | **29 dB** (−93 to −64) |
| Trace | −71 → −87 walking away → **−64** walking back → −87 |
| Trend | flipped WARMER 15×, COLDER 24×, STEADY 19× |
| Aperture swept | up to **88%**, heading spanning 284° |
| Steps detected | 47 |
| Stale samples | 5 brief dropouts, all flagged |
| Crashes | 0 |

The gyroscope-only heading swept 284 degrees of the compass and filled 88% of the
aperture, which is the evidence that the relative-heading fallback is genuinely
usable rather than merely present.

### A smaller one

At least one nearby device **does** advertise TX power (12 dBm), against 0 of 7
observed on Linux. The free-calibration path is rare but not nonexistent, and the
survey row flags it where it occurs.

## Known gaps

- **No native core yet** (above). The largest one.
- **No connected-link RSSI.** Only advertisements are observed, which is the
  noisier source. `readRemoteRssi()` would roughly halve the noise and is
  available on every supported API level — this is the next obvious win, and
  unlike on Linux it is genuinely reachable here.
- **Ranging is detected but not yet used.** `Capabilities` reports UWB, Channel
  Sounding and Wi-Fi RTT correctly; no session is opened yet. Note the constraint
  that shapes this: ranging is a negotiated two-way protocol, so the *lost*
  device must be awake, on Android 16, and cooperating — which is why the passive
  path is the load-bearing one and ranging is additive.
- **Tested on one device only** — Moto G40 Fusion, Android 12. The API 23–30
  permission path and the absolute-compass path have been written and compiled
  but not exercised on hardware that uses them.
- **The bearing itself is still unvalidated.** The walk proved that signal
  strength tracks movement and that the aperture fills — but the inferred bearing
  comes from the Rust core, so until that is built there is no arrow to check
  against ground truth.
