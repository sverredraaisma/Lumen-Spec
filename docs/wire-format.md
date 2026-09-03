The normative byte-level definition. [[Protocol]] explains *why*; this says exactly what goes on the wire. Where the two disagree, this document wins.

This is the source for `lumen-spec`'s IDL and conformance vectors ([[Tech Stack#The conformance runner]]).

## Conventions

- **Little-endian** throughout, matching every target CPU.
- `u8 u16 u32 u64 i16 i32` — fixed-width integers.
- `q16` — Q16.16 fixed point in an `i32`. Value = raw ÷ 65536.
- `uuid` — 16 bytes.
- `str` — `u8` length followed by that many UTF-8 bytes. Max 255.
- `blob` — `u16` length followed by bytes.
- Reserved fields are zero on send and **ignored on receive**, never rejected.
- Times are `show_time_us` (`u64`) unless a field is explicitly named `wall_*`.

## L1 header

Every datagram, on every transport.

```
off  size  field
0    1     magic          0x4C
1    1     version        major<<4 | minor
2    1     type           message type, table below
3    1     flags          bit0 encrypted, bit1 fragment, bit2 last, bit3 first, rest reserved
4    2     mesh_prefix    first 2 bytes of mesh_id
6    4     sender_prefix  first 4 bytes of sender uuid
10   4     sequence       per sender, per boot, increments every datagram
14   8     show_time_us   when this payload is VALID, not when it was sent
22   2     payload_len
24   n     payload
24+n 16    AEAD tag
```

**40 bytes of overhead.** Two deliberate choices:

`show_time_us` sits in the header so a receiver can discard a late packet without parsing or decrypting it. `mesh_prefix` is there so a device on a shared LAN drops another mesh's traffic on a 2-byte comparison — the AEAD tag would reject it anyway, but only after a wasted decrypt, and at 50 devices × 60 Hz that waste is real.

### AEAD

ChaCha20-Poly1305 (RFC 8439) under the mesh key. Bit 0 of `flags` selects whether the payload is encrypted-and-authenticated or authenticated-only — pixel data and audio bands are not secret, and skipping the cipher on them saves cycles that matter on a C3. Authentication is never optional.

The two modes differ only in what is handed to the AEAD, and both produce the same 16-byte tag in the same place:

| `flags` bit 0 | associated data | plaintext |
|---|---|---|
| 1 — encrypted | the 24-byte header | the payload |
| 0 — authenticated only | the 24-byte header ‖ the payload | empty |

Authenticated-only is therefore the *same* primitive with the payload moved into the associated data, not a second construction. That matters: an implementation needs one algorithm, not two, and there is no separate MAC whose key derivation could be got wrong. It is spelled out because "authenticated but not encrypted" has several plausible encodings and the tag is only interoperable if everyone picks the same one.

Bit 0 lives inside the header, and the header is authenticated in both modes, so an attacker cannot flip the mode without invalidating the tag. The two modes also feed Poly1305 different byte streams for the same datagram — RFC 8439 pads and length-encodes the associated data and the ciphertext separately — so a tag computed in one mode never verifies in the other.

Nonce, 12 bytes:

```
sender_prefix (4) ‖ sequence (4) ‖ boot_counter (4)
```

`boot_counter` is stored in NVS and incremented on every boot; receivers learn it from discovery and `HELLO`. Without it, a device rebooting would restart its sequence at zero and reuse nonces under the same key — the classic way to destroy a stream cipher. With it, nonce reuse needs 2³² datagrams within one boot, which at 100/s is over a year.

Replay protection is a 64-entry sliding window on `(sender_prefix, boot_counter)`.

## Message types

High nibble is the category, which keeps dispatch a jump table and leaves obvious room to grow.

| Code | Name | Transport |
|---|---|---|
| `0x01` | `HELLO` | TCP |
| `0x02` | `CAPS` | TCP |
| `0x03` | `GET` | TCP |
| `0x04` | `SET` | TCP |
| `0x10` | `TICK` | multicast |
| `0x11` | `SYNC_REQ` | unicast UDP |
| `0x12` | `SYNC_RESP` | unicast UDP |
| `0x20` | `ACTIVATE` | TCP, echoed multicast |
| `0x21` | `CHAN` | multicast |
| `0x22` | `CHAN_CLAIM` | multicast |
| `0x23` | `CHAN_RELEASE` | multicast |
| `0x24` | `FRAME` | multicast or unicast |
| `0x30` | `SRC_PUSH` | multicast |
| `0x31` | `SRC_RENEW` | multicast |
| `0x32` | `SRC_POP` | multicast |
| `0x40` | `EVENT` | multicast + TCP fan-out |
| `0x50` | `STATE_DIGEST` | TCP |
| `0x51` | `STATE_PULL` | TCP |
| `0x52` | `STATE_PUSH` | TCP |
| `0x60` | `PROG_BEGIN` | TCP |
| `0x61` | `PROG_CHUNK` | TCP |
| `0x62` | `PROG_END` | TCP |
| `0x70` | `FED_HELLO` | TCP, cross-mesh |
| `0x71` | `FED_EVENT` | TCP, cross-mesh |
| `0x72` | `FED_CUE` | TCP, cross-mesh |
| `0x80` | `PROBE_SET` | TCP |
| `0x81` | `PROBE_DATA` | TCP |
| `0x82` | `TIMECTL` | multicast |
| `0xF0`–`0xFF` | vendor / experimental, never assigned by the spec | — |

An unknown type is **ignored, not an error** — that rule is what makes minor-version additions safe.

> The `0x30` family did not exist in [[Protocol]]'s original catalogue. The source stack was specified without any wire representation, which meant nothing could actually push a source. Adding it here.

## Payloads

### `TICK` — 0x10, multicast, 1 Hz

```
u64  show_time_us        (also in header; repeated so TICK is self-contained in logs)
uuid master_uuid
u32  master_capacity
u32  election_epoch
u64  wall_time_us        0 if wall clock unknown
u8   wall_quality        0 none, 1 app-supplied, 2 NTP, 3 GPS/RTC
u8   reserved[3]
```

`wall_quality` matters because schedules must degrade explicitly when time is unknown ([[Protocol#Two clocks]]) rather than firing at a plausible-looking wrong moment.

### `SYNC_REQ` / `SYNC_RESP` — 0x11 / 0x12

```
REQ:   u64 t1
RESP:  u64 t1 (echoed)   u64 t2   u64 t3
```

`t4` is recorded locally by the requester. Offset `((t2-t1)+(t3-t4))/2`; RTT `(t4-t1)-(t3-t2)`. Discard any sample whose RTT exceeds 1.5× the running minimum.

### `ACTIVATE` — 0x20

```
u16  program_id
u8   slot                pool index
u8   reserved
u64  activate_at         show time
```

`slot` is an index into the device's **program pool**, not one of a fixed pair ([[Runtime Model#Concurrency dynamic admission-controlled]]). Pool size varies by device and is reported in `CAPS`; `0xFF` in `PROG_BEGIN` means "device chooses a free slot" and the chosen index comes back in the response, which is what a controller should normally send rather than guessing at another device's memory.

### `CHAN` — 0x21

```
u16   channel_id
u16   producer_seq
blob  payload
```

Latest-wins with hold. A receiver drops any `CHAN` whose `producer_seq` is older than the newest seen from the current owner.

The `payload` is opaque to the protocol: what is in it depends on what kind of channel it is. One layout is fixed here because every implementation has to read it — see below.

#### Audio channel payload

40 bytes, and the same whichever of the four sources produced it, so an effect never knows or cares where the audio came from.

```
0    32   bands[32]      u8 each, log-spaced, AGC-normalised
32   1    level
33   1    smoothed_level
34   1    flags           bit0 onset, rest reserved
35   1    confidence
36   2    beat_phase      u16, one beat spans the full range and wraps
38   2    bpm_x4          u16, quarter-BPM; 0 means unknown
```

Analysis happens at the source, once, and only the result is broadcast — never raw audio.

**`beat_phase`, not beat events.** A receiver that misses a packet can extrapolate where in the bar it is and stay on beat; an effect fed discrete beat events stutters instead. A beat delivered late as an event is worse than useless, because the flash lands after the drum. Every `u16` is a valid phase, so there is no malformed case to define on the hottest path in the system.

A payload **shorter** than 40 bytes is malformed. A **longer** one is accepted and the excess ignored, so a later minor version can append a field without taking the mesh's audio away from a device that has not been upgraded. Unknown `flags` bits are ignored for the same reason.

### `CHAN_CLAIM` / `CHAN_RELEASE` — 0x22 / 0x23

```
CLAIM:   u16 channel_id   u8 priority   u8 reserved   u32 lease_ms
RELEASE: u16 channel_id   u16 reserved
```

Strictly-greater priority preempts; equal priority does not, so two identical producers never fight. On lease expiry the channel is unowned and waiting claimants re-claim.

### `FRAME` — 0x24

```
u16  segment_id
u16  offset          first pixel index
u8   format          0 RGB8, 1 RGBW8, 2 RGB16, 3 CCT
u8   priority
u16  count
...  pixel data
```

Fragmentation uses header `flags` bits 1–3 when a segment exceeds the MTU.

### Datagram size

**A datagram is at most 1200 bytes, header and tag included.** That leaves 1160
bytes of message payload once the 24-byte header and the 16-byte tag are
subtracted, and it is what the compiler assumes when sizing a `CHAN` payload.

1200 rather than something closer to a 1500-byte Ethernet MTU because a
surprising number of home networks put a tunnel somewhere in the path — a VPN, a
mesh-WiFi backhaul, a carrier doing PPPoE — and each shaves the usable size.
Fragmentation exists for the cases that genuinely need more, but a *show* that
fragments every frame has turned one lost packet into two, so the common path is
sized to fit.

The limit is on the whole datagram rather than on the payload inside it, because
what has to survive the path is the packet, and a rule about the payload alone
would be 40 bytes wrong in exactly the case where it matters.

### Fragmentation

A message too large for one datagram is split across several, using `flags` bit 1
on every fragment, bit 3 on the first and bit 2 on the last. Fragments of one
message are **consecutive `sequence` values from the same sender**, so no
fragment index or message id is needed: the header already carries everything
reassembly requires, and adding a field for it would cost every datagram in the
system four bytes to serve the rare one.

**Bit 3 is not redundant, and leaving it out is unsound.** Without a mark on the
first fragment, a receiver that misses the opening one cannot tell: it sees a
fragment that continues nothing, begins a message with it, and on the last
fragment delivers a **truncated message as though it were whole**. Nothing
downstream can detect that — the payload is short, but a decoder cannot
distinguish a short message from a truncated one in general. The flag costs a
reserved bit rather than the four bytes an index would have cost, which is the
same trade the paragraph above makes.

A receiver reassembles by starting on a fragment with bit 3 set and holding
fragments until it sees one with bit 2. If a `sequence` is missing when that
arrives, or if a fragment arrives that neither starts a message nor continues the
one in progress, **the whole message is discarded**.
Waiting is not useful: the two things that fragment are a sim snapshot, which is
replaced 60 times a second and will be superseded before a retransmission could
arrive, and a state record, which travels over a reliable transport and does not
fragment on the wire at all.

**A receiver that drops a fragmented message keeps what it had.** For a sim
snapshot that means holding the last complete one rather than rendering a partial
state or nothing — the simulation appears to pause for a frame, which at 60 Hz is
invisible, where a partial state is a particle field with half its elements from
this frame and half from the last, and nothing at all is a visible blink that
reads as a fault. This is the same shape as the rule for a stale channel: hold
first, and only then degrade.

A reassembly buffer is bounded by the same rule as everything else here: a
receiver may hold at most one incomplete message per sender, and a fragment that
does not continue the message in progress replaces it. A sender that fragments
two messages at once is not conforming, and a receiver must not grow memory
waiting to find out.

### `SRC_PUSH` / `SRC_RENEW` / `SRC_POP` — 0x30–0x32

```
PUSH:
  uuid  source_id
  uuid  zone_id
  uuid  scene_id
  u8    priority
  u8    flags            bit0 has_expiry
  u16   fade_in_ms
  u16   fade_out_ms
  u16   reserved
  u64   expires_at       show time; absent when flags bit0 clear
  blob  param_overrides

RENEW:  uuid source_id   u64 expires_at
POP:    uuid source_id   u16 fade_out_ms
```

Expiry is an **absolute show time**, not a duration — every device already shares that clock, so a source expires at the same instant everywhere regardless of when each device received the push.

A `PUSH` with `priority > 63` and `flags` bit 0 clear must be **rejected**. This is the "stuck red at 3am" rule from [[-README#Cross-cutting rules]] enforced at the wire level, so no client can create the condition even by accident.

**63, not 0.** The priority bands in [[Runtime Model#The source stack]] give 0–63 to the default and ambient scene, and say of that band "never — this is the floor". A floor that had to expire would not be a floor: something has to hold the lights when every show, override and alert has gone, and that something cannot be on a timer. Earlier drafts of this document said `> 0`, which contradicted the band table and split the two implementations — the codec refused an ambient scene at priority 40 that the source stack accepted. The band table wins, because it is the more considered of the two.

### `EVENT` — 0x40

```
uuid  event_id          minted by the producer, never derived by a receiver
uuid  source_uuid
str   kind
q16   value
u64   wall_time_us      0 if unknown
```

Producer-minted ids are what let every keeper compute the same action id and collapse duplicate actions ([[Data Model#Evaluation every keeper idempotent actions]]).

### `STATE_DIGEST` / `STATE_PULL` / `STATE_PUSH` — 0x50–0x52

```
DIGEST: u16 count, then count × { uuid record_id, u64 hlc }
PULL:   u16 count, then count × uuid
PUSH:   u16 count, then count × {
          uuid record_id, u8 record_type, u64 hlc,
          uuid author, blob body, u8 sig[64]
        }
```

Signature covers `record_id ‖ record_type ‖ hlc ‖ author ‖ body`. Verify only on change — the digest exchange compares HLCs first, so steady-state gossip costs no signature checks.

**A `STATE_PUSH` carries at most 8 KiB of records, and never splits one.** As many whole records as fit under that go in a message and the rest follow in the next, so a gossip round stays responsive whatever any single record weighs. Without the cap one large `effect` record holds a round for as long as it takes to transfer, and a device that has just joined waits behind it before it can render anything.

The bound is on the receiver as much as the sender: the device with the least memory in the mesh is the one that has to hold what arrives, and it is not usually the device that sent it. A record larger than the cap on its own is refused when it is authored rather than at gossip time, where the failure would be somebody else's device quietly falling behind.

8 KiB rather than the datagram limit because this travels over the reliable transport, where framing costs little and a round trip costs a lot — sizing it to a datagram would mean a hundred messages to move a scene that could have gone in three.

### `PROG_BEGIN` / `PROG_CHUNK` / `PROG_END` — 0x60–0x62

```
BEGIN: u16 program_id  u8 slot  u8 vm_min_version  u32 total_len  str device_class
CHUNK: u16 program_id  u32 offset  blob data
END:   u16 program_id  u8 sha256[32]  u8 sig[64]
```

The slot is valid only if the hash **and** the signature verify. The hash proves the transfer was clean; the signature proves who sent it.

`vm_min_version` is the *minimum* VM version the program requires. Instructions are append-only within a VM major version ([[Bytecode VM#Version compatibility]]), so a device refuses only programs needing more than it implements — a firmware upgrade never invalidates a program already running.

### Federation — 0x70–0x72

```
FED_HELLO: uuid mesh_id  str mesh_name  u32 caps  u8 fed_pubkey[32]
FED_EVENT: (an EVENT payload)  uuid origin_mesh
FED_CUE:   str cue_name  u64 wall_at_us  uuid origin_mesh
```

Cross-mesh cues are scheduled against **wall time**, not show time — federated meshes have independent timebases ([[Protocol#Federation]]), so this is coarse by construction and the field name says so.

### Debug — 0x80–0x82

```
PROBE_SET:  u16 program_id  u16 probe_id  u16 pixel_index  u16 reserved
PROBE_DATA: u16 probe_id  u16 pixel_index  u64 frame_show_time  q16 value
TIMECTL:    u8 mode        0 run, 1 pause, 2 step, 3 set
            u8 reserved[3]
            u32 lease_ms
            u64 target_show_time
```

`TIMECTL` carries a lease so a crashed editor cannot leave a mesh frozen; on lapse a device resumes free-running ([[Desktop Application#Debugging effects]]).

## Version policy

The header carries `major << 4 | minor`.

**A minor version only adds.** New message types, new fields appended to an
existing payload, new `flags` bits. Every rule that makes this safe is already
stated where it applies: an unknown message type is ignored rather than refused,
a payload longer than expected is accepted and the excess ignored, and unknown
`flags` bits are ignored. A device therefore talks to any device sharing its
major version, in either direction, whichever is newer.

**A device supports the current major and the one before it.** That is what lets
a mesh be upgraded one device at a time, which is the only way it happens in a
house: somebody flashes the strip they can reach and gets to the others when a
ladder is free. A device meeting a major it does not implement ignores the
traffic rather than erroring — it is not addressed to something it can be.

Two majors back is not supported, and the difference is deliberate. Supporting
one means every change has to work against exactly one older shape, which is
testable; supporting two means every pair of adjacent versions has to keep
working together, and the combinations grow faster than anybody tests them.

None of this is a promise about a *program*: instructions are append-only within
a VM major version and `vm_min_version` handles that separately, so a firmware
upgrade never invalidates a program already running.

## State machines

Written as the sans-IO transitions an implementation must reproduce ([[Tech Stack#Sans-IO core]]), which is exactly the form the behavioural conformance vectors take.

### Time sync

| State | Event | Action | Next |
|---|---|---|---|
| `Unsynced` | `TICK` seen | send `SYNC_REQ` | `Syncing` |
| `Syncing` | `SYNC_RESP`, RTT ≤ 1.5× min | add sample | `Syncing` until 32 samples |
| `Syncing` | 32 good samples | set offset from the median, start drift fit | `Synced` |
| `Synced` | 30 s elapsed | send `SYNC_REQ` | `Synced` |
| `Synced` | 3 TICKs missed | — | `Unsynced`, start election |
| any | offset correction needed | **slew the rate, never step** | — |

Suppress tightly-synced shows while `Unsynced` rather than rendering them wrong.

**A burst is 32 samples and the offset is their median.** Median rather than
mean, because one surviving outlier drags a mean by an eighth of its error and a
median not at all, and the RTT filter does not catch a path that is
*consistently* asymmetric.

Quickest-wins is the other standard answer — a round trip that was quick had less
room to be asymmetric, and asymmetry is the whole error term. Measured against
the median on identical samples, it wins decisively at short bursts, loses at
long ones, and at 32 the two are indistinguishable:

| burst | quickest p95 | median p95 |
|---|---|---|
| 8 | 1 475 µs | 2 500 µs |
| **32** | **825 µs** | **850 µs** |
| 128 | 1 500 µs | 1 225 µs |

So the selector is not where the error is, and this specifies the median because
that is what is implemented and it is the more robust of two equals at the length
that matters. **The burst length is what moves the number**, and that is the
change worth making.

The 32 is measured rather than chosen ([[Spike S1]]). On two ESP32-C3s over a
domestic AP:

| burst | spans | p50 | p95 |
|---|---|---|---|
| 8 | 1.6 s | 325–425 µs | 1 475–1 500 µs |
| **32** | 6.4 s | **225–300 µs** | **675–825 µs** |
| 128 | 25.6 s | 875–925 µs | 1 500–1 775 µs |

Ranges rather than single figures because the network moved between runs, which
is itself the point: these are domestic conditions, not a lab.

**Longer is not monotonically better, and an implementation that assumes it is
will make things worse.** A follower's clock drifts on the order of 33 µs per
second against the master's — an ordinary crystal, well within its own spec — so
a 25 s burst accumulates around 800 µs of drift *inside itself*. Short bursts are
limited by network noise, long ones by drift, and the optimum sits where those
cross. Where it sits depends on the crystal rather than on the network, so a
device with a better oscillator can use a longer burst and one with a worse
oscillator must not.

**Power save must be off** on any device holding this clock. A station left in
the default mode parks its radio between the AP's beacons and wakes on DTIM,
which quantises every exchange to the beacon interval: measured, that alone moved
the minimum round trip from 4.3 ms to 17 ms and the 95th percentile error from
1 250 µs to 5 000 µs. The power cost is real and belongs in the device's budget;
it is not recoverable without giving up the shared clock.

### How close is close enough

The original target was ±500 µs at the 95th percentile. That figure was a guess
at what "does not visibly tear" means, and measurement suggests it is about three
times tighter than the requirement it stands for.

What the design actually needs is that a wave crossing several devices does not
tear, and the unit that matters there is a **frame**: 16 667 µs at 60 fps. The
measured 675 µs p95 is 4% of a frame. Stating the requirement as a fraction of a
frame rather than as an absolute keeps it meaningful at 30 fps as well, and stops
the next reader spending real effort closing a gap that is not open.

Devices still report their sync quality, and a show that genuinely needs tighter
agreement than this can still refuse to run — that is what `Unsynced`
suppression is for.

### Election — timebase, sim, keeper

Compare `(capacity, ~uuid)` lexicographically, capacity **only**, never load.

| State | Event | Action | Next |
|---|---|---|---|
| `Follower` | no `TICK` for 3 s | broadcast candidacy | `Candidate` |
| `Candidate` | better candidate seen | — | `Follower` |
| `Candidate` | 2 s with no better | assume role, `election_epoch++` | `Leader` |
| `Leader` | strictly better `TICK` seen for 3 consecutive ticks | yield | `Follower` |
| `Leader` | 1 s elapsed | send `TICK` | `Leader` |

The three-tick hysteresis on yielding stops a device rebooting with a marginally different benchmark from triggering a needless handover.

### Source lease

| State | Event | Next |
|---|---|---|
| `Active` | `SRC_RENEW` | `Active`, expiry extended |
| `Active` | `expires_at` reached | `FadingOut` for `fade_out_ms` |
| `Active` | `SRC_POP` | `FadingOut` |
| `FadingOut` | fade complete | removed; re-run admission |

### Channel ownership

| State | Event | Next |
|---|---|---|
| `Unowned` | `CHAN_CLAIM` | `Owned(claimant)` |
| `Owned(a)` | `CHAN_CLAIM` from b, `prio(b) > prio(a)` | `Owned(b)`; a stops immediately |
| `Owned(a)` | `CHAN_CLAIM` from b, `prio(b) ≤ prio(a)` | `Owned(a)` |
| `Owned(a)` | lease lapsed, or `CHAN_RELEASE` | `Unowned`; receivers begin `hold_ms` decay |

## Conformance vectors

Two kinds, both plain data files ([[Tech Stack#The conformance runner]]).

**Codec vectors** — a hex datagram plus its decoded structure as JSON. Checked both directions: bytes must parse to the structure, and the structure must re-encode to identical bytes. Includes deliberately malformed inputs, each with the required outcome (`ignore` or `reject`), because how an implementation handles rubbish is part of the protocol.

**Behavioural vectors** — an initial state, a sequence of `(time, event)` pairs, and the expected `(time, action)` sequence. Scenarios worth shipping from the start:

- Cold-start sync and convergence to ±500 µs
- Timebase master vanishing mid-show; re-election; no visible render interruption
- Two candidates with equal capacity, distinguished by UUID
- Three-way partition and heal, converging on HLC order
- A `SRC_PUSH` arriving after its own `expires_at` — must be ignored, not rendered briefly
- Channel preemption and hand-back on lease lapse
- A record with a bad signature — must be rejected and not gossiped onward
- A `SRC_PUSH` at priority 200 with no expiry — must be rejected

That last pair matters most. **Rejection cases are the ones independent implementations get wrong**, because a naive implementation accepts everything and looks perfectly healthy right up until something malformed reaches it.

## Open questions

- Should `FRAME` support run-length or delta encoding for Art-Net ingest at high universe counts? Worth measuring before adding — it complicates the hottest path in the system.
