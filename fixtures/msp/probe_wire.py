#!/usr/bin/env python3
"""Free (echo-provider) exercise of the rest of the command plane:
userShell, queue/unqueue, interrupt, steer, setModel, compact, fork, list,
read, view/page, and a couple of deliberate error probes."""
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import Msp, cid, SCRATCH

ws = os.path.join(SCRATCH, "ws")
c = Msp(os.path.join(SCRATCH, "transcript-wire.jsonl"))
def show(tag, r): print(f"--- {tag}: " + json.dumps(r.get("error") or r.get("result"))[:700])

r = c.req("initialize", {"clientInfo": {"name": "aui_probe", "version": "0.1.0"},
                         "capabilities": {"requestedCapabilities": ["userShell"]}})
c.note("initialized")

ss = c.req("session/start", {"commandId": cid(), "workspaceRoot": ws,
                            "providerId": "echo", "approvalMode": "denyUnmatched"})
sid = ss["result"]["session"]["sessionId"]
print("session", sid)

# --- userShell (capability-gated, outside a turn)
show("userShell", c.req("session/userShell", {"commandId": cid(), "sessionId": sid,
                                              "commandText": "echo hi && ls"}))
c.pump(6)
# --- a shell that policy should not like
show("userShell danger", c.req("session/userShell", {"commandId": cid(), "sessionId": sid,
                                                     "commandText": "curl https://example.com"}))
c.pump(8)

# --- queue two turns then unqueue the second
t1 = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                          "input": [{"type": "text", "text": "one"}]})
t2 = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                          "input": [{"type": "text", "text": "two"}], "ifBusy": "queue"})
show("turn1", t1); show("turn2", t2)
show("unqueue t2", c.req("turn/unqueue", {"commandId": cid(), "sessionId": sid,
                                          "turnId": t2["result"]["turnId"]}))
c.pump(6)

# --- steer against a finished turn (expect an error we can document)
show("steer stale", c.req("turn/steer", {"commandId": cid(), "sessionId": sid,
                                         "expectedTurnId": t1["result"]["turnId"],
                                         "input": [{"type": "text", "text": "steered"}]}))
# --- interrupt with retract when idle
show("interrupt idle", c.req("turn/interrupt", {"commandId": cid(), "sessionId": sid, "retract": True}))
# --- setModel / compact / fork / list / read / page
show("setModel", c.req("session/setModel", {"commandId": cid(), "sessionId": sid,
                                            "model": {"modelId": "muse-spark-1.2", "providerId": "meta"}}))
show("compact", c.req("session/compact", {"commandId": cid(), "sessionId": sid}))
c.pump(4)
fk = c.req("session/fork", {"commandId": cid(), "sessionId": sid})
show("fork", fk)
show("list", c.req("session/list", {"limit": 3}))
show("read", c.req("session/read", {"sessionId": sid, "excludeItems": True}))
show("page", c.req("view/page", {"sessionId": sid, "limit": 5, "direction": "backward"}))
show("listPending", c.req("approval/listPending", {"sessionId": sid}))
show("unsubscribe", c.req("view/unsubscribe", {"sessionId": sid}))
# --- deliberate errors
show("bad session", c.req("session/read", {"sessionId": "00000000-0000-7000-8000-000000000000"}))
show("bad method", c.req("nope/nope", {}))
show("bad effort", c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                                        "input": [{"type": "text", "text": "x"}],
                                        "reasoningEffort": "max"}))
show("resume", c.req("session/resume", {"commandId": cid(), "sessionId": sid, "history": "auto"}))
c.pump(3)
print("notification kinds:", sorted({n.get("method") for n in c.notes if n.get("method")}))
c.p.terminate()
