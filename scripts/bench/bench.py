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

Codex fairness (docs/88 C.1):
  * one binary on both sides: resolved once the way Baaz does (BAAZ_CODEX, else
    the login shell's PATH), passed to Baaz as BAAZ_CODEX and used as the bare
    argv; each run's `cli_version` is read from its rollout, and a pair whose
    versions differ fails;
  * --arm clean (gating): neither side gets the owner's config.toml, skills,
    plugins or AGENTS.md. The bare home holds only the auth.json link; the
    Baaz home is pre-seeded with the auth.json link and a dangling link for
    every other owner entry, which Baaz's home upkeep never replaces
    (`ensure_linked_dir` fills only absent names) and Codex reads as absent;
  * --arm installed (reported): bare gets copies of the owner's config.toml
    (minus `notify`), skills/, plugins/ and AGENTS.md; Baaz links them as in
    production;
  * Codex token metrics come from each rollout's per-request `token_count`
    (consecutive repeats dropped), so both sides are counted the same way.

Usage:
  python3 scripts/bench/bench.py --work <scratch dir> [--tasks 1-3] [--providers claude,codex]
      [--modes bare,baaz] [--claude-model haiku] [--codex-model gpt-5.6-luna] [--codex-effort medium]
      [--arm clean|installed] [--repeat N]
  python3 scripts/bench/bench.py --report <results.jsonl> [--out report.md]

Appends one JSON line per run to <work>/results.jsonl (a rerun skips runs already
there) and writes <work>/report.md. Exit 0 always: a rate is a measurement.
"""
import argparse, glob, json, math, os, shutil, sqlite3, subprocess, sys, tempfile, time

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


OWNER_CODEX = os.path.expanduser("~/.codex")
# The owner entries Baaz's Codex home links (provider-codex `LINKED_ENTRIES`).
CODEX_LINKED = ("config.toml", "auth.json", "skills", "plugins", "AGENTS.md")
CLEAN_BLOCKER = "/nonexistent/baaz-bench-clean-arm"


def resolve_codex():
    """The codex binary Baaz itself would run: BAAZ_CODEX, else the first
    `codex` on the login shell's PATH (`$SHELL -ilc`, as `resolve_program`
    reads it). Returns (path, version)."""
    path = os.environ.get("BAAZ_CODEX") or ""
    if not path:
        shell = os.environ.get("SHELL") or "/bin/zsh"
        out = subprocess.run([shell, "-ilc", "command -v codex"], capture_output=True, text=True, timeout=30).stdout
        path = (out.strip().splitlines() or [""])[-1]
    if not path or not os.path.isfile(path):
        raise SystemExit("bench: no codex binary found (set BAAZ_CODEX)")
    version = subprocess.run([path, "--version"], capture_output=True, text=True, timeout=30).stdout.strip().split()[-1]
    return path, version


def seed_codex_home(home, arm, baaz, owner=OWNER_CODEX):
    """A run's Codex home for `arm`. Bare: the dir CODEX_HOME names. Baaz:
    `<state>/codex-home`, seeded before Baaz's own upkeep runs (which only
    fills absent names). The auth.json link is shared by every arm."""
    os.makedirs(home, exist_ok=True)
    os.symlink(os.path.join(owner, "auth.json"), os.path.join(home, "auth.json"))
    if arm == "clean":
        if baaz:
            for name in CODEX_LINKED:
                if name != "auth.json":
                    os.symlink(CLEAN_BLOCKER, os.path.join(home, name))
        return
    if baaz:
        return  # installed: Baaz links the owner's entries as in production
    for name in ("skills", "plugins"):
        src = os.path.join(owner, name)
        if os.path.isdir(src):
            shutil.copytree(src, os.path.join(home, name), symlinks=True)
    if os.path.isfile(os.path.join(owner, "AGENTS.md")):
        shutil.copy(os.path.join(owner, "AGENTS.md"), os.path.join(home, "AGENTS.md"))
    if os.path.isfile(os.path.join(owner, "config.toml")):
        open(os.path.join(home, "config.toml"), "w").write(strip_notify(open(os.path.join(owner, "config.toml")).read()))


def strip_notify(config_toml):
    """The owner's config without its top-level `notify` (a bench run must not
    ping the owner's notifier). A multi-line array value is skipped whole."""
    out, skipping, table = [], False, False
    for line in config_toml.splitlines():
        t = line.strip()
        if t.startswith("["):
            table, skipping = True, False
        if skipping:
            if t.endswith("]"):
                skipping = False
            continue
        if not table and (t.startswith("notify ") or t.startswith("notify=")):
            skipping = t.count("[") > t.count("]")
            continue
        out.append(line)
    return "\n".join(out) + "\n"


def rollout_metrics(home):
    """Per-request numbers from every rollout under `<home>/sessions`: each
    `token_count` event's `last_token_usage` (consecutive repeats dropped:
    Codex re-emits the last one), the `cli_version`s seen, and how many
    `<model_switch>` blocks the model was sent."""
    reqs, versions, switches = [], set(), 0
    for path in sorted(glob.glob(os.path.join(home, "sessions", "**", "*.jsonl"), recursive=True)):
        for line in open(path):
            try:
                r = json.loads(line)
            except ValueError:
                continue
            pl = r.get("payload") or {}
            if r.get("type") == "session_meta" and pl.get("cli_version"):
                versions.add(pl["cli_version"])
            elif r.get("type") == "event_msg" and pl.get("type") == "token_count":
                u = (pl.get("info") or {}).get("last_token_usage")
                if u:
                    x = [u.get("input_tokens", 0), u.get("cached_input_tokens", 0), u.get("output_tokens", 0)]
                    if not reqs or reqs[-1] != x:
                        reqs.append(x)
            elif r.get("type") == "response_item" and "<model_switch>" in line:
                switches += line.count("<model_switch>")
    return {"requests": reqs, "cli_versions": sorted(versions), "model_switch": switches}


def with_rollout(m, home):
    """Fold a Codex run's rollout numbers into its row: tokens from the
    rollout (both sides counted the same way), plus requests, uncached and
    the first request."""
    ro = rollout_metrics(home)
    reqs = ro["requests"]
    m = dict(m, requests=reqs, cli_versions=ro["cli_versions"], model_switch=ro["model_switch"])
    if reqs:
        m.update(tokens_in=sum(r[0] for r in reqs), cached=sum(r[1] for r in reqs), tokens_out=sum(r[2] for r in reqs))
    m["uncached"] = m["tokens_in"] - m["cached"]
    m["first_in"] = reqs[0][0] if reqs else None
    return m


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


def bare_codex(repo, run_dir, prompt, model, effort, timeout, codex="codex", arm="clean"):
    home = os.path.join(run_dir, "codex-home")
    seed_codex_home(home, arm, baaz=False)
    env = dict(os.environ, CODEX_HOME=home)
    argv = [codex, "exec", "--json", "-m", model, "-c", f"model_reasoning_effort={effort}",
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
    m = {"tokens_in": tin, "tokens_out": tout, "cached": cached, "cost_usd": None, "final": final, "agent_turns": None,
         "wall_s": round(time.time() - start, 1), "error": None if seen else (p.stderr[-400:] or "no turn.completed")}
    return with_rollout(m, home)


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


def baaz_run(repo, run_dir, prompt, provider, model, effort, timeout, log, codex=None, arm="clean"):
    state = os.path.join(run_dir, "state")
    os.makedirs(state, exist_ok=True)
    env = dict(os.environ, BAAZ_STATE_DIR=state)
    if provider == "codex":
        if codex:
            env["BAAZ_CODEX"] = codex
        seed_codex_home(os.path.join(state, "codex-home"), arm, baaz=True)
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
    m = {"tokens_in": sum(r[0] for r in rows), "tokens_out": sum(r[1] for r in rows), "cached": sum(r[2] for r in rows),
         "cost_usd": round(sum(costs), 6) if costs else None, "final": baaz_final_text(state, provider), "agent_turns": len(rows),
         "wall_s": wall, "error": ("; ".join(failed) or None) if rows else ("; ".join(failed) or "no finished turn in baaz.db"),
         "stuck": bool(failed), "baaz_tokens_in": sum(r[0] for r in rows)}
    return with_rollout(m, os.path.join(state, "codex-home")) if provider == "codex" else m


def parse_range(text, n):
    if not text:
        return list(range(1, n + 1))
    out = []
    for part in text.split(","):
        a, _, b = part.partition("-")
        out += list(range(int(a), int(b or a) + 1))
    return out


def run_key(r):
    """A run's identity in results.jsonl: rows from before docs/88 C.1 carry
    no arm or repeat."""
    return (r["task"], r["provider"], r["mode"], r.get("variant", "base"), r.get("arm") or "-", r.get("repeat", 1))


def median(xs):
    xs = sorted(xs)
    return xs[len(xs) // 2] if xs else None


def pairs(runs):
    """(bare, baaz) per (provider, variant, arm, task, repeat)."""
    by = {}
    for r in runs:
        k = (r["provider"], r.get("variant", "base"), r.get("arm") or "-", r["task"], r.get("repeat", 1))
        by.setdefault(k, {})[r["mode"]] = r
    return {k: (v["bare"], v["baaz"]) for k, v in sorted(by.items()) if "bare" in v and "baaz" in v}


def version_mismatches(runs):
    """Codex pairs that did not run one version: a run with no version or
    several, or a pair whose versions differ. Each is a failed row."""
    bad = []
    for k, (bare, baaz) in pairs(runs).items():
        if k[0] != "codex":
            continue
        vb, vz = bare.get("cli_versions") or [], baaz.get("cli_versions") or []
        if len(vb) != 1 or len(vz) != 1 or vb != vz:
            bad.append((k, vb, vz))
    return bad


def paired(values):
    """Mean and standard error of paired differences."""
    n = len(values)
    if n == 0:
        return None, None
    mean = sum(values) / n
    if n < 2:
        return mean, None
    sd = math.sqrt(sum((v - mean) ** 2 for v in values) / (n - 1))
    return mean, sd / math.sqrt(n)


def gate(runs, provider="codex", arm="clean"):
    """docs/88 \u00a76's pass criteria over one arm, as (name, ok, detail) rows.
    Never loosened here: the gate reads these."""
    rs = [r for r in runs if r["provider"] == provider and (r.get("arm") or "-") == arm and r.get("variant", "base") == "base"]
    side = {m: [r for r in rs if r["mode"] == m] for m in ("bare", "baaz")}
    fin = {m: [r for r in side[m] if r["finished"]] for m in side}
    per = {m: (sum(r["tokens_in"] for r in side[m]) / len(fin[m]) if fin[m] else float("inf")) for m in side}
    unc = {m: sum(r.get("uncached", r["tokens_in"] - r["cached"]) for r in side[m]) for m in side}
    ps = [(b, z) for k, (b, z) in pairs(runs).items() if k[0] == provider and k[2] == arm and k[1] == "base"]
    first_bad = [(b["task"], b.get("repeat", 1)) for b, z in ps if b.get("first_in") is None or z.get("first_in") is None or not z["first_in"] < b["first_in"]]
    switches = sum(r.get("model_switch", 0) for r in rs)
    mism = [m for m in version_mismatches(runs) if m[0][0] == provider and m[0][2] == arm]
    return [
        ("input tokens per finished task: Baaz <= bare", per["baaz"] <= per["bare"], f"{per['baaz']:,.0f} vs {per['bare']:,.0f}"),
        ("uncached tokens: Baaz <= bare", unc["baaz"] <= unc["bare"], f"{unc['baaz']:,} vs {unc['bare']:,}"),
        ("finished: Baaz >= bare", len(fin["baaz"]) >= len(fin["bare"]), f"{len(fin['baaz'])} vs {len(fin['bare'])}"),
        ("first request: Baaz < bare on every run", bool(ps) and not first_bad, f"{len(ps) - len(first_bad)} of {len(ps)} pairs" + (f"; not on {first_bad}" if first_bad else "")),
        ("0 <model_switch> blocks", switches == 0, str(switches)),
        ("0 version mismatches", bool(ps) and not mism, f"{len(mism)}" + (f": {mism[:3]}" if mism else "")),
    ]


def report(results, out=None):
    runs = [json.loads(l) for l in open(results) if l.strip()]
    lines = ["# Baaz bench report", "", f"{len(runs)} runs. Finished = the task's own check passes after one prompt.", "",
             "| provider | mode | arm | runs | finished | tokens in / finished | uncached / finished | cached % | requests / task | first request (mean) | cost / finished | done-claims overruled | stuck | wall s (median) |",
             "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    groups = sorted({(r["provider"], r["mode"], r.get("variant", "base"), r.get("arm") or "-") for r in runs},
                    key=lambda g: (g[0], g[3], g[1] != "bare", g[2]))
    for provider, mode, variant, arm in groups:
        rs = [r for r in runs if (r["provider"], r["mode"], r.get("variant", "base"), r.get("arm") or "-") == (provider, mode, variant, arm)]
        fin = [r for r in rs if r["finished"]]
        tin = sum(r["tokens_in"] for r in rs)
        cached = sum(r["cached"] for r in rs)
        costs = [r["cost_usd"] for r in rs if r["cost_usd"] is not None]
        reqs = [len(r["requests"]) for r in rs if r.get("requests")]
        firsts = [r["first_in"] for r in rs if r.get("first_in")]
        per_tok = f"{tin // max(len(fin), 1):,}" if fin else "-"
        per_unc = f"{(tin - cached) // max(len(fin), 1):,}" if fin else "-"
        per_cost = f"${sum(costs) / len(fin):.4f}" if fin and costs else "-"
        overruled = sum(1 for r in rs if r["claims_done"] and not r["finished"])
        stuck = sum(1 for r in rs if r.get("stuck"))
        label = mode if variant == "base" else f"{mode} ({variant})"
        per_req = f"{sum(reqs) / len(reqs):.1f}" if reqs else "-"
        per_first = f"{sum(firsts) // len(firsts):,}" if firsts else "-"
        lines.append(f"| {provider} | {label} | {arm} | {len(rs)} | {len(fin)} | {per_tok} | {per_unc} | {100 * cached // max(tin, 1)}% | "
                     f"{per_req} | {per_first} | {per_cost} | {overruled} | {stuck} | {median([r['wall_s'] for r in rs])} |")
    ps = pairs(runs)
    if ps:
        lines += ["", "## Paired (Baaz \u2212 bare, per task and repeat)", "",
                  "| provider | arm | pairs | input tokens: mean diff \u00b1 SE | as % of bare mean | uncached: mean diff \u00b1 SE | first request: mean diff |", "|---|---|---|---|---|---|---|"]
        for provider, variant, arm in sorted({(k[0], k[1], k[2]) for k in ps}):
            pv = [v for k, v in ps.items() if (k[0], k[1], k[2]) == (provider, variant, arm)]
            d, se = paired([z["tokens_in"] - b["tokens_in"] for b, z in pv])
            du, seu = paired([z.get("uncached", z["tokens_in"] - z["cached"]) - b.get("uncached", b["tokens_in"] - b["cached"]) for b, z in pv])
            fd = [z["first_in"] - b["first_in"] for b, z in pv if z.get("first_in") and b.get("first_in")]
            bare_mean = sum(b["tokens_in"] for b, _ in pv) / len(pv)
            fmt = lambda m, e: f"{m:+,.0f} \u00b1 {e:,.0f}" if e is not None else f"{m:+,.0f}"
            pct = f"{100 * d / bare_mean:+.1f}% \u00b1 {100 * se / bare_mean:.1f}%" if se is not None and bare_mean else "-"
            lines.append(f"| {provider}{'' if variant == 'base' else f' ({variant})'} | {arm} | {len(pv)} | {fmt(d, se)} | {pct} | {fmt(du, seu)} | {f'{sum(fd) / len(fd):+,.0f}' if fd else '-'} |")
    for arm in sorted({r.get("arm") or "-" for r in runs if r["provider"] == "codex"}):
        if not any(k[0] == "codex" and k[2] == arm for k in ps):
            continue
        rows = gate(runs, "codex", arm)
        verdict = "PASS" if all(ok for _, ok, _ in rows) else "FAIL"
        lines += ["", f"## docs/88 \u00a76 checks, Codex, arm `{arm}`: {verdict}", "", "| check | ok | measured |", "|---|---|---|"]
        lines += [f"| {name} | {'yes' if ok else 'NO'} | {detail} |" for name, ok, detail in rows]
    lines += ["", "## Per run", "", "| task | repeat | provider | mode | arm | finished | tokens in | uncached | out | requests | first | version | cost | wall s | error |",
              "|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in sorted(runs, key=run_key):
        cost = f"${r['cost_usd']:.4f}" if r["cost_usd"] is not None else "-"
        mode = r["mode"] if r.get("variant", "base") == "base" else f"{r['mode']} ({r['variant']})"
        lines.append(f"| {r['task']} | {r.get('repeat', 1)} | {r['provider']} | {mode} | {r.get('arm') or '-'} | {'yes' if r['finished'] else 'NO'} | {r['tokens_in']:,} | "
                     f"{r.get('uncached', r['tokens_in'] - r['cached']):,} | {r['tokens_out']:,} | {len(r['requests']) if r.get('requests') else '-'} | "
                     f"{r.get('first_in') or '-'} | {','.join(r.get('cli_versions') or []) or '-'} | {cost} | {r['wall_s']} | {(r['error'] or '')[:80]} |")
    explore = [r for r in runs if r.get("variant") == "explore"]
    if explore:
        used = sum(1 for r in explore if any("haiku" in m for m in r.get("models", [])))
        lines += ["", f"Explore variant: the Haiku agent ran in {used} of {len(explore)} runs (modelUsage)."]
    lines += ["", "Interventions and approval wait: every run is one prompt in Full access (bare: the CLI's own bypass), so neither occurs by design; a run left waiting on the person is counted as stuck."]
    text = "\n".join(lines) + "\n"
    if out:
        open(out, "w").write(text)
    return text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work")
    ap.add_argument("--report", help="only write the report for this results.jsonl")
    ap.add_argument("--out")
    ap.add_argument("--tasks", default="")
    ap.add_argument("--providers", default="claude,codex")
    ap.add_argument("--modes", default="bare,baaz")
    ap.add_argument("--claude-model", default="haiku")
    ap.add_argument("--codex-model", default="gpt-5.6-luna")
    ap.add_argument("--codex-effort", default="medium")
    ap.add_argument("--timeout", type=int, default=600)
    ap.add_argument("--variant", default="base", choices=["base", "no-web", "explore"],
                    help="bare Claude only (docs/85 17.2): WebFetch/WebSearch off, or an Explore agent on Haiku")
    ap.add_argument("--arm", default="clean", choices=["clean", "installed"],
                    help="Codex homes (docs/88 \u00a73): clean = none of the owner's files on either side; installed = the owner's set on both")
    ap.add_argument("--repeat", type=int, default=1, help="run every task N times (the run key carries the repeat)")
    args = ap.parse_args()
    if args.report:
        print(report(args.report, args.out))
        return
    if not args.work:
        ap.error("--work is required")
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    results = os.path.join(work, "results.jsonl")
    done = set()
    if os.path.exists(results):
        done = {run_key(r) for r in map(json.loads, open(results))}
    codex, codex_version = resolve_codex() if "codex" in args.providers.split(",") else (None, None)
    tasks = sorted(os.listdir(TASKS))
    pick = [tasks[i - 1] for i in parse_range(args.tasks, len(tasks))]
    log = lambda msg: print(msg, flush=True)
    if codex:
        log(f"codex: {codex} ({codex_version}) on both sides; arm {args.arm}")
    for repeat in range(1, args.repeat + 1):
        for task in pick:
            spec = json.load(open(os.path.join(TASKS, task, "task.json")))
            for provider in args.providers.split(","):
                arm = args.arm if provider == "codex" else None
                for mode in args.modes.split(","):
                    if (task, provider, mode, args.variant, arm or "-", repeat) in done:
                        continue
                    suffix = ("" if args.variant == "base" else f"-{args.variant}") + (f"-{arm}" if arm else "") + (f"-r{repeat}" if args.repeat > 1 or repeat > 1 else "")
                    run_dir = os.path.join(work, f"{task}-{provider}-{mode}{suffix}")
                    shutil.rmtree(run_dir, ignore_errors=True)
                    os.makedirs(run_dir)
                    repo = make_repo(os.path.join(TASKS, task), run_dir)
                    log(f"{task} r{repeat} {provider} {mode} ...")
                    try:
                        if mode == "bare" and provider == "claude":
                            m = bare_claude(repo, run_dir, spec["prompt"], args.claude_model, args.timeout, args.variant)
                        elif mode == "bare":
                            m = bare_codex(repo, run_dir, spec["prompt"], args.codex_model, args.codex_effort, args.timeout, codex, arm)
                        else:
                            model = args.claude_model if provider == "claude" else args.codex_model
                            effort = args.codex_effort if provider == "codex" else None
                            m = baaz_run(repo, run_dir, spec["prompt"], provider, model, effort, args.timeout, log, codex, arm)
                    except subprocess.TimeoutExpired:
                        m = {"tokens_in": 0, "tokens_out": 0, "cached": 0, "cost_usd": None, "final": "", "agent_turns": None,
                             "wall_s": args.timeout, "error": "timeout", "stuck": True}
                    if provider == "codex" and m.get("cli_versions") not in (None, [codex_version]):
                        m["error"] = "; ".join(filter(None, [m.get("error"), f"version mismatch: ran {m.get('cli_versions')}, resolved {codex_version}"]))
                    check = sh(spec["check"], repo)
                    row = dict(m, task=task, provider=provider, mode=mode, variant=args.variant, arm=arm, repeat=repeat,
                               binary=codex if provider == "codex" else None, finished=check.returncode == 0,
                               claims_done=claims_done(m.get("final")), check_tail=(check.stdout + check.stderr)[-300:])
                    row["final"] = (row.get("final") or "")[:500]
                    with open(results, "a") as f:
                        f.write(json.dumps(row) + "\n")
                    log(f"  finished={row['finished']} tokens_in={row['tokens_in']} first={row.get('first_in')} err={row['error']}")
    print(report(results, os.path.join(work, "report.md")))


if __name__ == "__main__":
    main()
