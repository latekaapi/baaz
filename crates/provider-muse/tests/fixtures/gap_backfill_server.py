#!/usr/bin/env python3
"""A fake `muse serve` for `gap_backfill.rs`.

Not a wire capture, not a real server: a scripted stdin/stdout puppet, run as
the client's child process exactly like the real `muse serve` binary (its
argv, always led by `serve`, is ignored). It exists to drive
`Inner::dispatch`'s gap-buffering path (finding `client-adapter-2`)
deterministically, without spending a turn or depending on a running Muse
install.

Sequence:
  1. Emit `view/gap` for session `s1` — the client starts buffering that
     session and spawns its `view/page` backfill.
  2. Emit two `approval/request` **server requests** for `s1`: one with no
     `viewCursor` (must be released after the page), one whose `viewCursor`
     the page's own event will also carry (must be dropped by the same
     cursor-dedup pass that already covers parked notifications).
  3. Wait for the client's `view/page` request, answer it with one
     `item/completed` notification carrying `viewCursor: "v:1"` and
     `nextCursor: null` (one page, no more).
  4. Keep the pipe open (read and ignore) so the client can shut down
     cleanly.
"""
import json
import sys


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def main():
    send({
        "jsonrpc": "2.0",
        "method": "view/gap",
        "params": {"sessionId": "s1", "after": None},
    })
    send({
        "jsonrpc": "2.0",
        "id": "srv-undeduped",
        "method": "approval/request",
        "params": {"sessionId": "s1", "approvalId": "a-undeduped"},
    })
    send({
        "jsonrpc": "2.0",
        "id": "srv-deduped",
        "method": "approval/request",
        "params": {"sessionId": "s1", "approvalId": "a-deduped", "viewCursor": "v:1"},
    })

    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        if req.get("method") == "view/page":
            send({
                "jsonrpc": "2.0",
                "id": req["id"],
                "result": {
                    "events": [
                        {
                            "method": "item/completed",
                            "params": {
                                "sessionId": "s1",
                                "viewCursor": "v:1",
                                "item": {
                                    "itemId": "i-1",
                                    "kind": "agentMessage",
                                    "turnId": "t-1",
                                    "revision": 1,
                                    "status": "completed",
                                    "text": "paged",
                                },
                            },
                        }
                    ],
                    "nextCursor": None,
                },
            })
            # Keep draining stdin so the client's `shutdown()` (closing
            # stdin, killing the child) does not race a blocked write.


if __name__ == "__main__":
    main()
