import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import Msp, cid, SCRATCH
ws = os.path.join(SCRATCH, "ws")
c = Msp(os.path.join(SCRATCH, "transcript-approve.jsonl"))
c.req("initialize", {"clientInfo": {"name":"aui_probe","version":"0.1.0"},
                     "capabilities": {"requestedCapabilities":["userShell"]}})
c.note("initialized")
ss = c.req("session/start", {"commandId": cid(), "workspaceRoot": ws,
                             "providerId": "echo", "approvalMode": "promptUnmatched"})
sid = ss["result"]["session"]["sessionId"]
c.req("session/userShell", {"commandId": cid(), "sessionId": sid, "commandText": "echo hi && ls"})
seen=set(); t0=time.time(); stages=0
while time.time()-t0 < 40:
    try: m=c.q.get(timeout=1)
    except Exception: continue
    c.notes.append(m); meth=m.get("method"); p=m.get("params") or {}
    if meth in ("approval/requested","approval/request","approval/updated"):
        key=(p["approvalId"], json.dumps(p["currentRequirementId"]))
        print("##", meth, "REQ" if "id" in m else "note", "stage", p["currentRequirementId"])
        print("  choices:", json.dumps(p["availableChoices"]))
        if "change" in p: print("  change:", json.dumps(p["change"]))
        if key in seen: continue
        seen.add(key)
        ch = p["availableChoices"][0]
        r = c.req("approval/decide", {"commandId": cid(), "sessionId": sid,
                                      "approvalId": p["approvalId"], "choiceId": ch["choiceId"],
                                      "requirementId": p["currentRequirementId"]})
        print("  decide", ch["choiceId"], "->", json.dumps(r.get("result") or r.get("error")))
        stages+=1
    elif meth=="approval/resolved":
        print("## approval/resolved", json.dumps(p)[:500])
    elif meth in ("item/started","item/completed") and p["item"]["kind"]=="userShell":
        print("##", meth, json.dumps(p["item"])[:500])
        if meth=="item/completed": break
# stale-requirement + already-resolved error probes
if seen:
    aid, req = list(seen)[0][0], json.loads(list(seen)[0][1])
    print("stale decide ->", json.dumps(c.req("approval/decide", {"commandId": cid(), "sessionId": sid,
        "approvalId": aid, "choiceId": "allow_once", "requirementId": req}).get("error")))
    print("bad choice ->", json.dumps(c.req("approval/decide", {"commandId": cid(), "sessionId": sid,
        "approvalId": aid, "choiceId": "nope", "requirementId": req}).get("error")))
c.p.terminate()
