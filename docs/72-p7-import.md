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

## 6. As built (30 Oct 2026)

Built on `main` as §1–§3 say; deviations below.

**Engine (`crates/baaz/src/import.rs`, headless).** `scan_dirs(claude_config, codex_home)` walks once per home
through `history::stored_transcripts` / `rollout::stored_rollouts` and reads, per file, a **head** (line by line,
≤ 512 KB, stopping once cwd, first prompt and first timestamp are known) and a **tail** (the last 64 KB, for the last
timestamp); `stat` gives the fallback times. Claude: cwd from the first line carrying one, scripted when
`entrypoint == "sdk-cli"`, first prompt = first `user` line not `isMeta`, not `isSidechain`, not a tag block
(`<command-…>`, `<local-command-…>`, `<system-reminder>`), not `Caveat:`, not a tool result. Codex:
`session_meta.payload.cwd`, scripted when `originator == "codex_exec"` or `source == "exec"` (probed on all 95 of the
owner's rollouts: every one says both), first prompt = the first `event_msg` `user_message` (the `response_item` copy
is wrapped in injected context). Any cwd under `/tmp`, `/private/tmp`, `/private/var`, `/var/folders` or `$TMPDIR` is
scripted too. A file with no parseable line is reported by path (`Scan::unreadable`); a file with lines but no prompt
is counted as `empty` and never offered. `plan(scan, known, projects, home)` is pure: drops ids the registry holds,
groups by cwd, resolves each folder to a project by exact canonical root (never a prefix, `Projects::resolve`'s rule),
sorts **Baaz projects first, then newest activity** (so the ticked folders are never folded into "N more folders"),
counts per provider (`Tally { offered, scripted, known, folders }`). `execute_batch` reads each chosen file whole once
to count turns (pre-filtered lines, the same prompt predicates) and builds the record (`origin: "imported"`,
workspace = cwd, project = the folder's adoption, the scan's created/updated, `turns.max(1)`, first prompt, no title);
`land` inserts only ids the store lacks (never overwrites a Baaz record). `Journal` (`import-journal.json`): runs
`{id, atMs, sessions: [{provider, sessionId}]}`; `undo` removes exactly a run's ids, and only while they are still
imported records, then drops the run. `write_fixture_home` builds the perf/test fixture.

**The trap, closed.** `ProviderSessionRecord.origin` (`#[serde(default)]`, `ORIGIN_IMPORTED`, `is_imported()`).
`gate_resume` returns `LegacyOwner` for an imported record of that provider before anything else, whatever the
consent; `plan_migration` skips imported records. The `LegacyOwner` arm now calls `pin_legacy_resume_for(record)`:
an imported record pins at `import::owner_home()`. Test:
`session_migration::tests::an_imported_session_resumes_legacy_under_consent_and_never_moves` (consent recorded →
imported Claude and Codex gate `LegacyOwner`, a Baaz record still gates `LazyMove`, the plan lists only the Baaz
record, the arm pins and every owner-side file — transcript, companion dir, rollout — is still in place, nothing in
Baaz's home).

**Pins may name a home (provider crates).** `home::pin_legacy_resume_at(id, owner_home)` and
`home::legacy_owner_home(id)` in both adapters (the pin set became a map). A pin at the real `HOME` changes nothing; a
pin at another home (the probe's fixture) makes Claude Code read history from `<home>/.claude` and start the child
with that `CLAUDE_CONFIG_DIR`, and Codex start its child with `CODEX_HOME=<home>/.codex`. This is what lets a live
check reopen an imported session from a fixture home without touching the owner's.

**Wiring (`crates/baaz/src/import_ui.rs`).** `Harness.import: ImportUi { cache, scanning, rescan_owed, sheet,
journal, run, first_run }`. The scan + plan run on the background executor (`refresh_import_scan`); renders read the
cache only. Refresh triggers: Settings › Providers opens (only when the cache is older than 10 min), the sheet opens
(always, "Import more…" wants what is left), a run finishes, an Undo; a refresh asked for while one runs is owed and
runs when it lands. The card's counts are the cached plan **re-read against the live store** (`live_tally`), so a
session landed mid-run is "in Baaz" at once and the card never waits on a rescan. A run is batches of 25 on the
background executor; each batch lands as one `land` + journal note + `persist_provider_sessions` (the S1 off-thread
writer) + `merge_provider_rows`. At the end: `drop_empty`, the journal written, `rebuild_search_index` (the P3 path —
`provider_search::Homes::for_app` now names `import::owner_home()` as the owner home, so a fixture home is searched
too), the toast via `push_undo(UndoBatch::Imported(run_id))`, a re-plan. Undo (toast or `import-undo`) runs
`import::undo`, persists, re-merges, rebuilds search, re-plans. `BAAZ_IMPORT_OWNER_HOME` overrides the owner home;
under `BAAZ_DETERMINISTIC=1` nothing is scanned without it (`scan_allowed`), and the journal is neither read nor
written (records already never are), so a capture's two settle shots start alike. A capture stamps its run with the
newest imported session's time instead of the wall clock ("4 imported 24 Oct" reads the same every day).

**Dry run.** `baaz --import-dry-run`: scan + plan over the owner's homes (or `BAAZ_IMPORT_OWNER_HOME`), one JSON
object on stdout, reads the registry and `projects.json`, writes nothing (it returns before the state-dir migration and
the provider status boot).

**Probe.** Entries `settings-card-import`, `import-sheet`, `import-progress` (`import-run:2` holds after two),
`import-done`, plus `connect-import` (the first-run line), all offline on a fixture owner home under
`target/uiprobe-import-owner-<pid>` (not under `$TMPDIR`, which the scan counts as scripted; removed at exit) with
their own state dirs. Verbs `import-open` (scans synchronously so a script never races the background scan),
`import-toggle:<folder|path|scripted>`, `import-run[:<n>]`, `import-undo`. Perf budget `store.import_scan_ms`
(2,000-file fixture, 16 KB of tool output in each).

**Deviations from the board, and why.**
1. *The card line is drawn beside the library card, not inside it.* `aui::screens::provider_card` has no footer slot
   and agentic-ui was out of scope, so Baaz wraps the card and the line in one frame (the card's surface, radius and
   edge); the card's own inset edge draws the hairline above the line. It reads as one card.
2. *The first-run offer is the connect screen's assist line* ("Found N earlier sessions on this Mac. Import…", the
   link opens the sheet), not the board's boxed line with a secondary button — the library's connect screen has no
   slot between the rows and the footer either. Codex's sign-in assist owns the line while it runs.
3. *The sheet's frame is Baaz's own* with the setup sheet's measures (44 header, 16 pad, radius 12, surface 2, the
   floating shadow, the library scrim): `sheet_frame` always draws a provider mark and the board's sheet has none.
4. *Checkboxes are the library's (`aui::skills::checkbox`, 16 px, accent fill).* In the dark theme the accent is
   near-white and the check glyph is white, so a ticked box reads as a filled square with a faint check — the same
   as the Skills import preview. Fixing the glyph colour is an agentic-ui change.
5. *Folder order:* projects first, then newest activity (the spec says newest first; the board shows projects first).
6. *Counts the board did not have:* a session file with no prompt (opened and left — 50 on this Mac) is never
   offered or counted; Codex's card counts rollouts with a prompt, so it says "83 Codex sessions on this Mac, all
   scripted runs (codex exec)" here (95 files, 12 empty).
7. *"N more folders"* folds everything after seven visible rows (eight fit without a fold); its box is tri-state
   over the folded folders.
8. *The toast* is the app's toast (bottom right, `5 sessions imported · Undo`, 8 s), not a centred one.
9. *Result line* names projects and, when some sessions went to folders that are not projects, "and J other
   folders"; "Show the J" toggles a mono list of the unreadable paths (≤ 12, then "and K more").

**Owner's homes, read-only dry run (release build, 30 Oct):** 1,995 files; Claude Code 1,744 offered in 33 folders,
118 scripted (sdk-cli or a temp cwd), 0 already in Baaz; Codex 0 offered, 83 scripted, 12 empty; 50 empty in all; 0
unreadable. Top folders: lakshya-v2 842 (+10 scripted), lakshya-v2/platform/tools/exam 744, harness 66 (project),
cockpit 18, agentic-ui 10, then dharisanam-revival/storage/enrich/182/* (2–5 each). The defaults (projects ticked)
import 67 (harness and one of its worktrees). Scan: 1.7 s cold, ~220 ms warm.
