#!/usr/bin/env python3
"""ONE real-provider turn that triggers a shell tool call needing approval and
then a user-input question. Logs every raw line both directions."""
import json, os, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from probe import Msp, cid, SCRATCH

ws = os.path.join(SCRATCH, "ws")
c = Msp(os.path.join(SCRATCH, "transcript-real.jsonl"))

r = c.req("initialize", {"clientInfo": {"name": "aui_probe", "title": "AUI Probe", "version": "0.1.0"},
                         "capabilities": {"requestedCapabilities": ["userShell"]}})
assert "result" in r, r
c.note("initialized")

ss = c.req("session/start", {"commandId": cid(), "workspaceRoot": ws,
                            "approvalMode": "onRequest", "providerId": "meta"})
sid = ss["result"]["session"]["sessionId"]
print("session", sid, json.dumps(ss["result"]["session"]))

PROMPT = ("Run the shell command `ls` in the workspace, then use your question tool to "
          "ask me which of the listed files I want described. Do not do anything else.")
ts = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                          "input": [{"type": "text", "text": PROMPT}],
                          "reasoningEffort": "low"})
print("turn/start", json.dumps(ts["result"]))

decided_approvals, answered_inputs = set(), set()
t0 = time.time()
completed = False
while time.time() - t0 < 240 and not completed:
    try:
        msg = c.q.get(timeout=1.0)
    except Exception:
        continue
    c.notes.append(msg)
    m = msg.get("method")
    p = msg.get("params") or {}

    # a server->client REQUEST carries an id; answer it so nothing stalls
    if m and "id" in msg:
        print("SERVER REQUEST", m, json.dumps(msg)[:400])

    if m in ("approval/requested", "approval/request") and p.get("approvalId") not in decided_approvals:
        aid = p["approvalId"]; decided_approvals.add(aid)
        print("APPROVAL", json.dumps(p)[:900])
        choices = p["availableChoices"]
        pick = next((ch for ch in choices if ch["decision"] == "approved"), choices[0])
        d = c.req("approval/decide", {"commandId": cid(), "sessionId": sid, "approvalId": aid,
                                      "choiceId": pick["choiceId"],
                                      "requirementId": p["currentRequirementId"]})
        print("decide ->", json.dumps(d))
    elif m in ("userInput/requested", "userInput/request") and p.get("userInputId") not in answered_inputs:
        uid = p["userInputId"]; answered_inputs.add(uid)
        print("USERINPUT", json.dumps(p)[:1200])
        answers = []
        for q in p["questions"]:
            sel = q["selection"]["mode"]
            if q["options"]:
                if sel == "multiple":
                    answers.append({"questionId": q["id"], "selectedLabels": [q["options"][0]["label"]]})
                else:
                    answers.append({"questionId": q["id"], "selectedLabel": q["options"][0]["label"]})
            else:
                answers.append({"questionId": q["id"], "freeText": "README.md"})
        a = c.req("userInput/answer", {"commandId": cid(), "sessionId": sid,
                                       "userInputId": uid, "answers": answers})
        print("answer ->", json.dumps(a))
    elif m == "turn/completed":
        print("TURN COMPLETED", json.dumps(p)[:600])
        completed = True

c.pump(3)
print("notifications:", len(c.notes))
print("kinds:", sorted({n.get("method") for n in c.notes if n.get("method")}))
c.p.terminate()
