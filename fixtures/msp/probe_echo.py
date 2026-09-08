#!/usr/bin/env python3
"""Try to get an echo-provider session over the wire."""
import json, os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import Msp, cid, SCRATCH

ws = os.path.join(SCRATCH, "ws")
c = Msp(os.path.join(SCRATCH, "transcript-echo-try.jsonl"))
print(json.dumps(c.req("initialize", {"clientInfo": {"name": "aui_probe", "version": "0.1.0"}}))[:200])
c.note("initialized")
for attempt in [{"providerId": "echo"},
                {"providerId": "echo", "modelId": "echo"},
                {"modelId": "echo"},
                {"providerId": "fake"}]:
    p = dict(attempt); p["commandId"] = cid(); p["workspaceRoot"] = ws
    r = c.req("session/start", p)
    print(json.dumps(attempt), "->", json.dumps(r.get("error") or r["result"]["session"])[:300])
    if "result" in r:
        sid = r["result"]["session"]["sessionId"]
        print("  model/list:", json.dumps(c.req("model/list", {"sessionId": sid}))[:300])
        t = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                                 "input": [{"type": "text", "text": "say hi"}]})
        print("  turn:", json.dumps(t)[:200])
        c.pump(12, until=lambda m: m.get("method") == "turn/completed")
        for n in c.notes:
            if n.get("method") in ("item/delta", "item/completed", "turn/completed"):
                print("   ", json.dumps(n)[:260])
        break
c.p.terminate()
