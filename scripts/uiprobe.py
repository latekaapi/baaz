#!/usr/bin/env python3
"""uiprobe/1 adapter for baaz — a shim, not new capability.

Everything here already existed as a baaz flag. This only translates relay's
contract onto it and normalises the output to the contract's JSON.

Entries are named screens. All of them run offline (`--no-connect`), so a probe
run costs nothing and reaches no model.

    uiprobe.py --entry login-choose --probe-shot out.png
    uiprobe.py --entry login-choose --probe-idle 2000
"""
import argparse, json, os, shutil, subprocess, sys, tempfile

# How many captures to take looking for two that agree.
SETTLE_TRIES = 6

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BAAZ = os.path.join(REPO, "target", "debug", "baaz")

# baaz's own switch for this, and its doc comment says exactly what it is for:
# "Whether captures must be byte-identical run to run." It freezes the clock and
# holds the platform reduced-motion flag, so every tween, spring and shimmer
# resolves to its resting state in one frame.
#
# Without it, gpui renders on demand and an entrance animation advances by
# however many frames the machine had spare during the settle delay — so a
# baseline taken on an idle machine and compared on a busy one differs by the
# remaining fade. Measured: a whole sign-in card at partial opacity, 19 findings
# across five screens, not one of them a real change.
PROBE_ENV = {**os.environ, "BAAZ_DETERMINISTIC": "1"}

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
    "right-browser":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:browser"]},
    "right-diff":           {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:diff"]},
    "right-git":            {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:git"]},
    "right-files":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files"]},
    "right-closed":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;right:off"]},
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
        # Capture twice and require the two to agree. With BAAZ_DETERMINISTIC
        # set they always should, so this is cheap; when they do not, the screen
        # is genuinely still animating and reporting that beats baselining an
        # arbitrary frame.
        import filecmp, tempfile
        tmp, prev = tempfile.mkdtemp(), None
        for attempt in range(SETTLE_TRIES):
            shot = os.path.join(tmp, f"s{attempt}.png")
            p = subprocess.run(cmd + ["--screenshot", shot,
                                      "--screenshot-delay", str(a.delay_ms)],
                               cwd=REPO, capture_output=True, text=True,
                               timeout=180, env=PROBE_ENV)
            if p.returncode != 0 or not os.path.exists(shot):
                sys.stderr.write(p.stderr[-1500:] or p.stdout[-1500:])
                return 1
            if prev and filecmp.cmp(prev, shot, shallow=False):
                shutil.copyfile(shot, a.probe_shot)
                return 0
            prev = shot
        sys.stderr.write(
            f"uiprobe: '{a.entry}' never rendered the same twice in "
            f"{SETTLE_TRIES} captures - still animating, not baselined\n")
        return 1

    if a.probe_idle:
        cmd = boot(a.entry, "idle") + extra
        # `--bench` already measures frames drawn at rest and writes
        # `idle_frames_2s`. Translate it onto the contract's shape.
        out = os.path.join(tempfile.mkdtemp(), "bench.json")
        p = subprocess.run(cmd + ["--bench-frames", "60", "--bench-out", out],
                           cwd=REPO, capture_output=True, text=True, timeout=180,
                           env=PROBE_ENV)
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
