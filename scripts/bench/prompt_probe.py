#!/usr/bin/env python3
"""Offline Codex prompt probe (docs/88 C.2). Renders the model-visible prompt
input with `codex debug prompt-input` (no model turn, no tokens spent) for a
throwaway home plus a `-c` list, and prints each block's size.

Homes (built fresh under --work, never the owner's ~/.codex):
  clean      only the auth.json link (the bench's clean arm);
  installed  copies of the owner's config.toml (minus `notify`), skills/ and
             plugins/ (the bench's installed arm; copies, so nothing Codex
             does here can write into the owner's files).

Baaz's own `-c` list is rebuilt from the same rules as
`provider-codex` `spawn_args_for` (bundled + file servers off, each
config.toml plugin off, the tool-output limit, the CLAUDE.md fallback);
the bridge server is left out (its tools are not in the prompt input).

Usage:
  python3 scripts/bench/prompt_probe.py --work <scratch dir> [--codex PATH] [--out table.md]
      [--variant NAME ...]   (default: every variant below)
"""
import argparse, json, os, re, shutil, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import bench  # noqa: E402

BUNDLED_SERVERS = ["codex_app", "codex_apps", "computer-use", "cua_repl", "node_repl"]


def file_servers(config_toml):
    names = []
    for line in config_toml.splitlines():
        m = re.match(r"\s*\[mcp_servers\.([^\].]+)\]\s*$", line)
        if m:
            name = m.group(1).strip().strip('"\'')
            if name and name != "baaz" and name not in names:
                names.append(name)
    return names


def config_plugins(config_toml):
    return [m.group(1) for m in re.finditer(r'^\s*\[plugins\."?([^"\]]+)"?\]\s*$', config_toml, re.M)]


def baaz_args_before(config_toml):
    """Baaz's `-c` list before docs/88 C.3/C.4, minus the bridge: a
    per-plugin disable for each config.toml plugin."""
    args = ["-c", 'project_doc_fallback_filenames=["CLAUDE.md"]', "-c", "tool_output_token_limit=3000"]
    servers = BUNDLED_SERVERS + [s for s in file_servers(config_toml) if s not in BUNDLED_SERVERS]
    for s in servers:
        args += ["-c", f'mcp_servers.{s}={{enabled=false,command="/bin/true"}}']
    for p in config_plugins(config_toml):
        args += ["-c", f'plugins."{p}".enabled=false']
    return args


def baaz_args(config_toml):
    """Baaz's `-c` list now (`spawn_args_for`, own servers off), minus the
    bridge: server disables, `features.plugins=false` (C.3),
    `features.tool_suggest=false` (C.4), plus Baaz's note as
    `developer_instructions` (it rides `thread/start` in the app; the same
    developer message here) so its size shows (C.5)."""
    args = ["-c", 'project_doc_fallback_filenames=["CLAUDE.md"]', "-c", "tool_output_token_limit=3000",
            "-c", "features.tool_suggest=false"]
    servers = BUNDLED_SERVERS + [s for s in file_servers(config_toml) if s not in BUNDLED_SERVERS]
    for s in servers:
        args += ["-c", f'mcp_servers.{s}={{enabled=false,command="/bin/true"}}']
    args += ["-c", "features.plugins=false"]
    args += ["-c", "developer_instructions=" + json.dumps(baaz_note())]
    return args


def baaz_note():
    """The note `provider::note::compose` builds with the bridge, read from
    its source: the string literals between `pub fn compose` and the join."""
    src = open(os.path.join(HERE, "..", "..", "crates", "provider", "src", "note.rs")).read()
    body = src[src.index("pub fn compose("):]
    body = body[:body.index('lines.join("\\n")')]
    return "\n".join(json.loads(m) for m in re.findall(r'"(?:[^"\\]|\\.)*"', body) if "NoteTools" not in m)


def feature(*names):
    out = []
    for n in names:
        out += ["-c", f"features.{n}=false"]
    return out


def variants(config_toml):
    baaz = baaz_args_before(config_toml)
    clean_baaz = baaz_args_before("")
    return [
        ("clean", "clean", []),
        ("clean + baaz now (C.3-C.5)", "clean", baaz_args("")),
        ("installed + baaz now (C.3-C.5)", "installed", baaz_args(config_toml)),
        ("clean + baaz", "clean", clean_baaz),
        ("clean + baaz + tool_suggest off", "clean", clean_baaz + feature("tool_suggest")),
        ("clean + baaz + apps off", "clean", clean_baaz + feature("apps")),
        ("clean + baaz + plugins off", "clean", clean_baaz + feature("plugins")),
        ("installed", "installed", []),
        ("installed + baaz", "installed", baaz),
        ("installed + baaz + plugins off", "installed", baaz + feature("plugins")),
        ("installed + baaz + remote_plugin off", "installed", baaz + feature("remote_plugin")),
        ("installed + baaz + tool_suggest off", "installed", baaz + feature("tool_suggest")),
        ("installed + baaz + recommended_plugins off", "installed", baaz + feature("recommended_plugins")),
        ("installed + baaz + apps off", "installed", baaz + feature("apps")),
        ("installed + baaz + plugins, tool_suggest off", "installed", baaz + feature("plugins", "tool_suggest")),
    ]


def make_home(work, kind):
    home = os.path.join(work, f"home-{kind}")
    shutil.rmtree(home, ignore_errors=True)
    bench.seed_codex_home(home, "clean" if kind == "clean" else "installed", baaz=False)
    return home


def render(codex, home, repo, args, prompt="hi"):
    p = subprocess.run([codex, "debug", "prompt-input", "-c", "model=gpt-5.6-luna"] + args + [prompt],
                       cwd=repo, env=dict(os.environ, CODEX_HOME=home), capture_output=True, text=True, timeout=120)
    if p.returncode != 0:
        raise SystemExit(f"prompt-input failed ({p.returncode}): {p.stderr[-600:]}")
    return json.loads(p.stdout)


def blocks(items):
    """(role, tag, chars, text) per text block of the prompt input."""
    out = []
    for it in items:
        for b in it.get("content") or []:
            text = b.get("text") or ""
            m = re.match(r"\s*<([A-Za-z_][\w ]*?)>", text)
            tag = m.group(1) if m else ("baaz note" if text.startswith("You are running inside Baaz") else "(text)")
            out.append((it.get("role"), tag, len(text), text))
    return out


def skills_in(text):
    """Skill names listed in a <skills_instructions> block; plugin skills are
    namespaced `<plugin>:<skill>`."""
    names = re.findall(r"^- ([^:\s][^\s]*?)(?::\s| \()", text, re.M)
    return names


def summarise(name, bs):
    total = sum(c for _, _, c, _ in bs)
    skills = [s for r, t, c, x in bs if t == "skills_instructions" for s in skills_in(x)]
    plugin_skills = [s for s in skills if ":" in s]
    return {"variant": name, "total_chars": total, "blocks": {t: c for _, t, c, _ in bs},
            "skills": len(skills), "plugin_skills": len(plugin_skills), "plugin_skill_names": plugin_skills}


def table(rows):
    tags = []
    for r in rows:
        for t in r["blocks"]:
            if t not in tags and t != "(text)":
                tags.append(t)
    head = "| variant | " + " | ".join(tags) + " | skills (plugin) | total chars | ~tokens |"
    lines = [head, "|" + "---|" * (len(tags) + 4)]
    for r in rows:
        cells = [f"{r['blocks'].get(t, 0):,}" if t in r["blocks"] else "—" for t in tags]
        lines.append(f"| {r['variant']} | " + " | ".join(cells) + f" | {r['skills']} ({r['plugin_skills']}) | {r['total_chars']:,} | {r['total_chars'] // 4:,} |")
    return "\n".join(lines)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", required=True)
    ap.add_argument("--codex")
    ap.add_argument("--out")
    ap.add_argument("--json")
    ap.add_argument("--variant", action="append")
    args = ap.parse_args()
    work = os.path.abspath(args.work)
    os.makedirs(work, exist_ok=True)
    codex, version = (args.codex, None) if args.codex else bench.resolve_codex()
    version = version or subprocess.run([codex, "--version"], capture_output=True, text=True).stdout.strip()
    repo = os.path.join(work, "repo")
    if not os.path.isdir(repo):
        os.makedirs(repo)
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=repo, check=True)
    config = open(os.path.join(bench.OWNER_CODEX, "config.toml")).read() if os.path.isfile(os.path.join(bench.OWNER_CODEX, "config.toml")) else ""
    homes = {}
    rows = []
    for name, kind, extra in variants(config):
        if args.variant and name not in args.variant:
            continue
        home = homes.get(kind) or homes.setdefault(kind, make_home(work, kind))
        bs = blocks(render(codex, home, repo, extra))
        rows.append(summarise(name, bs))
        print(f"{name}: {rows[-1]['total_chars']:,} chars, skills {rows[-1]['skills']} ({rows[-1]['plugin_skills']} plugin)", flush=True)
    text = f"Codex {version} ({codex}), `codex debug prompt-input`, model gpt-5.6-luna, prompt \"hi\". Chars per block; ~tokens = chars / 4.\n\n" + table(rows) + "\n"
    print(text)
    if args.out:
        open(args.out, "w").write(text)
    if args.json:
        json.dump(rows, open(args.json, "w"), indent=1)


if __name__ == "__main__":
    main()
