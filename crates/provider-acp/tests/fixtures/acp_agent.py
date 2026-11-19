#!/usr/bin/env python3
"""A stand-in ACP agent for provider-acp's tests and the conformance suite.

Not an agent and not a wire capture: a stdin/stdout puppet that plays a
HAND-WRITTEN ACP recording (`fixtures/acp/*.jsonl`) to whatever the adapter
asks, run as the adapter's child process exactly like a real agent. No model
is ever reached (docs/82 5.7).

    acp_agent.py --state DIR --plan SPEC [--plan SPEC...]

Each spawn takes the next `--plan` (the last one repeats), counted in
`DIR/spawns`. A SPEC is `FIXTURE[#MODE]`, MODE one of:

* `play` (default);
* `crash-at-request` — exit 1 with stderr lines the moment the first agent
  request (a permission) is on the wire, never answering it;
* `crash-at-start` — the same at the first update after a prompt;
* `die` — write stderr and exit 1 before reading anything.

A recording is a script of envelopes, `{"_dir": "client->agent" |
"agent->client", "frame": ...}`, plus `{"_dir": "agent->client", "exit": 1,
"stderr": [...]}` for an agent that dies there. The play is driven by the
client: a client request moves the script to the next recorded request of
the same method, and the agent's frames after it play until the next
recorded client frame — a request (wait for the client's next one), the
answer to a permission request just written (wait for the client's answer),
or a notification such as `session/cancel` (wait until the client sends it).
A recorded answer goes out under the live request's id. A request the script
has no answer for gets a minimal fallback. Every outgoing `sessionId` is the
one the client last named, so a revived child serves the session the client
re-opens.

Every client frame is appended to `DIR/frames.log` as `<spawn> <json>`.
"""

import json
import os
import queue
import sys
import threading

STDERR_LINES = [
    "acp-agent: loading the conformance fixture",
    "acp-agent: the model stream closed unexpectedly (simulated)",
    "acp-agent: exiting with status 1",
]

FALLBACK = {
    "initialize": {"protocolVersion": 1,
                   "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": False}},
                   "agentInfo": {"name": "acp-stand-in", "version": "0.0.1"}, "authMethods": []},
    "session/load": None,
    "session/prompt": {"stopReason": "end_turn"},
}


def die(lines=None):
    for line in lines or STDERR_LINES:
        sys.stderr.write(line + "\n")
    sys.stderr.flush()
    sys.stdout.flush()
    os._exit(1)


def write(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def parse_args(argv):
    state, plans, i = None, [], 0
    while i < len(argv):
        if argv[i] == "--state":
            state = argv[i + 1]
            i += 2
        elif argv[i] == "--plan":
            plans.append(argv[i + 1])
            i += 2
        else:
            i += 1
    return state, plans


def next_spawn(state):
    path = os.path.join(state, "spawns")
    try:
        with open(path) as handle:
            count = int(handle.read().strip() or "0")
    except (OSError, ValueError):
        count = 0
    with open(path, "w") as handle:
        handle.write(str(count + 1))
    return count


class Client:
    """Client frames off stdin, on a thread, logged as they arrive."""

    def __init__(self, state, spawn):
        self.frames = queue.Queue()
        self.log = open(os.path.join(state, "frames.log"), "a", buffering=1)
        self.spawn = spawn
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in sys.stdin:
            line = line.strip()
            if not line:
                continue
            try:
                frame = json.loads(line)
            except ValueError:
                continue
            self.log.write("%d %s\n" % (self.spawn, json.dumps(frame)))
            self.frames.put(frame)
        self.frames.put(None)

    def next(self):
        frame = self.frames.get()
        if frame is None:
            sys.exit(0)
        return frame


def load(path):
    script = []
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        script.append(json.loads(line))
    return script


class Play:
    def __init__(self, client, script, mode):
        self.client, self.script, self.mode = client, script, mode
        self.cursor = 0
        self.session = None
        # Recorded request id -> the live id its answer goes out under.
        self.live_ids = {}
        self.prompted = False

    def name_session(self, frame):
        params = frame.get("params")
        if isinstance(params, dict) and params.get("sessionId"):
            self.session = params["sessionId"]

    def out(self, frame):
        frame = json.loads(json.dumps(frame))
        params = frame.get("params")
        if self.session and isinstance(params, dict) and "sessionId" in params:
            params["sessionId"] = self.session
        if "method" not in frame and "id" in frame:
            frame["id"] = self.live_ids.pop(json.dumps(frame["id"]), frame["id"])
        write(frame)

    def find(self, method):
        for at in range(self.cursor, len(self.script)):
            envelope = self.script[at]
            frame = envelope.get("frame") or {}
            if envelope.get("_dir") == "client->agent" and frame.get("method") == method and "id" in frame:
                return at
        return None

    def request(self, frame):
        """A live client request: answer it from the script."""
        self.name_session(frame)
        method = frame["method"]
        at = self.find(method)
        if at is None:
            if method in FALLBACK:
                write({"jsonrpc": "2.0", "id": frame["id"], "result": FALLBACK[method]})
            else:
                write({"jsonrpc": "2.0", "id": frame["id"],
                       "error": {"code": -32601, "message": "the stand-in has no %s" % method}})
            return
        self.live_ids[json.dumps(self.script[at]["frame"]["id"])] = frame["id"]
        self.cursor = at + 1
        if method == "session/prompt":
            self.prompted = True
        self.play_on()

    def play_on(self):
        """Play the agent's frames until the next recorded client frame."""
        while self.cursor < len(self.script):
            envelope = self.script[self.cursor]
            frame = envelope.get("frame") or {}
            if envelope.get("_dir") == "client->agent":
                if "method" in frame and "id" in frame:
                    return
                self.cursor += 1
                if "method" in frame:
                    self.wait_for_notification(frame["method"])
                # A recorded answer was already awaited after its request.
                continue
            self.cursor += 1
            if "exit" in envelope:
                die(envelope.get("stderr"))
            if "method" in frame and "id" in frame:
                self.out(frame)
                if self.mode == "crash-at-request":
                    die()
                # Wait for the client's answer when the script records one;
                # a script that goes on without one (the agent dies with
                # its request waiting) does not wait.
                if self.answer_recorded():
                    self.wait_for_answer(frame["id"])
                continue
            if "method" in frame and self.prompted and self.mode == "crash-at-start":
                die()
            self.out(frame)

    def answer_recorded(self):
        if self.cursor >= len(self.script):
            return True
        nxt = self.script[self.cursor]
        return nxt.get("_dir") == "client->agent" and "method" not in (nxt.get("frame") or {})

    def wait_for_answer(self, request_id):
        while True:
            frame = self.client.next()
            if "method" not in frame and frame.get("id") == request_id:
                # Skip the recorded answer: the live one stands in for it.
                if self.cursor < len(self.script):
                    nxt = self.script[self.cursor]
                    if nxt.get("_dir") == "client->agent" and "method" not in (nxt.get("frame") or {}):
                        self.cursor += 1
                return
            if "method" in frame and "id" in frame:
                self.fallback(frame)
            elif frame.get("method") == "session/cancel":
                # A cancel while a request waits: the client answers it
                # `cancelled` next; keep waiting for that answer.
                continue

    def wait_for_notification(self, method):
        while True:
            frame = self.client.next()
            if frame.get("method") == method and "id" not in frame:
                return
            if "method" in frame and "id" in frame:
                self.fallback(frame)

    def fallback(self, frame):
        method = frame["method"]
        result = FALLBACK.get(method, {})
        write({"jsonrpc": "2.0", "id": frame["id"], "result": result})

    def serve(self):
        while True:
            frame = self.client.next()
            if "method" in frame and "id" in frame:
                self.request(frame)


def main():
    state, plans = parse_args(sys.argv[1:])
    spawn = next_spawn(state)
    spec = plans[min(spawn, len(plans) - 1)]
    mode = "play"
    if "#" in spec:
        spec, mode = spec.split("#", 1)
    if mode == "die":
        die()
    Play(Client(state, spawn), load(spec), mode).serve()


if __name__ == "__main__":
    main()
