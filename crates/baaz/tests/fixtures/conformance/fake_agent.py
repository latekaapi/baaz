#!/usr/bin/env python3
"""A fake `claude` / `codex app-server` for the provider conformance suite.

Not a wire capture and not a real agent: a stdin/stdout puppet that replays
a RECORDED fixture (`fixtures/claude-code/*.jsonl`, `fixtures/codex/*.jsonl`,
or a hand-written one beside this file) to whatever the adapter asks, run as
the adapter's child process exactly like the real binary. No model is ever
reached and no turn is ever spent (docs/71 §4).

    fake_agent.py --dialect codex|claude --state DIR --plan SPEC [--plan SPEC…] [child args…]

Each session child takes the next `--plan` (the last one repeats), as
counted in `DIR/spawns`. A codex child becomes a session child at its first
`model/list`, `thread/start` or `thread/resume`: until then it answers as a
signed-in account, so Baaz's status probe (`initialize`, `account/read`,
`account/rateLimits/read`) never uses up a plan in a live check. A claude
child is a session child from its spawn. A SPEC is
`FIXTURE[@SEGMENT][#MODE]`:

* SEGMENT (default 0): codex — which `initialize` session of the fixture to
  play (a resume fixture records two); claude — which recorded turn the
  first user message plays (later messages play the turns after it).
* MODE: `play` (default); `crash-at-request` — exit 1 with stderr lines the
  moment the first server request (an approval) is on the wire, never
  answering it; `crash-at-start` — the same right after the turn starts;
  `die` — write stderr and exit 1 before reading anything (a child that
  cannot start).

Every client frame is appended to `DIR/frames.log` as `<spawn> <json>`, so a
test can read what the adapter sent.

The codex replay is driven by the client: a request is answered with the
fixture's recorded result for that method (the next unconsumed one), after
emitting whatever the server sent before it; after an answer the server's
notifications play on until the next recorded answer. A server request (an
approval) is written and the replay waits for the client's response to it.
Methods the fixture never answered get a minimal fallback.
"""

import json
import os
import queue
import sys
import threading

STDERR_LINES = [
    "fake-agent: loading config from the conformance fixture",
    "fake-agent: the model stream closed unexpectedly (simulated)",
    "fake-agent: exiting with status 1",
]


def crash():
    for line in STDERR_LINES:
        sys.stderr.write(line + "\n")
    sys.stderr.flush()
    sys.stdout.flush()
    os._exit(1)


def write(frame):
    sys.stdout.write(json.dumps(frame) + "\n")
    sys.stdout.flush()


def parse_args(argv):
    dialect, state, plans, rest = None, None, [], []
    i = 0
    while i < len(argv):
        arg = argv[i]
        if arg == "--dialect":
            dialect = argv[i + 1]
            i += 2
        elif arg == "--state":
            state = argv[i + 1]
            i += 2
        elif arg == "--plan":
            plans.append(argv[i + 1])
            i += 2
        else:
            rest.append(arg)
            i += 1
    return dialect, state, plans, rest


def parse_spec(spec):
    mode = "play"
    if "#" in spec:
        spec, mode = spec.split("#", 1)
    segment = 0
    if "@" in spec:
        spec, seg = spec.rsplit("@", 1)
        segment = int(seg)
    return spec, segment, mode


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


def current_spawn(state):
    """The index the next session child will take (the reader thread logs
    under it before the main thread counts it)."""
    try:
        with open(os.path.join(state, "spawns")) as handle:
            return int(handle.read().strip() or "0")
    except (OSError, ValueError):
        return 0


class Client:
    """Client frames off stdin, on a thread, logged as they arrive."""

    def __init__(self, state):
        self.frames = queue.Queue()
        self.log = open(os.path.join(state, "frames.log"), "a", buffering=1)
        self.state = state
        # The session-child index the frames are logged under (`-1` before
        # a codex child becomes one).
        self.spawn = -1
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
            if self.spawn < 0 and frame.get("method") in CODEX_SESSION_METHODS:
                self.spawn = current_spawn(self.state)
            self.log.write("%d %s\n" % (self.spawn, json.dumps(frame)))
            self.frames.put(frame)
        self.frames.put(None)

    def next(self):
        frame = self.frames.get()
        if frame is None:
            sys.exit(0)
        return frame


# ------------------------------------------------------------------ codex

CODEX_FALLBACK = {
    "model/list": {"data": [{"id": "gpt-test", "displayName": "GPT Test", "isDefault": True}],
                   "nextCursor": None},
    "account/rateLimits/read": {"rateLimits": None},
    "account/read": {"account": {"type": "chatgpt", "email": "fake@example.invalid", "planType": "pro"},
                     "requiresOpenaiAuth": True},
}

# The requests that make a codex child a session child (see the header).
CODEX_SESSION_METHODS = ("model/list", "thread/start", "thread/resume")


def codex_serve(client, state, plans):
    """Answer as a bare signed-in server until the child is a session child,
    then replay that session's plan from its first session request on."""
    while True:
        frame = client.next()
        if "method" not in frame or "id" not in frame:
            continue
        if frame["method"] not in CODEX_SESSION_METHODS:
            write({"id": frame["id"], "result": CODEX_FALLBACK.get(frame["method"], {})})
            continue
        spawn = next_spawn(state)
        fixture, segment, mode = parse_spec(plans[min(spawn, len(plans) - 1)])
        if mode == "die":
            crash()
        segments = codex_segments(fixture)
        replay = CodexReplay(client, segments[min(segment, len(segments) - 1)], mode)
        # The handshake was answered already: it is not replayed again.
        for at, (kind, name, _) in enumerate(replay.items):
            if kind == "response" and name == "initialize":
                replay.consumed[at] = True
        replay.request(frame)
        replay.serve()


def codex_segments(path):
    segments, current = [], None
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        envelope = json.loads(line)
        frame, direction = envelope["frame"], envelope["_dir"]
        if direction == "client->server" and frame.get("method") == "initialize":
            current = []
            segments.append(current)
        if current is None:
            current = []
            segments.append(current)
        current.append((direction, frame))
    return segments


class CodexReplay:
    def __init__(self, client, segment, mode):
        self.client, self.mode = client, mode
        methods = {}
        for direction, frame in segment:
            if direction == "client->server" and "method" in frame and "id" in frame:
                methods[json.dumps(frame["id"])] = frame["method"]
        # Server items: ("response", method, frame) | ("request", id, frame)
        # | ("note", method, frame).
        self.items = []
        for direction, frame in segment:
            if direction != "server->client":
                continue
            if "method" in frame and "id" in frame:
                self.items.append(["request", frame["id"], frame])
            elif "method" in frame:
                self.items.append(["note", frame["method"], frame])
            else:
                method = methods.get(json.dumps(frame.get("id")))
                self.items.append(["response", method, frame])
        self.consumed = [False] * len(self.items)
        self.cursor = 0

    def answer(self, request, at):
        frame = dict(self.items[at][2])
        self.consumed[at] = True
        frame["id"] = request["id"]
        write(frame)

    def fallback(self, request):
        write({"id": request["id"], "result": CODEX_FALLBACK.get(request["method"], {})})

    def find(self, method, start, end):
        for at in range(start, end):
            kind, name, _ = self.items[at]
            if kind == "response" and name == method and not self.consumed[at]:
                return at
        return None

    def request(self, request, advance=True):
        method = request["method"]
        ahead = self.find(method, self.cursor, len(self.items)) if advance else None
        if ahead is not None:
            # Everything the server said before this answer goes first.
            while self.cursor < ahead:
                self.step(skip_responses=True)
            self.answer(request, ahead)
            self.cursor = ahead + 1
            self.run_on()
            return
        behind = self.find(method, 0, self.cursor)
        if behind is not None:
            self.answer(request, behind)
        else:
            self.fallback(request)

    def step(self, skip_responses):
        kind, name, frame = self.items[self.cursor]
        if kind == "response":
            if not skip_responses:
                return False
            self.cursor += 1
            return True
        self.cursor += 1
        write(frame)
        if kind == "note" and name == "turn/started" and self.mode == "crash-at-start":
            crash()
        if kind == "request":
            if self.mode == "crash-at-request":
                crash()
            self.wait_for(name)
        return True

    def run_on(self):
        while self.cursor < len(self.items):
            if not self.step(skip_responses=False):
                return

    def wait_for(self, request_id):
        while True:
            frame = self.client.next()
            if "method" in frame and "id" in frame:
                self.request(frame, advance=False)
            elif "method" not in frame and frame.get("id") == request_id:
                return

    def serve(self):
        while True:
            frame = self.client.next()
            if "method" in frame and "id" in frame:
                self.request(frame)


# ----------------------------------------------------------------- claude

def claude_turns(path):
    turns, current = [], []
    saw_host_user = False
    for line in open(path):
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        frame = json.loads(line)
        if "_dir" in frame:
            if frame["frame"].get("type") == "user":
                if saw_host_user or current:
                    turns.append(current)
                    current = []
                saw_host_user = True
            continue
        # The CLI's answers to the RECORDED host's control requests: this
        # fake answers the live adapter's own requests itself.
        if frame.get("type") == "control_response":
            continue
        current.append(frame)
    turns.append(current)
    return turns


def claude_session_id(args):
    for flag in ("--session-id", "--resume"):
        if flag in args:
            at = args.index(flag)
            if at + 1 < len(args):
                return args[at + 1]
    return None


def claude_answer_control(request):
    subtype = (request.get("request") or {}).get("subtype")
    response = {}
    if subtype == "initialize":
        response = {"commands": [], "models": [
            {"value": "sonnet", "displayName": "Sonnet", "description": "Fake Sonnet"}]}
    write({"type": "control_response", "response": {
        "subtype": "success", "request_id": request.get("request_id"), "response": response}})


def claude_serve(client, turns, first, mode, session_id):
    index = first
    while True:
        frame = client.next()
        if frame.get("type") == "control_request":
            claude_answer_control(frame)
            continue
        if frame.get("type") != "user":
            continue
        turn = turns[min(index, len(turns) - 1)]
        index += 1
        for out in turn:
            out = dict(out)
            if session_id and "session_id" in out:
                out["session_id"] = session_id
            write(out)
            if mode == "crash-at-start" and out.get("type") == "assistant":
                crash()
            if out.get("type") == "control_request" and \
                    (out.get("request") or {}).get("subtype") == "can_use_tool":
                if mode == "crash-at-request":
                    crash()
                wanted = out.get("request_id")
                while True:
                    answer = client.next()
                    if answer.get("type") == "control_request":
                        claude_answer_control(answer)
                    elif answer.get("type") == "control_response" and \
                            (answer.get("response") or {}).get("request_id") == wanted:
                        break


def main():
    dialect, state, plans, rest = parse_args(sys.argv[1:])
    if "--version" in rest:
        print("codex-cli 0.200.0" if dialect == "codex" else "2.1.300 (Claude Code)")
        return
    if dialect == "codex":
        codex_serve(Client(state), state, plans)
        return
    spawn = next_spawn(state)
    fixture, segment, mode = parse_spec(plans[min(spawn, len(plans) - 1)])
    if mode == "die":
        crash()
    client = Client(state)
    client.spawn = spawn
    claude_serve(client, claude_turns(fixture), segment, mode, claude_session_id(rest))


if __name__ == "__main__":
    main()
