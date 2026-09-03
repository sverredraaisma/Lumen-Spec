# Implementation adapters

An adapter is a small program that speaks the runner's line protocol on
stdin/stdout and forwards each request into the implementation under test.
Adapters live with their implementations, not here. This directory holds the
line-protocol definition and a reference adapter to copy from.

Known adapters:

| Implementation | Adapter | Kinds |
|---|---|---|
| `lumen-device` | `adapters/conformance/` in that repo | behavioural |

The reference adapter in `echo/` is a **fixture**, not an implementation: it
answers from the corpus, so it passes by construction and proves nothing about
anybody's code. It exists to exercise the runner and to be copied from. An
implementation with no adapter of its own is an implementation the suite has
never checked, however many vectors are written for it.

Because the implementation core is sans-IO, an adapter is a loop around a pair
of pure functions and nothing else. No sockets, no threads.

```bash
cargo run -p lumen-conformance -- --adapter "path/to/adapter --flags" vectors/
```

---

## The line protocol

**Revision 2.** One request per line, one response per line, UTF-8,
`\n`-terminated. Text and line-oriented so an adapter can be written in
anything with a standard library, and so a failing exchange can be pasted into
a bug report and replayed by hand.

    runner → adapter    <verb> <json>
    adapter → runner    ok <json>  |  ignore  |  reject <text>  |  error <text>

Rules that hold for both sides:

- Exactly one response per request, in order. The runner blocks on each, so an
  adapter **must flush after every line** — a buffered answer is a deadlock,
  not a slow one.
- Blank lines and lines beginning with `#` are ignored by the reader. That is
  the diagnostic channel; without it every implementer invents one.
- Hex is **lowercase, no separators**. Fixing the spelling is what lets both
  sides compare datagrams as strings, and keeps a failure diff about bytes
  rather than about formatting.
- stderr belongs to the adapter and is passed through to the operator. Log
  there, never on stdout.
- The adapter exits when stdin closes.

### Requests

| Line | Meaning |
|---|---|
| `hello {"protocol":2}` | Always first. |
| `decode {"datagram":"<hex>"}` | Decode this datagram. |
| `encode {"header":{…},"tag":"<hex>","payload":{…}}` | Encode this structure. The body is a vector's `value`, forwarded verbatim. |
| `reset {"machine":"<name>","state":{…}}` | Throw away all state and build `machine` in this starting condition. Sent once before every behavioural vector. |
| `event {"at_us":<n>,"event":{…}}` | Deliver this event at this show time and answer with the actions it produced. |

`machine`, `state` and `event` are forwarded from the vector file **verbatim**.
The runner has no table of machines, events or actions; adding one is a change
to `vectors/` and to adapters, never to the runner.

### Responses

| Line | Meaning |
|---|---|
| `ok <json>` | Success. For `hello`, `{"name":"…","protocol":2,"kinds":[…]}`. For `decode`, the decoded `value`. For `encode`, `{"datagram":"<hex>"}`. For `reset`, `{}`. For `event`, `{"actions":[…]}`. |
| `ignore` | The datagram was dropped **silently**, with no error raised. |
| `reject <text>` | The datagram was refused. The text is for humans; the runner never compares it. |
| `error <text>` | The adapter itself failed. Never a conforming answer to anything, and always a failed check. |

### What an adapter can do: `kinds`

The handshake answer may carry `"kinds"`, listing the vector classes this
adapter runs — `"codec"`, `"behavioural"`, or both. **An adapter that omits it
is taken to run codec vectors only**, which is what every revision-1 adapter
does, so none of them need touching.

Vectors of a kind an adapter does not claim are reported as *skipped*, not as
failures, and counted separately in the tally. A codec-only adapter is a
perfectly good adapter; failing it for a half it never claimed would make the
report useless. Hiding the skips would be worse still.

### Why revision 2 rather than a silent extension

Adding `reset` and `event` without a version would be undetectable. A
revision-1 adapter handed a `reset` answers `error unknown request verb`, and
that is indistinguishable from a real defect in an adapter that meant to
support it. The version plus `kinds` turns the same situation into "this
adapter does codec vectors only", which is a fact rather than a failure.

### Two verbs, not three

There is deliberately no `actions` verb. Delivery and read-back are one
exchange because the actions of one event are known the moment it returns, and
a separate read would cost every adapter a queue to hold them in — state the
sans-IO contract does not otherwise need.

`ignore` and `reject` are not two words for the same thing. An unknown message
type must produce `ignore`; producing `reject` there is the single most
consequential conformance failure in the suite, because it looks healthy and
breaks every future minor version of the protocol. The runner says so by name
when it catches it.

### What the runner asks for

Per codec case: one `decode`. Then, only for a full round-trip case, one
`encode`. Negative and `accept` cases are decode-only — there is nothing to
encode from a datagram that must be refused, and asking would force every
adapter to invent an answer.

Per behavioural vector: one `reset`, then one `event` per step, in order,
stopping at the first step whose actions do not match. Past a divergence the
machine is in a state the vector never described, so whatever the later steps
report is noise. See `vectors/README.md` for both shapes.

### Worked exchange

```
→ hello {"protocol":2}
← ok {"name":"lumen-proto 0.1.0","protocol":2,"kinds":["codec","behavioural"]}
→ decode {"datagram":"4c01110001020304070000004…aa"}
← ok {"header":{"magic":76,…},"tag":"aaaa…","payload":{"t1":1000000}}
→ encode {"header":{"magic":76,…},"tag":"aaaa…","payload":{"t1":1000000}}
← ok {"datagram":"4c01110001020304070000004…aa"}
→ decode {"datagram":"4c01990001020304…"}
← ignore
→ decode {"datagram":"5801110001020304…"}
← reject magic 0x58 is not 0x4c
→ reset {"machine":"node","state":{"uuid":"1111…","capacity":1000,…}}
← ok {}
→ event {"at_us":3000000,"event":{"event":"tick"}}
← ok {"actions":[{"action":"send","to":"mesh","datagram":"4c0110…"},
                 {"action":"set_timer","in_us":1000000}]}
```

### Writing one

Answer `hello` with your implementation's name and the kinds you run. Then, for
whichever of those you claimed:

1. For `decode`: hand the bytes to your codec. Success is `ok` with the decoded
   structure spelled as `vectors/README.md` specifies; an unknown message type
   is `ignore`; anything else that fails is `reject`.
2. For `encode`: build your message type from the JSON and emit the bytes. This
   direction is the one people skip, and skipping it is what lets an encoder and
   a decoder drift together.
3. For `reset`: construct the named state machine from `state`. `error` if you
   do not have that machine — the runner reports it against the vector rather
   than pretending the scenario ran.
4. For `event`: translate the JSON into your core's event type, call
   `on_event(at_us, event)`, and translate the actions back. If your core is
   sans-IO, this is a `match` in each direction and nothing else; if it is not,
   this is the step that tells you so.

Steps 3 and 4 together are perhaps eighty lines in any language. The
translation tables — which event names, which action names, which fields — are
in `vectors/README.md`, and they are part of the spec rather than of the
runner, which is why adding a machine never means touching the runner.

---

## The reference adapter

`adapters/echo/main.rs`, built as `lumen-echo-adapter`.

It answers out of the vector corpus itself — a `decode` is a lookup by
datagram, an `encode` is a lookup by decoded structure, and a behavioural
replay is the corpus's own expectations handed straight back — and **contains
no codec and no state machines**. That is deliberate, and the obvious
alternative is worse.

The behavioural half makes the point unusually plainly. A behavioural
expectation may be a *bound* rather than a value, and the fixture answers with
the smallest thing that satisfies it. An adapter that can do that is obviously
not an implementation of anything.

The obvious alternative is to link `lumen-proto` from `lumen-core` and let the
reference adapter be a real implementation. Two things forbid it. The
dependency would point the wrong way: `lumen-spec` is the repo every other repo
implements, and a spec that builds against one implementation is that
implementation's documentation. And a second codec living here would quietly
become the normative one — when the prose and the code disagreed, everyone
would read the code.

So it is a **fixture for the runner**, not an implementation under test.
Pointed at the corpus it must report a clean run, which is what proves the
runner, the line protocol and the vector loader work end to end. It can never
prove anything about the protocol, and a clean run from it says nothing about
whether the vectors are right.

That second job belongs to `--self-test`, which checks the corpus against its
own schema and against the L1 header with no implementation involved, and to
the real adapters in the implementation repos. `lumen-core` additionally
asserts in its own CI that its hand-written codec round-trips every vector
here, which is the check that actually validates the bytes.

It builds as a target of the `lumen-conformance` package rather than as its own
crate, so the integration tests can spawn it through
`CARGO_BIN_EXE_lumen-echo-adapter`. A separate crate would need a path
dependency back to the runner for the same code and still would not hand the
tests a binary path.

```bash
# what CI runs to prove the suite is wired up
cargo test -p lumen-conformance
cargo run -p lumen-conformance -- \
    --adapter "target/debug/lumen-echo-adapter --vectors vectors" vectors/
```
