# lumen-spec

The protocol specification, the wire IDL and the conformance suite for the Lumen
ARGB mesh. **This repo is the product**; every other repo implements what is here.

- **Licence:** Apache-2.0 for code, CC-BY 4.0 for prose and vectors.
- **Main branch:** `main`
- **Status:** codec and behavioural vectors are in, and the runner drives both.
  Replication (W7) behaviour has no vectors yet — see `vectors/README.md`.

## Stack

- Prose: Markdown in `docs/`
- IDL: `idl/lumen.idl` — normative, but code generation is optional
- Runner: Rust 1.85+, one binary (`lumen-conformance`) driving adapters over
  stdin/stdout

## Commands

```bash
cargo test --workspace
cargo run -p lumen-conformance -- --adapter "<cmd>" vectors/
cargo run -p lumen-conformance -- --self-test vectors/       # schema-check the corpus
cargo clippy --workspace --all-targets       # CI runs with -D warnings
cargo fmt --all -- --check
cargo llvm-cov --workspace --summary-only    # coverage; must be >= 95%
```

## Layout

| Path | Contents |
|---|---|
| `docs/` | the normative prose |
| `idl/` | the wire IDL |
| `vectors/codec/` | bytes ↔ structure, both directions |
| `vectors/behavioural/` | event sequence → expected actions, one scenario per file |
| `runner/` | the one shared runner |
| `adapters/` | the line protocol, and a reference adapter |

The reference adapter is a **fixture**, not an implementation: it answers from
the corpus and holds no codec, because a second codec here would become the de
facto normative one and `lumen-spec` must not depend on `lumen-core`.
`adapters/README.md` explains the trade.

## Hard rules

- **Spec-first.** A protocol change lands here — IDL *plus* vectors — before any
  implementation. The vectors are what turn "has that repo caught up yet" into a
  CI answer instead of a memory exercise.
- **One shared runner, never one per implementation.** Divergent runners produce
  divergent notions of passing, which is the exact failure the suite exists to
  prevent. Adding an implementation means writing an adapter, never touching the
  runner.
- **Adding a vector must not require touching the runner.** If it does, the runner
  has grown a special case that belongs in the data.
- **Security fields stay in the envelope** even while unimplemented. Adding them
  later is a breaking change to every implementation that exists by then.
- **Coverage floor is 95%** for the runner. It is the thing that decides whether
  everything else is correct.

## Gotchas

> Living section. Add anything that cost real time.

- **The "cannot link / no local coverage" note used to be wrong; both now work.**
  `link.exe` was never missing. What was missing was the **Windows SDK**, so the
  linker had no `kernel32.lib` to link against and Rust reported that as
  "linker `link.exe` not found". Adding the SDK component to the existing VS
  2022 install fixed the MSVC toolchain and `cargo llvm-cov` together. If a
  fresh machine shows this symptom, install the C++ workload rather than
  switching to `windows-gnu`: that workaround builds, which is why nobody
  revisits it, and it silently costs you coverage because the `windows-gnu`
  toolchain ships no profiler runtime.
- **A codec vector that only tests one direction is nearly worthless** — an
  encoder and a decoder happily drift together. Always assert both.

## Specialized guides (loaded on demand — do not preload)

- Vector and IDL conventions: `.claude/rules/vectors.md` (auto-loads on those paths)
- Normative prose: `docs/protocol.md`, `docs/wire-format.md` — long; read the
  section you need
- Project-wide rules and the licence boundary: `CONTRIBUTING.md`

## Compact instructions

Preserve spec decisions, message-format changes, vector files added, and file
paths touched. Drop raw build and test output.
