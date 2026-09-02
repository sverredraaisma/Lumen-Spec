# lumen-spec

The protocol specification, the wire IDL and the conformance suite for the Lumen
ARGB mesh. **This repo is the product**; everything else in the project is an
implementation of what is written here.

Licensed permissively on purpose — a spec nobody may freely implement is not a
standard. Prose and vectors are CC-BY 4.0, the runner and any code here are
Apache-2.0. See `LICENSE-APACHE` and `LICENSE-CC-BY`.

## Layout

| Path | Contents |
|---|---|
| `docs/` | the normative prose spec — framing, discovery, sync, election, replication, source stack |
| `idl/` | the wire IDL. **Normative**; generating code from it is optional |
| `vectors/codec/` | encode/decode vectors: bytes in → structure, structure in → bytes |
| `vectors/behavioural/` | event sequences in → expected actions out |
| `runner/` | the one shared conformance runner (`lumen-conformance`) |
| `adapters/` | notes and examples for writing an implementation adapter |

## Conformance model

The runner is a binary plus data-file vectors. It drives an **implementation
adapter** over a line protocol on stdin/stdout, so an adapter can be written in
any language in an afternoon and adding an implementation never means touching
the runner.

Behavioural conformance is possible at all because every implementation's core
is sans-IO — `on_event(now, ev) -> Vec<Action>`. Conformance is literally "given
these events, did you emit these actions": no sockets, no timing, no flakiness.
A three-way split brain is just a longer vector file.

```bash
cargo run -p lumen-conformance -- --adapter "path/to/adapter" vectors/
cargo run -p lumen-conformance -- --self-test vectors/   # check the corpus itself
```

The codec vectors are in place: 26 files, 97 cases, every assigned message type
plus the L1 header and a `malformed.json` of negative cases. Each well-formed
case is checked **both ways** — bytes to structure and structure back to the
same bytes — because a one-directional vector lets an encoder and a decoder
drift together. `vectors/README.md` has the schema; `adapters/README.md` has the
line protocol.

## Changing the protocol

**Spec-first.** A protocol change lands here — IDL plus new conformance vectors —
and only then in `lumen-core` and its dependents. The vectors are what make
"has that repo caught up yet" a CI answer instead of a memory exercise.

A bug reproduced in the simulator (`lumen-device`) should be exported as a vector
here, so every implementation inherits the regression test.
