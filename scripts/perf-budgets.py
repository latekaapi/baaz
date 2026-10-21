#!/usr/bin/env python3
"""Performance budgets for baaz (docs/51 step M): measure, compare, fail.

Every number here comes from a flag baaz already has, run offline against
fixtures in a throwaway state dir — no model, no provider, none of the
owner's state:

  store    `baaz --perf-store <dir>`: the per-turn costs (index refresh,
           search sync, provider transcript reads) on a synthetic store the
           size of the owner's (2,600 sessions).
  boot     `--no-connect --login signed-in --screenshot`: process start to
           the first frame (`baaz-boot … first-frame`), median of runs.
  open     `--replay synthetic-stress-300`: the open-at-length mark, from
           the replay's open to the first transcript render of 300 turns.
  switch   the same window, `click:` away and back: a cached switch to the
           300-turn session, to its first transcript render.
  stream   `--bench synthetic-stress-300`: each streamed event's fold to the
           next transcript render (stream-to-render), and the fold itself;
           then a generated 1,000-turn transcript, where the per-chunk
           render work must not grow with the session (docs/51 #6).
  idle     the signed-in shell with the 300-turn replay open, left alone:
           CPU used (cumulative `ps` time, not the decaying `%cpu`), root
           renders and frames asked for (aui-motion's per-call-site
           counter) over a quiet window. At rest all must be ~zero; a miss
           on frames names the call sites that asked.

Budgets live in `scripts/perf-budgets.json`, per build profile, each with
the number measured on the reference machine beside it. A budget missed is
exit 1; a run that could not measure is exit 2 (never a pass).

    python3 scripts/perf-budgets.py                 # debug binary, all groups
    python3 scripts/perf-budgets.py --binary target/release/baaz
    python3 scripts/perf-budgets.py --only store --slack 3   # CI
    python3 scripts/perf-budgets.py --json          # one JSON object on stdout
"""
import argparse, json, os, re, shutil, statistics, subprocess, sys, tempfile, time

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUDGETS = os.path.join(REPO, "scripts", "perf-budgets.json")
STRESS = "fixtures/msp/synthetic-stress-300.jsonl"
STRESS_ID = "synthetic-stress-300"
SIDEBAR = "fixtures/sidebar/stress.json"
# Why a metric reads what it reads, printed beside a miss (e.g. which call
# sites asked for frames at rest).
NOTES = {}
GROUPS = ("store", "boot", "open", "switch", "stream", "idle")


class Unmeasured(Exception):
    pass


def env_for(state):
    """A throwaway state dir and Muse data home, deterministic, traced."""
    env = {k: v for k, v in os.environ.items() if not k.startswith("BAAZ_")}
    env.update({
        "BAAZ_STATE_DIR": os.path.join(state, "baaz"),
        "XDG_DATA_HOME": os.path.join(state, "xdg"),
        "BAAZ_DETERMINISTIC": "1",
        "BAAZ_TRACE": "1",
    })
    return env


def run(binary, args, state, timeout=180):
    p = subprocess.run([binary] + args, cwd=REPO, capture_output=True, text=True,
                       timeout=timeout, env=env_for(state))
    return p.returncode, p.stdout, p.stderr


def boot_ms(stderr, label):
    m = re.search(r"baaz-boot \+(\d+)ms " + re.escape(label), stderr)
    return int(m.group(1)) if m else None


def trace_marks(stderr):
    """[(micros, label)] in order."""
    return [(int(us), label) for us, label in re.findall(r"baaz-trace \+(\d+)us (.+)", stderr)]


def measure_store(binary, state, reps):
    out = {}
    for _ in range(reps):
        code, stdout, stderr = run(binary, ["--perf-store", os.path.join(state, "store")], state)
        if code != 0:
            raise Unmeasured(f"--perf-store exited {code}: {stderr[-400:]}")
        row = json.loads(stdout.strip().splitlines()[-1])
        for key, value in row.items():
            out.setdefault(f"store.{key}", []).append(value)
    return {k: statistics.median(v) for k, v in out.items()}


def measure_boot(binary, state, reps):
    samples = []
    shot = os.path.join(state, "boot.png")
    # One unmeasured launch first: the first run after a build pays dyld and
    # the page cache, which no person sees twice.
    run(binary, ["--no-connect", "--login", "signed-in", "--screenshot", shot, "--screenshot-delay", "200"], state)
    for _ in range(reps):
        _, _, stderr = run(binary, ["--no-connect", "--login", "signed-in",
                                    "--screenshot", shot, "--screenshot-delay", "200"], state)
        ms = boot_ms(stderr, "first-frame")
        if ms is None:
            raise Unmeasured("no first-frame mark (is BAAZ_TRACE honoured?)")
        samples.append(ms)
    return {"boot.first_frame_ms": statistics.median(samples)}


def measure_open_switch(binary, state, reps):
    opens, switches = [], []
    shot = os.path.join(state, "switch.png")
    steps = ";".join([f"click:s-stress-w02", "wait:300", f"click:{STRESS_ID}", "wait:300",
                      f"click:s-stress-w02", "wait:300", f"click:{STRESS_ID}"])
    for _ in range(reps):
        _, stdout, stderr = run(binary, ["--replay", STRESS, "--sidebar-fixture", SIDEBAR,
                                         "--steps", steps, "--screenshot", shot,
                                         "--screenshot-delay", "400"], state)
        stderr = stdout + stderr
        marks = trace_marks(stderr)
        # The replay open: first-content after replay-open.
        seen_open = False
        for i, (us, label) in enumerate(marks):
            if label == "replay-open":
                seen_open = True
            elif label == "first-content" and seen_open:
                opens.append(us / 1000.0)
                seen_open = False
        # Cached switches back to the 300-turn session: a resume of it that
        # hit the cache, to its first-content.
        for i, (us, label) in enumerate(marks):
            if label.startswith("resume ") and STRESS_ID.startswith(label[len("resume "):]):
                rest = marks[i + 1:i + 4]
                if any(l == "cache-hit" for _, l in rest):
                    first = next((u for u, l in rest if l == "first-content"), None)
                    if first is not None:
                        switches.append(first / 1000.0)
        if "steps: ran=7 failed=0" not in stderr:
            raise Unmeasured("the switch script did not run all 7 steps: " + stderr[-300:])
    if not opens:
        raise Unmeasured("no replay-open → first-content marks")
    if not switches:
        raise Unmeasured("no cached switch back to the 300-turn session (did the replay park?)")
    return {"open.replay_300_ms": statistics.median(opens),
            "switch.cached_300_ms": statistics.median(switches)}


def measure_stream(binary, state):
    out = os.path.join(state, "bench.json")
    code, stdout, stderr = run(binary, ["--bench", STRESS, "--bench-scroll", "tail",
                                        "--bench-frames", "60", "--bench-out", out], state, timeout=240)
    if not os.path.exists(out):
        raise Unmeasured(f"--bench wrote nothing (exit {code}): {stderr[-400:]}")
    row = json.load(open(out))
    stream = row.get("stream_to_render") or {}
    if not stream.get("n"):
        raise Unmeasured("--bench recorded no stream-to-render samples (no frames rendered?)")
    return {
        "stream.to_render_p50_ms": stream["p50_us"] / 1000.0,
        "stream.to_render_p99_ms": stream["p99_us"] / 1000.0,
        "stream.fold_apply_p99_ms": row["fold_apply"]["p99_us"] / 1000.0,
    }


def measure_stream_long(binary, state):
    """Streaming into a 1,000-turn transcript: the cost that used to grow
    with the session (docs/51 #6). The fixture is generated, not checked in
    (4 MB), with the same generator as the 300-turn one."""
    capture = os.path.join(state, "stress-1000.jsonl")
    subprocess.run([sys.executable, "fixtures/msp/make-stress-300.py", "--turns", "1000", "--out", capture],
                   cwd=REPO, check=True, capture_output=True)
    out = os.path.join(state, "bench-1000.json")
    code, _, stderr = run(binary, ["--bench", capture, "--bench-scroll", "tail", "--bench-cadence-ms", "1",
                                   "--bench-frames", "60", "--bench-out", out], state, timeout=300)
    if not os.path.exists(out):
        raise Unmeasured(f"--bench (1,000 turns) wrote nothing (exit {code}): {stderr[-400:]}")
    row = json.load(open(out))
    stream = row.get("stream_to_render") or {}
    if not stream.get("n") or not row.get("element", {}).get("n"):
        raise Unmeasured("--bench (1,000 turns) recorded no frames")
    return {
        "stream.long_1000_element_p99_ms": row["element"]["p99_us"] / 1000.0,
        "stream.long_1000_to_render_p99_ms": stream["p99_us"] / 1000.0,
    }


def cpu_seconds(pid):
    p = subprocess.run(["ps", "-o", "time=", "-p", str(pid)], capture_output=True, text=True)
    text = p.stdout.strip()
    if not text:
        raise Unmeasured("the idle window exited early")
    parts = [float(x) for x in text.replace("-", ":").split(":")]
    secs = 0.0
    for part in parts:
        secs = secs * 60 + part
    return secs


def measure_idle(binary, settle_s, window_s, state):
    log_path = os.path.join(state, "idle.log")
    with open(log_path, "w") as log:
        proc = subprocess.Popen([binary, "--replay", STRESS], cwd=REPO, stdout=log, stderr=log,
                                env=env_for(state))
    try:
        time.sleep(settle_s)
        if proc.poll() is not None:
            raise Unmeasured("the idle window exited before it settled")
        start_cpu, start_len = cpu_seconds(proc.pid), os.path.getsize(log_path)
        time.sleep(window_s)
        cpu = cpu_seconds(proc.pid) - start_cpu
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            proc.kill()
    with open(log_path) as fh:
        fh.seek(start_len)
        tail = fh.read()
    renders = sum(int(n) for n in re.findall(r"baaz-perf renders=(\d+)/2s", tail))
    frame_lines = re.findall(r"baaz-perf frames=(\d+) by (.*)", tail)
    if frame_lines:
        NOTES["idle.frame_requests"] = "; ".join(by for _, by in frame_lines)
    return {"idle.cpu_pct": round(100.0 * cpu / window_s, 2),
            "idle.renders": renders,
            "idle.frame_requests": sum(int(n) for n, _ in frame_lines)}


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--binary", default=os.path.join(REPO, "target", "debug", "baaz"))
    ap.add_argument("--only", action="append", choices=GROUPS)
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--slack", type=float, default=1.0,
                    help="multiply every budget (a slower machine, e.g. CI); never below 1")
    ap.add_argument("--budgets", default=BUDGETS, help="budget file (default scripts/perf-budgets.json)")
    ap.add_argument("--json", action="store_true", help="print one JSON object (the probe contract)")
    ap.add_argument("--idle-settle", type=float, default=4.0)
    ap.add_argument("--idle-window", type=float, default=10.0)
    a = ap.parse_args()

    binary = os.path.abspath(a.binary)
    if not os.path.exists(binary):
        sys.stderr.write(f"perf-budgets: no binary at {binary} — build it first (cargo build -p baaz)\n")
        return 2
    # A bundle's binary (`Baaz.app/Contents/MacOS/Baaz`) is a release build.
    profile = "release" if os.sep + "release" + os.sep in binary or ".app/Contents/MacOS/" in binary else "debug"
    config = json.load(open(a.budgets))
    budgets = config["profiles"][profile]["budgets"]
    groups = a.only or list(GROUPS)
    slack = max(1.0, a.slack)

    state = tempfile.mkdtemp(prefix="baaz-perf-")
    measured, errors = {}, []
    try:
        for group in groups:
            try:
                if group == "store":
                    measured.update(measure_store(binary, state, max(1, a.reps // 2)))
                elif group == "boot":
                    measured.update(measure_boot(binary, state, a.reps))
                elif group in ("open", "switch"):
                    # One window measures both; keep the groups asked for.
                    if not any(k.startswith(("open.", "switch.")) for k in measured):
                        both = measure_open_switch(binary, state, 3)
                        measured.update({k: v for k, v in both.items() if k.split(".", 1)[0] in groups})
                elif group == "stream":
                    measured.update(measure_stream(binary, state))
                    measured.update(measure_stream_long(binary, state))
                elif group == "idle":
                    measured.update(measure_idle(binary, a.idle_settle, a.idle_window, state))
            except (Unmeasured, subprocess.TimeoutExpired, OSError, ValueError, KeyError) as error:
                errors.append(f"{group}: {error}")
    finally:
        shutil.rmtree(state, ignore_errors=True)

    rows, misses = {}, []
    for name, value in sorted(measured.items()):
        spec = budgets.get(name)
        if spec is None:
            rows[name] = {"value": value, "budget": None, "ok": None}
            continue
        limit = spec["budget"] * slack
        ok = value <= limit
        rows[name] = {"value": value, "budget": limit, "unit": spec.get("unit", "ms"),
                      "reference": spec.get("measured"), "ok": ok}
        if not ok:
            note = f" ({NOTES[name]})" if name in NOTES else ""
            misses.append(f"{name} = {value} > budget {limit:g}{note}")
    # A budgeted metric in a group that ran but produced nothing is an error,
    # never a silent pass.
    for name in budgets:
        group = name.split(".", 1)[0]
        if group in groups and name not in measured and not any(e.startswith(group + ":") for e in errors):
            errors.append(f"{name}: not measured")

    result = {"probe": "perf-budgets/1", "profile": profile, "binary": binary, "slack": slack,
              "metrics": rows, "misses": misses, "errors": errors}
    if a.json:
        print(json.dumps(result))
    else:
        width = max((len(n) for n in rows), default=10)
        for name, row in rows.items():
            mark = "ok  " if row["ok"] else ("MISS" if row["ok"] is False else "    ")
            budget = "-" if row["budget"] is None else f"{row['budget']:g}"
            print(f"{mark} {name:<{width}}  {row['value']:>9g}  budget {budget}")
        for line in misses:
            if "(" in line:
                print(f"MISS {line}")
        for line in errors:
            print(f"ERR  {line}")
        print(f"perf-budgets ({profile}): {len(misses)} missed, {len(errors)} unmeasured")
    if errors:
        return 2
    return 1 if misses else 0


if __name__ == "__main__":
    sys.exit(main())
