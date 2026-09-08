#!/usr/bin/env python3
"""Phase 3 probes.

1. echo: does `turn/start` accept a non-default `reasoningEffort`, and does
   anything on the wire echo it back?
2. echo: does `turn/start` accept an `image` input part?
3. meta (ONE real turn): does the literal text `/plan …` fire the `/plan`
   skill server-side, or is it just text?

Run with `--real` to include probe 3.
"""
import base64, json, os, sys, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import Msp, cid, SCRATCH

ws = os.path.join(SCRATCH, "ws")
os.makedirs(ws, exist_ok=True)
c = Msp(os.path.join(SCRATCH, "transcript-phase3.jsonl"))
r = c.req("initialize", {"clientInfo": {"name": "aui_probe", "title": "AUI Probe", "version": "0.1.0"},
                         "capabilities": {"requestedCapabilities": ["userShell"]}})
assert "result" in r, r
c.note("initialized")

# ---------------------------------------------------------------- 1 and 2: echo
ss = c.req("session/start", {"commandId": cid(), "workspaceRoot": ws, "providerId": "echo"})
sid = ss["result"]["session"]["sessionId"]
print("echo session", sid)

for effort in ["high", "ultra", "none"]:
    ts = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                              "input": [{"type": "text", "text": "effort probe " + effort}],
                              "reasoningEffort": effort})
    print("EFFORT", effort, "->", json.dumps(ts.get("result") or ts.get("error"))[:300])
    c.pump(6, until=lambda m: m.get("method") == "turn/completed")

# a 1x1 transparent PNG
# A real 1x1 transparent PNG (valid CRCs). The first capture used bytes whose
# IDAT CRC was wrong and MSP accepted the part anyway: the wire checks base64
# and mediaType, never the image.
PNG = base64.b64encode(bytes.fromhex(
    "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c4"
    "890000000b49444154789c6360000200000500017a5eab3f0000000049454e44"
    "ae426082")).decode()
ts = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                          "input": [{"type": "text", "text": "image probe"},
                                    {"type": "image", "base64Data": PNG, "mediaType": "image/png",
                                     "width": 1, "height": 1}]})
print("IMAGE ->", json.dumps(ts.get("result") or ts.get("error"))[:400])
c.pump(8, until=lambda m: m.get("method") == "turn/completed")

# does anything echo the effort back? dump every userMessage item and turn/started
for m in c.notes:
    p = m.get("params") or {}
    if m.get("method") in ("item/started", "item/completed") and (p.get("item") or {}).get("kind") == "userMessage":
        print("USERMESSAGE ITEM", json.dumps(p["item"])[:600])
    if m.get("method") == "turn/started":
        print("TURN/STARTED", json.dumps(p)[:400])

# ---------------------------------------------------------------- 3: the /plan skill
if "--real" in sys.argv:
    ss = c.req("session/start", {"commandId": cid(), "workspaceRoot": ws, "providerId": "meta",
                                 "approvalMode": "denyUnmatched"})
    rsid = ss["result"]["session"]["sessionId"]
    print("meta session", rsid)
    PROMPT = "/plan reply with a one-line plan for printing hello"
    ts = c.req("turn/start", {"commandId": cid(), "sessionId": rsid,
                              "input": [{"type": "text", "text": PROMPT}],
                              "displayText": PROMPT, "reasoningEffort": "low"})
    print("turn/start", json.dumps(ts.get("result") or ts.get("error"))[:300])
    seen = []
    t0 = time.time()
    while time.time() - t0 < 240:
        try:
            msg = c.q.get(timeout=1.0)
        except Exception:
            continue
        c.notes.append(msg)
        m, p = msg.get("method"), msg.get("params") or {}
        if m in ("item/started", "item/completed"):
            it = p.get("item") or {}
            seen.append((m, it.get("kind"), it.get("tool"), (it.get("args") or "")[:200],
                         (it.get("text") or "")[:400]))
            print("ITEM", m, it.get("kind"), it.get("tool"), json.dumps(it.get("args"))[:200])
            if it.get("text"):
                print("   text:", it["text"][:400].replace("\n", " | "))
        if m == "turn/completed":
            print("COMPLETED", json.dumps(p)[:400])
            break
    print("\nSUMMARY of items:", json.dumps(seen, indent=1)[:4000])

c.p.terminate()
