# 62 — Handoff after speed P1 (Muse list) and P2 (2026-10-21)

Read this first in the next session. It replaces docs/61 as the "where are we"; docs/60 §4 (how to verify) and §5
(rules) still apply. Numbers and findings are in docs/51 §8–§9.

**Stage status: COMPLETE (2026-10-21).** The owner's one screenshot note on the test build (Files pane) is fixed
too — §2 item 7.

## 1. State of the code — committed on both mains, NOT pushed

| Repo | Head | Pushed | Notes |
|---|---|---|---|
| agentic-ui (library) | `c2ae1a1` (3 commits on `v0.6.1`) | no | markdown bench; `aui_motion::frames` counter; Files names 13 px |
| harness (Baaz) | see `git log -1` (9 commits on `0226ea5`) | no | `Cargo.toml` still pins `v0.6.1` |

**Harness main only builds with the local `[patch]`** (`.cargo/config.toml` → `../agentic-ui`): the frames commit
uses `aui_motion::frames`, which v0.6.1 does not have. At "push": commit and tag agentic-ui **v0.6.2**, push it, then
the repin recipe in docs/61 §1 (config aside → sed the tag → `cargo update -p aui …` → build + test → commit
`Cargo.toml` + `Cargo.lock` → push → config back). The committed `Cargo.lock` already carries criterion (regenerated
with the patch aside, so it pins the git v0.6.1 library).

## 2. What this stage did

Speed P1, the rest (docs/61 §4a):
1. **Probe:** muse 1.4.11 `session/list` honours `updatedAfter` (strict, on `updatedAt`, µs, paged; listing is ordered
   by `lastActivityAt`). docs/51 §8.
2. **Incremental listing** (`list_cache.rs`): saved copy in `session-list-cache.json`, newer rows only (5 s overlap),
   full re-list when missing / a day old / another Muse dir / on error. Rows compacted to 1,000 chars (the uncompacted
   copy was 233 MB). Launch listing 27 pages 4.0–4.2 s → 1 page 1.4–1.6 s (Muse's cold first call); after a turn
   ~2.5 s → 130–360 ms. Offline budgets `store.list_*` (pages budget 1).
3. **Search rows no longer rewrite at rest:** one label ladder for boot rows, list join and `rejoin` (warm boot 63 → 0).
4. Found on the way: a second boot install dropped every boot row; the sidebar showed skeletons until the list reply
   (regressed by the D3 screen) — both fixed, rows paint at ~0.6 s again.

P2 (docs/61 §4b):
5. Criterion benches: fold (muse-adapter), markdown (agentic-ui aui), render cache + `visible_sessions` (baaz, as
   ignored tests). Results in docs/51 §9.
6. `aui_motion::frames`: every frame request attributed to `file:line`; Baaz traces `baaz-perf frames=…`;
   `idle.frame_requests` budget 0.

Owner note on the test build (screenshot, 21 Oct evening):
7. **Files pane** — the first row was washed out under the pane row's fade (the fade now draws over the tree only once
   it scrolls: `right.rs` `fade_under_row`), and the names were 14.3 px against the sidebar's 13 (now `TextRole::Body`,
   agentic-ui `files.rs`). Seen in captures (`right-files`, new `right-files-scrolled`; `wheel:` takes `@x,y`). The two
   Files probe entries differ from their baselines on purpose: **regenerate baselines on main at push**, own commit.

## 3. Open items (carry)

1. **Render cache: a new turn costs O(transcript)** (192 µs at 300 turns → 1.07 ms at 3,000). Chunks inside a turn
   are cheap since P1; turn arrival is not. `refresh_render_cache` when `cached_turns.len()` changes.
2. **List reply joins all rows on the UI thread** (64–144 ms per finished Muse turn) though the listing knows which rows
   changed. Join only the newer rows into the existing entries.
3. **Markdown 100 KB re-parse 1.9 ms per chunk** — docs/51 #6 settled-prefix parse.
4. `visible_sessions` 2.3 ms after any list change (2,600 rows).
5. **Idle CPU 0.6–0.8 % with the window visible** (budget 0.5, renders 0, frame requests 0): gpui's CVDisplayLink
   wakes the main thread every vsync while visible; occluded windows read 0.0–0.1 %. A gpui-pre change. docs/51 §9.
6. Muse's own first `session/list` after `muse serve` starts took 1.4 s usually, **20 s once** — server side; report
   upstream with the 1.38 GB WAL note if it repeats.
7. Flaky test seen once: `sidebar::tests::a_side_session_hides_from_the_wire_alone_with_no_local_record` (passes
   alone and on reruns) — likely `side_workspace_dir` racing a test that sets `BAAZ_STATE_DIR`.
8. Still from docs/61 §3: "page stops rendering after sliding" (not reproduced), H.264, Muse billing.

## 4. Process notes for the next session

- **Every Baaz launch activates the app** (`cx.activate(true)`): a BaazCheck launch takes keyboard focus. If the owner
  is at the machine (`ioreg -c IOHIDSystem` → `HIDIdleTime` near 0), wait for idle before live checks.
- Release benches + debug tests fill the disk: this session hit ENOSPC once (20 GB of `target/debug/incremental`).
  Prune `target/*/incremental` before long chains; `target/debug/deps` is 49 GB and worth a clean at some point.
- Next on the tracker: **P4 — Muse onto the provider seam** (docs/50), then D5 Settings beside R2.
