#!/usr/bin/env python3
"""Drives the agent's browser tools against a real Chromium page (docs/27 §6, R2).

Runs the check bundle offline on a local page with a console error, an uncaught
error, a failing request and a Pay button, then calls the tools over Baaz's own
socket, the way mcp-bridge does. No provider, no model, no turn. Skips where there
is no check bundle.

    scripts/baaz-check.sh build && python3 scripts/agent-live-check.py
"""
import http.server
import json
import os
import pathlib
import shutil
import signal
import socket
import socketserver
import subprocess
import sys
import tempfile
import threading
import time

ROOT = pathlib.Path(__file__).resolve().parent.parent
BIN = ROOT / "target/check/Baaz.app/Contents/MacOS/Baaz"
SESSION = "fixture-live"

PAGE = b"""<!doctype html><title>Checkout</title><meta name=viewport content="width=device-width">
<h1>Your cart</h1>
<label>Name <input id=name aria-label="Name"></label>
<button id=next onclick="document.querySelector('h1').textContent='Step two';console.log('went to step two')">Next</button>
<button id=pay onclick="window.paid=true">Pay 97</button>
<p id=out></p>
<script>
console.error('checkout: price missing');
setTimeout(function () { throw new TypeError('cart is not a function'); }, 0);
fetch('/missing');
</script>"""


class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        ok = self.path in ("/", "/checkout")
        self.send_response(200 if ok else 404)
        self.send_header("Content-Type", "text/html")
        self.end_headers()
        self.wfile.write(PAGE if ok else b"nope")

    def log_message(self, *_):
        pass


FAILED = []


def check(name, ok, detail=""):
    print(("ok    " if ok else "FAIL  ") + name + ("" if ok else f"   {detail}"))
    if not ok:
        FAILED.append(name)


class Tools:
    def __init__(self, path):
        self.path = path
        self.next = 1

    def send(self, tool, **params):
        request = {"id": self.next, "session": SESSION, "tool": tool, "params": params}
        self.next += 1
        conn = socket.socket(socket.AF_UNIX)
        conn.connect(self.path)
        conn.sendall((json.dumps(request) + "\n").encode())
        return conn

    @staticmethod
    def receive(conn, timeout):
        conn.settimeout(timeout)
        data = b""
        while not data.endswith(b"\n"):
            chunk = conn.recv(1 << 20)
            if not chunk:
                break
            data += chunk
        return json.loads(data)

    def call(self, tool, timeout=40, **params):
        reply = self.receive(self.send(tool, **params), timeout)
        if not reply.get("ok"):
            raise RuntimeError(f"{tool}: {reply.get('error')}")
        return reply["result"]


def main():
    if not BIN.exists():
        print("SKIP: no check bundle (scripts/baaz-check.sh build)")
        return 0
    server = socketserver.ThreadingTCPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_address[1]}/"
    state = tempfile.mkdtemp(prefix="baaz-agent-live.")
    env = dict(os.environ, BAAZ_STATE_DIR=state)
    app = subprocess.Popen(
        [str(BIN), "--workspace", str(ROOT), "--no-connect", "--login", "signed-in", "--steps", "fixture-session:Live"],
        env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        log = pathlib.Path(state) / "logs/baaz.log"
        path = None
        for _ in range(200):
            if log.exists():
                for line in log.read_text().splitlines():
                    if "terminal service socket: " in line:
                        path = line.split("terminal service socket: ", 1)[1].strip()
            if path:
                break
            time.sleep(0.1)
        check("the socket is up", bool(path))
        tools = Tools(path)
        for _ in range(100):
            try:
                tools.call("browser_tabs", timeout=5, op="list")
                break
            except (RuntimeError, OSError):
                time.sleep(0.2)
        opened = None
        for _ in range(60):
            try:
                opened = tools.call("browser_navigate", url=url, timeout=40)
                break
            except RuntimeError as error:
                if not any(word in str(error) for word in ("not ready", "no browser", "unknown session")):
                    raise
                time.sleep(0.5)
        if opened is None:
            print(log.read_text()[-1500:])
        check("navigate loads the page", opened and opened.get("title") == "Checkout", opened)

        snap = tools.call("browser_snapshot")["snapshot"]
        print(snap)
        refs = {}
        for line in snap.splitlines():
            for name in ("Next", "Pay 97", "Name"):
                if f'"{name}"' in line and "[ref=" in line:
                    refs[name] = line.split("[ref=")[1].split("]")[0]
        check("the snapshot names the controls", {"Next", "Pay 97", "Name"} <= set(refs), snap)

        tools.call("browser_wait", ms=500)
        lines = tools.call("browser_console")["lines"]
        check("the console has the page's error", any("price missing" in l["text"] for l in lines), lines)
        errors = tools.call("browser_errors")["errors"]
        check("the uncaught error has its place", any("cart is not a function" in e["text"] for e in errors), errors)
        failed = tools.call("browser_network", failed_only=True)["requests"]
        check("the failed request is listed", any(r["url"].endswith("/missing") and r["status"] == 404 for r in failed), failed)

        filled = tools.call("browser_act", action="fill", ref=refs["Name"], text="Bharani")
        check("fill answers", "effect" in filled, filled)
        value = tools.call("browser_run", code="return document.querySelector('#name').value")
        check("trusted typing landed in the field", "Bharani" in value["result"], value)

        acted = tools.call("browser_act", action="click", ref=refs["Next"])
        check("a click changes the page and says so", "Step two" in acted["changes"], acted)
        lines = tools.call("browser_console", since="last_action")["lines"]
        check("since last_action shows only what the click caused",
              [l["text"] for l in lines] == ["went to step two"], lines)

        conn = tools.send("browser_act", action="click", ref=refs["Pay 97"])
        stopped = False
        try:
            Tools.receive(conn, 4)
        except socket.timeout:
            stopped = True
        check("Pay waits for the person", stopped)
        paid = tools.call("browser_run", code="return String(window.paid)")
        check("and nothing was clicked", "undefined" in paid["result"], paid)

        shot = tools.call("browser_screenshot")
        check("a screenshot comes back", len(shot.get("image_base64", "")) > 1000)
        kept = pathlib.Path(shot.get("path", ""))
        check(
            "the screenshot is kept as a PNG file the transcript can open",
            kept.is_file() and kept.read_bytes()[:8] == b"\x89PNG\r\n\x1a\n",
        )
        tools.call("browser_emulate", device="phone")
        width = tools.call("browser_run", code="return innerWidth")
        check("the phone viewport applies", "390" in width["result"], width)
        tools.call("browser_emulate", reset=True)

        second = tools.call("browser_tabs", op="open", url=url + "checkout")
        check("a second tab opens", "tab" in second, second)
        time.sleep(1.5)
        listed = tools.call("browser_tabs", op="list")["tabs"]
        check("both tabs are listed", len(listed) >= 2, listed)
        title = tools.call("browser_read", tab=second["tab"])
        check("a background tab reads by id", title.get("title") == "Checkout", title)
    finally:
        app.send_signal(signal.SIGTERM)
        try:
            app.wait(timeout=12)
        except subprocess.TimeoutExpired:
            app.kill()
            app.wait()
        subprocess.run([str(BIN), "--sweep-clones"], stdout=subprocess.DEVNULL, check=False)
        shutil.rmtree(state, ignore_errors=True)
    print("agent-live-check: " + ("FAILED " + ", ".join(FAILED) if FAILED else "passed"))
    return 1 if FAILED else 0


if __name__ == "__main__":
    sys.exit(main())
