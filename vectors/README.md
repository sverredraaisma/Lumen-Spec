# Conformance vectors

Two classes, both plain data files so that adding a case never means touching
the runner.

| Class | What it checks | How |
|---|---|---|
| `codec/` | framing and message encode/decode | feed bytes, compare parsed structure; feed structure, compare bytes |
| `behavioural/` | sync, election, source-stack resolution and admission | feed an event sequence, compare the emitted actions |

The behavioural class is the valuable half.

**Write the vector when you write the behaviour.** Retrofitting a suite across
seven repos is miserable.

Both classes live in one tree and load through one command, because an
implementer runs one thing and gets one verdict. A file's `kind` selects which
it is; `"codec"` is the default, so every file written before behavioural
vectors existed still reads.

---

## Codec vectors

One file per message type, named after it in lower case — `tick.json`,
`src_push.json` — plus `framing.json` for the L1 header and `malformed.json`
for the negative cases. 26 files, 97 cases at the time of writing, alongside 12
behavioural scenarios.

Run them:

```bash
cargo run -p lumen-conformance -- --self-test vectors/            # check the corpus itself
cargo run -p lumen-conformance -- --adapter "<cmd>" vectors/      # check an implementation
```

### File shape

```json
{
  "schema": 1,
  "message": "TICK",
  "code": "0x10",
  "description": "why this message exists, in one or two sentences",
  "cases": [ ... ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `schema` | yes | Always `1`. Bumped only for a change the runner cannot read. |
| `message` | yes | Message name, or `L1_HEADER` / `MALFORMED`. Documentation and the first half of a case id; **the runner never switches on it**. |
| `code` | no | The wire type byte, as a string, for the reader's benefit. |
| `description` | yes | Prose. |
| `cases` | yes | Non-empty array. |

### Case shape

```json
{
  "name": "gps_locked_master",
  "description": "why this case is here and what breaks without it",
  "datagram": "4c01100…aa",
  "value": {
    "header":  { "magic": 76, "version_major": 0, "version_minor": 1, "type": 16,
                 "flags": 0, "mesh_prefix": "abcd", "sender_prefix": "01020304",
                 "sequence": 7, "show_time_us": 123456, "payload_len": 44 },
    "tag":     "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    "payload": { "master_capacity": 1000, "wall_quality": 3, "…": "…" }
  }
}
```

`datagram` is the **whole** datagram — 24-byte header, payload, 16-byte AEAD
tag — as lowercase hex with no separators. `value` is what it decodes to.

An optional `expect` selects the required outcome; omitting it means the
strongest one:

| `expect` | Directions checked | Use it for |
|---|---|---|
| *(absent)* | decode **and** encode | Every well-formed message. The default on purpose. |
| `"accept"` | decode only | Legal but non-canonical input: a dirty reserved byte, a field a newer minor version appended. Re-encoding normalises it, so demanding byte equality would be wrong. |
| `"ignore"` | decode only | Must be dropped silently. Unknown message types. `value` is forbidden — there is nothing to decode. |
| `"reject"` | decode only | Must be refused with an error. |

A negative case may carry `reason`: prose for the reader, never compared
against an implementation's error text.

**Both directions or it is not a vector.** A case with no `expect` and no
`value` is refused by the schema check, because bytes-only or structure-only
testing lets an encoder and a decoder drift together and agree with each other
forever.

### How values are spelled

The runner forwards `value` to an adapter untouched and compares what comes
back for structural equality — object key order is not significant, nothing
else is normalised. So the spellings below are the contract:

| Wire type | JSON | Note |
|---|---|---|
| `u8 u16 u32 u64` | number | Integers, never quoted. `u64` uses its full range: the runner's JSON parser keeps numbers as text so `18446744073709551615` survives. |
| `q16` | number | The **raw** `i32`, not the divided value. `1.5` is `98304`. Negatives appear as negatives. |
| `uuid` | 32 lowercase hex digits | |
| `str` | string | UTF-8. The `u8` length prefix is implied. |
| `blob` | lowercase hex string | Possibly empty. The `u16` length prefix is implied. |
| fixed byte arrays (`sha256`, `sig`, `fed_pubkey`) | lowercase hex string | |
| enums (`wall_quality`, `format`, `mode`) | number | The wire discriminant. A name would need a table in the runner, which is exactly the special case that belongs in the data. |
| optional (`SRC_PUSH.expires_at`) | number or `null` | `null` means the flag bit is clear and the field is **absent from the wire**, not zero. |
| counted lists (`STATE_DIGEST`, `STATE_PULL`, `STATE_PUSH`) | array | The `u16` count is derived from the array's length, never written twice. |

Reserved bytes never appear in `value`. They are zero on send and ignored on
receive, so representing them would invite an implementation to round-trip
something it is supposed to discard.

### The rules the corpus exists to pin

`malformed.json` is the half that catches real bugs, because a naive
implementation accepts everything and looks perfectly healthy until something
malformed reaches it. Three cases in it are load-bearing:

- **`unknown_message_type` → `ignore`, not `reject`.** An implementation that
  rejects an unassigned type code turns every future message type into a
  compatibility break, and nothing will tell it so until someone ships one.
- **`src_push_priority_200_without_expiry` → `reject`.** The "stuck red at 3am"
  rule at the wire level. `src_push_priority_1_without_expiry` is there too:
  the ambient floor is priority 0, and an implementation that guessed at some
  higher threshold passes the first and fails the second.
- **`higher_minor_version_is_accepted` → `accept`.** The other half of forward
  compatibility. Together with the unknown-type rule it is the whole story.

### Adding a codec vector

1. Write the case into the right file. If the message is new, the file is new,
   and the IDL entry lands in the same change — a message without vectors is
   the drift this repo exists to prevent.
2. `cargo run -p lumen-conformance -- --self-test vectors/`. This checks the
   schema, that the hex parses, and that the L1 header you declared matches the
   header bytes you wrote — including `payload_len`, which is the field a
   hand-edited vector gets wrong, because it is the only one that is a function
   of the rest of the file.
3. Run it against an implementation.

The runner needs no change for any of that, and if it ever does, it has grown a
special case that belongs in the data.

---

## Behavioural vectors

One scenario per file, named for the behaviour. Each is one state machine, an
initial state, and a list of steps: at each step an event goes in at a stated
time and a list of actions must come out. That is the sans-IO contract every
implementation core is written to — `on_event(now, event) -> Vec<Action>` —
expressed as data.

```bash
cargo run -p lumen-conformance -- --self-test vectors/
cargo run -p lumen-conformance -- --adapter "<cmd>" vectors/behavioural/
```

### File shape

```json
{
  "schema": 1,
  "kind": "behavioural",
  "machine": "node",
  "name": "sync_cold_start_converges",
  "description": "what failure this vector pins, in a sentence or two",
  "initial_state": { "uuid": "1111…", "capacity": 1000, "…": "…" },
  "steps": [
    {
      "at_us": 1000000,
      "event": { "event": "datagram", "bytes": "4c0110…" },
      "description": "optional, for a step doing something subtle",
      "expect": [
        { "action": "send", "to": "0a0a0a0a",
          "datagram": { "$starts_with": "4c011100abcd11111111" } },
        { "action": "set_timer", "in_us": { "$between": [1, 1000000] } }
      ]
    }
  ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `schema` | yes | Always `1`. |
| `kind` | yes | `"behavioural"`. Absent means codec. |
| `machine` | yes | Which state machine to build. Passed to the adapter verbatim; **the runner never switches on it**. |
| `name` | yes | The scenario's name, and the second half of its check id. |
| `description` | yes | Prose. Say what breaks without it, not what the scenario does. |
| `initial_state` | yes | Object, forwarded verbatim as the machine's starting condition. |
| `steps` | yes | Non-empty. `at_us` never runs backwards. |
| `steps[].expect` | yes | The actions, **in order and exhaustively**. `[]` for a step that must produce nothing. |

The runner reads that shape and nothing else. It does not know what a `node`
is, what a `tick` does, or that `set_timer` has an `in_us` — so adding a
machine, an event or an action is a change to this directory and to adapters,
never to the runner.

### How strictly actions are compared

**Exhaustively.** The actions must be exactly these and no others. A check that
only looked for the actions it wanted would pass an implementation that also
emitted a spurious `sync_lost` in the middle of a show, and a spurious action
is a real defect. `"expect": []` is the strongest thing a behavioural vector
can say, which is why it has to be written out rather than omitted — an absent
list would read as "nobody looked".

**In order.** The sans-IO contract hands the shell a *list* and the shell
executes it in order: two `send`s in one batch leave in the order they were
given, and `role` before the `TICK` that role now owes is the causal order a
shell reads. Ordering is also the rule an implementer can check against without
guessing — "some permutation is acceptable" would need the spec to say which
permutations, and it does not.

The order within one event is: **election effects, then sync effects, then a
sync-state announcement, then exactly one `set_timer`, last.** That ordering is
a decision this suite makes; the state-machine tables in `docs/wire-format.md`
do not imply one.

**But not down to the value, where the spec constrains only a bound.** An
expected value may be a *matcher* instead of a literal: an object whose single
key begins with `$`.

| Matcher | Matches | Used for |
|---|---|---|
| `{"$any": true}` | any value; the field must be present | a field whose presence is the requirement |
| `{"$between": [lo, hi]}` | an integer in `lo..=hi` | timer deadlines, clock corrections |
| `{"$starts_with": "4c01…"}` | a string with that prefix | the identifying bytes of a datagram |

Three, and deliberately no more. The `$` prefix is what keeps a matcher
distinguishable from a payload field — every wire field name is a snake_case
identifier — and `--self-test` rejects an unknown `$name` rather than treating
it as one.

Two matchers do nearly all the work, and each answers a question the task of
writing these vectors forces:

- **`set_timer` is pinned as an upper bound, never as a value.** A timer is
  documented as a hint: waking late is a quality problem, waking early is free.
  An implementation that computes a shorter deadline than another is not less
  conforming, so a vector demanding an exact number would fail conforming
  implementations. Every bound in this corpus is one of the intervals the spec
  actually names — 1 s (the leader's `TICK` interval and the `TICK` period),
  2 s (candidate settling), 3 s (follower timeout), 30 s (resync) — chosen as
  the shortest one that is pending at that step. The lower bound is `1`, not
  `0`: a shell that honoured a zero delay would spin.
- **`send` is pinned by the first ten bytes of the datagram**, which are magic,
  version, type, flags, `mesh_prefix` and `sender_prefix` — "a datagram of this
  type, from this node, on this mesh", together with the exact `to`. The rest
  is left free on purpose. The trailing AEAD tag is zeroes until W14 and
  pinning it would break every implementation the moment real crypto lands; the
  `sequence` numbering has no defined starting value in `docs/wire-format.md`;
  and the payload encoding is pinned exhaustively by the codec vectors already,
  where it belongs.

  Less is lost than it looks. The election epoch a `TICK` carries is pinned by
  the `role` action in the same step, and a `SYNC_REQ`'s `t1` is pinned
  *transitively*: a later step feeds back a `SYNC_RESP` echoing the `t1` the
  vector expects, and an implementation that sent a different one ignores that
  response and diverges immediately.

### Machines, events and actions

Two machines exist so far. The spellings below are the contract, and they are
the ones `lumen-sim`'s exporter already emits, so a recording can become a
vector without a translation layer.

`"machine": "node"` — election and time sync together, decoding datagrams and
emitting them.

```json
{ "uuid": "<32 hex>", "capacity": 1000, "mesh_id": "<32 hex>",
  "boot_counter": 1, "now_us": 0 }
```

| Event | Fields |
|---|---|
| `tick` | — a timer the core asked for has fired |
| `datagram` | `bytes` (lowercase hex) |
| `peer_discovered` / `peer_lost` | `prefix` (8 hex digits) |

| Action | Fields |
|---|---|
| `set_timer` | `in_us` |
| `send` | `to` (`"mesh"` or an 8-hex-digit peer prefix), `datagram` |
| `discipline` | `offset_us` — applied by **slewing the rate**, never stepping |
| `role` | `role` (`"follower"` / `"candidate"` / `"leader"`), `epoch` |
| `sync_lost` / `sync_acquired` | — |

`"machine": "sources"` — one zone's source stack, with admission control.

```json
{ "budget": 100, "max_concurrent": 3 }
```

| Event | Fields |
|---|---|
| `push` | `id`, `zone`, `scene`, `priority`, `expires_at_us` (integer or `null`), `fade_in_ms`, `fade_out_ms`, `cost`. The step's `at_us` is the push time. |
| `pop` | `id` |
| `renew` | `id`, `expires_at_us` |
| `advance` | — the frame tick: expire what has lapsed and finish fades |

| Action | Fields |
|---|---|
| `admitted` | `id` |
| `rejected` | `id`, `cost`, `spare` — refused for budget, and by how much |
| `removed` | `id`, `reason` (`"expired"` / `"popped"` / `"superseded"`) |
| `fade_finished` | `id` |
| `refused` | `reason` (`"no_expiry"` with `priority`, `"already_expired"` with `expires_at_us`, `"no_room"`, `"not_found"`) |

A refusal is a `Result` at the API boundary rather than an emitted action, but
a vector spells it as one so that a step's outcome is a single ordered list.
The alternative — a separate `error` channel per step — would need its own
comparison rule for no gain.

### What is here, and what is deliberately not

The scenarios `docs/wire-format.md` names as worth shipping from the start,
against what exists:

| Scenario | File |
|---|---|
| Cold-start sync and convergence | `sync_cold_start_converges.json` |
| RTT filtering (the half that makes convergence trustworthy) | `sync_discards_a_slow_sample.json` |
| Master vanishing mid-show, re-election | `master_vanishes_mid_show.json` |
| Equal capacity, decided by UUID | `equal_capacity_lower_uuid_wins.json`, `equal_capacity_higher_uuid_loses.json` |
| A leader that must not yield, and one that must | `leader_does_not_yield_to_a_worse_candidate.json`, `leader_yields_after_three_better_ticks.json` |
| `SRC_PUSH` after its own `expires_at` | `source_push_after_its_expiry_is_refused.json` |
| `SRC_PUSH` at priority 200 with no expiry | `source_above_the_ambient_floor_must_expire.json` |
| Source-stack resolution and fallback | `source_stack_falls_back_as_each_source_expires.json` |
| Admission under budget pressure | `source_admission_drops_the_least_important.json` |
| Pop, fade-out, and a pop for something already gone | `source_pop_fades_out_before_it_is_gone.json` |

**Three-way partition and HLC convergence are absent on purpose.** Replication
is W7 and does not exist in any implementation yet, so a vector for it would be
one nothing can pass — which trains people to ignore failures, the one habit a
conformance suite cannot survive. The same goes for channel preemption and
signature rejection: `CHAN_CLAIM` ownership and record signing have codec
vectors but no state machine behind them. They land with the behaviour.

**A behavioural vector drives one machine, not a mesh.** `lumen-sim` exports
recordings in this shape and its exporter carries a seed, a network fault
model and several nodes; those fields are dropped here. An implementation must
never need a PRNG, a latency model or our simulator's scheduler to pass a
vector, and anything that only reproduces under one of those is not a
conformance requirement at all. What survives the translation is exactly the
sans-IO contract: events in, actions out, at stated times.

That has one honest cost. "The master vanishes mid-show with **no visible
render interruption**" is a mesh-level property, and a single node cannot
assert it — from where it stands there is a bounded window between `sync_lost`
and `sync_acquired` during which tightly-synced content is suppressed, which is
the defined degradation rather than a fault. `master_vanishes_mid_show.json`
pins the window's shape: three missed `TICK`s to lose sync, two seconds of
candidacy, then the role and the epoch. Whether a *mesh* covers that window is
a question for a multi-node harness, and it is not one this format can ask.

### Two places the spec is not self-consistent

Written down rather than quietly resolved, because a vector that picks a side
in an argument the prose has not settled is a vector that will be wrong later.

- **The expiry threshold.** `docs/wire-format.md` says a `SRC_PUSH` with
  `priority > 0` and no expiry must be rejected, and `malformed.json` ships
  that rule. But `docs/protocol.md` gives band 0–63 to "ambient / default
  scene", which reads as the whole band being the floor. The behavioural
  vectors here test only priorities where both readings agree: 200 and 64 must
  be refused, 0 must be admitted. **Priorities 1–63 are deliberately not
  pinned** until the prose picks one.
- **`RoleChanged` for a candidate.** The election table has a `Candidate`
  state, but standing for election is not taking or losing the timebase, and a
  shell has nothing to do about it. These vectors expect a `role` action only
  for `follower` and `leader`; a candidacy is announced by the `TICK` it sends
  and by nothing else.

### Adding a behavioural vector

1. New file, named for the behaviour. One scenario per file.
2. `cargo run -p lumen-conformance -- --self-test vectors/` — the schema check
   also validates every matcher, so a mistyped `$between` is caught here rather
   than as a mysterious failure against somebody's adapter.
3. Run it against a real implementation, and then **against a deliberately
   broken one**. A behavioural vector that cannot fail is worse than no vector,
   and the failure is silent: it passes, it looks like coverage, and it is not.
   Breaking one rule in a scratch copy of an implementation and watching the
   right vector go red takes ten minutes and is the only thing that proves the
   vector asserts what its description claims.
