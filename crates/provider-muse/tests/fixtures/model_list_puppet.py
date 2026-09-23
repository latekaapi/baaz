#!/usr/bin/env python3
"""A fake `muse serve` for the `provider-muse` tests.

Not a wire capture, not a real server: a scripted stdin/stdout puppet, run
as the client's child process exactly like the real binary (its argv, always
led by `serve`, is ignored). It accepts ANY method: it records every
`{method, params}` it receives and answers with a minimally valid result for
that method, so each `Command` arm can prove it called the right
`muse-client` method with the right params. Unknown methods still come back
`-32601`, exactly as before. No prompt is ever run and no turn is ever
spent.

Recording is an append-to-file, not a summary on exit: the puppet appends
one JSON object per incoming request and flushes it BEFORE writing the
response, so by the time a client's `send` returns (it waits for the
matching response), the record is already durable on disk and the test can
read exactly one new line per command. A summary on exit would race the
child's teardown instead. Recording is off unless `--record PATH` is given,
so the original `roundtrip.rs` (which passes no flags) behaves exactly as
before.

`--emit-bad-tap` makes the puppet send one malformed `approval/request`
server request right after `initialize`: real enough to travel the reader
thread, the event pump and the fold, malformed enough that the tap decode
must fail. The adapter has to surface that as a visible error, not silence.
"""

import json
import sys


def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n")
    sys.stdout.flush()


def result(req_id, payload):
    send({"jsonrpc": "2.0", "id": req_id, "result": payload})


def failure(req_id, code, message):
    send({"jsonrpc": "2.0", "id": req_id,
          "error": {"code": code, "message": message}})


def fake_session(session_id="s-1", title="Fake Session"):
    return {
        "activeTurnId": None,
        "createdAt": "2026-09-22T00:00:00Z",
        "path": "",
        "sessionId": session_id,
        "status": "idle",
        "title": title,
        "turnCount": 0,
        "updatedAt": "2026-09-22T00:00:00Z",
    }


def fake_history():
    return {"items": None, "mode": "none", "noneReason": "excluded"}


def fake_approval():
    return {
        "approvalId": "a-1",
        "availableChoices": [
            {"choiceId": "c-yes", "decision": "approved", "label": "Yes", "scope": "once"},
        ],
        "currentRequirementId": {"approvalId": "a-1", "sourceIndex": 3},
        "itemId": "i-9",
        "judgeEscalated": False,
        "protectedWrite": False,
        "rawArgs": "{}",
        "sessionId": "s-1",
        "sourceRange": {
            "first": {"id": "r-1", "sequence": 1},
            "last": {"id": "r-1", "sequence": 1},
            "stream": {"id": "run", "kind": "run"},
        },
        "subject": {"command": "rm -rf /tmp/x", "kind": "shell"},
        "taskId": "task-1",
        "toolCallId": "call-1",
        "toolName": "shell",
        "turnId": "t-1",
        "viewCursor": "v:1",
    }


def canned(method, params):
    """A minimally valid result for `method`, or None when unknown."""
    command_id = (params or {}).get("commandId", "cmd-1")
    session = fake_session()
    if method == "initialize":
        return {
            "experimentalApi": False,
            "grantedCapabilities": [],
            "museHome": "/tmp/fake-muse-home",
            "platformFamily": "unix",
            "platformOs": "macos",
            "schema": {"fingerprint": "fake", "version": 1},
            "serverInfo": {"name": "fake-muse", "version": "0.0.0-test"},
            "userAgent": "fake/0.0.0-test",
        }
    if method == "session/start":
        return {"session": session, "viewCursor": "v:0"}
    if method in ("session/resume", "session/fork", "session/read"):
        return {
            "history": fake_history(),
            "pendingRequests": [],
            "session": session,
            "viewCursor": "v:0",
        }
    if method == "session/list":
        return {"nextCursor": None, "sessions": [session]}
    if method in ("session/compact", "session/setModel", "session/userShell"):
        return {"commandId": command_id, "status": "accepted"}
    if method == "session/setApprovalMode":
        return {
            "applyOutcome": "completed",
            "commandId": command_id,
            "effectiveMode": {"mode": "onRequest", "source": "approvalReconfigure"},
            "status": "accepted",
        }
    if method == "turn/start":
        return {
            "commandId": command_id,
            "disposition": "started",
            "startedNewTurn": True,
            "status": "accepted",
            "turnId": "t-1",
        }
    if method in ("turn/steer", "turn/interrupt", "turn/cancel", "turn/unqueue"):
        return {"commandId": command_id, "status": "accepted", "turnId": "t-2"}
    if method == "model/list":
        return {
            "models": [{
                "displayLabel": "Fake Pro",
                "isActive": False,
                "isDefault": True,
                "modelId": "fake-pro",
                "providerId": "fake",
            }],
            "profileId": None,
            "providerId": "fake",
            "source": "fakeCatalog",
        }
    if method == "approval/decide":
        return {
            "approvalId": "a-1",
            "commandId": command_id,
            "status": "accepted",
            "terminal": True,
        }
    if method == "approval/listPending":
        return {"approvals": [fake_approval()], "userInputs": []}
    if method in ("userInput/answer", "userInput/cancel", "userInput/clarify"):
        return {"commandId": command_id, "status": "accepted", "userInputId": "q-1"}
    if method == "view/page":
        return {"events": [], "nextCursor": "v:2"}
    if method == "view/subscribe":
        return {"viewCursor": "v:9"}
    if method == "view/unsubscribe":
        return {}
    if method == "item/readOutput":
        return {
            "byteLen": 5,
            "content": "hello",
            "encoding": "utf8",
            "eof": True,
            "mediaType": "text/plain",
            "offsetBytes": 0,
        }
    if method == "account/read":
        return {
            "credentialRequired": True,
            "label": "fake@example.com",
            "state": "apiKey",
        }
    if method == "account/loginStart":
        return {
            "userCode": "CODE-1",
            "verificationUrl": "https://example.test/verify",
        }
    if method == "account/loginCancel":
        return {"cancelled": True}
    if method == "account/logout":
        return {"credentialRequired": False, "state": "loggedOut"}
    return None


def main():
    record_path = None
    emit_bad_tap = False
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--record" and i + 1 < len(args):
            record_path = args[i + 1]
            i += 2
        elif args[i].startswith("--record="):
            record_path = args[i][len("--record="):]
            i += 1
        elif args[i] == "--emit-bad-tap":
            emit_bad_tap = True
            i += 1
        else:
            i += 1  # `serve` and the client's own flags: ignored.

    record = None
    if record_path is not None:
        record = open(record_path, "a", buffering=1)

    initialized = False
    for line in sys.stdin:
        line = line.strip()
        if not line:
            continue
        try:
            req = json.loads(line)
        except ValueError:
            continue
        method = req.get("method")
        if method is None:
            continue  # Notification (`initialized`): nothing to answer.
        params = req.get("params")
        if record is not None:
            # Flushed BEFORE the response below, so a client that waits for
            # the matching response can rely on this record being on disk.
            record.write(json.dumps({"method": method, "params": params}) + "\n")
        req_id = req.get("id")
        if req_id is None:
            continue  # A notification: recorded, never answered.
        payload = canned(method, params if isinstance(params, dict) else {})
        if payload is None:
            failure(req_id, -32601, "fake server knows no method " + str(method))
        else:
            result(req_id, payload)
        if method == "initialize" and not initialized:
            initialized = True
            if emit_bad_tap:
                # First an unknown server request: the tap must stay silent
                # on it (no tap, no error), so the test can tell the next
                # frame is the one that has to be loud.
                send({
                    "jsonrpc": "2.0",
                    "id": "srv-unknown-1",
                    "method": "frobnicate/request",
                    "params": {"sessionId": "s-1"},
                })
                # Then a server request the tap cannot decode: it carries a
                # session so it is never parked, but none of the approval
                # fields, so `tap_for` must fail on it loudly.
                send({
                    "jsonrpc": "2.0",
                    "id": "srv-bad-1",
                    "method": "approval/request",
                    "params": {"sessionId": "s-1", "note": "not an approval"},
                })


if __name__ == "__main__":
    main()
