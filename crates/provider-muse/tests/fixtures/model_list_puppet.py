#!/usr/bin/env python3
"""A fake `muse serve` for `roundtrip.rs`.

Not a wire capture, not a real server: a scripted stdin/stdout puppet, run
as the client's child process exactly like the real binary (its argv, always
led by `serve`, is ignored). It answers `initialize` and `model/list` with
canned results and refuses everything else, so the test proves the adapter
routed its command to the right `muse-client` call: any other method comes
back an error, and only `model/list` yields the catalog the test asserts on.
No prompt is ever run and no turn is ever spent.
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


def main():
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
        req_id = req.get("id")
        if method == "initialize":
            result(req_id, {
                "experimentalApi": False,
                "grantedCapabilities": [],
                "museHome": "/tmp/fake-muse-home",
                "platformFamily": "unix",
                "platformOs": "macos",
                "schema": {"fingerprint": "fake", "version": 1},
                "serverInfo": {"name": "fake-muse", "version": "0.0.0-test"},
                "userAgent": "fake/0.0.0-test",
            })
        elif method == "model/list":
            result(req_id, {
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
            })
        else:
            failure(req_id, -32601, "fake server knows no method " + str(method))


if __name__ == "__main__":
    main()
