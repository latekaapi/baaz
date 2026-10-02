#!/usr/bin/env python3
"""uiprobe/1 adapter for baaz — a shim, not new capability.

Everything here already existed as a baaz flag. This only translates relay's
contract onto it and normalises the output to the contract's JSON.

Entries are named screens. All of them run offline (`--no-connect`), so a probe
run costs nothing and reaches no model.

    uiprobe.py --entry login-choose --probe-shot out.png
    uiprobe.py --entry login-choose --probe-idle 2000
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time

# How many captures to take looking for two that agree.
SETTLE_TRIES = 6

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BAAZ = os.path.join(REPO, "target", "debug", "baaz")

# The scripted statuses behind `connect-first-run` and
# `launch-cached-shell`: one Connected (Muse, with email and plan), one
# Signed out (Claude Code, installed), one Not installed (Codex). Shaped
# exactly as `<state>/provider-status.json`, the cache the status service
# reads at boot.
CONNECT_MIXED = json.dumps([
    {"provider": "muse", "installed": {"Yes": {"version": "1.4.0", "path": "/usr/local/bin/muse"}},
     "auth": {"SignedIn": {"email": "ada@example.com", "plan": "Pro", "method": "account"}},
     "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
    {"provider": "claude-code", "installed": {"Yes": {"version": "2.1.276", "path": "/opt/homebrew/bin/claude"}},
     "auth": "SignedOut",
     "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
    {"provider": "codex", "installed": "No", "auth": "Unknown",
     "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
])

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

def _migrate_fixture():
    """The `migrate-sessions` fixture: a fake registry in a temp state dir
    plus a fake owner home holding those sessions' files. Built once at
    import; the entry below points baaz at both (offline, temp dirs only —
    the real HOME, ~/.claude, ~/.codex and the real Baaz state dir are
    never touched). No baselines: capture and look."""
    state = tempfile.mkdtemp(prefix="uiprobe-migrate-state-")
    owner = tempfile.mkdtemp(prefix="uiprobe-migrate-owner-")
    now_ms = int(time.time() * 1000)
    registry = {
        "sess-probe-1": {"provider": "claude-code", "sessionId": "sess-probe-1",
                         "createdMs": now_ms, "updatedMs": now_ms},
        "thread-probe-1": {"provider": "codex", "sessionId": "thread-probe-1",
                           "createdMs": now_ms, "updatedMs": now_ms},
    }
    with open(os.path.join(state, "provider-sessions.json"), "w") as f:
        json.dump(registry, f)
    slug = os.path.join(owner, ".claude", "projects", "-work")
    os.makedirs(slug)
    with open(os.path.join(slug, "sess-probe-1.jsonl"), "w") as f:
        f.write('{"id":1}\n')
    day = os.path.join(owner, ".codex", "sessions", "2026", "10", "02")
    os.makedirs(day)
    with open(os.path.join(day, "rollout-2026-10-02-x-thread-probe-1.jsonl"), "w") as f:
        f.write('{}\n')
    return state, owner


_MIGRATE_STATE, _MIGRATE_OWNER = _migrate_fixture()


def _usage_script():
    now = int(time.time())
    def status(provider, email, plan, usage):
        return {
            "provider": provider,
            "installed": {"Yes": {"version": "9.9.9", "path": "/bin/" + provider}},
            "auth": {"SignedIn": {"email": email, "plan": plan, "method": "account"}},
            "enabled": True,
            "advisory": "None",
            "checked_at": now,
            "usage": usage,
        }
    def usage(provider, plan, windows):
        return {
            "provider": provider,
            "plan": plan,
            "windows": [
                {"label": label, "used_fraction": used, "resets_at": resets}
                for (label, used, resets) in windows
            ],
            "as_of": now,
        }
    return [
        status("muse", "latekaapi@gmail.com", "High Usage", usage(
            "muse", "High Usage", [("Weekly", 0.01, now + 2 * 86400)])),
        status("claude-code", None, "Pro", usage(
            "claude-code", "Pro", [("Session · 5h", 0.42, now + 41 * 60),
                                      ("Weekly", 0.83, now + (2 * 24 + 4) * 3600)])),
        status("codex", None, "prolite", usage(
            "codex", "prolite", [("Weekly", 0.19, now + 7 * 86400)])),
    ]

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
    # The Browser pane on a loaded page: `browse:` navigates the scripted
    # backend (captures always run the fake page, never a native view), so
    # the entry captures deterministically like `right-browser` does.
    "right-browser-page":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:browser;browse:https://example.com"]},
    "right-changes":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:changes"]},
    "right-files":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files"]},
    # The Files pane previewing a small file: the same preview a tree
    # click opens (header plus the file's own bytes), reached through the
    # click's own `files-select:` verb. No baseline yet — generate on main
    # after merge, never to silence a finding.
    "right-file-preview":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;files-select:uiprobe.json"]},
    "right-closed":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;right:off"]},
    # The ⌘K palette, open. It had no coverage at all, which is why a list
    # that never scrolls to its selection went unseen: nothing ever drew it.
    "palette":              {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "palette"]},
    # The Settings page's Shortcuts section: the live keymap rows grouped
    # by category with the Reserved group folded to one row. `settings:`
    # is a window-only steps verb, so it runs with no session open. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "settings-shortcuts":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:shortcuts"]},
    # The Settings page's General section (model housekeeping): offline,
    # idempotent (`settings:` opens, never toggles). No baseline yet —
    # generate on main after merge, never to silence a finding.
    "settings-general":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:general"]},
    # The Settings page's Archived section: offline, idempotent. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "settings-archived":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:archived"]},
    # A Settings provider sub-page (Codex), on scripted statuses like
    # `settings-providers`: offline, idempotent, deterministic. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "settings-provider-codex": {"shot": ["--no-connect", "--login", "signed-in",
                                         "--steps", "settings:providers/codex"],
                                "env": {"BAAZ_PROVIDER_STATUS_SCRIPT":
                                         '[{"provider":"muse","installed":{"Yes":{"version":"1.4.0","path":"/bin/muse"}},'
                                         '"auth":{"SignedIn":{"email":"ada@example.com","plan":"Pro","method":"oauth"}},'
                                         '"enabled":true,"advisory":"None","checked_at":1,"usage":null},'
                                         '{"provider":"claude-code","installed":{"Yes":{"version":"2.1.276","path":"/bin/claude"}},'
                                         '"auth":"SignedOut","enabled":true,"advisory":"None","checked_at":1,"usage":null},'
                                         '{"provider":"codex","installed":"No",'
                                         '"auth":"Unknown","enabled":true,"advisory":"None","checked_at":1,"usage":null}]'}},
    # The composer's three pickers, each open over a fresh session. `new`
    # heads every script: offline no boot session opens, so a bare session
    # verb never becomes ready — and the head is what makes these start.
    # Each run boots with every picker closed, so the toggle lands open on
    # every capture; the verbs persist nothing, so the second run agrees
    # with the first (unlike the old toggling `right:<kind>` verb, which
    # wrote `layout.json` and flapped).
    "composer-provider":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;provider"]},
    # The provider menu on a session WITH turns: each other backend reads
    # "Hand off to X…" first, then "New session on X". The send goes to
    # the scripted lane (offline, free); the wait lets its deltas land so
    # `has_turns` is true before the menu opens. No baseline yet —
    # generate on main after merge, never to silence a finding.
    "composer-handoff":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:codex;send:hello handoff probe;wait:3000;provider"]},
    "composer-model":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;model"]},
    "composer-effort":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;effort"]},
    # One composer per non-muse lane, reached by the scripted provider
    # pick. `setprovider:` is a setter through the picker's own path —
    # not a toggle — so both runs land on the lane's composer and
    # agree. Like the menu, the pick is remembered in the store as the
    # default for new sessions, so running these moves `provider.json`.
    "composer-claude-code":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:claude-code"]},
    "composer-codex":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:codex"]},
    # One empty provider session per non-muse lane, reached by the same
    # scripted pick as the composer entries above: the empty state plus
    # the composer on the lane, so Tier V can see provider sessions.
    # Offline only — no turn is ever sent from an entry.
    "lane-claude-code":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:claude-code"]},
    "lane-codex":            {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:codex"]},
    # The inline reopen failure (Z4): the scripted lane mints one fixed id
    # (`s-scripted`), so `send:` settles a first prompt onto the record —
    # a turn-less record would open fresh, not fail — and `open:` reopens
    # that same lane through `ResumeSession`, which the scripted provider
    # refuses: the honest offline failure, inline on the clicked session.
    # Offline, deterministic, free like `composer-handoff`'s send. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "reopen-failed":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:codex;send:hello reopen probe;wait:3000;open:s-scripted"]},
    # Settings → Providers, open on scripted statuses covering Connected
    # (Muse), Signed out (Claude Code) and Not installed (Codex):
    # offline, deterministic, no baseline yet — generate on main after
    # merge, never to silence a finding. Capture and look.
    "settings-providers":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:providers"],
                             "env": {"BAAZ_PROVIDER_STATUS_SCRIPT":
                                      '[{"provider":"muse","installed":{"Yes":{"version":"1.4.0","path":"/bin/muse"}},'
                                      '"auth":{"SignedIn":{"email":"ada@example.com","plan":"Pro","method":"oauth"}},'
                                      '"enabled":true,"advisory":"None","checked_at":1,"usage":null},'
                                      '{"provider":"claude-code","installed":{"Yes":{"version":"2.1.276","path":"/bin/claude"}},'
                                      '"auth":"SignedOut","enabled":true,"advisory":"None","checked_at":1,"usage":null},'
                                      '{"provider":"codex","installed":"No",'
                                      '"auth":"Unknown","enabled":true,"advisory":"None","checked_at":1,"usage":null}]'}},
    # The Skills page, open on a fixture catalog (no CLI, deterministic,
    # offline): the full page with its detail pane, and the empty-project
    # state. `skills:` is a window-only steps verb, so `--login signed-in
    # --no-connect` runs it with no session open.
    "skills-page":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:page"]},
    "skills-empty":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:empty"]},
    # The K3 dialogs over the page fixture (no CLI, deterministic,
    # offline): the Add menu with its cached trails, the import preview
    # over fixed rows, and the New dialog with empty fields. Each steps
    # verb sets its state rather than toggling it, so both settle shots
    # agree.
    "skills-add-menu":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:add-menu"]},
    "skills-import-preview": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:import-preview"]},
    "skills-new":           {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:new"]},
    # The Skills page with the terminal dock open (Y3a): the dock steals
    # centre height, which once pushed the dock itself below the fold (a
    # full-height page root) and centred the scroll box up into the header
    # (a cross-centred body row). `project:.` adopts the checkout the probe
    # runs from so the dock has a root; `terminal-dock:` opens the
    # deterministic FakePty tab. Offline. No baseline yet — generate on
    # main after merge, never to silence a finding.
    "skills-dock":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "project:.;skills:page;terminal-dock:terminal"]},
    "transcript-markdown":  {"idle": ["--bench", "fixtures/msp/synthetic-markdown.jsonl"]},
    "transcript-toolshapes": {"idle": ["--bench", "fixtures/msp/synthetic-toolshapes.jsonl"]},
    "transcript-stress":    {"idle": ["--bench", "fixtures/msp/synthetic-stress-300.jsonl"]},
    # Y5: the first-run Connect your providers screen, offline with scripted
    # statuses (`BAAZ_PROVIDER_STATUS_SCRIPT` — the only statuses a
    # deterministic run reports). `connect-first-run` is mixed rows
    # (Connected / Signed out / Not installed); `connect-checking` scripts
    # nothing, so every row reads Checking. `launch-cached-shell` is the
    # returning launch: cached statuses, `--login signed-in`, straight into
    # the shell with no sign-in screen.
    "connect-first-run":    {"shot": ["--no-connect", "--login", "connect"],
                             "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": CONNECT_MIXED}},
    "connect-checking":     {"shot": ["--no-connect", "--login", "connect"]},
    "launch-cached-shell":  {"shot": ["--no-connect", "--login", "signed-in"],
                             "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": CONNECT_MIXED}},
    # The account menu, open: one usage card per Connected provider (the
    # weekly card at 83% exercises the warning ink), scripted through
    # BAAZ_PROVIDER_STATUS_SCRIPT — offline, deterministic, free. The
    # timestamps are fixed at import so every settle capture agrees. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "account-usage":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "account"],
                               "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": json.dumps(_usage_script())}},
    # Settings → Providers with one Claude Code and one Codex session
    # still waiting in the fixture owner home: the page carries the B4M
    # migration row (the "Move 2 …" prompt surface with its dry-run paths
    # and Move button). Offline, deterministic, fixture only — no
    # baseline yet, generate on main after merge, never to silence a
    # finding. Capture and look.
    "migrate-sessions":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:providers"],
                             "env": {"BAAZ_STATE_DIR": _MIGRATE_STATE,
                                     "BAAZ_MIGRATION_OWNER_HOME": _MIGRATE_OWNER,
                                     "BAAZ_MIGRATION_FIXTURE": "1"}},
}


# B8 merged Diff review and Git changes into one Changes view: the old
# `right-diff` and `right-git` entry names stay as aliases of
# `right-changes` (same steps, same screen), so older invocations keep
# working. No baselines: capture and look.
ENTRIES["right-diff"] = ENTRIES["right-changes"]
ENTRIES["right-git"] = ENTRIES["right-changes"]


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

    # An entry's own environment rides on top of the probe's: scripted
    # statuses and other per-screen fixtures that must not leak across
    # entries. `boot` validates the entry first, so the lookup below
    # cannot KeyError.
    def merged_env(verb):
        boot(a.entry, verb)
        return {**PROBE_ENV, **ENTRIES[a.entry].get("env", {})}

    if a.probe_shot:
        env = merged_env("shot")
        cmd = boot(a.entry, "shot") + extra
        # Per-entry scripted environment (provider statuses for the connect
        # entries): merged over the deterministic base, entry only.
        env = {**PROBE_ENV, **ENTRIES[a.entry].get("env", {})}
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
                               timeout=180, env=env)
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
        env = merged_env("idle")
        cmd = boot(a.entry, "idle") + extra
        # `--bench` already measures frames drawn at rest and writes
        # `idle_frames_2s`. Translate it onto the contract's shape.
        out = os.path.join(tempfile.mkdtemp(), "bench.json")
        entry_env = {**PROBE_ENV, **ENTRIES[a.entry].get("env", {})}
        p = subprocess.run(cmd + ["--bench-frames", "60", "--bench-out", out],
                           cwd=REPO, capture_output=True, text=True, timeout=180,
                           env=entry_env)
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
