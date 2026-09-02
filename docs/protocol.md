Working codename: **Lumen**. Replace throughout if you pick a real name — it appears in the mDNS service type, so decide before anything ships.

The mDNS service is advertised as `_tcp` on the control port even though most traffic is UDP, because that is the port a client actually connects to first; the multicast group and sync port are learned from the control connection rather than advertised separately.

The protocol is the actual product. [[Firmware]], [[App]] and [[Desktop Application]] are all just implementations against it. Anything not in this document is not guaranteed to work between two implementations.

## Design goals

1. **No required controller.** A mesh of devices runs a full show forever with every app powered off.
2. **Bandwidth independent of LED count.** Adding 5000 LEDs must not add network traffic. Achieved by shipping *programs*, not pixels — see [[Bytecode VM]].
3. **Sub-frame sync.** Two devices rendering the same effect must be visually indistinguishable in timing.
4. **Degrade, never fail.** Every failure mode has a defined visual outcome. A dropped packet holds the last value; a lost master triggers re-election; a lost network keeps rendering.
5. **Extensible without breaking.** Unknown message types are ignored, unknown fields are preserved on retransmit, capabilities are advertised and never assumed.

## Layer model

| Layer | Content |
|---|---|
| L0 Transport | UDP multicast (realtime), UDP unicast (sync), TCP/WebSocket (reliable), UART/SPI/BLE/Zigbee (bridged) |
| L1 Framing | Header, sequence, AEAD authentication — see [[#Security]] |
| L2 Messages | The message catalogue below |
| L3 Semantics | Show state, roles, arbitration, replication |

## L0 — Transport and discovery

| Purpose | Transport | Endpoint |
|---|---|---|
| Realtime broadcast (TICK, CHAN, FRAME) | UDP multicast | 239.42.7.1:7420 |
| Time sync exchange | UDP unicast | :7421 |
| Control, state, program upload | TCP (WebSocket, JSON + binary) | :7422 |
| Outbound events | HTTP POST / MQTT publish | user configured |
| Discovery | mDNS | `_lumen._tcp.local`, advertising the control port |

Realtime traffic is multicast and **unreliable by design** — never retransmit a frame, the moment has passed. Anything that must arrive goes over TCP.

### Discovery TXT records

```
uuid   32 hex chars, generated on FIRST BOOT and persisted in NVS — survives reflashing
mesh   mesh_id this device belongs to; traffic from other meshes is ignored
name   user-visible label
pv     protocol version, e.g. 1.0
fw     firmware version
vm     bytecode VM version supported
chip   esp32s3 | esp32c3 | esp32c6 | rp2040 | ...
cap    capacity score — static, measured at boot, never varies with load
load   current load 0..255 — advisory only, never used in elections
leds   LED count driven by this device (0 for a device with no lights)
topo   LED topology: path | lattice | freeform | none
mapq   mapping quality: synthetic | rough | mapped
caps   comma list, see below
boot   boot counter, from NVS — feeds the AEAD nonce, see [[Wire Format#AEAD]]
state  HLC of this device's newest replicated record
```

**Capability tokens** — one canonical list, referenced everywhere else:

| Token | Meaning |
|---|---|
| `render` | runs the [[Bytecode VM]] and drives LEDs |
| `keeper` | holds and gossips replicated state |
| `timebase` | currently holds the show clock |
| `sim` | currently runs shared simulations |
| `compile` | can compile [[Effect Language]] source on-device |
| `gateway` | terminates Art-Net / MQTT / HTTP / Home Assistant |
| `bridge` | proxies non-WiFi downstream nodes |
| `federate` | holds links to peered meshes — see [[#Federation]] |
| `audio` | has a mic or line-in and publishes the audio channel |
| `imu` | has an IMU |
| `input` | has buttons, encoders or faders |
| `pairing` | currently in pairing mode |

Note that `mapped` is **not** a capability — mapping is a matter of degree, so it is the `mapq` field instead. Anything binary that is really a spectrum belongs in a field, not in `caps`.

`caps` is the extension point. A new capability is a new token; old devices ignore it. Never add a boolean field where a capability token would do.

Devices also gossip their peer table (the auto-discovery list in [[Firmware]]), so a client that reaches **one** device gets the whole mesh in a single request. mDNS is the bootstrap, not the source of truth.

A mesh does **not** span subnets or VLANs — multicast does not route, and a relay role would compromise the sync guarantees that justify the whole architecture. Two locations are two meshes, linked by [[#Federation]]. That is a better answer than a relay: it is honest about the sync limitation instead of hiding it.

## L1 — Framing

Exact byte layouts for this and every message live in [[Wire Format]], which is normative. In outline:

```
offset size  field
0      1     magic 0x4C
1      1     version (major<<4 | minor)
2      1     type
3      1     flags
4      2     mesh prefix (first 2 bytes of mesh_id)
6      4     sender UUID prefix
10     4     sequence number (per sender, per boot)
14     8     show_time_us  — when this payload is VALID, not when it was sent
22     2     payload length
24     n     payload
24+n   16    AEAD tag (ChaCha20-Poly1305, header as associated data)
```

40 bytes of overhead. Three deliberate choices: `show_time_us` in the header makes every realtime packet self-timing, so a late packet is discarded without being parsed or decrypted. The mesh prefix lets a device on a shared LAN drop another mesh's traffic on a two-byte comparison rather than a wasted decrypt. And the AEAD nonce includes a **boot counter** from NVS, so a rebooting device restarting its sequence at zero cannot reuse a nonce under the same key.

## L2 — Message catalogue

### Timing

**TICK** — multicast, 1 Hz, the timebase master's heartbeat. Carries `show_time_us`, master UUID, master **capacity score** ([[Firmware#PPOS capacity not current load]]), the show-clock-to-wall-clock offset and its quality, and an epoch counter incremented on every re-election. A device that sees a TICK from a better (capacity, inverse-UUID) pair than the current master yields immediately.

**SYNC_REQ / SYNC_RESP** — unicast four-timestamp exchange with the timebase master. `t1` client send, `t2` master receive, `t3` master send, `t4` client receive. Offset is `((t2-t1)+(t3-t4))/2`, round trip is `(t4-t1)-(t3-t2)`. Discard any sample whose RTT exceeds 1.5x the running minimum — that filter is what makes this work over WiFi, where the median RTT is noise but the *minimum* is honest.

Each device keeps a linear fit of offset over time to estimate crystal drift, so it stays synced between exchanges and can back off to one exchange per 30 s once converged. **Target: ±500 µs across the mesh**, comfortably under a frame at 60 fps.

Master election is highest **capacity score**, ties broken by lowest UUID. Capacity is static and measured at boot; it must never include current load, or taking a role would lower the score that won the role and the mesh would flap between masters. See [[Firmware#PPOS capacity not current load]]. A device with a better clock source should advertise a capacity bonus rather than becoming a special case in the election rule.

### Two clocks

**Decided: a monotonic show clock for rendering, a separate wall clock for scheduling.** They are different things and conflating them causes a specific, ugly failure.

| | Show clock | Wall clock |
|---|---|---|
| Source | timebase master, mesh-local | NTP where available, else a paired app, else free-running RTC |
| Units | microseconds since mesh epoch | UTC, with a configured timezone for display and rules |
| Property | **never steps backwards, never jumps** | corrected whenever a better source appears |
| Used by | all rendering, ACTIVATE, CHAN validity, cues | schedules, sunrise/sunset rules, logs, event timestamps |

The show clock is disciplined by rate, not by stepping: a device that finds itself ahead slows its clock slightly until it converges. A step correction on the render clock would jump every device in the mesh simultaneously and glitch visibly — which is exactly why the two are separate.

Wall clock is **optional**. A mesh with no internet still renders everything correctly; it just cannot evaluate "at sunset". Schedules must degrade to a stated behaviour when wall time is unknown rather than firing at the wrong moment or not at all, and the app should say plainly that time is unset.

Mapping between them is a single offset the timebase master publishes in TICK, so any device can convert. Sunrise and sunset need a latitude and longitude, which is a one-time setup value and worth asking for during provisioning rather than when a user first tries to build a sunset rule.

### Rendering

**ACTIVATE** — sent over TCP to every device, then confirmed by multicast: bring program slot *n* into use at show time *T*. All devices flip on the same show-clock instant, which is why a program is uploaded into a *free* pool slot ahead of time rather than over the running one ([[Runtime Model#Concurrency dynamic admission-controlled]]). A device that did not receive it in time keeps rendering what it has rather than glitching.

**CHAN** — multicast, per frame, a shared-state channel. Payload is a channel id (u16), a sequence number, and an opaque blob that the running program reads as uniforms.

| Channel class | Producer | Typical rate / size |
|---|---|---|
| Audio bands | audio node — see [[Effects]] | 60–100 Hz, 32–64 B |
| Simulation state | sim master | 30–60 Hz, 64–512 B |
| Sensor stream | any device with IMU etc. | 50 Hz, 16–32 B |
| Automation values | timeline or external integration | on change, small |
| Palette / parameters | controller | on change, small |

Channels are **latest-wins with hold**: a receiver renders the newest blob it has and holds it if the next is late. Every channel declares a `hold_ms` after which it decays to a defined default — so a dead audio source fades the lights to steady rather than freezing them mid-beat. Interpolation between the last two blobs is a per-channel flag.

### Channel ownership

A channel has **one producer at a time**, decided by claim-and-lease. This is deliberately the same shape as the source stack, so there is one idea to learn rather than two.

**`CHAN_CLAIM`** — a producer claims a channel id with a declared priority and a lease duration, renewing while it transmits. A claim at a higher priority preempts the incumbent, which stops transmitting immediately. Equal priority does not preempt: the incumbent keeps it, so two identical sources do not fight.

When a lease lapses — the producer crashed, was unplugged, or left the network — the channel is unowned and the **next-best waiting claimant takes over automatically**. Producers that lost a claim keep re-claiming quietly at their own priority, so recovery needs no coordination.

Suggested audio priorities, which give the behaviour you actually want with no configuration:

| Producer | Priority |
|---|---|
| Desktop loopback | 180 — exact signal, takes over when you plug in |
| Line-in / dedicated analyser node | 160 — best permanent source |
| I2S mic on a device | 120 — the standalone default |
| Phone mic | 100 — convenient, most variable |

So plugging in the desktop takes over from the room mic, and closing it hands back within one lease — which is the behaviour that makes this feel finished.

A **manual pin** overrides the auto-handover: the user can fix a channel to a chosen producer, which then holds the claim at maximum priority until unpinned. Worth having, because automatic handover is right almost always and infuriating the one time you wanted the other source.

Between the lapse and the takeover the channel is briefly unowned, which the existing `hold_ms` decay already covers — the lights ease toward the default rather than freezing.

**FRAME** — direct pixel data for DIRECT mode, and the ingest format for Art-Net / E1.31 / DDP translation. Carries universe or segment id, offset, and packed pixels.

### Arbitration

When a stream and a local program both target the same pixels, **highest priority wins**; equal priority resolves to the most recent source. Every source (program, stream, status override, manual control) declares a priority 0–255 and a `timeout_ms`. On expiry the pixel range reverts to the next-highest active source.

This single rule is what makes status lighting safe. A critical alert publishes at priority 200 with a 5 s timeout: it cannot be stomped by an ambient effect, and it releases on its own if the publisher dies. It generalises into the source stack in [[Runtime Model#The source stack]], which is the one mechanism shows, schedules, alerts, manual control and external streams all reduce to. Suggested bands:

| Range | Use |
|---|---|
| 0–63 | ambient / default scene |
| 64–127 | scheduled shows, audio-reactive |
| 128–191 | user manual control, app preview |
| 192–223 | system self-health |
| 224–255 | user status and alerts |

### Events and state

**EVENT** — a discrete thing happened: IMU tap, button, motion, threshold crossed. Payload is `{event_id, source_uuid, kind, value, show_time_us}`.

`event_id` is **minted by the producer** and is never derived by a receiver. Binding evaluation runs on every keeper and relies on all of them computing the same action id for the same event ([[Data Model#Evaluation every keeper idempotent actions]]); if receivers derived the id from arrival time they would disagree and a button press would fire several times. The producer is the only party that knows there was exactly one event.

Fan-out uses the webhook pattern from [[Firmware]], and also lands on MQTT and Home Assistant. Events drive trigger bindings in [[Data Model]].

**STATE_DIGEST / STATE_PULL / STATE_PUSH** — gossip replication of show state, see [[Data Model#Replication]].

**PROG_BEGIN / PROG_CHUNK / PROG_END** — program upload into a slot, over TCP. PROG_END carries the SHA-256 and the Ed25519 signature. The slot is marked valid only if both verify. The hash proves the transfer was clean; the signature proves *you* sent it.

**SRC_PUSH / SRC_RENEW / SRC_POP** — the wire form of the source stack ([[Runtime Model#The source stack]]): push a scene onto a zone at a priority, renew its lease, or pop it with a fade. Expiry is an **absolute show time**, so a source expires at the same instant on every device regardless of when each received the push.

A push at priority > 0 with no expiry is **rejected at the wire level**. The "stuck red at 3am" rule is worth enforcing where no client can bypass it, rather than trusting every controller to behave.

**HELLO / CAPS / GET / SET** — plain control and introspection.

## Security

Chosen model: pairing plus signed programs.

1. **Pairing.** A device in pairing mode (button, or first boot) advertises `caps=pairing`. The app performs an X25519 ECDH confirmed out of band, by the QR code on the device or by a short code blinked on the LEDs themselves — blink confirmation reuses machinery [[App]] already needs for AR identification.
2. **Mesh key.** The controller hands over a 256-bit mesh key. All L1 frames are ChaCha20-Poly1305 authenticated under it. Sequence numbers give replay protection with a small sliding window.
3. **Controller keys.** Each device stores a list of authorised Ed25519 public keys and only activates programs signed by one of them. **Every replicated record is signed too**, not just programs — see [[Data Model#Signing]], where a device signs its own `device` record with its identity key and everything else requires a controller key. Revocation is a signed record that replicates like any other state.
4. **Unauthenticated integrations** (Art-Net, plain MQTT, HTTP) terminate at a device with `caps=gateway`, explicitly bound to a pixel range and a priority ceiling. They never get to push programs.

Rekeying: the mesh key is a replicated record with a generation counter; devices accept generation *n* and *n-1* during a rollover window.

### Trust in an open-source system

Because firmware is open and anyone can build it, **there is no vendor CA and no secret baked into the image**. The trust model is therefore entirely user-rooted:

- Each mesh is its own trust domain. The user's controller keypair is the root; the pairing step is trust-on-first-use, confirmed out of band by QR or blink code.
- A device ships with no keys. Its identity keypair is generated on first boot, so two devices flashed from the same binary are still distinct.
- A third-party or self-built device joins by pairing like any other. Nothing in the protocol privileges "official" hardware, which is the correct outcome for an open project — but it does mean **the pairing confirmation is the only thing standing between the user and a hostile device**, so it must be a deliberate physical act (button press or on-device QR), never a soft "accept" in an app.
- Because the mesh key is symmetric and shared, any paired device can impersonate any other at the transport layer. Programs **and every replicated record** are separately Ed25519-signed, so a compromised renderer can forge traffic but cannot push code, change a schedule, redefine a zone or alter a binding. It can lie about itself, since it signs its own `device` record — a deliberately small blast radius. If you later want per-device authenticity on realtime traffic, the header already carries the sender UUID prefix and the framing can move to per-sender keys without a version bump.

> **Open question:** do you want a "guest" pairing tier — a device that can receive frames and render but is not given the mesh key for state replication? Useful for a friend's device at a party, and cheap to specify now.

## Bridges

A device with `caps=bridge` presents downstream nodes as first-class devices with their own UUIDs, proxying their discovery records and forwarding their traffic. Downstream links: UART/RS485, SPI, BLE, Zigbee. Upstream sees no difference except a lower capacity score and a declared latency.

Bridged links use **compacted framing** — on a physically trusted wired link the AEAD tag and `show_time_us` are dropped and re-derived from the bridge's own clock. The bridge declares `link_latency_us` so the timing layer compensates. BLE and Zigbee are not physically trusted and keep full framing.

### Thin and full nodes

A downstream node declares whether it can render, and the bridge treats it accordingly.

| | Thin node | Full node |
|---|---|---|
| Advertises | no `render` capability | `render` |
| Receives | FRAMEs — rendered pixels | programs and CHAN, like any WiFi device |
| Runs | a pixel sink and nothing else | the [[Bytecode VM]] |
| Suits | Zigbee bulbs, BLE devices, AVR-class MCUs, anything on a slow link | RP2040 or better on UART/SPI/RS485 |
| Costs | the bridge's CPU and downstream bandwidth, so node count per bridge is capped | a VM port and clock discipline over the link |

Thin nodes are what make the long tail work: a Zigbee bulb becomes an ordinary participant in a volumetric effect rather than a special case in the effect system, and the compromise is frame rate rather than capability. Full nodes are what make a wired RS485 run of a dozen RP2040 boards scale without saturating the bridge.

The bridge advertises the aggregate upstream. Two consequences worth designing for: rendering on behalf of thin nodes is a **permanent deduction from the bridge's capacity score**, exactly like the LEDs it drives itself — it is not a load figure, because the work cannot be handed away — or the election will give it a role it has no room for. And a bridge must declare a **maximum thin-node pixel budget**, so the failure mode when you add one strip too many is a clear warning rather than a frame rate collapse across every node behind it.

## Mesh identity and federation

A **mesh** is the unit of trust, state and time. A house, a workshop and a garden are three meshes, not three zones in one.

Each mesh has a `mesh_id` (UUID), a name, its own mesh key, its own authorised controller keys, its own timebase master, its own replicated state and its own coordinate origin. **A device belongs to exactly one mesh.** Discovery records carry `mesh_id`, and a device ignores traffic from any other mesh on the same network — so two meshes can share a LAN with no interference and no configuration.

This is what makes the "everything is one coordinate space" problem go away. Trying to give a house and a detached workshop a single origin is unpleasant, and it forces one trust domain across places that have no reason to trust each other.

### Federation

Two meshes may peer. A device with `caps=federate` in each mesh holds a link to the other, authenticated by a **federation keypair exchanged once** — separate from either mesh key, so peering never shares a mesh's internal credentials. Peering is explicitly mutual and revocable from either side.

| Crosses a federation link | Does not cross |
|---|---|
| Events, including alerts | Replicated state — each mesh keeps its own |
| Cue triggers and scene activation, by name | Programs and effect uploads |
| Wall clock and a common cue reference | The show clock — each mesh keeps its own timebase |
| Presence and health summaries | Per-frame channels: audio, sim, sensors |
| Global commands: blackout, all-off, panic | Coordinates and zone membership |

**The show clocks stay independent, and that is the important limitation.** Two federated meshes cannot run a frame-accurate volumetric effect between them, because sub-millisecond sync is a property of one timebase master and one multicast domain. What they *can* do is start the same scene at the same wall-clock instant, which is coarse — tens of milliseconds — but entirely adequate for "put the whole house in party mode" or "flash everywhere, the doorbell rang". Say this plainly in the UI, or someone will try to sweep a plane across two buildings and conclude the sync is broken.

Federation is also how a mesh spans subnets or VLANs without a relay role: put a mesh on each side and link them, accepting coarse rather than frame-accurate sync. That is a better answer than making multicast route.

> **Open question:** should a federated mesh be able to *see* the other's devices for monitoring, or only exchange events? Read-only visibility is useful and low risk; making it explicit avoids it happening by accident.

## Interop mapping

| External | Direction | Mapping |
|---|---|---|
| Art-Net / sACN (E1.31) | in | universe + channel to a FRAME into a pixel range at a configured priority |
| Art-Net / sACN | out | a virtual device's rendered output published as universes, so Lumen can drive commercial fixtures |
| DDP | in/out | closest match to FRAME, cheapest to support first |
| Home Assistant | both | light entities per device and zone, scene selection, EVENT as HA events, HA states as automation channels |
| MQTT | both | topic per device and per channel, the generic escape hatch |
| DMX out | out | via a bridge with an RS485 transceiver |

## Versioning

Major version bump means incompatible framing. Minor means additive messages and capability tokens. Devices state `pv` in discovery; a controller refuses to push programs to a device with an unknown major version but still shows and controls it.

## Scale target: ~50 devices, 10k+ LEDs

Consequences of that target, since several defaults above only hold at small scale:

- **Multicast becomes the main risk.** 50 devices on consumer WiFi with IGMP snooping and multicast-to-unicast conversion is exactly where multicast quietly stops working. Every device must measure its own CHAN loss rate and report it, so the diagnostics in [[Desktop Application]] can show the problem rather than leaving you to guess why the lights stutter.
- **Cap the keeper set.** Do not let 50 devices all gossip full state — that is 2500 potential digest exchanges. Elect **5–7 keepers** by flash size then capacity score; everyone else pulls read-only. Gossip cost then stays flat as the mesh grows.
- **Sync load is fine.** 50 devices at one exchange per 30 s is under two requests a second at the timebase master.
- **Program upload must be batched.** 50 devices in maybe 6 device classes means 6 compiled programs, each uploaded once per device. Multicast the program body with a TCP fallback for stragglers, or upload sequentially in the background and only `ACTIVATE` once all have verified.
- **Mapping UX must survive 50 devices.** The identify-by-blink flow needs the batch coding in [[App]]; sequential per-device identification does not scale to a room this size.
- **The device list becomes a fleet view.** Grouping, filtering, bulk edit and health-at-a-glance stop being nice-to-have at roughly 20 devices.

## Open questions

- Multicast reliability varies badly across consumer APs (IGMP snooping, multicast-to-unicast conversion, low multicast data rates). Do you want a measured fallback that unicasts CHAN to N devices when multicast is detected as lossy?
- A machine-readable wire-format IDL is close to mandatory for an open-source project: it is what lets someone write a fourth implementation without reading prose. Suggest defining it alongside a **conformance test suite** — a set of recorded exchanges any implementation must reproduce. That test suite, more than this document, is what actually keeps implementations compatible.
- Protocol version policy needs writing down before the first public release: what "minor" guarantees, how long old majors are supported, and how a device behaves when it meets something newer than itself.
