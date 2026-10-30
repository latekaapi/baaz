# 72 — P7: import earlier Claude Code and Codex sessions

30 Oct 2026. Board **P7Import** on the "Baaz v3" canvas (v61–62), **approved by the owner 30 Oct** with three answers:
**in place**, **once + "Import more…"**, **keep Undo**. Generator: `docs/34-refs/v3-redo/canvas/gen_p7.py`. The board
is the spec for the surfaces; this note is the spec for the engine and the wiring.

## 1. What import means

An imported session is a `ProviderSessionRecord` (`claude-code` / `codex`) plus its search row. Its transcript stays
where the CLI wrote it (`~/.claude/projects/<slug>/<id>.jsonl`, `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`).
Resuming it in Baaz runs the child against the **owner's** home, so it continues the same session the CLI sees.
Nothing is copied, moved or deleted, ever.

**The trap this must close.** The owner's state has `session-migration.consented`, so `gate_resume` returns
`LazyMove` for any registered session whose files are owner-side, and `reopen_provider` would *move* an imported
session's files into `claude-home` / `codex-home` on its first resume. An imported record therefore carries
`origin: "imported"` (new optional field, `#[serde(default)]`), and `gate_resume` returns `LegacyOwner` for it
whatever the consent — pinned on the owner's home every time, never moved. The migration planner (`plan_migration`)
skips imported records too. Tests pin both.

## 2. The engine (`crate::import`, headless, no UI)

- **Scan** (off the UI thread, one walk per home, reusing `history::stored_transcripts` /
  `rollout::stored_rollouts`): per session — provider, id, cwd (Claude: the first line carrying `cwd`; Codex:
  `session_meta.payload.cwd`), created/updated (first/last timestamp, else mtime), first real user prompt (Claude:
  not `isMeta`, not `<command-…>`, not sidechain; Codex: first user `response_item`), turns (user prompts), and
  **scripted** (Claude `entrypoint == "sdk-cli"`; Codex `originator` of `codex_exec` / an exec source — probe the
  owner's files read-only for the exact field before writing the rule; also anything whose cwd is under `$TMPDIR`,
  `/private/var`, `/tmp`). Unreadable or unparseable files are collected by path, never dropped silently.
- **Already in Baaz:** an id with a record (Baaz's own sessions, previous imports) is not offered again.
- **Plan** = the scan grouped by folder (cwd), newest activity first; a folder is *ticked* when it resolves to a Baaz
  project (`Projects::resolve`); counts per provider; scripted runs counted separately. `plan()` is pure over
  explicit dirs (tests use temp homes, never the real `HOME`) and is the dry run.
- **Execute** (background, cancel-safe, idempotent): for each chosen session write the record (`origin: imported`,
  workspace = cwd, project = the resolved adoption, created/updated from the scan, `turns`, `first_prompt`, title
  none), then the search row through the P3 path (`provider_search` already reads owner-home transcripts for ids
  not in Baaz's homes). Records are written in batches through the S1 off-thread writer, the sidebar merges rows
  as batches land (`merge_provider_rows`), progress is reported per batch. Re-running the same plan writes nothing.
- **Journal:** `import-journal.json` in the state dir: each run's id, time, and the session ids it added — what
  **Undo** reads (remove exactly those records and search rows, never a file) and what the card's tally reads
  ("122 imported 30 Oct · 1,774 left on this Mac").

## 3. The surfaces (as on the board)

- **Settings › Providers › Claude Code / Codex card:** one line under Details — "N sessions on this Mac, in M
  folders. None are in Baaz yet. [Import…]", after an import "N imported <date> · K left [Import more…]", nothing
  when the home has none. Counts come from a cached scan (stale-while-revalidate like provider status), never a
  walk on the render path.
- **First run (connect screen):** once a provider is Connected and its home has sessions not in Baaz, one line under
  the rows: "Found N earlier sessions on this Mac. [Import…]". Shown on first run only.
- **Import sheet** (one for both providers): folder rows with checkbox, name, faint parent path, "project" tag for
  Baaz projects, per-provider counts with the provider mark; "Skip scripted runs (claude -p, codex exec · N)" ticked;
  the in-place note; Cancel / "Import N sessions" (counts live). Then progress (bar, "Importing 48 of 122… <folder>",
  Close keeps it running) and result ("N sessions imported into K projects. J couldn't be read and were left out.",
  "Show the J", Show in sidebar / Done).
- **Sidebar:** imported sessions are ordinary rows (no tag) in their project, sorted by last activity; a toast
  "N sessions imported · Undo".
- Accessibility: every control gets a role and a label (CLAUDE.md); new `--steps` verbs `import-open`,
  `import-toggle:<folder>`, `import-run`, `import-undo`; probe entries `import-sheet`, `import-progress`,
  `import-done`, `settings-card-import` against fixture homes (`BAAZ_IMPORT_OWNER_HOME`, like
  `BAAZ_MIGRATION_OWNER_HOME`), never the real home.

## 4. Muse's Baaz-private home (docs/50 §6.2) — DEFERRED (decided 30 Oct)

Probed read-only (muse 1.4.14): Muse keeps sessions, its session index (`session-index.db`), skills, plugins, the model
catalog and runtime state together under `$XDG_DATA_HOME/muse` (`~/.local/share/muse`, 10 GB here); sign-in lives
apart in `$XDG_CONFIG_HOME/muse`. There is no setting for the session store alone (`muse serve --help`, `settings.json`
keys, `muse config`). The only lever, `XDG_DATA_HOME`, moves all of it — Baaz's Muse would lose the owner's skills,
plugins and the 2,661 existing sessions, or need a second `muse serve` on the owner's store and two lists — and it is
inherited by every shell the agent runs.

The owner left the call to me ("decide what's best for a complete multi-provider harness"). **Deferred:** Muse keeps
one store, shared with the muse CLI. Baaz is a harness over the providers the person already uses, not a fork of
them; since S1 (docs/70) Baaz's own records say which Muse sessions are Baaz's, which is what the private home was
for, and a Baaz Muse session showing in `muse resume` is the same two-way interop P7 gives Claude Code. Revisit if
Muse grows a sessions-dir setting.

**Probe facts for the engine:** every one of the owner's 95 Codex sessions is `originator: "codex_exec"`, `source:
"exec"` — scripted. With "Skip scripted runs" ticked (the approved default) Codex offers none, and its card says so
("95 Codex sessions on this Mac, all scripted runs (codex exec)") instead of an Import button that imports nothing.
Counts on every surface exclude scripted runs unless the box is unticked. Claude: `entrypoint == "sdk-cli"` marks
`claude -p` (31 here).

## 5. Done when

Engine tests (temp homes): scan both formats, scripted detection, unreadable files reported, plan grouping and
ticking, idempotent execute, Undo removes exactly the journal's ids, **an imported record resumes `LegacyOwner`
under consent and is never moved**, the migration planner skips it. Surfaces pass Tier V with the new entries.
Live: a throwaway copy of a few of the owner's session files in a fixture owner home → import → reopen one (history
paints) → Undo. The owner's real import is the owner's to click.
