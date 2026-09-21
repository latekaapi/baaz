#!/usr/bin/env python3
"""uiprobe/1 adapter for baaz — a shim, not new capability.

Everything here already existed as a baaz flag. This only translates relay's
contract onto it and normalises the output to the contract's JSON.

Entries are named screens. All of them run offline (`--no-connect`), so a probe
run costs nothing and reaches no model.

    uiprobe.py --entry login-choose --probe-shot out.png
    uiprobe.py --entry login-choose --probe-idle 2000
"""
import argparse, json, os, subprocess, sys, tempfile

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BAAZ = os.path.join(REPO, "target", "debug", "baaz")

# Two kinds of entry, because baaz measures them with different flags.
#   shot: a login-screen state, booted offline. Free, ~2s, no running turn.
#   idle: a transcript capture driven through `--bench`, which already reports
#         frames drawn at rest. Free, ~4s.
# An entry that declares only one kind cannot answer the other verb, and says
# so rather than pretending: relay records that as unverified, never as passed.
ENTRIES = {
    "login-choose":         {"shot": ["--no-connect", "--login", "choose"]},
    "login-apikey":         {"shot": ["--no-connect", "--login", "apikey"]},
    "login-apikey-error":   {"shot": ["--no-connect", "--login", "apikey-error"]},
    "login-device":         {"shot": ["--no-connect", "--login", "device"]},
    "login-validating":     {"shot": ["--no-connect", "--login", "validating"]},
    "login-error":          {"shot": ["--no-connect", "--login", "error"]},
    "signed-in":            {"shot": ["--no-connect", "--login", "signed-in"]},
    "transcript-markdown":  {"idle": ["--bench", "fixtures/msp/synthetic-markdown.jsonl"]},
    "transcript-toolshapes": {"idle": ["--bench", "fixtures/msp/synthetic-toolshapes.jsonl"]},
    "transcript-stress":    {"idle": ["--bench", "fixtures/msp/synthetic-stress-300.jsonl"]},
}


def boot(entry, verb):
    spec = ENTRIES.get(entry)
    if spec is None:
        sys.exit(f"uiprobe: unknown entry {entry!r}; have: {', '.join(sorted(ENTRIES))}")
    if verb not in spec:
        sys.exit(f"uiprobe: entry {entry!r} has no {verb!r} probe "
                 f"(it supports: {', '.join(sorted(spec))})")
    return [BAAZ] + spec[verb]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--entry", required=True)
    ap.add_argument("--probe-shot")
    ap.add_argument("--probe-idle", type=int)
    ap.add_argument("--delay-ms", type=int, default=1200)
    ap.add_argument("--extra", default="", help="extra baaz flags, space separated")
    a = ap.parse_args()

    extra = a.extra.split() if a.extra else []

    if a.probe_shot:
        cmd = boot(a.entry, "shot") + extra
        cmd += ["--screenshot", a.probe_shot, "--screenshot-delay", str(a.delay_ms)]
        p = subprocess.run(cmd, cwd=REPO, capture_output=True, text=True, timeout=180)
        if p.returncode != 0 or not os.path.exists(a.probe_shot):
            sys.stderr.write(p.stderr[-1500:] or p.stdout[-1500:])
            return 1
        return 0

    if a.probe_idle:
        cmd = boot(a.entry, "idle") + extra
        # `--bench` already measures frames drawn at rest and writes
        # `idle_frames_2s`. Translate it onto the contract's shape.
        out = os.path.join(tempfile.mkdtemp(), "bench.json")
        p = subprocess.run(cmd + ["--bench-frames", "60", "--bench-out", out],
                           cwd=REPO, capture_output=True, text=True, timeout=180)
        if not os.path.exists(out):
            sys.stderr.write("bench produced no output:\n" + (p.stderr[-1000:] or p.stdout[-1000:]))
            return 1
        row = json.load(open(out))
        print(json.dumps({"probe": "uiprobe/1",
                          "frames": row.get("idle_frames_2s", 0),
                          "window_ms": 2000,
                          "settle_frames": row.get("frames", 0)}))
        return 0

    sys.exit("uiprobe: give --probe-shot or --probe-idle")


if __name__ == "__main__":
    sys.exit(main())
