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
