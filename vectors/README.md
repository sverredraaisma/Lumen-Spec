# Conformance vectors

Two classes, both plain data files so that adding a case never means touching
the runner.

| Class | What it checks | How |
|---|---|---|
| `codec/` | framing and message encode/decode | feed bytes, compare parsed structure; feed structure, compare bytes |
| `behavioural/` | sync, election, replication, source-stack resolution, arbitration | feed an event sequence, compare the emitted actions |

The behavioural class is the valuable half.

**Write the vector when you write the behaviour.** Retrofitting a suite across
seven repos is miserable.

---

## Codec vectors

One file per message type, named after it in lower case — `tick.json`,
`src_push.json` — plus `framing.json` for the L1 header and `malformed.json`
for the negative cases. 26 files, 97 cases at the time of writing.

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

### Adding a vector

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
