#!/usr/bin/env python3
"""Histogram of resolved font sizes in aui + Baaz (task 2.10, docs/82).

How it counts, and why the count is complete:

* Since task 2.10 the only text-sizing entry points in aui-tokens'
  `AuiStyled` take a `TextRole` (`text_role`, `role_in`, `ui`, `mono`,
  `role_size`); the px entry points (`text_px`, `scaled`, `text_role_in`, the
  f32 `ui`/`mono`) are deleted, so the compiler rejects a literal size there.
  Every production mention of `TextRole::<Role>` is therefore a site that
  resolves to that role, and this script resolves each role to px from
  `agentic-ui/design/tokens/tokens.json` (the same file `build.rs` reads).
* What the compiler cannot see is a raw gpui/gpui-kit call that sizes text
  without the trait: `.text_size(...)`, `font_size: ...`, `rems(...)`. The
  script lists each such call whose argument does not come from a role, so
  the "≤ 9 sizes" claim never rests on an unchecked escape hatch. The known
  ones are glyph-like and say so in code: an initial scaled with its tile
  (avatar, project mark), the icon fallback glyph (aui-icons).

Production code only: `crates/*/src` of agentic-ui (gallery excluded: it is
a specimen sheet) and `crates/baaz/src` of harness, with `#[cfg(test)]`
modules cut off.

    python3 scripts/type-histogram.py            # comfortable, default size
    python3 scripts/type-histogram.py --density compact --size large
Exit 1 when more than 9 distinct sizes resolve, or an unexplained raw call is found.
"""
import argparse, collections, json, os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
HARNESS = os.path.dirname(HERE)
AUI = os.path.join(os.path.dirname(HARNESS), "agentic-ui")
TOKENS = os.path.join(AUI, "design", "tokens", "tokens.json")
ROOTS = [
    os.path.join(AUI, "crates", c, "src")
    for c in ("aui", "aui-tokens", "aui-terminal", "aui-webview", "aui-cef", "aui-motion", "aui-icons")
] + [os.path.join(HARNESS, "crates", "baaz", "src")]

# role -> token stem (`t-<stem>-fs`)
STEM = {"Display": "display", "Title": "title", "Subtitle": "subtitle", "Body": "body", "Caption": "caption",
        "Micro": "micro", "Mono": "mono", "Read": "read", "ReadMono": "read-mono", "H1": "h1", "H2": "h2", "H3": "h3"}
# TextSize::Large borrows the next role's size (aui-tokens `TextRole::larger`).
LARGER = {"Micro": "Caption", "Caption": "Body", "Body": "Read", "Subtitle": "Read", "Mono": "ReadMono",
          "ReadMono": "Body", "Read": "H3", "H3": "Title", "Title": "H2", "H2": "H1", "H1": "H1", "Display": "Display"}
# Raw sizing calls that are glyph-like by design (a mark scaled with its box).
EXPLAINED = [
    ("data/avatar.rs", "the initial scales with its tile, from TextRole::Micro"),
    ("nav/project_mark.rs", "the initial scales with its tile, from TextRole::Micro"),
    ("aui-icons/src/lib.rs", "icon fallback glyph sized with the icon"),
    ("aui-tokens/src/styled.rs", "the role helpers themselves"),
    ("aui-tokens/src/theme.rs", "pushes the active Body/Mono size to gpui-kit"),
]


def px(tok, key):
    return float(tok[key].rstrip("px"))


def size_of(tok, role, density, text_size):
    if text_size == "large":
        role = LARGER[role]
    suffix = "-c" if density == "compact" else ""
    return px(tok, f"t-{STEM[role]}-fs{suffix}")


def production_text(path):
    text = open(path, encoding="utf-8").read()
    cut = re.search(r"^#\[cfg\(test\)\]\s*\n\s*mod tests", text, re.M)
    return text[: cut.start()] if cut else text


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--density", choices=["comfortable", "compact"], default="comfortable")
    ap.add_argument("--size", choices=["default", "large"], default="default")
    a = ap.parse_args()
    tok = json.load(open(TOKENS))["shared"]

    roles = collections.Counter()
    raw = []
    for root in ROOTS:
        for d, dirs, files in os.walk(root):
            dirs[:] = [x for x in dirs if x not in ("target", "tests")]
            for f in files:
                if not f.endswith(".rs"):
                    continue
                p = os.path.join(d, f)
                rel = os.path.relpath(p, os.path.dirname(HARNESS))
                text = production_text(p)
                # aui-tokens defines the roles; it does not use them.
                for m in re.finditer(r"\bTextRole::([A-Z][A-Za-z0-9]*)\b", "" if "aui-tokens" in p else text):
                    if m.group(1) in STEM:
                        roles[m.group(1)] += 1
                for m in re.finditer(r"\.text_size\(([^;\n]*)|\bfont_size\s*[:=]\s*([^;,\n]*)|\brems\(([^)\n]*)\)", text):
                    arg = next(g for g in m.groups() if g is not None)
                    arg = arg.strip()
                    # A role-fed size; a method with no argument (`TextSizePref::text_size()`);
                    # a parameter's type (`font_size: Pixels`); text inside a string literal.
                    if any(k in arg for k in ("TextRole", "role", "spec", "term_px")) or arg in ("", ")") \
                            or arg.startswith(")") or arg.startswith("Pixels") or '"' in arg:
                        continue
                    line = text.count("\n", 0, m.start()) + 1
                    why = next((w for k, w in EXPLAINED if p.endswith(k) or k in p), None)
                    raw.append((rel, line, m.group(0).strip(), why))

    hist = collections.Counter()
    for role, n in roles.items():
        hist[size_of(tok, role, a.density, a.size)] += n
    total = sum(hist.values())
    print(f"type histogram ({a.density}, {a.size}): {total} role sites, {len(hist)} sizes")
    by_size = collections.defaultdict(list)
    for role, n in roles.items():
        by_size[size_of(tok, role, a.density, a.size)].append(f"{role} {n}")
    for size in sorted(hist):
        print(f"  {size:5g} px  {hist[size]:4d}  ({', '.join(sorted(by_size[size]))})")
    unexplained = [r for r in raw if r[3] is None]
    if raw:
        print("raw sizing calls (not through a role):")
        for rel, line, call, why in raw:
            print(f"  {rel}:{line}: {call[:70]}  — {why or 'UNEXPLAINED'}")
    ok = len(hist) <= 9 and not unexplained
    print("PASS" if ok else "FAIL", f"({len(hist)} sizes, limit 9; {len(unexplained)} unexplained raw calls)")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
