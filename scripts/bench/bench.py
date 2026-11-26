#!/usr/bin/env python3
"""The Baaz bench (docs/85 17.1, D-Q9): fixed small tasks, each run through the
bare CLI and through Baaz with the same model, then judged by the task's own
check. A budgeted LIVE script: every run is a real turn (or several, inside one
agent turn). Never run from `cargo test`.

Per run (task x provider x mode):
  * a throwaway repo under --work (rule 24): the task's files, one commit, a
    local bare remote as `origin`;
  * bare: the CLI itself in print/exec mode, in a throwaway home (rule 25:
    Claude CLAUDE_CONFIG_DIR, Codex CODEX_HOME with only auth.json linked),
    with every permission granted (Claude bypassPermissions, Codex
    --dangerously-bypass-approvals-and-sandbox): the same ground as Baaz's
    Full access;
  * baaz: the check build (target/check, `scripts/baaz-check.sh build`) with
    its own BAAZ_STATE_DIR: one offline pass makes the project and a session
    on the provider, one connected pass sets the model, effort and Full
    access, sends the prompt once and waits for the turn to end
    (`wait-idle`); metrics come from that state's baaz.db `turn` table;
  * the task's check command runs in the repo afterwards: exit 0 = finished.

Each Baaz pass waits until the Mac has been idle (HIDIdleTime) - the check build
takes focus, and a person clicking in it would move its active session.

Usage:
  python3 scripts/bench/bench.py --work <scratch dir> [--tasks 1-3] [--providers claude,codex]
      [--modes bare,baaz] [--claude-model haiku] [--codex-model gpt-5.6-luna] [--codex-effort medium]

Appends one JSON line per run to <work>/results.jsonl (a rerun skips runs already
there) and writes <work>/report.md. Exit 0 always: a rate is a measurement.
"""
import argparse, glob, json, os, shutil, sqlite3, subprocess, sys, tempfile, time

HARNESS = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
TASKS = os.path.join(HARNESS, "scripts", "bench", "tasks")
BAAZ = os.path.join(HARNESS, "target", "check", "Baaz.app", "Contents", "MacOS", "Baaz")
WIRE = {"claude": "claude-code", "codex": "codex"}
DONE_WORDS = ("done", "fixed", "pass", "implemented", "added", "renamed", "extracted", "complete", "updated", "all tests")


def sh(cmd, cwd, timeout=120):
    return subprocess.run(["bash", "-c", cmd], cwd=cwd, capture_output=True, text=True, timeout=timeout)


def idle_seconds():
    out = subprocess.run(["ioreg", "-c", "IOHIDSystem"], capture_output=True, text=True).stdout
    for line in out.splitlines():
        if "HIDIdleTime" in line:
            return int(line.split()[-1]) // 1_000_000_000
    return 0


def wait_for_quiet(log):
    """A Baaz launch resets HIDIdleTime itself: wait 15 s after the last pass,
    then for 12 s of quiet (up to 10 minutes), so nobody is mid-click."""
    time.sleep(15)
    waited = 0
    while idle_seconds() < 12:
        if waited == 0:
            log("  someone is at the machine: waiting for quiet")
        time.sleep(10)
        waited += 10
        if waited > 600:
            raise SystemExit("bench: the machine never went quiet; stopping (rerun to resume)")


def make_repo(task_dir, run_dir):
    repo = os.path.join(run_dir, "repo")
    shutil.copytree(os.path.join(task_dir, "repo"), repo)
    bare = os.path.join(run_dir, "origin.git")
    subprocess.run(["git", "init", "-q", "--bare", bare], check=True)
    for cmd in (["git", "init", "-q", "-b", "main"], ["git", "add", "-A"],
                ["git", "-c", "user.email=b@b", "-c", "user.name=bench", "commit", "-qm", "init"],
                ["git", "remote", "add", "origin", bare], ["git", "push", "-q", "origin", "main"]):
        subprocess.run(cmd, cwd=repo, check=True, capture_output=True)
    return repo


def claims_done(text):
    text = (text or "").lower()
    return any(word in text for word in DONE_WORDS)


EXPLORE_AGENT = json.dumps({"Explore": {
    "description": "Fast read-only exploration of the codebase: finding files and symbols and working out how something works. Use it for open-ended searches.",
    "prompt": "You explore code read-only. Search with Grep and Glob, read only what you need, and report concise findings with file paths and line numbers.",
    "tools": ["Read", "Grep", "Glob"], "model": "haiku"}})


def bare_claude(repo, run_dir, prompt, model, timeout, variant="base"):
    home = os.path.join(run_dir, "claude-home")
    os.makedirs(home, exist_ok=True)
    env = dict(os.environ, CLAUDE_CONFIG_DIR=home, CLAUDE_SECURESTORAGE_CONFIG_DIR="")
    argv = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose",
            "--permission-mode", "bypassPermissions"] + (["--model", model] if model != "default" else [])
    # docs/85 17.2 variants: WebFetch and WebSearch off (10.6), an Explore
    # agent on Haiku (12.7).
    if variant == "no-web":
        argv += ["--disallowedTools", "WebFetch,WebSearch"]
    elif variant == "explore":
        argv += ["--agents", EXPLORE_AGENT]
    start = time.time()
    p = subprocess.run(argv, cwd=repo, env=env, capture_output=True, text=True, timeout=timeout)
    open(os.path.join(run_dir, "bare.ndjson"), "w").write(p.stdout)
    result = {}
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("type") == "result":
            result = m
    usage = result.get("usage") or {}
    tin = usage.get("input_tokens", 0) + usage.get("cache_read_input_tokens", 0) + usage.get("cache_creation_input_tokens", 0)
    return {"tokens_in": tin, "tokens_out": usage.get("output_tokens", 0), "cached": usage.get("cache_read_input_tokens", 0),
            "cost_usd": result.get("total_cost_usd"), "final": result.get("result", ""), "agent_turns": result.get("num_turns"),
            "wall_s": round(time.time() - start, 1), "error": None if result else (p.stderr[-400:] or "no result frame"),
            "models": sorted((result.get("modelUsage") or {}).keys())}


def bare_codex(repo, run_dir, prompt, model, effort, timeout):
    home = os.path.join(run_dir, "codex-home")
    os.makedirs(home, exist_ok=True)
    os.symlink(os.path.expanduser("~/.codex/auth.json"), os.path.join(home, "auth.json"))
    env = dict(os.environ, CODEX_HOME=home)
    argv = ["codex", "exec", "--json", "-m", model, "-c", f"model_reasoning_effort={effort}",
            "--dangerously-bypass-approvals-and-sandbox", "-C", repo, prompt]
    start = time.time()
    p = subprocess.run(argv, cwd=repo, env=env, capture_output=True, text=True, timeout=timeout, stdin=subprocess.DEVNULL)
    open(os.path.join(run_dir, "bare.ndjson"), "w").write(p.stdout)
    tin = tout = cached = 0
    final = ""
    seen = False
    for line in p.stdout.splitlines():
        try:
            m = json.loads(line)
        except ValueError:
            continue
        if m.get("type") == "turn.completed":
            seen = True
            u = m.get("usage") or {}
            tin += u.get("input_tokens", 0)
            tout += u.get("output_tokens", 0)
            cached += u.get("cached_input_tokens", 0)
        item = m.get("item") or {}
        if m.get("type") == "item.completed" and item.get("type") == "agent_message":
            final = item.get("text", "")
    return {"tokens_in": tin, "tokens_out": tout, "cached": cached, "cost_usd": None, "final": final, "agent_turns": None,
            "wall_s": round(time.time() - start, 1), "error": None if seen else (p.stderr[-400:] or "no turn.completed")}


def baaz_final_text(state, provider):
    """The agent's last message, from the provider's own files under the
    run's state dir (rule 18: the provider's files are the authority)."""
    if provider == "claude":
        rows = []
        for path in glob.glob(os.path.join(state, "claude-home", "projects", "*", "*.jsonl")):
            rows += [json.loads(l) for l in open(path) if l.strip()]
        texts = [" ".join(b.get("text", "") for b in (r.get("message") or {}).get("content") or [] if isinstance(b, dict) and b.get("type") == "text")
                 for r in rows if r.get("type") == "assistant"]
        texts = [t for t in texts if t.strip()]
        return texts[-1] if texts else ""
    final = ""
    for path in sorted(glob.glob(os.path.join(state, "codex-home", "sessions", "**", "*.jsonl"), recursive=True)):
        for line in open(path):
            try:
                m = json.loads(line)
            except ValueError:
                continue
            p = m.get("payload") or {}
            if p.get("type") == "agent_message":
                final = p.get("message", final)
    return final


def baaz_run(repo, run_dir, prompt, provider, model, effort, timeout, log):
    state = os.path.join(run_dir, "state")
    os.makedirs(state, exist_ok=True)
    env = dict(os.environ, BAAZ_STATE_DIR=state)
    wait_for_quiet(log)
    # Run from inside the repo: the connected pass boots its own session in
    # the app's working folder (the offline pass's scripted session does not
    # carry over), so that folder must be the throwaway repo (rule 24).
    p0 = subprocess.run([BAAZ, "--no-connect", "--login", "signed-in", "--steps", f"project:{repo};new;setprovider:{WIRE[provider]}",
                         "--screenshot", os.path.join(run_dir, "baaz-0.png"), "--screenshot-delay", "1500"],
                        env=env, capture_output=True, text=True, timeout=180, cwd=repo)
    open(os.path.join(run_dir, "baaz-pass0.log"), "w").write(p0.stdout + p0.stderr)
    steps = [f"setmodel:{model}"] + ([f"seteffort:{effort}"] if effort else []) + ["setmode:AllowAll", f"send:{prompt}", f"wait-idle:{timeout * 1000}"]
    wait_for_quiet(log)
    start = time.time()
    p1 = subprocess.run([BAAZ, "--steps", ";".join(steps), "--screenshot", os.path.join(run_dir, "baaz-1.png"), "--screenshot-delay", "1500"],
                        env=env, capture_output=True, text=True, timeout=timeout + 300, cwd=repo)
    wall = round(time.time() - start, 1)
    out = p1.stdout + p1.stderr
    open(os.path.join(run_dir, "baaz-pass1.log"), "w").write(out)
    shutil.copy(os.path.join(state, "logs", "baaz.log"), os.path.join(run_dir, "baaz.log")) if os.path.exists(os.path.join(state, "logs", "baaz.log")) else None
    failed = [l for l in out.splitlines() if l.startswith("steps: failed step")]
    log_text = open(os.path.join(run_dir, "baaz.log")).read() if os.path.exists(os.path.join(run_dir, "baaz.log")) else ""
    elsewhere = sorted({l.split("workspace=", 1)[1].strip() for l in log_text.splitlines() if "provider lane workspace" in l and "workspace=" in l} - {repo, os.path.realpath(repo)})
    if elsewhere:
        failed.append(f"a session opened outside the repo: {elsewhere}")
    rows = []
    db = os.path.join(state, "baaz.db")
    if os.path.exists(db):
        con = sqlite3.connect(db)
        rows = con.execute("SELECT COALESCE(tokens_in,0), COALESCE(tokens_out,0), COALESCE(cache_read,0)+COALESCE(cached_tokens,0), cost_usd, cost_source, duration_ms "
                           "FROM turn WHERE role='assistant' AND finished_at IS NOT NULL").fetchall()
        con.close()
    subprocess.run([BAAZ, "--sweep-clones"], env=env, capture_output=True)
    costs = [r[3] for r in rows if r[3] is not None and r[4] != "unknown"]
    return {"tokens_in": sum(r[0] for r in rows), "tokens_out": sum(r[1] for r in rows), "cached": sum(r[2] for r in rows),
            "cost_usd": round(sum(costs), 6) if costs else None, "final": baaz_final_text(state, provider), "agent_turns": len(rows),
            "wall_s": wall, "error": ("; ".join(failed) or None) if rows else ("; ".join(failed) or "no finished turn in baaz.db"),
            "stuck": bool(failed)}


def parse_range(text, n):
    if not text:
        return list(range(1, n + 1))
    out = []
    for part in text.split(","):
        a, _, b = part.partition("-")
        out += list(range(int(a), int(b or a) + 1))
    return out


def report(work):
    runs = [json.loads(l) for l in open(os.path.join(work, "results.jsonl"))]
    lines = ["# Baaz bench report", "", f"{len(runs)} runs. Finished = the task's own check passes after one prompt.", "",
             "| provider | mode | runs | finished | tokens in / finished | cached % | cost / finished | done-claims overruled | stuck | wall s (median) |",
             "|---|---|---|---|---|---|---|---|---|---|"]
    groups = sorted({(r["provider"], r["mode"], r.get("variant", "base")) for r in runs}, key=lambda g: (g[0], g[1] != "bare", g[2]))
    for provider, mode, variant in groups:
        rs = [r for r in runs if r["provider"] == provider and r["mode"] == mode and r.get("variant", "base") == variant]
        if not rs:
            continue
        fin = [r for r in rs if r["finished"]]
        tin = sum(r["tokens_in"] for r in rs)
        cached = sum(r["cached"] for r in rs)
        costs = [r["cost_usd"] for r in rs if r["cost_usd"] is not None]
        per_tok = f"{tin // max(len(fin), 1):,}" if fin else "-"
        per_cost = f"${sum(costs) / len(fin):.4f}" if fin and costs else "-"
        overruled = sum(1 for r in rs if r["claims_done"] and not r["finished"])
        stuck = sum(1 for r in rs if r.get("stuck"))
        walls = sorted(r["wall_s"] for r in rs)
        label = mode if variant == "base" else f"{mode} ({variant})"
        lines.append(f"| {provider} | {label} | {len(rs)} | {len(fin)} | {per_tok} | {100 * cached // max(tin, 1)}% | {per_cost} | {overruled} | {stuck} | {walls[len(walls) // 2]} |")
    lines += ["", "## Per run", "", "| task | provider | mode | finished | tokens in | out | cached | cost | agent turns | wall s | error |", "|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in sorted(runs, key=lambda r: (r["task"], r["provider"], r["mode"], r.get("variant", "base"))):
        cost = f"${r['cost_usd']:.4f}" if r["cost_usd"] is not None else "-"
        mode = r["mode"] if r.get("variant", "base") == "base" else f"{r['mode']} ({r['variant']})"
        lines.append(f"| {r['task']} | {r['provider']} | {mode} | {'yes' if r['finished'] else 'NO'} | {r['tokens_in']:,} | {r['tokens_out']:,} | {r['cached']:,} | {cost} | {r['agent_turns'] if r['agent_turns'] is not None else '-'} | {r['wall_s']} | {(r['error'] or '')[:80]} |")
    explore = [r for r in runs if r.get("variant") == "explore"]
    if explore:
        used = sum(1 for r in explore if any("haiku" in m for m in r.get("models", [])))
        lines += ["", f"Explore variant: the Haiku agent ran in {used} of {len(explore)} runs (modelUsage)."]
    lines += ["", "Interventions and approval wait: every run is one prompt in Full access (bare: the CLI's own bypass), so neither occurs by design; a run left waiting on the person is counted as stuck."]
    open(os.path.join(work, "report.md"), "w").write("\n".join(lines) + "\n")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--tasks", default="")
    ap.add_argument("--providers", default="claude,codex")
    ap.add_argument("--modes", default="bare,baaz")
    ap.add_argument("--claude-model", default="haiku")
    ap.add_argument("--codex-model", default="gpt-5.6-luna")
    ap.add_argument("--codex-effort", default="medium")
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--variant", default="base", choices=["base", "no-web", "explore"],
                    help="bare Claude only (docs/85 17.2): WebFetch/WebSearch off, or an Explore agent on Haiku")
    args = ap.parse_args()
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    results = os.path.join(work, "results.jsonl")
    done = set()
    if os.path.exists(results):
        done = {(r["task"], r["provider"], r["mode"], r.get("variant", "base")) for r in map(json.loads, open(results))}
    tasks = sorted(os.listdir(TASKS))
    pick = [tasks[i - 1] for i in parse_range(args.tasks, len(tasks))]
    log = lambda msg: print(msg, flush=True)
    for task in pick:
        spec = json.load(open(os.path.join(TASKS, task, "task.json")))
        for provider in args.providers.split(","):
            for mode in args.modes.split(","):
                if (task, provider, mode, args.variant) in done:
                    continue
                suffix = "" if args.variant == "base" else f"-{args.variant}"
                run_dir = os.path.join(work, f"{task}-{provider}-{mode}{suffix}")
                shutil.rmtree(run_dir, ignore_errors=True)
                os.makedirs(run_dir)
                repo = make_repo(os.path.join(TASKS, task), run_dir)
                log(f"{task} {provider} {mode} ...")
                try:
                    if mode == "bare" and provider == "claude":
                        m = bare_claude(repo, run_dir, spec["prompt"], args.claude_model, args.timeout, args.variant)
                    elif mode == "bare":
                        m = bare_codex(repo, run_dir, spec["prompt"], args.codex_model, args.codex_effort, args.timeout)
                    else:
                        model = args.claude_model if provider == "claude" else args.codex_model
                        effort = args.codex_effort if provider == "codex" else None
                        m = baaz_run(repo, run_dir, spec["prompt"], provider, model, effort, args.timeout, log)
                except subprocess.TimeoutExpired:
                    m = {"tokens_in": 0, "tokens_out": 0, "cached": 0, "cost_usd": None, "final": "", "agent_turns": None,
                         "wall_s": args.timeout, "error": "timeout", "stuck": True}
                check = sh(spec["check"], repo)
                row = dict(m, task=task, provider=provider, mode=mode, variant=args.variant, finished=check.returncode == 0,
                           claims_done=claims_done(m.get("final")), check_tail=(check.stdout + check.stderr)[-300:])
                row["final"] = (row.get("final") or "")[:500]
                with open(results, "a") as f:
                    f.write(json.dumps(row) + "\n")
                log(f"  finished={row['finished']} tokens_in={row['tokens_in']} cost={row['cost_usd']} err={row['error']}")
    print(report(work))


if __name__ == "__main__":
    main()
