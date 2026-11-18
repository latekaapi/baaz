#!/usr/bin/env python3
"""The flat gate (task 2.12, docs/83 §8 items 1, 2 and 5).

Three checks, each printed with what it found, so a failure says where:

1. **No depth vocabulary** (§8 item 1). The names of the deleted depth tokens
   and helpers — `shadow_1…8`, `shadow_edge`, `inset_highlight`, `CONTACT_W`,
   `LIST_FADE`, `SHEET_INSET` — appear nowhere in `agentic-ui/crates` or
   `harness/crates` production code. Cut before matching: `target/`, `tests/`
   directories, `#[cfg(test)]` items, comments. A comment may still say what
   was deleted; code may not call it.

2. **No colour literal outside tokens** (§8 item 2), in `crates/baaz/src` and
   `agentic-ui/crates/aui/src` (production code as above). A colour literal is
   any of:
     - `rgb(` / `rgba(` — gpui's hex constructors; the tokens give `Hsla`, so
       any call is a literal (or a hex `const` passed through one);
     - `hsla(` with a numeric first argument;
     - an `Hsla { h: … }` struct literal (an alpha change on a token,
       `Hsla { a: 0.1, ..token }`, is not one: `.alpha()` spelled out);
     - `#RRGGBB` / `#RRGGBBAA` inside a string;
     - gpui's named colours `black()`, `white()`, `red()`, `green()`,
       `blue()`, `yellow()` (`transparent_black()` is "no colour" and passes).
   The tokens themselves (`aui-tokens`), the terminal ANSI table (`aui-terminal`,
   a palette by definition) and the provider marks (SVG assets in `aui-icons`)
   are outside the scanned roots. What remains is allowed only by an entry
   in `ALLOW` below, each with its reason; an entry that matches nothing
   fails the gate too, so the list cannot rot.

3. **Pixels of the populated dark shell** (§8 item 5), measured on the
   `shell-populated` capture (taken fresh through `scripts/uiprobe.py`, or
   `--shot <png>` to measure one already taken): ground `#191919`, sidebar
   `#232323`, exactly one 1 px line between sidebar and centre, no gradient
   run at a region edge (sidebar/centre, header bottom, footer top), session
   row pitch 34, header 44. Colours pass within ±3 per channel; a 1 px line
   at a 1× capture may read a few levels off its token (the downsample rings
   into its neighbours, docs/82-status 2.4), so a line passes when it is one
   pixel wide and differs from both sides.

    python3 scripts/flat-gate.py                  # all three; captures shell-populated once the
                                                  # machine has been idle 60 s (--no-idle-wait skips)
    python3 scripts/flat-gate.py --shot out.png   # measure a capture already taken
    python3 scripts/flat-gate.py --no-pixels      # greps only (no window opens)
    python3 scripts/flat-gate.py --json           # one JSON object on stdout
    python3 scripts/flat-gate.py --self-test [--shot out.png]   # plant defects in copies, expect failure
    python3 scripts/flat-gate.py -v               # also list each allowed literal and its reason

Exit 0 only when every check passes; 1 otherwise; 2 when the capture could not
be taken (that is not a pass).
"""
import argparse, json, os, re, shutil, subprocess, sys, tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
HARNESS = os.path.dirname(HERE)
AUI = os.path.join(os.path.dirname(HARNESS), "agentic-ui")
PROJECTS = os.path.dirname(HARNESS)

DEPTH_ROOTS = [os.path.join(AUI, "crates"), os.path.join(HARNESS, "crates")]
COLOUR_ROOTS = [os.path.join(AUI, "crates", "aui", "src"), os.path.join(HARNESS, "crates", "baaz", "src")]

DEPTH_RE = re.compile(r"shadow_[1-8]|shadow_edge|inset_highlight|CONTACT_W|LIST_FADE|SHEET_INSET")

COLOUR_RULES = [
    ("rgb()", re.compile(r"\brgba?\(")),
    ("hsla()", re.compile(r"\bhsla\(\s*[-0-9.]")),
    ("Hsla {h..}", re.compile(r"\bHsla\s*\{\s*h\s*:")),
    ("#hex", re.compile(r"#[0-9A-Fa-f]{6}(?:[0-9A-Fa-f]{2})?\b")),
    ("named", re.compile(r"(?<![A-Za-z0-9_])(?:gpui::)?(black|white|red|green|blue|yellow)\(\)")),
]

# Every allowed colour literal: (path suffix, text the hit's line contains, reason).
# Keep each reason honest: it is the documentation of the exception.
ALLOW = [
    ("aui/src/overlay/mod.rs", "gpui::black().opacity(scale::SCRIM)",
     "the scrim is black at the `scrim` token's opacity (§2.1); black is not a theme colour"),
    ("aui/src/overlay/card.rs", "black().opacity(aui_tokens::scale::SCRIM",
     "the same scrim, faded with the card's presence"),
    ("aui/src/interact/paint.rs", "gpui::black().alpha(theme.a) } else { gpui::white().alpha(theme.a)",
     "the ov-hover/selected/pressed tokens are alphas of black on light grounds and white on dark (§2.1)"),
    ("aui/src/shell/app_shell.rs", "let dot = |c: u32| div().size(px(12.0)).rounded_full().bg(gpui::rgb(c));",
     "macOS window controls painted for a gallery card (`painted_lights`): the system's colours, not the theme's"),
    ("aui/src/transcript/syntax.rs", "HighlightStyle { color: Some(Hsla { h: index as f32 / 8.0",
     "a sentinel hue that encodes a capture index for gpui-kit's resolver and is decoded at once; never painted"),
]

PIXEL_SPEC = {
    "ground": (0x19, 0x19, 0x19),
    "sidebar": (0x23, 0x23, 0x23),
    "colour_tol": 3,
    "row_pitch": 34,
    "header": 44,
}


# --- source reading ---------------------------------------------------------

def strip_comments(text):
    """Blank out // and /* */ comments, keep strings and line numbers."""
    out, i, n = [], 0, len(text)
    in_str = in_chr = False
    while i < n:
        c = text[i]
        if in_str:
            out.append(c)
            if c == "\\" and i + 1 < n:
                out.append(text[i + 1]); i += 2; continue
            if c == '"':
                in_str = False
            i += 1; continue
        if c == "r" and re.match(r'r#*"', text[i:i + 4]) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == "_")):
            m = re.match(r'r(#*)"', text[i:])
            end = text.find('"' + m.group(1), i + len(m.group(0)))
            end = n if end < 0 else end + 1 + len(m.group(1))
            out.append(text[i:end]); i = end; continue
        if c == '"':
            in_str = True; out.append(c); i += 1; continue
        if c == "'" and re.match(r"'(\\.|[^\\'])'", text[i:i + 4]):
            m = re.match(r"'(\\.|[^\\'])'", text[i:])
            out.append(m.group(0)); i += len(m.group(0)); continue
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i)); i = j; continue
        if text.startswith("/*", i):
            j = text.find("*/", i + 2)
            j = n if j < 0 else j + 2
            out.append(re.sub(r"[^\n]", " ", text[i:j])); i = j; continue
        out.append(c); i += 1
    return "".join(out)


def cut_cfg_test(text):
    """Blank every `#[cfg(test)]` item (a module, fn or impl: up to its
    matching brace, or to the `;` of a `mod x;`). Line numbers are kept."""
    chars = list(text)
    for m in reversed(list(re.finditer(r"#\[cfg\((?:test|all\(test[^\]]*)\)\]", text))):
        i, depth, end = m.end(), 0, None
        while i < len(text):
            if text[i] == "{":
                depth += 1
            elif text[i] == "}":
                depth -= 1
                if depth == 0:
                    end = i + 1; break
            elif text[i] == ";" and depth == 0:
                end = i + 1; break
            i += 1
        end = end or len(text)
        for k in range(m.start(), end):
            if chars[k] != "\n":
                chars[k] = " "
    return "".join(chars)


def production(path):
    return cut_cfg_test(strip_comments(open(path, encoding="utf-8").read()))


def test_only_modules(root):
    """Files whose `mod` line is `#[cfg(test)]` (`#[cfg(test)] mod hotpath_bench;`)."""
    out = set()
    for d, dirs, files in os.walk(root):
        dirs[:] = [x for x in dirs if x != "target"]
        for f in files:
            if not f.endswith(".rs"):
                continue
            text = open(os.path.join(d, f), encoding="utf-8").read()
            here = d if f in ("mod.rs", "lib.rs", "main.rs") else os.path.join(d, f[:-3])
            for m in re.finditer(r"#\[cfg\(test\)\]\s*(?:pub(?:\([^)]*\))?\s+)?mod\s+(\w+)\s*;", text):
                out.add(os.path.join(here, m.group(1) + ".rs"))
                out.add(os.path.join(here, m.group(1), "mod.rs"))
    return out


def rust_files(root):
    skip = test_only_modules(root)
    for d, dirs, files in os.walk(root):
        dirs[:] = sorted(x for x in dirs if x not in ("target", "tests", "benches", "generated"))
        for f in sorted(files):
            p = os.path.join(d, f)
            if f.endswith(".rs") and f != "tests.rs" and not f.endswith("_tests.rs") and p not in skip:
                yield p


def rel(p):
    return os.path.relpath(p, PROJECTS)


def scan_depth(roots):
    hits = []
    for root in roots:
        for p in rust_files(root):
            text = production(p)
            for m in DEPTH_RE.finditer(text):
                line = text.count("\n", 0, m.start()) + 1
                hits.append({"file": rel(p), "line": line, "match": m.group(0)})
    return hits


def scan_colour(roots, allow):
    found, used = [], [0] * len(allow)
    for root in roots:
        for p in rust_files(root):
            text = production(p)
            lines = text.split("\n")
            for no, line in enumerate(lines, 1):
                for rule, rx in COLOUR_RULES:
                    for m in list(rx.finditer(line))[:1]:
                        why = None
                        for k, (suffix, needle, reason) in enumerate(allow):
                            if p.endswith(suffix) and needle in line:
                                why, used[k] = reason, used[k] + 1
                                break
                        found.append({"file": rel(p), "line": no, "rule": rule,
                                      "text": line.strip()[:110], "allowed": why})
    stale = [{"file": s, "needle": n, "reason": r} for (s, n, r), u in zip(allow, used) if u == 0]
    return found, stale


# --- pixels -----------------------------------------------------------------

def idle_seconds():
    try:
        out = subprocess.run(["ioreg", "-c", "IOHIDSystem"], capture_output=True, text=True, timeout=10).stdout
    except (OSError, subprocess.SubprocessError):
        return None
    for line in out.splitlines():
        if "HIDIdleTime" in line:
            return int(line.split()[-1]) // 1_000_000_000
    return None


def wait_idle(need=60, max_wait=3 * 3600):
    """A capture launches Baaz, which can take focus: wait until nobody has
    touched the machine for `need` seconds (CLAUDE.md standing rule 6)."""
    import time
    start = time.time()
    while True:
        idle = idle_seconds()
        if idle is None or idle >= need:
            return
        if time.time() - start > max_wait:
            raise RuntimeError(f"the machine never went idle for {need} s; no capture taken")
        sys.stderr.write(f"flat-gate: idle {idle} s, waiting for {need} s before capturing\n")
        time.sleep(max(5, need - idle))


def capture_shell_populated(out, idle_wait=True):
    if idle_wait:
        wait_idle()
    if not os.path.exists(os.path.join(HARNESS, "target", "debug", "baaz")):
        raise RuntimeError("target/debug/baaz is missing: run `cargo build -p baaz` first")
    p = subprocess.run([sys.executable, os.path.join(HERE, "uiprobe.py"), "--entry", "shell-populated",
                        "--probe-shot", out], cwd=HARNESS, capture_output=True, text=True, timeout=600)
    if p.returncode != 0 or not os.path.exists(out):
        raise RuntimeError("uiprobe capture failed: " + (p.stderr[-800:] or p.stdout[-800:]))


def runs(im, axis, fixed, lo, hi, tol=2):
    """Segments of near-constant colour along a row ('x') or column ('y'):
    (start, end, (r,g,b)), the colour read at the run's middle so a ringing
    first pixel does not stand for it. Unlike `runs()` in
    docs/81-audit/measure-reference.md §1, a pixel is compared with the run's
    FIRST pixel, not the previous one, so a slow gradient (1 level per px)
    breaks into short runs instead of merging into one."""
    get = (lambda v: im.getpixel((v, fixed))[:3]) if axis == "x" else (lambda v: im.getpixel((fixed, v))[:3])
    out, first, start = [], None, lo
    for v in range(lo, hi):
        px = get(v)
        if first is None or max(abs(a - b) for a, b in zip(px, first)) > tol:
            if first is not None:
                out.append((start, v - 1, get((start + v - 1) // 2)))
            start, first = v, px
    out.append((start, hi - 1, get((start + hi - 1) // 2)))
    return out


def hexc(c):
    return "#%02x%02x%02x" % tuple(c[:3])


def close(c, want, tol):
    return max(abs(a - b) for a, b in zip(c[:3], want)) <= tol


def edge_ok(seg, at, span=24):
    """Is the boundary at `at` (a 1 px line run) clean? Within `span` px on
    each side there may be the fill and at most one 1–2 px ringing neighbour
    (the 1× downsample of a 1 px line reads a few levels off, docs/82-status
    2.4): two runs per side at most. A fade or gradient is a staircase of runs
    and fails. Returns (ok, runs per side)."""
    before = [r for r in seg if r[1] < at and r[1] >= at - span]
    after = [r for r in seg if r[0] > at and r[0] <= at + span]
    ringing_ok = all(r[1] - r[0] < 2 for r in before[1:] + after[:-1]) if before and after else False
    # with two runs a side, the one touching the line must be the short one
    if len(before) == 2:
        ringing_ok = ringing_ok and before[1][1] - before[1][0] < 2
    if len(after) == 2:
        ringing_ok = ringing_ok and after[0][1] - after[0][0] < 2
    return len(before) <= 2 and len(after) <= 2 and ringing_ok, f"{len(before)}+{len(after)}"


def clean_row(im, ys, lo, hi, min_first):
    """The first probe row (from `ys`) whose first run is a long flat fill:
    a row through the sidebar that crosses no text or icon."""
    for y in ys:
        seg = runs(im, "x", y, lo, hi)
        if seg[0][1] - seg[0][0] >= min_first:
            return y, seg
    return None, None


def check_pixels(path):
    from PIL import Image
    im = Image.open(path).convert("RGB")
    W, H = im.size
    S = PIXEL_SPEC
    tol = S["colour_tol"]
    checks = []

    def add(name, ok, measured, want):
        checks.append({"check": name, "ok": bool(ok), "measured": measured, "want": want})

    # A row through the sidebar and the centre that crosses no text: from the
    # bottom up (the footer's empty strip, then the composer's foot).
    y, row = clean_row(im, range(H - 2, H // 2, -3), 0, min(W, 700), 150)
    if row is None:
        add("clean probe row", False, "every row crosses text", "a text-free row")
        return checks, {"size": [W, H]}
    side, line, ground = row[0], (row[1] if len(row) > 1 else None), (row[2] if len(row) > 2 else None)
    sidebar_end = side[1]
    add("sidebar colour", close(side[2], S["sidebar"], tol), f"{hexc(side[2])} (x 0–{sidebar_end}, y {y})", "#232323 ±3")
    one_px = line is not None and line[0] == line[1] and ground is not None and ground[1] - ground[0] > 40
    distinct = one_px and not close(line[2], side[2], tol) and not close(line[2], ground[2], tol)
    add("one 1 px line sidebar|centre", distinct, f"x {line[0]} {hexc(line[2])}" if line else "none",
        "exactly one 1 px run between the fills")
    add("ground colour", ground is not None and close(ground[2], S["ground"], tol),
        f"{hexc(ground[2])} (x {ground[0]}–{ground[1]})" if ground else "none", "#191919 ±3")
    if line:
        ok, n = edge_ok(row, line[0])
        add("no gradient at sidebar|centre", ok, f"runs either side within 24 px: {n}", "≤ 2 a side (fill + 1 ringing px)")
    centre_x0 = (line[0] + 1) if line else sidebar_end + 1

    # Header: down a text-free column of the centre (just past the divider),
    # ground, then one 1 px line whose last pixel is y 43.
    hx = centre_x0 + 6
    col = runs(im, "y", hx, 0, H // 3)
    hdr = next((r for r in col if r[0] == r[1] and r[0] > 20 and not close(r[2], S["ground"], tol)), None)
    header_h = hdr[1] + 1 if hdr else None
    add("header height", header_h == S["header"],
        f"{header_h} (line y {hdr[0]} {hexc(hdr[2])}, x {hx})" if hdr else "no line", "44 incl. its 1 px line")
    if hdr:
        ok, n = edge_ok(col, hdr[0])
        add("no gradient at header bottom", ok, f"runs either side within 24 px: {n}", "≤ 2 a side (fill + 1 ringing px)")
        above = next((r for r in col if r[1] < hdr[0]), None)
        add("header ground", above is not None and close(above[2], S["ground"], tol),
            hexc(above[2]) if above else "none", "#191919 ±3")

    # Sidebar footer: up a text-free sidebar column (x 4), the first 1 px line.
    scol = runs(im, "y", 4, H // 2, H)
    ftr = next((r for r in reversed(scol) if r[0] == r[1] and not close(r[2], S["sidebar"], tol)), None)
    if ftr:
        ok, n = edge_ok(scol, ftr[0])
        below = next((r for r in scol if r[0] > ftr[0]), None)
        add("no gradient at footer top", ok and below is not None and close(below[2], S["sidebar"], tol),
            f"line y {ftr[0]} {hexc(ftr[2])}, footer {hexc(below[2]) if below else '?'}; runs either side {n}",
            "≤ 2 a side (fill + 1 ringing px)")
    else:
        add("no gradient at footer top", False, "no footer line found at x 4", "a 1 px line over the footer")

    # Session row pitch: the rows' text tops (ink brighter than 0x80 in the
    # text band x 18–200) repeat every 34 px; the commonest gap is the pitch.
    top_y = header_h or S["header"]
    bottom_y = ftr[0] if ftr else H - 44
    lit = [any(max(im.getpixel((x, yy))) > 0x80 for x in range(18, min(200, sidebar_end - 40)))
           for yy in range(top_y, bottom_y)]
    tops = [i + top_y for i in range(1, len(lit)) if lit[i] and not lit[i - 1]]
    gaps = [b - a for a, b in zip(tops, tops[1:])]
    pitch = max(set(gaps), key=gaps.count) if gaps else None
    n = gaps.count(pitch) if gaps else 0
    add("session row pitch", pitch == S["row_pitch"], f"{pitch} ({n} of {len(gaps)} text-top gaps)", "34")

    # The selected row's fill, for the record: 32 tall in a 34 pitch.
    fill = next((r for r in runs(im, "y", 150, top_y, bottom_y)
                 if r[1] - r[0] >= 20 and not close(r[2], S["sidebar"], tol)), None)
    if fill:
        add("selected row fill height", fill[1] - fill[0] + 1 == 32,
            f"{fill[1] - fill[0] + 1} ({hexc(fill[2])}, y {fill[0]}–{fill[1]})", "32")
    return checks, {"size": [W, H], "probe_row_y": y, "divider_x": line[0] if line else None}


# --- report -----------------------------------------------------------------

def run(args):
    report = {"depth": None, "colour": None, "pixels": None}
    depth = scan_depth(DEPTH_ROOTS)
    report["depth"] = {"ok": not depth, "hits": depth}
    found, stale = scan_colour(COLOUR_ROOTS, ALLOW)
    bad = [f for f in found if not f["allowed"]]
    report["colour"] = {"ok": not bad and not stale, "unallowed": bad, "stale_allow": stale,
                        "allowed": [f for f in found if f["allowed"]]}
    if not args.no_pixels:
        shot = args.shot
        tmp = None
        try:
            if not shot:
                tmp = tempfile.mkdtemp()
                shot = os.path.join(tmp, "shell-populated.png")
                capture_shell_populated(shot, not args.no_idle_wait)
            checks, meta = check_pixels(shot)
            report["pixels"] = {"ok": all(c["ok"] for c in checks), "shot": shot if args.shot else "fresh capture",
                                "checks": checks, **meta}
            if args.keep_shot and tmp:
                shutil.copyfile(shot, args.keep_shot)
        except RuntimeError as e:
            report["pixels"] = {"ok": False, "error": str(e)}
        finally:
            if tmp:
                shutil.rmtree(tmp, ignore_errors=True)
    return report


def print_report(r, verbose=False):
    d, c, p = r["depth"], r["colour"], r["pixels"]
    print(f"flat gate (docs/83 §8)")
    print(f"  1 depth names      {'PASS' if d['ok'] else 'FAIL'}  {len(d['hits'])} hits")
    for h in d["hits"]:
        print(f"      {h['file']}:{h['line']}: {h['match']}")
    print(f"  2 colour literals  {'PASS' if c['ok'] else 'FAIL'}  {len(c['unallowed'])} unallowed, "
          f"{len(c['allowed'])} allowed by {len(ALLOW)} entries, {len(c['stale_allow'])} stale entries")
    for h in c["unallowed"]:
        print(f"      {h['file']}:{h['line']}: [{h['rule']}] {h['text']}")
    for s in c["stale_allow"]:
        print(f"      stale allow entry: {s['file']} '{s['needle']}' matches nothing")
    if verbose:
        for h in c["allowed"]:
            print(f"      allowed {h['file']}:{h['line']}: {h['allowed']}")
    if p is None:
        print("  5 pixels           SKIPPED (--no-pixels)")
    elif "error" in p:
        print(f"  5 pixels           FAIL  {p['error']}")
    else:
        print(f"  5 pixels           {'PASS' if p['ok'] else 'FAIL'}  shell-populated {p['size'][0]}×{p['size'][1]} ({p['shot']})")
        for k in p["checks"]:
            print(f"      {'ok ' if k['ok'] else 'BAD'} {k['check']:<32} {k['measured']}   (want {k['want']})")


def exit_code(r):
    if r["pixels"] is not None and "error" in r["pixels"]:
        return 2
    return 0 if all(v["ok"] for v in r.values() if v is not None) else 1


def static_verdict(depth_roots, colour_roots):
    depth = scan_depth(depth_roots)
    found, stale = scan_colour(colour_roots, ALLOW)
    bad = [f for f in found if not f["allowed"]]
    return depth, bad, stale


def self_test(shot):
    """Show the gate fails on planted defects, without touching the repos:
    (a) fixtures, one per rule, plus a clean file whose comment and test
    module mention the same text; (b) a copy of the real scanned trees,
    which must pass clean and fail once a shadow call and a hex literal are
    appended to one file; (c) with `--shot`, a copy of the capture with a
    24 px fade painted under the header and a second line beside the
    divider, which the pixel checks must reject."""
    tmp = tempfile.mkdtemp()
    results = []
    try:
        root = os.path.join(tmp, "fixtures")
        os.makedirs(root)
        plants = {
            "shadow.rs": "fn f(d: Div, p: &Palette) -> Div { d.shadow(p.shadow_1.to_vec()) }\n",
            "hex.rs": "fn f(d: Div) -> Div { d.bg(gpui::rgb(0x123456)) }\n",
            "hsla.rs": "fn f() -> Hsla { Hsla { h: 0.1, s: 0.2, l: 0.3, a: 1.0 } }\n",
            "string.rs": "const C: &str = \"#A1B2C3\";\n",
            "named.rs": "fn f(d: Div) -> Div { d.text_color(gpui::white()) }\n",
            "clean.rs": "// rgb(0x123456) in a comment and shadow_1 in a comment\n"
                        "fn f(d: Div, p: &Palette) -> Div { d.bg(p.surface_2).border_color(p.line.alpha(0.5)) }\n"
                        "#[cfg(test)]\nmod tests { fn t() { let _ = gpui::rgb(0xFFFFFF).shadow_2; } }\n",
        }
        for name, body in plants.items():
            open(os.path.join(root, name), "w").write(body)
        got_depth = {os.path.basename(h["file"]) for h in scan_depth([root])}
        got_colour = {os.path.basename(f["file"]) for f in scan_colour([root], [])[0]}
        want_colour = {"hex.rs", "hsla.rs", "string.rs", "named.rs"}
        results.append(("fixtures: each rule fires, comments and tests do not",
                        got_depth == {"shadow.rs"} and got_colour == want_colour,
                        f"depth {sorted(got_depth)}, colour {sorted(got_colour)}"))

        # The real trees, copied.
        aui_src = os.path.join(tmp, "agentic-ui", "crates", "aui", "src")
        baaz_src = os.path.join(tmp, "harness", "crates", "baaz", "src")
        shutil.copytree(COLOUR_ROOTS[0], aui_src)
        shutil.copytree(COLOUR_ROOTS[1], baaz_src)
        d0, b0, s0 = static_verdict([aui_src, baaz_src], [aui_src, baaz_src])
        results.append(("copied tree, untouched: passes", not d0 and not b0 and not s0,
                        f"{len(d0)} depth, {len(b0)} colour, {len(s0)} stale"))
        target = os.path.join(aui_src, "data", "card.rs")
        with open(target, "a") as f:
            f.write("\nfn planted(d: gpui::Div, p: &Palette) -> gpui::Div "
                    "{ d.shadow(p.shadow_1.to_vec()).bg(gpui::rgb(0x123456)) }\n")
        d1, b1, _ = static_verdict([aui_src, baaz_src], [aui_src, baaz_src])
        results.append(("copied tree + `.shadow(p.shadow_1…)` and `rgb(0x123456)` in data/card.rs: fails",
                        len(d1) == 1 and len(b1) == 1,
                        f"{len(d1)} depth hit ({d1[0]['match'] if d1 else '-'}), "
                        f"{len(b1)} colour hit ({b1[0]['text'][-40:] if b1 else '-'})"))

        if shot:
            from PIL import Image
            im = Image.open(shot).convert("RGB")
            checks, meta = check_pixels(shot)
            x0 = meta["divider_x"]
            px = im.load()
            for dy in range(24):          # a fade under the header line
                v = 0x19 + (24 - dy) // 2
                for x in range(x0 + 1, im.size[0]):
                    px[x, 44 + dy] = (v, v, v)
            for y in range(im.size[1]):   # a second line beside the divider
                px[x0 + 2, y] = (0x33, 0x33, 0x33)
            bad = os.path.join(tmp, "planted.png")
            im.save(bad)
            checks2, _ = check_pixels(bad)
            failed = sorted(c["check"] for c in checks2 if not c["ok"])
            results.append(("capture + header fade + doubled divider: fails", bool(failed) and all(c["ok"] for c in checks),
                            "failed: " + ", ".join(failed)))
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
    for name, ok, detail in results:
        print(f"  {'ok ' if ok else 'BAD'} {name}  [{detail}]")
    ok = all(r[1] for r in results)
    print("self-test", "PASS" if ok else "FAIL")
    return 0 if ok else 1


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--shot", help="measure this shell-populated capture instead of taking one")
    ap.add_argument("--keep-shot", help="copy the fresh capture here")
    ap.add_argument("--no-pixels", action="store_true", help="greps only")
    ap.add_argument("--no-idle-wait", action="store_true", help="capture without waiting for 60 s of idle")
    ap.add_argument("--json", action="store_true")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("-v", "--verbose", action="store_true", help="list every allowed literal with its reason")
    a = ap.parse_args()
    if a.self_test:
        return self_test(a.shot)
    r = run(a)
    if a.json:
        print(json.dumps(r, indent=1))
    else:
        print_report(r, a.verbose)
    return exit_code(r)


if __name__ == "__main__":
    sys.exit(main())
