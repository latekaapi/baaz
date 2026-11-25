#!/usr/bin/env python3
"""Tool-choice eval (docs/85 13.6, rule 19): does each provider pick Baaz's
right bridge tool for what a person says?

A budgeted LIVE script: every phrasing is one real one-line turn on the
provider's cheapest model. Never run from `cargo test`.

How it works, per provider:
  * a throwaway repository under --work (rule 24): a few files, one commit,
    an uncommitted change, a local bare remote as `origin`;
  * the real `mcp-bridge` binary as the provider's MCP server, pointed at a
    stub socket this script serves: it records each call (tool, arguments)
    and answers a short canned success, so the agent can finish its turn;
  * Baaz's appended note (`crates/provider/src/note.rs`, the bridge lines)
    is given the way Baaz gives it (Claude: --append-system-prompt; Codex:
    developer_instructions; Muse: the prompt's preface, Muse has no channel);
  * a phrasing passes when the first Baaz tool the agent calls is one of the
    expected ones.

Usage:
  python3 scripts/tool_choice_eval.py --work <scratch dir> [--providers claude,codex,muse]
      [--only 1-10] [--codex-model <id>] [--claude-model haiku] [--muse-model <id>]

Writes <work>/results-<provider>.jsonl and prints a table. Exit 0 always
(a rate is a measurement, not a gate).
"""
import argparse, json, os, re, shutil, socket, subprocess, sys, threading, time

HARNESS = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BRIDGE = os.path.join(HARNESS, "target", "debug", "mcp-bridge")

# (phrasing, acceptable first tools). File names refer to the fixture repo.
CASES = [
    ("Open src/app.py in the preview.", {"pane_show"}),
    ("Show me src/app.py at line 3 in the preview pane.", {"pane_show"}),
    ("Open README.md in the preview so I can read it.", {"pane_show"}),
    ("Reveal src/util.py in the file tree.", {"pane_show"}),
    ("Show me the diff in src/app.py.", {"pane_show"}),
    ("Show me the diff of src/app.py in the pane.", {"pane_show"}),
    ("Show what you changed last turn.", {"pane_show"}),
    ("Open the Changes pane.", {"pane_show"}),
    ("Switch to the terminal.", {"pane_show"}),
    ("Show me the Tasks pane.", {"pane_show"}),
    ("Show me the pull request.", {"pane_show", "pr_status"}),
    ("Switch the pane to the browser.", {"pane_show"}),
    ("Put the file tree on screen.", {"pane_show"}),
    ("What did the last turn change?", {"changes_read"}),
    ("What have you changed in this session? Just the list of files.", {"changes_read"}),
    ("Give me a diff stat of all uncommitted changes, as Baaz's Changes pane has it.", {"changes_read"}),
    ("What changed in turn 2?", {"changes_read"}),
    ("Read the diff of src/app.py from the Changes pane.", {"changes_read"}),
    ("Undo your change to src/app.py.", {"changes_revert"}),
    ("Revert src/util.py to how it was in the last commit, through the Changes pane.", {"changes_revert"}),
    ("Take back what the last turn did to src/app.py.", {"changes_revert"}),
    ("Leave a review comment on line 2 of src/app.py saying the name is unclear.", {"changes_comment"}),
    ("Add a comment in the Changes pane on src/util.py line 1: needs a test.", {"changes_comment"}),
    ("Commit this with the message 'Tidy app'.", {"changes_commit"}),
    ("Commit and push this.", {"changes_commit"}),
    ("Commit the changes through the commit box, message 'WIP'.", {"changes_commit"}),
    ("Did CI pass on the pull request?", {"pr_status"}),
    ("Any review comments on the PR?", {"pr_status"}),
    ("What's the state of the pull request's checks?", {"pr_status"}),
    ("Find the session where we decided the release day.", {"session_search"}),
    ("Have we talked about the postcode rule in an earlier session? Look it up.", {"session_search"}),
    ("Search past sessions for anything about the login bug.", {"session_search"}),
    ("What did I say in an earlier session about tabs versus spaces?", {"session_search"}),
    ("Remember that releases go out on Thursdays.", {"memory_save"}),
    ("Save a note for later sessions: we use pnpm, not npm.", {"memory_save"}),
    ("Forget the note about release day.", {"memory_edit"}),
    ("Where is load_config defined?", {"code_find"}),
    ("Find every use of load_config in the project.", {"code_find"}),
    ("Find the definition of the class Store.", {"code_find"}),
    ("Run the tests in the terminal so I can watch.", {"terminal_run", "terminal_open"}),
    ("Start the dev server in a terminal tab.", {"terminal_run", "terminal_open"}),
    ("What's on the terminal screen right now?", {"terminal_screen", "terminal_read", "terminal_list"}),
    ("List the terminal tabs.", {"terminal_list"}),
    ("Open example.com in the browser.", {"browser_navigate", "browser_tabs"}),
    ("Take a screenshot of the page in the browser pane.", {"browser_screenshot", "browser_navigate"}),
    ("What's on the page in my browser pane?", {"browser_snapshot", "browser_read", "browser_tabs", "browser_screenshot"}),
    ("Show me the file src/util.py at line 1.", {"pane_show"}),
    ("Open the diff for src/util.py so I can review it.", {"pane_show"}),
    ("Show me what changed since this session began, in the pane.", {"pane_show"}),
    ("Check the PR checks and tell me which ones fail.", {"pr_status"}),
]

STUB_ANSWERS = {
    "session_search": {"results": [], "scope": "project"},
    "changes_read": {"scope": "last turn", "files": [{"path": "src/app.py", "change": "modified", "added": 1, "removed": 0}], "total": {"files": 1, "added": 1, "removed": 0}},
    "pr_status": {"pr": {"number": 7, "state": "open"}, "checks": {"passed": 2, "failing": 0, "running": 0, "failing_checks": []}, "comments": []},
    "code_find": {"results": [{"path": "src/util.py", "line": 1, "kind": "definition", "text": "def load_config():"}]},
}


def note_text():
    """The appended note's lines as Baaz composes it for a bridge session."""
    src = open(os.path.join(HARNESS, "crates", "provider", "src", "note.rs")).read()
    body = src[src.index("pub fn compose"):src.index("lines.join")]
    out = []
    for literal in re.findall(r'"((?:[^"\\]|\\.)*)"', body, re.S):
        text = re.sub(r"\\\s*\n\s*", "", literal).replace('\\"', '"')
        out.append(text)
    return "\n".join(out)


class Stub:
    """The Baaz socket, stubbed: records each call per session."""

    def __init__(self, path):
        self.path = path
        self.calls = {}
        self.lock = threading.Lock()
        if os.path.exists(path):
            os.unlink(path)
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.bind(path)
        self.sock.listen(16)
        threading.Thread(target=self.serve, daemon=True).start()

    def serve(self):
        while True:
            try:
                conn, _ = self.sock.accept()
            except OSError:
                return
            threading.Thread(target=self.answer, args=(conn,), daemon=True).start()

    def answer(self, conn):
        with conn, conn.makefile("rw") as f:
            line = f.readline()
            if not line:
                return
            req = json.loads(line)
            with self.lock:
                self.calls.setdefault(req.get("session"), []).append((req.get("tool"), req.get("params")))
            result = STUB_ANSWERS.get(req.get("tool"), {"ok": True, "note": "done"})
            f.write(json.dumps({"id": req.get("id"), "ok": True, "result": result}) + "\n")
            f.flush()

    def first(self, session):
        with self.lock:
            calls = self.calls.get(session, [])
        return calls[0] if calls else (None, None)


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


def fixture(work):
    repo = os.path.join(work, "repo")
    bare = os.path.join(work, "origin.git")
    for d in (repo, bare):
        shutil.rmtree(d, ignore_errors=True)
    os.makedirs(os.path.join(repo, "src"))
    open(os.path.join(repo, "README.md"), "w").write("# Shop\n\nA tiny shop.\n")
    open(os.path.join(repo, "src", "app.py"), "w").write("from util import load_config\n\nconfig = load_config()\nprint(config)\n")
    open(os.path.join(repo, "src", "util.py"), "w").write("def load_config():\n    return {}\n\nclass Store:\n    pass\n")
    git(work, "init", "-q", "--bare", "-b", "main", bare)
    git(repo, "init", "-q", "-b", "main")
    git(repo, "-c", "user.name=eval", "-c", "user.email=eval@example.invalid", "add", ".")
    git(repo, "-c", "user.name=eval", "-c", "user.email=eval@example.invalid", "commit", "-qm", "init")
    git(repo, "remote", "add", "origin", bare)
    open(os.path.join(repo, "src", "app.py"), "a").write("print('changed')\n")
    return repo


def bridge_args(sock, session):
    return ["--terminal", "--socket", sock, "--session", session]


def run_claude(case_i, phrase, repo, work, sock, model, note):
    home = os.path.join(work, "claude-home")
    os.makedirs(home, exist_ok=True)
    session = f"eval-claude-{case_i}"
    mcp = os.path.join(work, f"mcp-{session}.json")
    json.dump({"mcpServers": {"baaz": {"type": "stdio", "command": BRIDGE, "args": bridge_args(sock, session), "env": {"BAAZ_BRIDGE_TOKEN": "eval"}}}}, open(mcp, "w"))
    env = dict(os.environ, CLAUDE_CONFIG_DIR=home, CLAUDE_SECURESTORAGE_CONFIG_DIR="")
    argv = ["claude", "-p", phrase, "--output-format", "stream-json", "--verbose", "--model", model,
            "--permission-mode", "bypassPermissions", "--mcp-config", mcp, "--strict-mcp-config",
            "--append-system-prompt", note, "--max-turns", "4"]
    out = subprocess.run(argv, cwd=repo, env=env, capture_output=True, text=True, timeout=300)
    cost = None
    for line in out.stdout.splitlines():
        try:
            m = json.loads(line)
        except Exception:
            continue
        if m.get("type") == "result":
            cost = m.get("total_cost_usd")
    return session, out.stdout, cost


def run_codex(case_i, phrase, repo, work, sock, model, note):
    home = os.path.join(work, "codex-home")
    os.makedirs(home, exist_ok=True)
    auth = os.path.join(home, "auth.json")
    if not os.path.lexists(auth):
        os.symlink(os.path.expanduser("~/.codex/auth.json"), auth)
    session = f"eval-codex-{case_i}"
    toml_list = "[" + ",".join(json.dumps(a) for a in bridge_args(sock, session)) + "]"
    argv = ["codex", "exec", "--json", "--skip-git-repo-check", "-s", "workspace-write",
            "-c", f"mcp_servers.baaz.command={json.dumps(BRIDGE)}",
            "-c", f"mcp_servers.baaz.args={toml_list}",
            "-c", 'mcp_servers.baaz.env={BAAZ_BRIDGE_TOKEN="eval"}',
            "-c", f"developer_instructions={json.dumps(note)}"]
    if model:
        argv += ["-m", model]
    argv.append(phrase)
    env = dict(os.environ, CODEX_HOME=home)
    out = subprocess.run(argv, cwd=repo, env=env, capture_output=True, text=True, timeout=300)
    return session, out.stdout + out.stderr[-2000:], None


def run_muse(case_i, phrase, repo, work, sock, model, note):
    session = f"eval-muse-{case_i}"
    servers = {"baaz": {"command": BRIDGE, "args": bridge_args(sock, session), "env": {"BAAZ_BRIDGE_TOKEN": "eval"}}}
    argv = ["muse", "exec", "--json", "--workspace", repo, "-c", f"mcpServers={json.dumps(servers)}", "--max-model-steps", "4"]
    if model:
        argv += ["--model", model]
    argv.append(f"{note}\n\n{phrase}")
    out = subprocess.run(argv, cwd=repo, capture_output=True, text=True, timeout=300)
    return session, out.stdout + out.stderr[-2000:], None


RUNNERS = {"claude": run_claude, "codex": run_codex, "muse": run_muse}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--providers", default="claude,codex,muse")
    ap.add_argument("--only", default="")
    ap.add_argument("--claude-model", default="haiku")
    ap.add_argument("--codex-model", default="")
    ap.add_argument("--muse-model", default="")
    args = ap.parse_args()
    if not os.path.exists(BRIDGE):
        sys.exit(f"build the bridge first: cargo build -p mcp-bridge ({BRIDGE})")
    os.makedirs(args.work, exist_ok=True)
    cases = list(enumerate(CASES, 1))
    if args.only:
        lo, _, hi = args.only.partition("-")
        cases = [c for c in cases if int(lo) <= c[0] <= int(hi or lo)]
    note = note_text()
    sock = os.path.join(args.work, "baaz-stub.sock")
    stub = Stub(sock)
    summary = {}
    for provider in [p for p in args.providers.split(",") if p]:
        model = {"claude": args.claude_model, "codex": args.codex_model, "muse": args.muse_model}[provider]
        results_path = os.path.join(args.work, f"results-{provider}.jsonl")
        passed = total = 0
        with open(results_path, "a") as results:
            for i, (phrase, expected) in cases:
                repo = fixture(args.work)
                started = time.time()
                try:
                    session, raw, cost = RUNNERS[provider](i, phrase, repo, args.work, sock, model, note)
                except subprocess.TimeoutExpired:
                    session, raw, cost = f"eval-{provider}-{i}", "TIMEOUT", None
                tool, params = stub.first(session)
                ok = tool in expected
                passed += ok
                total += 1
                row = {"case": i, "phrase": phrase, "expected": sorted(expected), "first_tool": tool, "params": params, "pass": ok,
                       "secs": round(time.time() - started, 1), "cost_usd": cost}
                results.write(json.dumps(row) + "\n")
                results.flush()
                with open(os.path.join(args.work, f"raw-{provider}-{i}.log"), "w") as f:
                    f.write(raw)
                print(f"{provider:6} {i:2} {'PASS' if ok else 'FAIL'} {tool or '-':18} {phrase}", flush=True)
        summary[provider] = (passed, total)
    print()
    for provider, (passed, total) in summary.items():
        print(f"{provider}: {passed}/{total} = {100 * passed / max(total, 1):.0f}%")


if __name__ == "__main__":
    main()
