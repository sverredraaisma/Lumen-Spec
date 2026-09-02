# Wire IDL

**The IDL is normative; generating code from it is optional.**

`lumen-proto` in `lumen-core` stays hand-written Rust. CI there asserts it
round-trips every vector in `../vectors/codec/`. That captures most of the value
of code generation for almost none of the cost — the vectors catch drift, which
is the actual failure mode, while hand-written code stays readable and needs no
generator to maintain.

If a second or third implementation appears and the codecs start disagreeing,
generation becomes worth building, and the IDL will already be here.
