---
paths:
  - "vectors/**"
  - "idl/**"
---

# Vectors and IDL are normative

A change here is a change to what every implementation must do, so it lands
*before* the code that implements it, never after.

## Adding a vector

- **Codec vectors** are bidirectional. Bytes in, compare the parsed structure;
  structure in, compare the bytes. A vector that only tests one direction lets an
  encoder and a decoder drift together.
- **Behavioural vectors** are an event sequence and the actions expected out. They
  work because implementation cores are sans-IO; keep them free of timing, socket
  and ordering assumptions the contract does not actually make.
- One scenario per file, named for the behaviour, not for the bug number.
- A hostile case is just a longer file. Split brain, clock step, keeper death
  mid-write — write them.

## Adding to the IDL

The IDL is normative but code generation is optional: `lumen-proto` stays
hand-written and CI asserts it round-trips every codec vector. So a new message is
**IDL + vectors in the same change**, or the drift check has nothing to check.

Security fields stay in the envelope even while unimplemented. Adding them later
is a breaking change to every implementation that exists by then.
