#!/usr/bin/env python3
"""uiprobe/1 adapter for baaz — a shim, not new capability.

Everything here already existed as a baaz flag. This only translates relay's
contract onto it and normalises the output to the contract's JSON.

Entries are named screens. All of them run offline (`--no-connect`), so a probe
run costs nothing and reaches no model.

    uiprobe.py --entry signed-in --probe-shot out.png
    uiprobe.py --entry signed-in --probe-idle 2000
"""
import argparse, json, os, shutil, subprocess, sys, tempfile, time
import atexit

# Every temporary folder this process makes lives under one run folder that
# goes when the process ends. The entries below build their state folders
# at import, one per entry, and a capture uses one: before this, each run
# left ~150 behind (267,000 `uiprobe-populated-state-*` folders, 40 GB, by
# Gate 6). `REAL_TMP` is the system temp folder, for the one fixture that
# is meant to outlive a run (`_new_row_repo`: its path shows in a menu, so
# it keeps one fixed home).
REAL_TMP = tempfile.gettempdir()
RUN_TMP = tempfile.mkdtemp(prefix="uiprobe-run-")
atexit.register(shutil.rmtree, RUN_TMP, ignore_errors=True)
tempfile.tempdir = RUN_TMP


def keep_display_awake(seconds=900):
    """Hold the display awake while this script measures or captures.

    gpui-pre 0.3.12 paces frames on the display link, which stops while the
    display sleeps: a capture then misses frames that settle a layout (a
    banner that grows in a second step sat under the composer, 16 Nov), and
    `switch.cached_300_ms` read ~930 ms instead of ~10. `caffeinate -u`
    declares user activity, which wakes the display; it takes no focus and
    the process ends with this script. macOS only; elsewhere a no-op.

    Measured 16 Nov: after the display slept on its own this restores both
    (banner whole, switch 10 ms). After a forced `pmset displaysleepnow` it
    does not: the display wakes but the window still is not painted. So a
    ~930 ms switch, or a grown banner cut under the composer, means the
    window was not painting, not a regression; rerun with the screen on.
    """
    import atexit
    if sys.platform != "darwin" or not os.path.exists("/usr/bin/caffeinate"):
        return
    proc = subprocess.Popen(["/usr/bin/caffeinate", "-u", "-t", str(seconds)],
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    atexit.register(proc.terminate)
    time.sleep(1.0)

def pointer_over_capture(w=1440, h=900):
    """The pointer's position when it rests over the capture window (opened
    at the display's top-left, `w`×`h` points), else None. A resting pointer
    there hovers whatever sits under it: on 16 Nov `launch-failure` captured
    a sidebar hover card (with a real path in it) because the pointer had
    been left at (150, 349). macOS only; elsewhere None."""
    if sys.platform != "darwin":
        return None
    try:
        import ctypes, ctypes.util
        cg = ctypes.CDLL(ctypes.util.find_library("CoreGraphics"))

        class Point(ctypes.Structure):
            _fields_ = [("x", ctypes.c_double), ("y", ctypes.c_double)]

        cg.CGEventCreate.restype = ctypes.c_void_p
        cg.CGEventGetLocation.restype = Point
        cg.CGEventGetLocation.argtypes = [ctypes.c_void_p]
        cg.CFRelease.argtypes = [ctypes.c_void_p]
        event = cg.CGEventCreate(None)
        at = cg.CGEventGetLocation(event)
        cg.CFRelease(event)
    except (OSError, AttributeError):
        return None
    return (at.x, at.y) if 0 <= at.x < w and 0 <= at.y < h else None


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
#   shot: a screen state, booted offline. Free, ~2s, no running turn.
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
        status("muse", "ada@example.com", "High Usage", usage(
            "muse", "High Usage", [("Weekly", 0.01, now + 2 * 86400)])),
        status("claude-code", None, "Pro", usage(
            "claude-code", "Pro", [("Session · 5h", 0.42, now + 41 * 60),
                                      ("Weekly", 0.83, now + (2 * 24 + 4) * 3600)])),
        status("codex", None, "prolite", usage(
            "codex", "prolite", [("Weekly", 0.19, now + 7 * 86400)])),
    ]

# The P6 entries' representative fixture state (see them below). Each
# entry gets its own throwaway state dir, made once at import — never the
# real store, and never another entry's (a scripted send lands a row and a
# provider pick there). On it: the scripted sidebar the README captures use
# (`--sidebar-fixture`, rows instead of skeleton bars — allowed offline) and
# scripted statuses for the footer account and the Providers rows, so the
# capture reads like a signed-in window instead of "Checking providers...".
def _p6_sidebar_fixture(name="projects.json"):
    """fixtures/sidebar/<name> re-dated relative to now. The rows'
    labels ("3d", "12d") and date groups read against the newest row —
    the live session in `child-lost-*` (stamped at the capture's wall
    clock), the newest fixture row in `launch-failure*` — so the fixture's
    fixed September dates would drift a day in every baseline each day
    ("47d" → "48d"). The offsets are 3.5 days plus whole days: whole days
    apart from each other (so against the newest fixture row every row
    keeps its time of day and its calendar group), and half a day past a
    whole day against the wall clock, never where the day before or after
    would cross a 2/7/31-day group edge. Any time of day reads the same."""
    with open(os.path.join(REPO, "fixtures", "sidebar", name)) as f:
        rows = json.load(f)
    days = [3.5, 3.5, 4.5, 4.5, 5.5, 7.5, 8.5, 9.5, 10.5, 12.5, 14.5, 16.5, 18.5, 20.5,
            22.5, 25.5, 27.5, 29.5]
    days += [35.5 + 5 * i for i in range(max(0, len(rows) - len(days)))]
    now = time.time()
    for row, back in zip(rows, days):
        at = now - back * 86400
        row["updatedAt"] = time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime(at))
    path = os.path.join(tempfile.mkdtemp(prefix="uiprobe-p6-sidebar-"), "sidebar.json")
    with open(path, "w") as f:
        json.dump(rows, f)
    return path


P6_SIDEBAR = ["--sidebar-fixture", _p6_sidebar_fixture()]


def _p6_statuses(claude_auth):
    return json.dumps([
        {"provider": "muse", "installed": {"Yes": {"version": "1.4.13", "path": "/Users/ada/.local/bin/muse"}},
         "auth": {"SignedIn": {"email": "ada@example.com", "plan": "Pro", "method": "account"}},
         "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
        {"provider": "claude-code", "installed": {"Yes": {"version": "2.1.276", "path": "/Users/ada/.local/bin/claude"}},
         "auth": claude_auth, "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
        {"provider": "codex", "installed": {"Yes": {"version": "codex-cli 0.144.6", "path": "/opt/homebrew/bin/codex"}},
         "auth": {"SignedIn": {"email": "ada@example.com", "plan": "ChatGPT Pro", "method": "chatgpt"}},
         "enabled": True, "advisory": "None", "checked_at": 1790000000, "usage": None},
    ])


_P6_CLAUDE = {"SignedIn": {"email": "ada@example.com", "plan": "Claude Max", "method": "claude.ai"}}


def _p6_entry(steps, claude_auth=None):
    claude_auth = claude_auth or _P6_CLAUDE
    return {"shot": ["--no-connect", "--login", "signed-in"] + P6_SIDEBAR + ["--steps", steps],
            "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-p6-state-"),
                    "BAAZ_PROVIDER_STATUS_SCRIPT": _p6_statuses(claude_auth)}}

def _new_row_repo():
    """docs/82 6.6: a throwaway git repository for the new-session row's
    entries, at one fixed path (its name, `acme-web`, is what the folder
    control reads, and its path is the folder menu's detail line, so both
    stay the same run to run). Built once, in a scratch folder renamed into
    place, so two probes importing at once never see half a repository.
    Never the person's repository: the entries adopt this one. They never
    flip the worktree toggle on — a capture makes no worktree anyway
    (`worktree_move_refusal`)."""
    parent = os.path.join(REAL_TMP, "baaz-uiprobe-new-row")
    root = os.path.join(parent, "acme-web")
    if os.path.isdir(os.path.join(root, ".git")):
        return root
    try:
        os.makedirs(parent, exist_ok=True)
        scratch = tempfile.mkdtemp(prefix="acme-web-", dir=parent)
        git = ["git", "-c", "user.name=Probe", "-c", "user.email=probe@example.com", "-C", scratch]
        subprocess.run(git + ["init", "-q", "-b", "main"], check=True, capture_output=True)
        with open(os.path.join(scratch, "README.md"), "w") as f:
            f.write("# acme-web\n")
        subprocess.run(git + ["add", "-A"], check=True, capture_output=True)
        subprocess.run(git + ["commit", "-q", "-m", "first"], check=True, capture_output=True)
        try:
            os.rename(scratch, root)
        except OSError:
            shutil.rmtree(scratch, ignore_errors=True)  # another probe won
    except (OSError, subprocess.CalledProcessError) as error:
        print(f"uiprobe: the new-row fixture repo was not built ({error})", file=sys.stderr)
    return root


_NEW_ROW_REPO = _new_row_repo()


def _new_row_entry(steps=""):
    """A fresh Codex draft in the throwaway repository, on its own state dir."""
    return {"shot": ["--no-connect", "--login", "signed-in",
                     "--steps", f"project:{_NEW_ROW_REPO};new;setprovider:codex;wait:300" + steps],
            "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-new-row-state-")}}


ENTRIES = {
    "signed-in":            {"shot": ["--no-connect", "--login", "signed-in"]},
    "right-browser":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:browser"]},
    # The Browser pane on a loaded page: `browse:` navigates the scripted
    # backend (captures always run the fake page, never a native view), so
    # the entry captures deterministically like `right-browser` does.
    "right-browser-page":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:browser;browse:https://example.com"]},
    # B2a-H2: Sessions ↔ Browse in one window. `browse-empty` enters
    # Browse with no person tab (one blank tab opens, URL field focused);
    # `browse-tabs` enters Browse over two person tabs; `sessions-switch`
    # stays in Sessions with the new switch row visible above "New
    # session". Offline, idempotent (`mode:` sets, never toggles), so
    # both settle captures agree. No baselines — capture and look.
    "browse-empty":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse"]},
    "browse-tabs":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://www.wikipedia.org/;tab-new:https://docs.rs/"]},
    # B2a-H4: the Browse toolbar, omnibox, session chip, Browse button and
    # new-tab page. `tab-new:` navigates (ending any address edit), and the
    # scripted page badges the URL's own classification (Secure for
    # https, Local for localhost) since it reports no Security of its own.
    "browse-toolbar":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com"]},
    "browse-toolbar-dev":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:http://localhost:5173/overview"]},
    "browse-omnibox":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;omnibox:example"]},
    # R4: history from browser.db in the Field's rows — a title match, a
    # words-only match with its line, and an open tab that hides its own
    # history row (sidebar hidden, so the rows draw in the main window).
    "field-history":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;sidebar;tab-new:https://example.com;history:https://tokio.rs/tokio/tutorial/spawning|Tokio tutorial · Spawning|2|;history:https://without.boats/blog/pin/|Pin and suffering|9|Self-referential futures in Rust need Pin to stay put.;history:https://example.com/|Example Domain|1|;omnibox:rust"]},
    # R4 bookmarks: a query shows Bookmarks between tabs and history (a
    # bookmarked page's history row folds into it). The scoped Bookmarks
    # view is gone (docs/82 8.9: the Field's one Bookmarks row opens the
    # Bookmarks & History page, `browse-bookmarks-page`).
    "field-bookmarks":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;sidebar;tab-new:https://example.com;bookmark:https://doc.rust-lang.org/book/|The Rust Programming Language|;bookmark:https://doc.rust-lang.org/rust-by-example/|Rust by Example|Learning;history:https://doc.rust-lang.org/book/|The Rust Programming Language|1|;history:https://without.boats/blog/pin/|Pin and suffering|9|Rust needs Pin.;omnibox:rust"]},
    # docs/82 8.9: the Field's command rows for "bookmark" — "Bookmark this
    # page" and the one "Bookmarks" (which opens the Bookmarks & History
    # page; "Manage bookmarks" is gone). No baseline yet.
    "field-commands":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;sidebar;tab-new:https://example.com;omnibox:bookmark"]},
    # R4 Peek (⇧-click) and Split (⌥-click), board-approved gallery look.
    "browse-peek":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;peek:https://example.org/"]},
    "browse-split":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;split:https://example.org/"]},
    # R4 Import from Chrome over Settings › Browser, on the synthetic profile
    # in fixtures/ (a capture never reads the person's Chrome).
    "chrome-import-sheet":  {"env": {"BAAZ_CHROME_ROOT": "fixtures/chrome-profile"},
                             "shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:browser;chrome-import;wait:600"]},
    "browse-newtab":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;tab-new:about:blank"]},
    "browse-from-session":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "fixture-session:Fix ledger chart"]},
    "sessions-switch":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400"]},
    # B2a-H3: the full Browse sidebar — 3 person tabs with 1 pinned, one
    # session with 2 tabs in its task folder. `new` heads the
    # script so the session exists; `tab-new` in Sessions opens session
    # tabs, in Browse person tabs. Offline, fake backend, no baselines —
    # capture and look.
    "browse-sidebar":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;tab-new:https://www.wikipedia.org/;tab-new:https://docs.rs/;mode:browse;tab-new:https://example.com/;tab-new:https://rust-lang.org/;tab-pin:2"]},
    # B2a-H3: a person tab's right-click menu, open on the second tab.
    # Offline, fake backend, no baselines — capture and look.
    "browse-tab-menu":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://www.wikipedia.org/;tab-new:https://docs.rs/;tab-menu:2"]},
    # B2a-H6b: the sidebar-hidden one-row chrome — lights, toggle, nav, a
    # wide field (security glyph, title, zoom pill) with the ⋯ menu and
    # `+`; no rail, the page full width. `sidebar` toggles the sidebar
    # shut. Offline, fake backend, no baselines — capture and look.
    "browse-minimal":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;sidebar;tab-new:https://www.wikipedia.org/"]},
    # B2a-H7: a link from another app, driven through the scripted twin of
    # the open-URL event (`open-url:` runs the same path `on_open_urls`
    # drains into): a person tab on wikipedia in Browse. Offline, fake
    # backend, no baselines — capture and look.
    "browse-opened-link":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "open-url:https://www.wikipedia.org/"]},
    # B2a-H5: the find strip (scripted "3 of 12" lands through the poll
    # path, hence the wait), the permission strip, the downloads popover
    # (open), the ⋯ page-actions menu, and the Baaz crashed page. Offline,
    # fake backend, no baselines — capture and look.
    "browse-find":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;find:price;wait:200"]},
    "browse-permission":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;permission:camera,microphone;wait:200"]},
    "browse-downloads":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;download:big.zip;wait:200"]},
    # The page controls' real ⋯ (the library's page menu, docs/82 8.3; Baaz's
    # own copy was reachable only from a probe verb and is gone). It draws
    # in the overlay window over the page, which a shot of the main window
    # does not show: this pins that the menu opens (a failed step exits 1),
    # not its look — the gallery's `screens/sidebar-browse-page-offers` is.
    "browse-more-menu":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;page-actions:234,102"]},
    # docs/82 8.6, work-browser basics. A site's own Settings page (the
    # per-site sheet: kept answers as Ask / Allow / Block, the agents' rule,
    # its zoom, Forget), reached from the page menu's "Site settings…" and
    # the site information; Settings › Browser › Site permissions lists the
    # sites. The Bookmarks and History pages fill the Browse centre. The
    # Zoom row lives on the page ⋯ menu, which draws in the overlay window a
    # main-window shot does not see: `browse-zoom-menu` pins that two zoom
    # steps and the menu open (a failed step exits 1); its look is the
    # gallery's `screens/sidebar-browse-page-offers`.
    "browse-site-sheet":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;site-set:https://example.com|camera=allow,microphone=block,notifications=block,zoom=125;settings:browser/site:https://example.com"]},
    "settings-site-permissions": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "site-set:https://example.com|camera=allow,zoom=125;site-set:https://meet.example.org|camera=allow,microphone=allow;site-set:https://news.example.net|notifications=block;settings:browser"]},
    "browse-history-page":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;history:https://tokio.rs/tokio/tutorial/spawning|Tokio tutorial · Spawning|0|;history:https://without.boats/blog/pin/|Pin and suffering|0|;history:https://doc.rust-lang.org/book/|The Rust Programming Language|1|;history:https://news.ycombinator.com/|Hacker News|3|;library:history;wait:300"]},
    "browse-bookmarks-page": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;bookmark:https://doc.rust-lang.org/book/|The Rust Programming Language|;bookmark:https://news.ycombinator.com/|Hacker News|;bookmark:https://doc.rust-lang.org/rust-by-example/|Rust by Example|Learning;library:bookmarks"]},
    "browse-zoom-menu":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;zoom:in;zoom:in;page-actions:234,102"]},
    # docs/82 8.6: a session acting on the page Browse shows — the agent
    # cursor on the element it acted on (drawn in the main window over the
    # scripted page; over a native page its own float draws it), the
    # driving bar naming the session in the page float (not in a shot).
    "browse-agent-cursor":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "fixture-session:Fix ledger chart;agent-drive:248,226|clicking “Choose”;wait:400"]},
    # docs/82 8.3: a session's task-folder tab right-clicked in Browse (Keep
    # this tab, Send page to session…). The menu draws in the overlay
    # window, so the shot shows the session's tab shown and its row lit,
    # not the menu (the gallery's `screens/sidebar-browse-tab-offers` does).
    "browse-tab-menu-session": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "fixture-session:Fix ledger chart;tab-menu:shown"]},
    # docs/82 8.3: the Page at two sizes — the session's Page opened in
    # Browse and brought back: the Page still shows the same URL.
    "right-page-expand":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;right-width:400;right:browser;browse:https://example.com;page-expand;page-collapse"]},
    # docs/82 8.2, one Ask: the Field's Ask (`browse-ask:`, the ✦ path
    # too) opens the ⌘J line on the shown page with the question asked
    # there in place — no session, no draft. Scripted lanes: the browser
    # chat's lane sends nothing anywhere. `browse-shot-picker`: a Browse
    # screenshot with no composer on screen opens "Add to which session?"
    # over the page (`shot-pick` lands a fixture picture: the scripted page
    # takes none); the P6 sidebar fixture gives it session rows to list.
    # CAUTION: in Browse the line and the picker draw in overlay windows
    # (over the native page), and a shot renders the main window only, so
    # these two show the page without them — like `browse-omnibox`'s Field.
    # They pin that the script runs (a failed step exits 1), not the look.
    # `settings-shot-picker` is the same picker where the main window hosts
    # it (Settings open, no composer on screen): that one shows it.
    "browse-ask-line":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;browse-ask:What is the refund window?;wait:300"]},
    "browse-shot-picker":   {"shot": ["--no-connect", "--login", "signed-in", *P6_SIDEBAR,
                                      "--steps", "mode:browse;tab-new:https://example.com;shot-pick"]},
    "settings-shot-picker": {"shot": ["--no-connect", "--login", "signed-in", *P6_SIDEBAR,
                                      "--steps", "settings:general;shot-pick"]},
    "browse-crashed":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;crashed:the page crashed (-1);wait:200"]},
    # R3: a toast over a live Browse page — the header chip, not the
    # top-right stack (the native page would cover the stack). `tab-new:`
    # lands the scripted page first (captures always run the fake page,
    # which the predicate counts like a real one), then `toast:` raises
    # the production toast through the production path. Offline, fake
    # backend.
    # docs/82 8.7: the one-time default-browser offer above the Browse page
    # (a capture never raises it by itself; the verb does, and nothing ever
    # changes the system default).
    "browse-default-offer": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;default-browser-offer"]},
    "toast-header":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "mode:browse;tab-new:https://example.com;toast:Saved · 3 files|/tmp/baaz-downloads"]},
    "right-changes":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:changes"]},
    # Task 7.2: a row unticked — its box empty, its section's tick-all
    # mixed, the button "Commit 4 files"; New branch…'s field open on the
    # default branch (`changes-branch:main` puts the fixture tree there,
    # level with origin/main, so Amend is not offered); "Amend last commit"
    # ticked on the fixture's unpushed branch (↑2), the message HEAD's
    # (a fixture message: captures read no HEAD). No baselines yet.
    "right-changes-untick": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:changes;untick:docs/02-app.md"]},
    "right-changes-new-branch": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:changes;changes-branch:main;new-branch"]},
    "right-changes-amend":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:changes;amend:on"]},
    "right-files":          {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files"]},
    # The Files tree scrolled a little: the pane row's fade is back over
    # the rows (at the tree's top it is not — owner 2026-10-21, "text is
    # getting cut under the shadow"). Offline, no baseline yet.
    "right-files-scrolled": {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;files-reveal:crates/baaz/src;wheel:-60@1240,400"]},
    # The Files pane previewing a small file: the same preview a tree
    # click opens (header plus the file's own bytes), reached through the
    # click's own `files-select:` verb. No baseline yet — generate on main
    # after merge, never to silence a finding.
    "right-file-preview":   {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;files-select:uiprobe.json"]},
    # B9: a folder link's reveal (ancestors expanded, the folder selected
    # and scrolled into view, no preview) and a Rust file's preview (the
    # same tree-click path, syntax-highlighted with tree-sitter on).
    # Offline steps only, no baselines — capture and look.
    "right-files-reveal":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;files-reveal:crates/baaz/src"]},
    "right-file-preview-rs":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;files-select:crates/baaz/src/main.rs"]},
    "right-closed":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "right-width:400;right:files;right:off"]},
    # The ⌘K palette, open. It had no coverage at all, which is why a list
    # that never scrolls to its selection went unseen: nothing ever drew it.
    "palette":              {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "palette"]},
    # 5.8 (Gate 4): the palette when the query matches nothing — no empty
    # "Commands" heading, the one "Nothing matches" line instead.
    # `palette:<query>` opens (never toggles) and types. No baseline yet —
    # generate on main after merge, never to silence a finding.
    "palette-no-match":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "palette:zzqxj"]},
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
    # Task 8.8: Settings › Git & worktrees (the branch prefix and the
    # pull-request switch, moved off General). Offline, idempotent. No
    # baseline yet — generate on main after merge.
    "settings-git":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:git"]},
    # Task 8.8: Settings › MCP — per provider, the "Use my own MCP servers"
    # switch, Baaz's own `baaz` server and what the provider's config files
    # name, read-only. A capture reads the adapters' recorded fixtures
    # (`fixtures/codex/mcp-config.toml`, `fixtures/claude-code/mcp-*.json`),
    # never the person's files and never a CLI. Offline, idempotent.
    "settings-mcp":         {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:mcp"]},
    # Task 8.8: Settings › Data — the folder (the real run's path, never the
    # capture's throwaway one) with Reveal, Export…, and Clear… for browsing
    # and prompt history. Nothing is clicked: a capture opens no panel and
    # clears nothing. Offline, idempotent.
    "settings-data":        {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:data"]},
    # Settings › Appearance (task 2.9): the theme choice and the summary
    # line. `-light` switches the theme by the page's own setter
    # (`settings-set:theme=light`), not by `--theme`, so it proves the live
    # switch repaints the whole window. A capture reads no store, so it
    # writes none either. Offline, idempotent (set, never toggle). No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "settings-appearance":  {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:appearance"]},
    "settings-appearance-light": {"shot": ["--no-connect", "--login", "signed-in",
                                           "--steps", "settings:appearance;settings-set:theme=light"]},
    # Task 2.10: Density and Text size, set by the page's own setters
    # (`settings-set:density=` / `text-size=`), so the capture proves the
    # live switch re-lays the whole window. Compact is the type and 28 px
    # rows Baaz drew before 2.10. Offline, idempotent (set, never toggle).
    # No baseline yet — generate on main after merge.
    "settings-appearance-compact": {"shot": ["--no-connect", "--login", "signed-in",
                                             "--steps", "settings:appearance;settings-set:density=compact"]},
    "settings-appearance-large": {"shot": ["--no-connect", "--login", "signed-in",
                                           "--steps", "settings:appearance;settings-set:text-size=large"]},
    # The Settings page's Archived section: offline, idempotent. No
    # baseline yet — generate on main after merge, never to silence a
    # finding.
    "settings-archived":    {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:archived"]},
    # Task 3.5: Settings › Notifications, at its defaults (every event on,
    # the badge on, no sound). Offline, idempotent.
    "settings-notifications": {"shot": ["--no-connect", "--login", "signed-in",
                                        "--steps", "settings:notifications"]},
    # B2a-H6b: the Settings page's Browser section (tabs layout, search
    # engine, downloads, the Default-browser row with its fixed capture
    # text; the Chromium row only where the engine loads chrome:// pages).
    # Offline, idempotent. No baseline yet — generate on main after merge,
    # never to silence a finding.
    "settings-browser":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "settings:browser"]},
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
    # under its "Continue in…" header. The send goes to
    # the scripted lane (offline, free); the wait lets its deltas land so
    # `has_turns` is true before the menu opens. No baseline yet —
    # generate on main after merge, never to silence a finding.
    "composer-handoff":     {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;setprovider:codex;send:hello handoff probe;wait:3000;provider"]},
    "composer-model":       {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;model"]},
    "composer-effort":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "new;effort"]},
    # The model menu opened upward from the bottom composer with a row's
    # reasoning-effort submenu open (owner 2026-10-30: the submenu ran past
    # the window's bottom edge). `fixture-models` fills a fixed Muse-shaped
    # catalog offline, `submenu` opens the highlighted row's levels as →
    # does. Its own state dir, so no other entry's provider pick leaks in.
    # `setprovider:muse` first: since 5.8 a new session starts on the first
    # connected provider, and the Muse-shaped catalog's submenu does not draw
    # on a Claude Code composer (Gate 5 found the submenu silently absent).
    "composer-model-submenu": {"shot": ["--no-connect", "--login", "signed-in",
                                        "--steps", "new;setprovider:muse;wait:300;fixture-models;model;submenu"],
                               "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-submenu-state-")}},
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
    # `--provider codex`: the first scripted lane (`s-scripted`) must be the
    # Codex one the send lands on — since 5.8 a bare `new` opens on the
    # first connected provider in switcher order, not Muse.
    "reopen-failed":        {"shot": ["--no-connect", "--login", "signed-in", "--provider", "codex",
                                      "--steps", "new;setprovider:codex;send:hello reopen probe;wait:3000;open:s-scripted"]},
    # Settings → Providers, open on scripted statuses covering Connected
    # (Muse), Signed out (Claude Code) and Not installed (Codex):
    # offline, deterministic, no baseline yet — generate on main after
    # merge, never to silence a finding. Capture and look.
    # P6 (docs/71): the typed "Couldn't start" view of a session that
    # never opened — a crash with the child's stderr well, and a
    # signed-out open with its Sign in — then a scripted Codex lane whose
    # child dies once (the quiet banner with Retry now) and twice (the
    # banner grown into the typed failure, under the transcript). Offline,
    # no child, no turn, on the representative fixture state above (rows,
    # a footer account), each on its own throwaway state dir.
    "launch-failure":       _p6_entry("launch-failed:codex:crashed"),
    "launch-failure-signed-out": _p6_entry("launch-failed:claude-code:signed-out", "SignedOut"),
    "child-lost-banner":    _p6_entry("new;setprovider:codex;send:hello lost probe;wait:3000;child-lost"),
    "child-lost-twice":     _p6_entry("new;setprovider:codex;send:hello lost probe;wait:3000;child-lost;child-lost"),
    # docs/82 6.2: a new draft opened in the folder where another scripted
    # session has a turn running (`[hold <ms>]` keeps it running) wears the
    # shared-folder banner above the composer, "Run in a worktree" and
    # Dismiss at its right. Offline, no worktree is made (a capture never
    # makes one), no baseline yet — generate on main after merge.
    "shared-folder-banner": _p6_entry("new;setprovider:codex;send:[hold 9000] working in this folder;wait:500;"
                                      "new;setprovider:codex;wait:500"),
    # docs/82 6.6: the new-session row on a fresh draft — `acme-web ▾` and
    # "Own worktree" (off) left of the provider — and its folder menu open
    # (the projects, this one checked, then "Choose a folder…"). Offline,
    # in a throwaway repo the entry builds; no baseline yet — generate on
    # main after merge.
    "composer-new-row":     _new_row_entry(),
    "composer-menu-folder": _new_row_entry(";folder"),
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
    # Muse is connected in the add-menu entry, so its own "Ask Muse to write
    # one" row (shown only while Muse is connected) is in the capture.
    "skills-add-menu":      {"shot": ["--no-connect", "--login", "signed-in",
                                      "--steps", "skills:add-menu"],
                             "env": {"BAAZ_PROVIDER_STATUS_SCRIPT":
                                      '[{"provider":"muse","installed":{"Yes":{"version":"1.4.0","path":"/bin/muse"}},'
                                      '"auth":{"SignedIn":{"email":"ada@example.com","plan":"Pro","method":"oauth"}},'
                                      '"enabled":true,"advisory":"None","checked_at":1,"usage":null}]'}},
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
    # B12: the quiet transcript, both states, off the small toolgroup
    # fixture (25 events — terminates). `transcript-folded` replays to
    # rest: settled turns fold behind one TurnFold row (the last turn's
    # three bash calls and answer fold; the first turn's approval and
    # error stay visible). `transcript-live` holds the last turn open
    # (`--bench-open-turn` stops before its `turn/completed`): the
    # open-turn chrome with a running turn. Under BAAZ_DETERMINISTIC
    # every turn draws settled rows, so true live-row pixels are checked
    # on real sessions, never claimed from these captures. No baselines:
    # generate on main after merge, never to silence a finding.
    "transcript-folded":    {"idle": ["--bench", "fixtures/msp/synthetic-toolgroup.jsonl"]},
    # `--bench` takes the capture as its own value: the flag after it would
    # be read as the path (the entry printed usage and never ran).
    "transcript-live":      {"idle": ["--bench", "fixtures/msp/synthetic-toolgroup.jsonl",
                                      "--bench-open-turn"]},
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


# D5 Providers (docs/66 §3, the approved board): one entry per board state.
# All offline and scripted through BAAZ_PROVIDER_STATUS_SCRIPT (or a
# `--steps` verb that sets the state with sample values); the setup sheet's
# terminal is a scripted backend in deterministic mode, never a shell.
def _status(provider, installed, auth, advisory="None"):
    return {"provider": provider, "installed": installed, "auth": auth, "enabled": True,
            "advisory": advisory, "checked_at": 1790000000, "usage": None}


def _yes(version, path):
    return {"Yes": {"version": version, "path": path}}


def _signed_in(email, plan, method, source=None):
    auth = {"email": email, "plan": plan, "method": method}
    if source:
        auth["source"] = source
    return {"SignedIn": auth}


D5_ALL_MISSING = json.dumps([
    _status("claude-code", "No", "Unknown"),
    _status("codex", "No", "Unknown"),
    _status("muse", "No", "Unknown"),
])
D5_ALL_SIGNED_OUT = json.dumps([
    _status("claude-code", _yes("2.1.276", "/Users/ada/.local/bin/claude"), "SignedOut"),
    _status("codex", _yes("codex-cli 0.144.6", "/opt/homebrew/bin/codex"), "SignedOut"),
    _status("muse", _yes("1.4.13", "/Users/ada/.local/bin/muse"), "SignedOut"),
])
D5_TOO_OLD = json.dumps([
    _status("claude-code", _yes("2.0.9 (Claude Code)", "/Users/ada/.local/bin/claude"),
            _signed_in("ada@example.com", "Claude Max", "claude.ai"), {"TooOld": {"need": "2.1.276"}}),
    _status("codex", _yes("codex-cli 0.144.6", "/opt/homebrew/bin/codex"),
            _signed_in("ada@example.com", "ChatGPT Pro", "chatgpt")),
    _status("muse", _yes("", "/Users/ada/.local/bin/muse"), "Unknown",
            {"CantRun": {"stderr": "timed out after 5s", "timed_out": True}}),
])
D5_CLAUDE_CONNECTED = json.dumps([
    _status("claude-code", _yes("2.1.276 (Claude Code)", "/Users/ada/.local/bin/claude"),
            _signed_in("ada@example.com", "Claude Max", "claude.ai")),
    _status("codex", _yes("codex-cli 0.144.6", "/opt/homebrew/bin/codex"), "SignedOut"),
    _status("muse", "No", "Unknown"),
])
D5_TWO_CONNECTED = json.dumps([
    _status("claude-code", _yes("2.1.276 (Claude Code)", "/Users/ada/.local/bin/claude"),
            _signed_in("ada@example.com", "Claude Max", "claude.ai")),
    _status("codex", _yes("codex-cli 0.144.6", "/opt/homebrew/bin/codex"),
            _signed_in("ada@example.com", "ChatGPT Pro", "chatgpt")),
    _status("muse", "No", "Unknown"),
])
D5_API_KEY = json.dumps([
    _status("claude-code", _yes("2.1.276 (Claude Code)", "/Users/ada/.local/bin/claude"),
            _signed_in(None, None, "api_key", "ANTHROPIC_API_KEY")),
    _status("codex", _yes("codex-cli 0.144.6", "/opt/homebrew/bin/codex"), "SignedOut"),
    _status("muse", "No", "Unknown"),
])


def _d5(steps=None, script=None, login="connect"):
    shot = ["--no-connect", "--login", login]
    if steps:
        shot += ["--steps", steps]
    entry = {"shot": shot}
    if script:
        entry["env"] = {"BAAZ_PROVIDER_STATUS_SCRIPT": script}
    return entry


ENTRIES.update({
    # B1 · the connect screen
    "connect-all-missing":     _d5(script=D5_ALL_MISSING),
    "connect-all-signed-out":  _d5(script=D5_ALL_SIGNED_OUT),
    "connect-too-old":         _d5(script=D5_TOO_OLD),
    "connect-menu":            _d5("connect-menu", D5_TWO_CONNECTED),
    # B2 · the setup sheet: waiting (Claude signed out) and done (Connected)
    "setup-sheet-install":     _d5("setup:claude-code:install", D5_ALL_MISSING),
    "setup-sheet-signin":      _d5("setup:claude-code:signin", D5_ALL_SIGNED_OUT),
    "setup-sheet-done":        _d5("setup:claude-code:signin", D5_CLAUDE_CONNECTED),
    # Codex installed but signed out: the sheet's next step, Sign in to Codex
    "setup-sheet-codex-installed": _d5("setup:codex:install", D5_ALL_SIGNED_OUT),
    # B3 · Codex in its row (browser link, device code) and Muse's sheet
    "codex-signing-in":        _d5("codex-login:waiting", D5_CLAUDE_CONNECTED),
    "codex-device-code":       _d5("codex-login:device", D5_CLAUDE_CONNECTED),
    "muse-sheet":              _d5("muse-sheet", D5_ALL_SIGNED_OUT),
    # B4 · the Settings card: Details open, and an API-key login
    "settings-card-details":   _d5("settings:providers/claude-code;details:claude-code",
                                   D5_CLAUDE_CONNECTED, login="signed-in"),
    "settings-card-api-key":   _d5("settings:providers/claude-code;details:claude-code",
                                   D5_API_KEY, login="signed-in"),
    # Settings › Providers' "New sessions start in" picker with its menu
    # open (the connect footer's own control; `connect-menu` sets the one
    # menu flag both share). Two Connected, so the menu lists two.
    "settings-default-menu":   _d5("settings:providers;connect-menu", D5_TWO_CONNECTED, login="signed-in"),
    # B5 · the signed-out banner above the composer
    "signed-out-banner":       _d5("new;signed-out", login="signed-in"),
})

# P7 import (docs/72, the approved P7Import board): a fixture owner home
# with a few hand-made Claude Code and Codex session files, and a state dir
# of its own per entry. `BAAZ_IMPORT_OWNER_HOME` points the scan at the
# fixture — never the real ~/.claude or ~/.codex — and a capture writes no
# journal and no records to disk, so both settle shots start from the same
# state. Offline, no child, no turn. `project:` adopts the fixture's
# harness and agentic-ui folders so they read as Baaz projects (ticked).
def _import_fixture():
    # Not under $TMPDIR: the scan counts a run in a temp dir as scripted, so
    # the fixture's folders live in the repo's target dir instead (removed
    # at exit; the path never paints — parents read `~/Projects`).
    import atexit
    owner = os.path.join(REPO, "target", f"uiprobe-import-owner-{os.getpid()}")
    shutil.rmtree(owner, ignore_errors=True)
    os.makedirs(owner)
    atexit.register(shutil.rmtree, owner, True)

    def claude(folder, sid, prompt, day, entry="claude-desktop", turns=2):
        cwd = os.path.join(owner, "Projects", folder)
        os.makedirs(cwd, exist_ok=True)
        slug = os.path.join(owner, ".claude", "projects", cwd.replace("/", "-"))
        os.makedirs(slug, exist_ok=True)
        lines = []
        for n in range(turns):
            ts = f"2026-10-{day:02d}T1{n}:00:00.000Z"
            text = prompt if n == 0 else f"follow-up {n}"
            lines.append({"type": "user", "message": {"role": "user", "content": text},
                          "cwd": cwd, "entrypoint": entry, "sessionId": sid, "timestamp": ts})
            lines.append({"type": "assistant", "message": {"role": "assistant",
                          "content": [{"type": "text", "text": f"answer {n}"}]}, "timestamp": ts})
        with open(os.path.join(slug, sid + ".jsonl"), "w") as f:
            f.write("".join(json.dumps(l) + "\n" for l in lines))
        return cwd

    def codex(folder, tid, prompt, day, exec_run):
        cwd = os.path.join(owner, "Projects", folder)
        os.makedirs(cwd, exist_ok=True)
        dated = os.path.join(owner, ".codex", "sessions", "2026", "10", f"{day:02d}")
        os.makedirs(dated, exist_ok=True)
        lines = [
            {"timestamp": f"2026-10-{day:02d}T09:00:00.000Z", "type": "session_meta",
             "payload": {"id": tid, "cwd": cwd, "originator": "codex_exec" if exec_run else "codex_cli_rs",
                         "source": "exec" if exec_run else "cli"}},
            {"timestamp": f"2026-10-{day:02d}T09:00:01.000Z", "type": "event_msg",
             "payload": {"type": "user_message", "message": prompt}},
        ]
        with open(os.path.join(dated, f"rollout-2026-10-{day:02d}T09-00-00-{tid}.jsonl"), "w") as f:
            f.write("".join(json.dumps(l) + "\n" for l in lines))

    harness = claude("harness", "c0000001-0000-4000-8000-000000000001", "Tier V probe for right pane", 21)
    claude("harness", "c0000001-0000-4000-8000-000000000002", "Sidebar fluid round, FF values", 24)
    claude("harness", "c0000001-0000-4000-8000-000000000003", "Relay: L3 gate failing on pty", 19)
    claude("harness", "c0000001-0000-4000-8000-000000000004", "summarise the diff", 25, entry="sdk-cli")
    aui = claude("agentic-ui", "c0000001-0000-4000-8000-000000000005", "Checkbox tri-state in tokens", 23)
    codex("agentic-ui", "01a15e17-bdc8-7393-bb12-000000000006", "rename the router", 22, False)
    claude("cockpit", "c0000001-0000-4000-8000-000000000007", "Ledger double-counts a turn", 20)
    claude("lakshya-v2", "c0000001-0000-4000-8000-000000000008", "Exam tracker layout", 18)
    codex("art-src", "01a15e17-bdc8-7393-bb12-000000000009", "make one image", 17, True)
    codex("art-src", "01a15e17-bdc8-7393-bb12-00000000000a", "make another image", 16, True)
    # One file that is not a transcript: reported, never dropped.
    bad = os.path.join(owner, ".claude", "projects", harness.replace("/", "-"))
    with open(os.path.join(bad, "c0000001-0000-4000-8000-0000000000ff.jsonl"), "w") as f:
        f.write("not json\n")
    return owner, harness, aui


_IMPORT_OWNER, _IMPORT_HARNESS, _IMPORT_AUI = _import_fixture()


def _import_entry(steps):
    pre = f"project:{_IMPORT_HARNESS};project:{_IMPORT_AUI};settings:providers/claude-code"
    return {"shot": ["--no-connect", "--login", "signed-in", "--steps", f"{pre};{steps}" if steps else pre],
            "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": D5_CLAUDE_CONNECTED,
                    "BAAZ_IMPORT_OWNER_HOME": _IMPORT_OWNER,
                    "BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-import-state-")}}


ENTRIES.update({
    # The offer on the Claude Code card, before any import.
    "settings-card-import":    _import_entry("wait:300"),
    # The sheet: folders (projects ticked), the option, the note.
    "import-sheet":            _import_entry("import-open"),
    # The run held after 2 of the chosen sessions.
    "import-progress":         _import_entry("import-open;import-run:2"),
    # The result: what landed, the unreadable file, the toast's Undo.
    "import-done":             _import_entry("import-open;import-run;wait:500"),
    # First run: Claude Code Connected and the fixture home holds sessions
    # not in Baaz — one line under the connect rows.
    "connect-import":          {"shot": ["--no-connect", "--login", "connect", "--steps", "wait:300"],
                                "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": D5_CLAUDE_CONNECTED,
                                        "BAAZ_IMPORT_OWNER_HOME": _IMPORT_OWNER,
                                        "BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-import-state-")}},
})

# docs/82 8.7: the first run after Continue. "Where will you work?" with
# Claude Code Connected and nothing adopted (`--no-project`: a Finder launch
# adopts nothing), so its "Not now" line names the default workspace.
ENTRIES.update({
    "first-run-folder":        {"shot": ["--no-connect", "--login", "connect", "--no-project",
                                         "--steps", "connect-continue"],
                                "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": D5_CLAUDE_CONNECTED,
                                        "BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-first-run-state-")}},
    # Then a folder dropped on the step (the throwaway `acme-web` repo): the
    # new Claude Code draft in it, with the four regions named once.
    "first-run-regions":       {"shot": ["--no-connect", "--login", "connect", "--no-project",
                                         "--steps", f"connect-continue;first-run-folder:{_NEW_ROW_REPO};wait:300"],
                                "env": {"BAAZ_PROVIDER_STATUS_SCRIPT": D5_CLAUDE_CONNECTED,
                                        "BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-first-run-state-")}},
})

D5_ENTRIES = [
    "connect-first-run", "connect-checking", "connect-all-missing", "connect-all-signed-out",
    "connect-too-old", "connect-menu", "setup-sheet-install", "setup-sheet-signin",
    "setup-sheet-done", "setup-sheet-codex-installed", "codex-signing-in", "codex-device-code", "muse-sheet",
    "settings-card-details", "settings-card-api-key", "signed-out-banner",
]


# "Jump to latest" over the banners above the composer: scrolled up one
# wheel, the pill sits clear of the banner's words (owner 2026-11-04: it sat
# on the composer and covered them), and without a banner 8 above the
# composer. A short fixture on purpose: a list with rows it has not measured
# (the 300-turn stress) cannot say it left its end, and draws no pill.
def _jump_entry(*extra, steps="wait:1500;wheel:300;wait:800"):
    return {"shot": ["--no-connect", "--login", "signed-in",
                     "--replay", "fixtures/msp/synthetic-markdown.jsonl",
                     *extra, "--steps", steps],
            "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-jump-state-")}}


# docs/82 5.5 (product parity): the new states, each offline. Expected to
# change with this row on purpose — rebaseline on main after merge, never
# to silence a finding.
#   approval-vocabulary   a pending Muse approval in the one vocabulary
#                         (Once / Always here / Deny), its title provider-free
#   error-card            a failed turn's error card with Retry (the Muse
#                         replay; Claude Code and Codex now mint the same card)
#   composer-plan         the Plan toggle on (Muse's plan skill: the pill)
#   composer-commands-codex  the `/` menu on Codex: no /plan, /compact, /fork,
#                         /skills or /logout, and no Muse skills
ENTRIES.update({
    "approval-vocabulary":     {"shot": ["--no-connect", "--login", "signed-in",
                                         "--replay", "fixtures/msp/transcript-approve-stage1.jsonl",
                                         "--steps", "wait:1500;end"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-approval-state-")}},
    "error-card":              {"shot": ["--no-connect", "--login", "signed-in",
                                         "--replay", "fixtures/msp/synthetic-error-retry.jsonl",
                                         "--steps", "wait:1500;end"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-error-state-")}},
    "composer-plan":           {"shot": ["--no-connect", "--login", "signed-in",
                                         "--steps", "new;plan;wait:500"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-plan-state-")}},
    "composer-commands-codex": {"shot": ["--no-connect", "--login", "signed-in",
                                         "--steps", "new;setprovider:codex;command"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-commands-state-")}},
})

# docs/82 5.7 (provider #4): a hand-written ACP recording replayed into the
# app through the registry (`--replay` asks every registered provider; ACP's
# own `replay` reads it), offline, with no agent configured or run. The
# session is the ACP provider's: its badge, its label on its own row.
#   acp-transcript   fixtures/acp/plan.jsonl: the plan card refined in place,
#                    a read, an edit with its diff chip, the reply
#   acp-approval     fixtures/acp/permission-pending.jsonl: a shell call that
#                    asked first and is still waiting (Once / Always here / Deny)
ENTRIES.update({
    "acp-transcript":          {"shot": ["--no-connect", "--login", "signed-in",
                                         "--replay", "fixtures/acp/plan.jsonl",
                                         "--steps", "wait:1500;end"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-acp-state-")}},
    "acp-approval":            {"shot": ["--no-connect", "--login", "signed-in",
                                         "--replay", "fixtures/acp/permission-pending.jsonl",
                                         "--steps", "wait:1500;end"],
                                "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-acp-approval-state-")}},
})

ENTRIES.update({
    "jump-pill":               _jump_entry("--tier", "subscription"),
    "jump-pill-plan-banner":   _jump_entry("--tier", "unknown"),
    "jump-pill-signed-out":    _jump_entry("--tier", "subscription",
                                           # One wheel once the banner has settled: two
                                           # wheels while it was still growing landed ~10 px
                                           # apart run to run.
                                           steps="wait:1500;signed-out;wait:1500;wheel:60;wait:800"),
})


# docs/82 1.5 · populated captures: what the app looks like in use, where
# every older entry shows an empty or near-empty window. The parts:
#   - fixtures/sidebar/populated.json: 24 sessions over two projects and
#     three providers, two running, three waiting on the person (two
#     approvals, one question), two failed, two unread (task 3.1), the
#     rest idle.
#   - fixtures/msp/synthetic-full.jsonl (make-full.py): one transcript
#     with markdown, code, a table, every tool card, a diff, a plan, and an
#     approval and a question still waiting.
#   - a throwaway git repo named acme-web with an edit, a new file and a
#     deletion, for the Files pane (the Changes pane is scripted in
#     deterministic runs already).
#   - the same scripted provider statuses as the P6 entries, so the footer
#     reads signed in rather than "Checking providers...".
# Every entry is offline and its own state dir; nothing sends a turn.
def _fixture_repo():
    root = os.path.join(tempfile.mkdtemp(prefix="uiprobe-repo-"), "acme-web")
    os.makedirs(os.path.join(root, "src"))
    def write(rel, text):
        with open(os.path.join(root, rel), "w") as f:
            f.write(text)
    write("README.md", "# acme-web\n\nThe storefront.\n")
    write("src/main.rs", 'fn main() {\n    println!("{}", acme_web::cents(1999));\n}\n')
    write("src/lib.rs", "pub fn total(a: u32, b: u32) -> u32 {\n    a + b\n}\n")
    git_env = {**os.environ, "GIT_AUTHOR_DATE": "2026-11-01T10:00:00Z",
               "GIT_COMMITTER_DATE": "2026-11-01T10:00:00Z",
               "GIT_AUTHOR_NAME": "Ada", "GIT_AUTHOR_EMAIL": "ada@example.com",
               "GIT_COMMITTER_NAME": "Ada", "GIT_COMMITTER_EMAIL": "ada@example.com"}
    for cmd in (["git", "init", "-q", "-b", "main"], ["git", "add", "-A"],
                ["git", "commit", "-q", "-m", "init"]):
        subprocess.run(cmd, cwd=root, env=git_env, check=True, capture_output=True)
    write("src/lib.rs", "pub fn total(a: u32, b: u32) -> u32 {\n    a.saturating_add(b)\n}\n\n"
          "/// Cents as `d.cc`.\npub fn cents(v: u32) -> String {\n"
          '    format!("{}.{:02}", v / 100, v % 100)\n}\n')
    write("src/router.rs", "pub fn route(path: &str) -> &str {\n    path\n}\n")
    os.remove(os.path.join(root, "README.md"))
    return root


_REPO_ACME = _fixture_repo()
_POPULATED_SIDEBAR = ["--sidebar-fixture", _p6_sidebar_fixture("populated.json")]
_FULL = ["--replay", "fixtures/msp/synthetic-full.jsonl"]


# The fixture's two projects. Its rows name them by `workspaceRoot`, but a
# row only sits under a project the store has adopted; a fresh state dir
# has adopted only the launch directory, so without these every row fell
# under "Unfiled" beside an empty project head. Adopting live is not
# enough on its own: the fixture's claude-code and codex rows carry no
# provider record, so a live rejoin never re-resolves them (a real row of
# those providers always has one and does regroup) and only the Muse rows
# moved. So the probe primes the entry's state dir with these adoptions in
# a run of their own before the first capture ([`prime_adoptions`]): every
# attempt boots with the projects stored, files every row at boot, and the
# steps' own `project:` then finds them adopted and changes nothing.
_ADOPT = "project:fixtures/ws/acme-web;project:fixtures/ws/acme-internal"


def _populated(steps=None, replay=False, theme=None, workspace=None, sidebar=True):
    args = ["--no-connect", "--login", "signed-in"]
    if sidebar:
        args += _POPULATED_SIDEBAR
        # Every populated sidebar shows the sessions grouped under the two
        # projects (docs/83 §8 item 4), not only `sidebar-projects`. Not
        # for an entry that opens its own repo (`workspace`): adopting
        # leaves acme-internal current, so the Files pane looked for the
        # repo's files in the wrong folder ("This file is gone", Gate 2).
        if not workspace and (not steps or "project:" not in steps):
            steps = _ADOPT + (";" + steps if steps else "")
    if replay:
        args += _FULL
    if theme:
        args += ["--theme", theme]
    if workspace:
        args += ["--workspace", workspace]
    if steps:
        args += ["--steps", steps]
    return {"shot": args,
            "env": {"BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-populated-state-"),
                    "BAAZ_PROVIDER_STATUS_SCRIPT": _p6_statuses(_P6_CLAUDE)}}


_PROJECTS = _ADOPT + ";group-by:project"


def _with_sidebar(entry, name):
    """A populated entry over another sidebar fixture (fixtures/sidebar/<name>)."""
    entry["shot"] = [_p6_sidebar_fixture(name) if a == _POPULATED_SIDEBAR[1] else a for a in entry["shot"]]
    return entry

ENTRIES.update({
    "shell-populated":         _populated(replay=True),
    "shell-populated-light":   _populated(replay=True, theme="light"),
    "sidebar-projects":        _populated(_PROJECTS),
    # Task 3.2: acme-internal closed, so its header shows the strongest
    # state of the sessions it hides (one waiting: the amber dot); acme-web
    # stays open with a row in every state. `project-close` is a setter.
    "sidebar-projects-collapsed": _populated(_PROJECTS + ";project-close:acme-internal"),
    # Task 3.4: the "Needs you" row at the top of the nav, its filter on —
    # only the three sessions waiting on the person, the row lit. A setter.
    "sidebar-needs-you":       _populated(_PROJECTS + ";needs-you-filter:on"),
    "sidebar-projects-light":  _populated(_PROJECTS, theme="light"),
    "transcript-full":         _populated(replay=True, sidebar=False),
    "transcript-full-light":   _populated(replay=True, sidebar=False, theme="light"),
    "transcript-full-top":     _populated("top", replay=True, sidebar=False),
    "transcript-full-mid":     _populated("mid", replay=True, sidebar=False),
    "transcript-full-compact": _populated("settings-set:density=compact", replay=True, sidebar=False),
    "sidebar-projects-compact": _populated(_PROJECTS + ";settings-set:density=compact"),
    "transcript-full-large":   _populated("settings-set:text-size=large", replay=True, sidebar=False),
    # docs/82 7.5: ⌘F's bar over the populated transcript, every match of
    # "validation" tinted and the current one stronger, the count reading
    # "N of M"; then one ⌘G on, the next match current and scrolled to.
    "transcript-find":         _populated("transcript-find:validation", replay=True, sidebar=False),
    "transcript-find-next":    _populated("transcript-find:validation;transcript-find-next", replay=True, sidebar=False),
    "composer-menu-model":     _populated("fixture-models;model", replay=True),
    "composer-menu-effort":    _populated("effort", replay=True),
    "composer-menu-provider":  _populated("provider", replay=True),
    "composer-menu-mode":      _populated("mode", replay=True),
    "right-changes-full":      _populated("right-width:440;right:changes;changes-open:crates/baaz/src/right.rs", replay=True),
    # Task 6.3: the Changes scope row over a session's checkpoints, on Last
    # turn with a row open. A capture reads and writes no refs (it runs in
    # the owner's repository): `changes-scope:` loads a fixture index of two
    # finished turns over the fixture tree. No baseline yet.
    "right-changes-scope-last": _populated("right-width:440;right:changes;changes-scope:last;changes-open:crates/baaz/src/layout.rs", replay=True),
    # Task 6.4's confirms, from fixture data: a capture takes no
    # checkpoints and runs in the owner's repository, so neither button can
    # apply anything (the session refuses: no checkpoints).
    # Task 6.5: the Tasks screen from fixture tasks (a dev server running on
    # :5173, a test run exited 0, a lint exited 1, a watcher stopped), with
    # no process behind any of them; its empty state; and the sidebar row's
    # "N running" in its trailing slot (On-call runbook, p-int-5: its slot is otherwise empty; the open session's holds an attention dot, which wins), from the same
    # fixture). No baselines yet.
    "right-tasks":             _populated("right-width:440;tasks-fixture;right:tasks", replay=True),
    "right-tasks-empty":       _populated("right-width:440;right:tasks", replay=True),
    "sidebar-tasks-running":   _populated("tasks-fixture:p-int-5", replay=True),
    # docs/82 8.4, switching modes keeps your place. `sidebar-page-count`:
    # Audit log export owns two pages and Search scopes one (fixture tabs, no
    # page loaded) — "2 pages" in an empty slot at rest, "1 page" beside
    # the ⋯ on the hovered row — and the open session owns two, so its
    # header says "2 pages". `browse-footer-context`: Browse over a page,
    # its footer saying "Needs you" with the waiting count and naming the
    # session Sessions stood on (the replay) with its state mark. No
    # baselines yet.
    "sidebar-page-count":      _populated("pages-fixture;pages-fixture:p-int-2,2;pages-fixture:p-web-5,1;"
                                          "sidebar-hover:p-web-5", replay=True),
    "browse-footer-context":   _populated("mode:browse;tab-new:https://example.com", replay=True),
    # docs/82 8.5, the session row's one menu. `sidebar-row-menu`: Audit log
    # export's menu open under its ⋯ over the populated sidebar — the row
    # given the local record a real row of its provider carries
    # (`fixture-record:`, in memory only), so every row it can offer shows:
    # Rename, Pin, Fork (where its provider forks), Continue in…, Move to
    # project ›, Reveal folder, Archive, Delete…. `sidebar-delete-confirm`:
    # its Delete… confirm, naming what goes. `composer-continue-in`: the
    # row menu's Continue in… on the open session (the replay, which has
    # turns) — its provider menu reads "Continue in…". No baselines yet.
    "sidebar-row-menu":        _populated(_PROJECTS + ";fixture-record:p-int-2;sidebar-hover:p-int-2;row-menu:p-int-2"),
    "sidebar-delete-confirm":  _populated(_PROJECTS + ";fixture-record:p-int-2;row-action:p-int-2,delete"),
    # docs/82 8.9: the header's ⋯ is the same menu — the open session's
    # (the replay's) row rows in the row's order, then what only the open
    # session has (here Undo last turn: the replay's last turn is
    # undoable). No baseline yet.
    "session-menu-header":     _populated("overflow", replay=True),
    # docs/82 8.9, one word each: the ⌘K rows for "continue" (/continue,
    # "Continue in another provider…") and "page" (/page, "Show the Page in
    # the right pane"; Skills' row too, set up here). No baselines yet.
    "palette-continue":        _populated("palette:continue"),
    "palette-page":            _populated("palette:page"),
    "composer-continue-in":    _populated("row-action:continue-in", replay=True),
    "undo-turn-confirm":       _populated("undo-turn:last", replay=True),
    "revert-file-confirm":     _populated("right-width:440;right:changes;revert-file:crates/baaz/src/layout.rs", replay=True),
    "right-files-preview":     _populated("right-width:440;right:files;files-select:src/lib.rs",
                                          workspace=_REPO_ACME),
    # Task 7.1: lines selected in the pane, the selection bar hung over
    # them, and Quote's chip in the composer. `select-lines:` holds the
    # selection a press-and-drag makes; `quote-lines` is the bar's Quote.
    # Over the fixture diff (two added lines and two more), over the acme
    # repo's lib.rs preview (lines 5-8), and the chip of two removed lines
    # (numbered on the old side). The replay is the open session whose
    # composer the quote goes to. No baselines yet.
    "right-changes-select":    _populated("right-width:440;right:changes;changes-open:crates/baaz/src/right.rs;wait:800;"
                                          "select-lines:crates/baaz/src/right.rs:2823-2826", replay=True),
    "right-preview-select":    _populated("right-width:440;right:files;files-select:src/lib.rs;wait:800;"
                                          "select-lines:src/lib.rs:5-8", replay=True, workspace=_REPO_ACME),
    "composer-quote-lines":    _populated("right-width:440;right:changes;changes-open:crates/baaz/src/right.rs;wait:800;"
                                          "select-lines:crates/baaz/src/right.rs:old:2823-2824;quote-lines", replay=True),
    # Task 7.3: the PR from recorded `gh` output (fixtures/gh; a capture
    # runs no gh). `pr-details:` makes the recorded PR the fixture tree's
    # (clean, on its branch) and opens Details: the failing check first
    # with Send to agent, the review asking for changes, the line comment
    # at src/lib.rs:7. `pr-fixture:merged`: the merged card offering
    # Archive this session. The replay is the open session both act on.
    # No baselines yet.
    "right-pr-details":        _populated("right-width:440;pr-details:failing", replay=True),
    "right-pr-merged":         _populated("right-width:440;pr-fixture:merged", replay=True),
    # The row: p-int-2 (Audit log export) sits on fix-checkout in
    # acme-internal, the pane's project; its summary line and hover card
    # carry the PR after the branch.
    "sidebar-pr":              _with_sidebar(_populated("settings-set:summary-line=on;pr-fixture:failing;click:p-int-2;wait:400;row-detail:p-int-2;wait:400", replay=True), "pr.json"),
    "right-page":              _populated("right-width:440;right:browser;browse:https://example.com", replay=True),
    # The Page at the narrowest the pane goes (docs: under 300 px back / forward
    # leave the nav row): the address must still read the host.
    "right-page-narrow":       _populated("right-width:280;right:browser;browse:https://example.com", replay=True),
    "terminal-dock":           _populated("terminal-dock:zsh", replay=True),
    # docs/82 8.1, the pane as the session's workspace. The same FakePty
    # tab as `terminal-dock`, moved into the pane through the header
    # control's path (`terminal-place:pane`): as the top view with the pane
    # closed; below the Page when the Page is on top (browser over
    # terminal); and Changes over Files with the default divider.
    "right-terminal":            _populated("right-width:440;terminal-dock:zsh;terminal-place:pane", replay=True),
    "right-page-terminal":       _populated("right-width:440;right:browser;browse:https://example.com;"
                                            "terminal-dock:zsh;terminal-place:pane", replay=True),
    "right-changes-lower-files": _populated("right-width:440;right:changes;right-lower:files", replay=True),
    "settings-sidebar":        _populated("settings:sidebar"),
    "palette-populated":       _populated("palette"),
    # Task 2.4's peek, held open with the sidebar closed: `sidebar` closes
    # it (deterministic runs never write the layout, so both settle shots
    # start open and agree) and `sidebar-peek` holds the hover peek with
    # no pointer. The populated rows, so the peek reads filled.
    "sidebar-peek":            _populated("sidebar;sidebar-peek", replay=True),
    "dialog":                  _populated("error-dialog:offline"),
    "menu":                    _populated("view-menu"),
    "browse-spaces":           _populated("mode:browse;tab-new:https://example.com;tab-new:https://docs.rs/;"
                                          "space-new:Research;tab-new:https://www.wikipedia.org/;"
                                          "tab-new:https://doc.rust-lang.org/book/"),
})

# Task 7.4 · Run and preview. A folder at one fixed path (its name shows in
# the header crumb and the hero) declaring a Run that serves the folder
# with Python's static server on the session's port, and its preview.
# `run-offered`: the header ⋯ menu offers Run on a session in it (no
# process). `run-preview`: Run pressed, the preview's port answers, the
# session's Page opens on it (a capture's Page is the scripted page, so the
# address row is what shows the URL). The session is in no worktree, so its
# block is the top one, 60990, while nothing else holds it; quit ends the
# server's group (6.5), so the next attempt finds the port free again.
def _run_fixture():
    root = os.path.join(REAL_TMP, "baaz-uiprobe-run", "static-site")
    try:
        os.makedirs(os.path.join(root, ".baaz"), exist_ok=True)
        with open(os.path.join(root, ".baaz", "project.toml"), "w") as f:
            f.write('run = "python3 -m http.server $BAAZ_PORT --bind 127.0.0.1"\n'
                    'preview_url = "http://127.0.0.1:${BAAZ_PORT}/"\n')
        with open(os.path.join(root, "index.html"), "w") as f:
            f.write("<!doctype html><title>Static site</title><h1>Static site</h1>\n")
    except OSError as error:
        print(f"uiprobe: the run fixture was not built ({error})", file=sys.stderr)
    return root


_RUN_SITE = _run_fixture()

ENTRIES.update({
    "run-offered":  {"shot": ["--no-connect", "--login", "signed-in",
                              "--steps", f"project:{_RUN_SITE};new;wait:300;overflow"]},
    "run-preview":  {"shot": ["--no-connect", "--login", "signed-in",
                              "--steps", f"project:{_RUN_SITE};new;wait:300;run;run-wait-preview:30"]},
})

# Task 2.11 · light theme pass: the light twin of the screens a person
# sees filled, plus one of each floating kind (menu, palette, dialog, sheet)
# and the pages, so "renders light everywhere" is looked at rather than
# assumed. Populated twins go through `_populated(theme=...)`; the rest take
# the same `--theme light` the populated ones do, on a fresh state dir so no
# dark run's store leaks in. Offline, nothing sends a turn.
def _light(entry):
    spec = dict(ENTRIES[entry])
    spec["shot"] = ["--theme", "light"] + list(spec["shot"])
    spec["env"] = {**spec.get("env", {}),
                   "BAAZ_STATE_DIR": tempfile.mkdtemp(prefix="uiprobe-light-state-")}
    return spec


ENTRIES.update({
    "composer-menu-model-light": _populated("fixture-models;model", replay=True, theme="light"),
    "right-changes-full-light":  _populated("right-width:440;right:changes;changes-open:crates/baaz/src/right.rs",
                                            replay=True, theme="light"),
    "right-files-preview-light": _populated("right-width:440;right:files;files-select:src/lib.rs",
                                            workspace=_REPO_ACME, theme="light"),
    "terminal-dock-light":       _populated("terminal-dock:zsh", replay=True, theme="light"),
    "palette-populated-light":   _populated("palette", theme="light"),
    "dialog-light":              _populated("error-dialog:offline", theme="light"),
    "menu-light":                _populated("view-menu", theme="light"),
    "browse-spaces-light":       _populated("mode:browse;tab-new:https://example.com;tab-new:https://docs.rs/;"
                                            "space-new:Research;tab-new:https://www.wikipedia.org/;"
                                            "tab-new:https://doc.rust-lang.org/book/", theme="light"),
    "settings-general-light":    _light("settings-general"),
    "skills-page-light":         _light("skills-page"),
    "import-sheet-light":        _light("import-sheet"),
    "setup-sheet-install-light": _light("setup-sheet-install"),
    # Code blocks, diffs and tool output in the transcript, and the two
    # Browse pieces that were drawn dark in both themes before 2.11 (the
    # Peek's caption and ×) beside the new-tab page.
    "transcript-full-top-light": _populated("top", replay=True, sidebar=False, theme="light"),
    "transcript-full-mid-light": _populated("mid", replay=True, sidebar=False, theme="light"),
    "browse-newtab-light":       _light("browse-newtab"),
    "browse-peek-light":         _light("browse-peek"),
})

LIGHT_ENTRIES = sorted(e for e in ENTRIES if e.endswith("-light"))


POPULATED_ENTRIES = [
    "shell-populated", "shell-populated-light", "sidebar-projects", "sidebar-projects-light",
    "transcript-full", "transcript-full-light", "transcript-full-top", "transcript-full-mid",
    "composer-menu-model", "composer-menu-effort", "composer-menu-provider", "composer-menu-mode",
    "right-changes-full", "right-files-preview", "right-page", "terminal-dock", "settings-sidebar",
    "right-terminal", "right-page-terminal", "right-changes-lower-files",
    "palette-populated", "dialog", "menu", "browse-spaces", "sidebar-projects-collapsed",
    "sidebar-needs-you",
]


def boot(entry, verb):
    spec = ENTRIES.get(entry)
    if spec is None:
        sys.exit(f"uiprobe: unknown entry {entry!r}; have: {', '.join(sorted(ENTRIES))}")
    if verb not in spec:
        sys.exit(f"uiprobe: entry {entry!r} has no {verb!r} probe "
                 f"(it supports: {', '.join(sorted(spec))})")
    return [BAAZ] + spec[verb]


def attempt_state(env, pristine):
    """`env` with BAAZ_STATE_DIR pointed at a fresh copy of `pristine`
    (the entry's state dir as it stood before any capture, plus its primed
    adoptions), or `env`
    unchanged when the entry names none. Every capture attempt starts from
    the same state, as an entry with no state dir already does (baaz makes
    a throwaway one per process): reusing one dir let attempt two read
    attempt one's sessions, and the scripted lanes, which number their ids
    from `s-scripted` again in every process, opened attempt two's new
    draft on attempt one's busy session — its title in the header, its row
    selected (`shared-folder-banner`, 6.2)."""
    if pristine is None:
        return env
    fresh = os.path.join(tempfile.mkdtemp(prefix="uiprobe-attempt-"), "state")
    shutil.copytree(pristine, fresh)
    return {**env, "BAAZ_STATE_DIR": fresh}


def prime_adoptions(cmd, env, pristine):
    """Run the entry once with only its `project:` steps on `pristine`,
    so the projects are stored before any capture boots (see `_ADOPT`).
    A no-op for an entry that adopts nothing or names no state dir. The
    run is a capture like any other (offline, takes no focus); its image
    is thrown away. Returns an error string, or None."""
    if pristine is None or "--steps" not in cmd:
        return None
    at = cmd.index("--steps") + 1
    adopt = [s for s in cmd[at].split(";") if s.strip().startswith("project:")]
    if not adopt:
        return None
    prime = cmd[:at] + [";".join(adopt)] + cmd[at + 1:]
    shot = os.path.join(tempfile.mkdtemp(prefix="uiprobe-prime-"), "prime.png")
    p = subprocess.run(prime + ["--screenshot", shot, "--screenshot-delay", "200"],
                       cwd=REPO, capture_output=True, text=True, timeout=180,
                       env={**env, "BAAZ_STATE_DIR": pristine})
    if p.returncode != 0:
        return "uiprobe: priming the adoptions failed:\n" + (p.stderr[-1500:] or p.stdout[-1500:])
    return None


def main():
    keep_display_awake()
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
        over = pointer_over_capture()
        if over:
            sys.stderr.write(f"uiprobe: the pointer rests over the capture area at "
                             f"({over[0]:.0f}, {over[1]:.0f}); move it off the top-left "
                             f"1440x900 and rerun (a hover would be captured)\n")
            return 1
        env = merged_env("shot")
        cmd = boot(a.entry, "shot") + extra
        # Per-entry scripted environment (provider statuses for the connect
        # entries): merged over the deterministic base, entry only.
        env = {**PROBE_ENV, **ENTRIES[a.entry].get("env", {})}
        # Capture twice and require the two to agree. With BAAZ_DETERMINISTIC
        # set they always should, so this is cheap; when they do not, the screen
        # is genuinely still animating and reporting that beats baselining an
        # arbitrary frame. Each attempt runs on its own copy of the entry's
        # state dir as it stood before the first ([`attempt_state`]), with
        # the entry's adoptions primed into it ([`prime_adoptions`]), so
        # the two compared are the same run, not a run and its sequel.
        import filecmp
        tmp, prev = tempfile.mkdtemp(), None
        state = env.get("BAAZ_STATE_DIR")
        pristine = None
        if state and os.path.isdir(state):
            pristine = os.path.join(tempfile.mkdtemp(prefix="uiprobe-pristine-"), "state")
            shutil.copytree(state, pristine)
        failed = prime_adoptions(cmd, env, pristine)
        if failed:
            sys.stderr.write(failed + "\n")
            return 1
        for attempt in range(SETTLE_TRIES):
            shot = os.path.join(tmp, f"s{attempt}.png")
            p = subprocess.run(cmd + ["--screenshot", shot,
                                      "--screenshot-delay", str(a.delay_ms)],
                               cwd=REPO, capture_output=True, text=True,
                               timeout=180, env=attempt_state(env, pristine))
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
