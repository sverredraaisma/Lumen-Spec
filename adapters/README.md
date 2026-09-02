# Implementation adapters

An adapter is a small program that speaks the runner's line protocol on
stdin/stdout and forwards each line into the implementation under test.

    > event  {"at_us": 0, "kind": "Tick"}
    < action {"kind": "SendFrame", ...}
    < ok

Because the implementation core is sans-IO, an adapter is a loop around
`on_event(now, ev) -> Vec<Action>` and nothing else. No sockets, no threads.

Adapters live with their implementations, not here. This directory holds the
line-protocol definition and a reference adapter to copy from.
