#!/usr/bin/env python3
"""Minimal MSP client over `muse serve` stdio. Logs every raw line both ways."""
import json, os, subprocess, sys, threading, time, uuid, queue

SCRATCH = os.path.dirname(os.path.abspath(__file__))

class Msp:
    def __init__(self, logpath, args=None, env=None):
        self.log = open(logpath, "w", buffering=1)
        e = dict(os.environ)
        if env: e.update(env)
        self.p = subprocess.Popen(["muse", "serve"] + (args or []),
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True, bufsize=1, env=e)
        self.q = queue.Queue()
        self.notes = []
        self.pending = {}
        self._n = 0
        threading.Thread(target=self._read, daemon=True).start()
        threading.Thread(target=self._readerr, daemon=True).start()

    def _read(self):
        for line in self.p.stdout:
            line = line.rstrip("\n")
            self.log.write("<-- " + line + "\n")
            try: msg = json.loads(line)
            except Exception: continue
            self.q.put(msg)

    def _readerr(self):
        for line in self.p.stderr:
            self.log.write("!!! stderr: " + line.rstrip("\n") + "\n")

    def send(self, obj):
        s = json.dumps(obj)
        self.log.write("--> " + s + "\n")
        self.p.stdin.write(s + "\n"); self.p.stdin.flush()

    def req(self, method, params=None, timeout=60):
        self._n += 1
        rid = self._n
        m = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None: m["params"] = params
        self.send(m)
        t0 = time.time()
        while time.time() - t0 < timeout:
            try: msg = self.q.get(timeout=timeout)
            except queue.Empty: break
            if msg.get("id") == rid and ("result" in msg or "error" in msg):
                return msg
            self.notes.append(msg)
        raise TimeoutError(method)

    def note(self, method, params=None):
        m = {"jsonrpc": "2.0", "method": method}
        if params is not None: m["params"] = params
        self.send(m)

    def pump(self, seconds, until=None):
        t0 = time.time()
        while time.time() - t0 < seconds:
            try: msg = self.q.get(timeout=0.2)
            except queue.Empty: continue
            self.notes.append(msg)
            if until and until(msg): return True
        return False

def cid():
    """UUIDv7: 48-bit unix ms, version 7, variant 10."""
    import os as _os, time as _t
    ms = int(_t.time() * 1000)
    b = bytearray(_os.urandom(16))
    b[0:6] = ms.to_bytes(6, "big")
    b[6] = (b[6] & 0x0F) | 0x70
    b[8] = (b[8] & 0x3F) | 0x80
    return str(uuid.UUID(bytes=bytes(b)))


def main():
    which = sys.argv[1] if len(sys.argv) > 1 else "echo"
    ws = sys.argv[2] if len(sys.argv) > 2 else os.path.join(SCRATCH, "ws")
    prompt = sys.argv[3] if len(sys.argv) > 3 else "hello from the probe"
    provider = None if which == "echo" else which
    logp = os.path.join(SCRATCH, f"transcript-{which}.jsonl")
    c = Msp(logp)

    r = c.req("initialize", {"clientInfo": {"name": "aui_probe", "title": "AUI Probe", "version": "0.1.0"},
                             "capabilities": {"requestedCapabilities": ["userShell"], "experimentalApi": False}})
    print("initialize ->", json.dumps(r, indent=1)[:1500])
    c.note("initialized")

    ml = c.req("model/list", {})
    print("model/list ->", json.dumps(ml)[:1200])

    sp = {"commandId": cid(), "workspaceRoot": ws, "approvalMode": os.environ.get("AUI_MODE", "onRequest")}
    if provider: sp["providerId"] = provider
    if os.environ.get("AUI_MODEL"): sp["modelId"] = os.environ["AUI_MODEL"]
    ss = c.req("session/start", sp)
    print("session/start ->", json.dumps(ss)[:1500])
    if "error" in ss:
        # retry without providerId
        sp2 = dict(sp); sp2.pop("providerId", None); sp2["commandId"] = cid()
        ss = c.req("session/start", sp2)
        print("retry session/start ->", json.dumps(ss)[:1200])
    sid = ss["result"]["session"]["sessionId"]

    ml2 = c.req("model/list", {"sessionId": sid})
    print("model/list(session) ->", json.dumps(ml2)[:800])

    ts = c.req("turn/start", {"commandId": cid(), "sessionId": sid,
                              "input": [{"type": "text", "text": prompt}],
                              "ifBusy": "queue", "reasoningEffort": os.environ.get("AUI_EFFORT", "low")})
    print("turn/start ->", json.dumps(ts))

    done = c.pump(float(os.environ.get("AUI_WAIT", "45")),
                  until=lambda m: m.get("method") == "turn/completed")
    print("turn completed:", done, "notifications:", len(c.notes))

    # answer any pending approval / user input automatically if asked
    c.req("session/list", {"limit": 5})
    c.req("session/read", {"sessionId": sid, "excludeItems": False})
    c.req("view/page", {"sessionId": sid, "limit": 50, "direction": "forward"})
    c.req("session/setApprovalMode", {"commandId": cid(), "sessionId": sid, "mode": "promptUnmatched"})
    c.pump(2)
    print("log:", logp)
    c.p.terminate()

if __name__ == "__main__":
    main()
