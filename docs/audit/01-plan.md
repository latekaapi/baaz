# Audit → implementation plan (2026-09-12)

Inputs: `00-findings.md` (85 unique findings; both HIGHs verified in source by the
reviewer: `fold.rs::reindex` keeps stale `Slot.turn` indices after a `TurnRemoved`, and
`client.rs::dispatch` never parks `ServerRequest` during a `view/gap` backfill). Mechanical
evidence: ~600 pedantic/nursery lints, `cargo machete` hits in both repos, three
caller-less client methods, one `allow(dead_code)`, twelve phase-era scripts. Baseline:
a 300-turn `--replay` costs 0.39 s CPU / 200 MB RSS in a debug build; element
construction p50 8 µs; nothing measures layout, paint, streaming or scroll yet.

## Rules for every package

- One package = one `muse exec` run from a brief in `docs/briefs/`, sequential (the
  packages touch the same files). Library packages run in agentic-ui on branch
  `audit-2026-09-12` (off `login-methods`) and commit there; harness packages run on
  `main` and do **not** commit — the reviewer audits, reruns a gate, compares captures and
  commits.
- Regression proof for a refactor is **byte-identical captures**: after P0 lands, the
  reference set `scratchpad/before/` (every replay fixture × both themes + the login
  states, taken with `HARNESS_DETERMINISTIC=1`) must compare equal after the package,
  except where the brief names an intended visual change. Adapter snapshots
  (`UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter`) must show no diff unless the brief
  names one.
- Performance claims are numbers from `--bench` (P0), before and after, in the report.
- Spend: `muse exec --model muse-spark-1.3-contributor` on the owner's API key. If a run
  ends `run.terminal.failed` for credit/billing, that package is re-run by a Claude Opus
  subagent from the same brief in an isolated worktree, then audited the same way.

## Packages, in order

| # | name | repo | size | contents |
|---|---|---|---|---|
| P0 | deterministic captures + bench | harness | M | `HARNESS_DETERMINISTIC=1`: fixed clock for relative labels, animations at rest, caret/spinner/shimmer frozen, so a capture is byte-identical run to run; `--bench` (D-PERF-8: cadenced streaming replay, programmatic scroll, whole-frame + element + fold-apply timing, RSS, JSON out); A-MECH-14 |
| E | library hot paths | agentic-ui | M | E-LIB-1…13 (E-LIB-14 becomes a gallery idle-frame check) |
| A | mechanical + the two HIGHs | harness | S | A-MECH-1…23 with a fixture test for each HIGH |
| B | dead and legacy code | harness | S | B-DEAD-1…9; decisions: delete `harness-probe`, `probe_*.py`, `run*.py`, `run_slash.py` (git keeps them; docs updated), keep `drive.py`, `make-stress-300.py`, `bundle.sh`; `reconnect_after_login` stays until the owner's live check |
| C1 | structure, part 1 | harness | M | C-STR-6 `wire_call` helper, then C-STR-5 `steps.rs`, C-STR-1 `login.rs` |
| C2 | structure, part 2 | harness | L | rest of C-STR-1 (sidebar view, dialogs, tier, resize drag), C-STR-2, C-STR-3, C-STR-4, C-STR-7, C-STR-8, C-STR-9 |
| D1 | performance: adapter and wire | harness | M | D-PERF-2, 5, 6, 7, 12, 13 + C-STR-10 |
| D2 | performance: rendering | harness | M | D-PERF-1, 3, 4, 9, 10, 11, 14, 15, 16 |
| F | verification and docs | harness + agentic-ui | S | full gates both repos, capture comparison, bench table before/after, `07-architecture.md`, `CHANGELOG.md`, `05-handoff.md` refreshed |

Every package's brief lists its findings by id; Muse reads the finding text from
`docs/audit/*.md` rather than the brief repeating it.
