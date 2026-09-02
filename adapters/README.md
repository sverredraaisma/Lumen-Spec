# Implementation adapters

An adapter is a small program that speaks the runner's line protocol on
stdin/stdout and forwards each request into the implementation under test.
Adapters live with their implementations, not here. This directory holds the
line-protocol definition and a reference adapter to copy from.

Because the implementation core is sans-IO, an adapter is a loop around a pair
of pure functions and nothing else. No sockets, no threads.

```bash
cargo run -p lumen-conformance -- --adapter "path/to/adapter --flags" vectors/
```

---

## The line protocol

**One request per line, one response per line, UTF-8, `\n`-terminated.** Text
and line-oriented so an adapter can be written in anything with a standard
library, and so a failing exchange can be pasted into a bug report and replayed
by hand.

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
| `hello {"protocol":1}` | Always first. |
| `decode {"datagram":"<hex>"}` | Decode this datagram. |
| `encode {"header":{…},"tag":"<hex>","payload":{…}}` | Encode this structure. The body is a vector's `value`, forwarded verbatim. |

### Responses

| Line | Meaning |
|---|---|
| `ok <json>` | Success. For `hello`, `{"name":"…","protocol":1}` — the name appears at the top of the report. For `decode`, the decoded `value`. For `encode`, `{"datagram":"<hex>"}`. |
| `ignore` | The datagram was dropped **silently**, with no error raised. |
| `reject <text>` | The datagram was refused. The text is for humans; the runner never compares it. |
| `error <text>` | The adapter itself failed. Never a conforming answer to anything, and always a failed check. |

`ignore` and `reject` are not two words for the same thing. An unknown message
type must produce `ignore`; producing `reject` there is the single most
consequential conformance failure in the suite, because it looks healthy and
breaks every future minor version of the protocol. The runner says so by name
when it catches it.

### What the runner asks for

Per vector case: one `decode`. Then, only for a full round-trip case, one
`encode`. Negative and `accept` cases are decode-only — there is nothing to
encode from a datagram that must be refused, and asking would force every
adapter to invent an answer. See `vectors/README.md` for the case shape.

### Worked exchange

```
→ hello {"protocol":1}
← ok {"name":"lumen-proto 0.1.0","protocol":1}
→ decode {"datagram":"4c01110001020304070000004…aa"}
← ok {"header":{"magic":76,…},"tag":"aaaa…","payload":{"t1":1000000}}
→ encode {"header":{"magic":76,…},"tag":"aaaa…","payload":{"t1":1000000}}
← ok {"datagram":"4c01110001020304070000004…aa"}
→ decode {"datagram":"4c01990001020304…"}
← ignore
→ decode {"datagram":"5801110001020304…"}
← reject magic 0x58 is not 0x4c
```

### Writing one

Three things, in this order, and the third is where the work is:

1. Answer `hello` with your implementation's name.
2. For `decode`: hand the bytes to your codec. Success is `ok` with the decoded
   structure spelled as `vectors/README.md` specifies; an unknown message type
   is `ignore`; anything else that fails is `reject`.
3. For `encode`: build your message type from the JSON and emit the bytes. This
   direction is the one people skip, and skipping it is what lets an encoder and
   a decoder drift together.

---

## The reference adapter

`adapters/echo/main.rs`, built as `lumen-echo-adapter`.

It answers out of the vector corpus itself — a `decode` is a lookup by
datagram, an `encode` is a lookup by decoded structure — and **contains no
codec**. That is deliberate, and the obvious alternative is worse.

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
