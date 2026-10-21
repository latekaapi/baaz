#!/usr/bin/env python3
"""A stub `muse serve` for the mode-pick tests: logs every request line to the
last argv entry and answers what a mode pick, a resume and a fork need.

Flags (argv): fail-mode refuses `session/setApprovalMode`; fail-resume refuses a
profile-carrying `session/resume` (unless allow-read-only and the profile is
`:read-only`); mode=<m> and profile=<p> set the resumed session's stored mode
and profile.
"""
import sys, json
log = open(sys.argv[-1], "a", buffering=1)
args = sys.argv[1:-1]
flag = lambda name: name in args
opt = lambda name, default: next((a.split("=", 1)[1] for a in args if a.startswith(name + "=")), default)
mode = opt("mode", "onRequest")
profile = opt("profile", ":auto-review")

def session(sid, mode, profile):
    return {"sessionId": sid, "path": "", "status": "idle", "activeTurnId": None,
            "createdAt": "2026-10-16T00:00:00Z", "updatedAt": "2026-10-16T00:00:00Z",
            "workspaceRoot": None, "providerId": None, "modelId": None, "turnCount": 1, "forkedFrom": None,
            "approvalMode": {"mode": mode, "source": "startup", "lastCommandId": None},
            "permissionState": {"profileId": profile}}

def rejected(rid, reason):
    return {"jsonrpc": "2.0", "id": rid, "error": {"code": -32030, "message": "commandRejected: " + reason,
            "data": {"kind": "commandRejected", "reason": reason}}}

for line in sys.stdin:
    line = line.strip()
    if not line:
        continue
    try:
        req = json.loads(line)
    except Exception:
        continue
    log.write(line + "\n")
    rid = req.get("id")
    if rid is None:
        continue
    method = req.get("method")
    params = req.get("params") or {}
    if method == "initialize":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"experimentalApi": False, "grantedCapabilities": [],
                "museHome": "/tmp/stub", "platformFamily": "unix", "platformOs": "macos",
                "schema": {"fingerprint": "stub", "version": 1},
                "serverInfo": {"name": "stub-muse", "version": "1.4.11"}, "userAgent": "stub"}}
    elif method == "session/resume" and params.get("permissionProfile") and flag("fail-resume") \
            and not (params.get("permissionProfile") == ":read-only" and flag("allow-read-only")):
        resp = rejected(rid, "permission_profile_unavailable")
    elif method == "session/resume":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"session": session(params.get("sessionId"), mode, profile),
                "viewCursor": "v:1", "history": {"mode": "none", "items": None, "noneReason": "excluded"},
                "pendingRequests": []}}
    elif method == "session/fork":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"session": session("s-fork", "allowAll", ":read-only"),
                "viewCursor": "v:1", "history": {"mode": "none", "items": None, "noneReason": "excluded"},
                "pendingRequests": []}}
    elif method == "session/setApprovalMode" and flag("fail-mode"):
        resp = rejected(rid, "policy_rejected")
    elif method == "session/setApprovalMode":
        resp = {"jsonrpc": "2.0", "id": rid, "result": {"commandId": params.get("commandId"), "status": "accepted",
                "applyOutcome": "completed", "effectiveMode": {"mode": params.get("mode"),
                "source": "approvalReconfigure", "lastCommandId": params.get("commandId")}}}
    else:
        resp = {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "stub knows no " + str(method)}}
    sys.stdout.write(json.dumps(resp) + "\n")
    sys.stdout.flush()
