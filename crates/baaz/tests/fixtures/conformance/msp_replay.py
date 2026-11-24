#!/usr/bin/env python3
"""A fake `muse serve` that replays a RECORDED msp fixture, for the provider
conformance suite (`crates/baaz/tests/conformance/replay.rs`).

Not a wire capture and not a real server: a stdin/stdout stand-in run as the
hub's child exactly like the real binary, so the fixture travels the real
`provider_muse::client` reader, the hub's routing, the tap and the fold. No model is
ever reached and no turn is ever spent.

    msp_replay.py --replay FIXTURE [serve and the client's own flags…]

Answers come from the provider-muse puppet's `canned` table
(`crates/provider-muse/tests/fixtures/model_list_puppet.py`), so the
handshake and every pull answer as they do in the provider tests. What
replays is the server half of FIXTURE (`<--` lines), re-keyed from the
fixture's first session onto the session the client asked for:

* `session/start` (or `session/resume`) answers, then plays everything the
  fixture said before its first `turn/started`;
* each `turn/start` answers with the next recorded turn's id, then plays
  that turn: from its first `turn/started` up to the next turn's;
* a recorded server request (`approval/request`, `userInput/request`) is
  written and the replay waits for its answer (`approval/decide`,
  `userInput/answer`) before playing on; `approval/listPending` lists the
  approvals still waiting, so the client can read their stage tokens; a
  second decision for the same approval is refused `approvalAlreadyResolved`;
* a recorded `view/page` result plays as the notifications it carries,
  less those whose cursor already played (the client drops those);
  recorded responses are dropped (this stand-in answers for itself).
"""

import importlib.util
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
PUPPET = os.path.join(HERE, "../../../../provider-muse/tests/fixtures/model_list_puppet.py")
spec = importlib.util.spec_from_file_location("model_list_puppet", PUPPET)
puppet = importlib.util.module_from_spec(spec)
spec.loader.exec_module(puppet)


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def load(path):
    """The fixture's server items, as (kind, frame-text) in order, plus the
    first session id it names."""
    items, primary, cursors = [], None, set()
    for line in open(path):
        line = line.rstrip("\n")
        if not line.startswith("<-- "):
            continue
        body = line[4:]
        try:
            frame = json.loads(body)
        except ValueError:
            continue
        if "method" not in frame:
            result = frame.get("result")
            if isinstance(result, dict) and isinstance(result.get("events"), list):
                for event in result["events"]:
                    if isinstance(event, dict) and "method" in event:
                        # A page answers with events the client may hold
                        # already: it drops those by cursor, so they never
                        # play twice.
                        cursor = (event.get("params") or {}).get("viewCursor")
                        if cursor is not None and cursor in cursors:
                            continue
                        cursors.add(cursor)
                        note = {"jsonrpc": "2.0", "method": event["method"],
                                "params": event.get("params")}
                        items.append(("note", json.dumps(note)))
            continue
        kind = "request" if "id" in frame else "note"
        cursors.add((frame.get("params") or {}).get("viewCursor"))
        items.append((kind, body))
    for _, text in items:
        params = json.loads(text).get("params") or {}
        session = params.get("sessionId") or (params.get("session") or {}).get("sessionId")
        if session:
            primary = session
            break
    return items, primary


def turn_started(text, primary):
    frame = json.loads(text)
    params = frame.get("params") or {}
    return frame.get("method") == "turn/started" and params.get("sessionId") == primary


class Replay:
    def __init__(self, path):
        items, self.primary = load(path)
        self.prelude, self.turns = [], []
        seen = set()
        for kind, text in items:
            if self.primary and turn_started(text, self.primary) \
                    and json.loads(text)["params"].get("turnId") not in seen:
                turn_id = json.loads(text)["params"].get("turnId")
                seen.add(turn_id)
                self.turns.append((turn_id, [(kind, text)]))
            elif self.turns:
                self.turns[-1][1].append((kind, text))
            else:
                self.prelude.append((kind, text))
        self.session = None
        self.queue = []
        self.waiting = None
        self.pending = {}
        self.decided = set()

    def rekey(self, text):
        if self.primary and self.session:
            text = text.replace(self.primary, self.session)
        return text

    def play(self, items):
        self.queue.extend(items)
        self.pump()

    def pump(self):
        while self.queue and self.waiting is None:
            kind, text = self.queue.pop(0)
            frame = json.loads(self.rekey(text))
            send(frame)
            if kind == "request":
                params = frame.get("params") or {}
                key = params.get("approvalId") or params.get("userInputId") or frame.get("id")
                if frame.get("method") == "approval/request":
                    self.pending[key] = params
                self.waiting = key

    def resume(self, key):
        if self.waiting is not None and key == self.waiting:
            self.waiting = None
            self.pump()


def main():
    path = None
    args = sys.argv[1:]
    for at, arg in enumerate(args):
        if arg == "--replay" and at + 1 < len(args):
            path = args[at + 1]
    replay = Replay(path)
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        method = req.get("method")
        req_id = req.get("id")
        params = req.get("params") if isinstance(req.get("params"), dict) else {}
        if method is None:
            # A response to a server request.
            replay.resume(req_id)
            continue
        if req_id is None:
            continue  # `initialized` and other notifications.
        if method == "approval/decide":
            approval = params.get("approvalId")
            if approval in replay.decided or approval not in replay.pending:
                send({"jsonrpc": "2.0", "id": req_id, "error": {
                    "code": -32051, "message": "approval %s is already resolved" % approval,
                    "data": {"kind": "approvalAlreadyResolved", "retryable": False}}})
                continue
            replay.decided.add(approval)
            replay.pending.pop(approval, None)
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "approvalId": approval, "commandId": params.get("commandId", "cmd-1"),
                "status": "accepted", "terminal": True}})
            replay.resume(approval)
            continue
        if method in ("userInput/answer", "userInput/cancel", "userInput/clarify"):
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "commandId": params.get("commandId", "cmd-1"), "status": "accepted",
                "userInputId": params.get("userInputId")}})
            replay.resume(params.get("userInputId"))
            continue
        if method == "approval/listPending":
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "approvals": list(replay.pending.values()), "userInputs": []}})
            continue
        if method == "turn/start" and replay.turns:
            turn_id, items = replay.turns.pop(0)
            send({"jsonrpc": "2.0", "id": req_id, "result": {
                "commandId": params.get("commandId", "cmd-1"), "disposition": "started",
                "startedNewTurn": True, "status": "accepted", "turnId": turn_id}})
            replay.play(items)
            continue
        payload = puppet.canned(method, params)
        if payload is None:
            send({"jsonrpc": "2.0", "id": req_id,
                  "error": {"code": -32601, "message": "fake server knows no method " + str(method)}})
            continue
        send({"jsonrpc": "2.0", "id": req_id, "result": payload})
        if method in ("session/start", "session/resume") and replay.session is None:
            replay.session = params.get("sessionId") or payload["session"]["sessionId"]
            replay.play(replay.prelude)


if __name__ == "__main__":
    main()
