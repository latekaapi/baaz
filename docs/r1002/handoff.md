# r1002 — Handoff defects (owner recording 2026-10-02 ~13:58)

Read-only diagnosis against main @ 8bbbc41 (the running bundle, pid 22059). Nothing edited, no turns sent.

## Evidence used

- Session "hello", project harness: Codex `01a0fba6-87cb…` → Claude Code `01a0fba9-d93f…` → Muse `01a0fbba-7142-7ed2-894c-428ea4b8e378`.
- Baaz: `~/Library/Application Support/baaz/handoff/01a0fbba-7142….json` (the activation snapshot: `"to_model": "gpt-6-astra"`), `sessions.json` (dest row `lastError` = 503 overload), `projects.json` (harness `defaults.modelId`).
- Muse: `~/.local/share/muse/sessions/2026/10/02/<id>/session.jsonl` for three sessions started that minute:
  - `01a0fbba-1f9e…` handoff-summary side session (prompt "Summarise a handed-off chat session…").
  - `01a0fbba-7142…` the destination.
  - `01a0fbba-7b1d…` an auto-title side session for the destination.
- Baaz stderr is `/dev/null` (lsof on pid 22059), so no `baaz_log!` lines survive. The timeline below comes from Muse's `recorded_at` stamps, with t=0 at 13:57:55.0 (the summary side session's start).

| t (s) | event (Muse log) |
|---|---|
| 0.16 | summary side session starts on `muse-spark-1.3` |
| 3.45 | its model stream fails; Muse schedules a retry **in 60 000 ms** (503 "backend temporarily overloaded") |
| 20.10 | destination `session/start` arrives with `model_id: "gpt-6-astra"` |
| 20.95 | pack `turn/start` arrives; the run is configured `provider muse / model gpt-6-astra` |
| 22.84 | pack turn **terminal failed**: "model `gpt-6-astra` does not exist or you lack access" |
| 23.39 | title side session starts for the destination (its title is later "Multi Agent Handoff Greeting") |
| 24.98 | Baaz `activated_ms` (snapshot): the run goes Active, after the pack turn had already failed |
| 128.2 | `model_reconfigure` gpt-6-astra → `muse-spark-1.3-contributor` |
| 130.7 | "hi" submitted; 503 overloaded, 10 attempts at 60 s backoff |
| 761.4 | "hi" terminal failed: "API error 503 … (after 10 provider attempts)" → row "Failed" |

---

## 1. Handoff takes a long time

**Root cause.** With "Summarise handoffs with a model" on, the destination does not open until the model summary lands or the 20 s watchdog fires.

- The gate is at `crates/baaz/src/app/lifecycle.rs:4216-4237`: `should_model_summary` → `start_handoff_summary` → `return`.
- The watchdog is `SUMMARY_TIMEOUT_SECS = 20` at `crates/baaz/src/handoff.rs:244`. Its arming and firing are at `app/titles.rs:621-643`.

On 2026-10-02 Muse's backend was returning 503, and the summary side session sat in a 60 s retry. Baaz therefore always paid the full 20 s, and the destination's `session/start` came at t=20.10. The earlier Codex → Claude Code hop had the same problem: its summary side session `01a0fba9-87fe…` took 70 s, so it also hit the 20 s timeout.

In both hops the summary was pointless. The packs are 2 and 4 turns, all carried verbatim under "Recent turns". The "Conversation summary" in the snapshot is the extractive fallback: assistant texts joined with `---`.

A second, smaller delay: about 4 s passed between Muse accepting the pack `turn/start` (t=20.95) and activation (t=24.98). Activation waits on the `turn/start` ack (`session/handoff.rs:280-285`). I could not attribute this gap without Baaz logs. It needs a `BAAZ_TRACE` run to confirm.

**Confidence.** High for the 20 s summary block, which the log stamps prove. Low for the 4 s ack gap.

**Fix.**
1. Skip the model summary when it cannot add anything: the pack's `recent` already covers every turn of the source, or the excerpt is under about 2k characters. Add this to `handoff::should_model_summary` by passing the pack in.
2. Lower the watchdog to about 8 s.
3. Better: stop blocking on the summary. Open the destination with the extractive pack immediately, and use the model summary only when it is already back. The summary matters only for long sessions, where the pack truncates.

**Verify.**
- `cargo test -p baaz handoff::tests::a_pack_that_carries_every_turn_skips_the_model_summary`
- `cargo test -p baaz handoff_summary_never_delays_the_destination_open_past` (a gpui test that asserts `pending_handoff` is set within N ms with a never-answering client).

## 2. No progress indicator on the card

**Root cause.** The card is a single state pill plus one text line. It is in agentic-ui v0.3.14, at `crates/aui/src/transcript/handoff.rs:288-301` (checkout `~/.cargo/git/checkouts/agentic-ui-*/b072936`). It has no steps, no elapsed time, and no bound.

During the summary wait, the run is `Checkpointed` and the line reads "Checkpoint captured — building the pack…". That is misleading: the pack is already built. The only hint of the wait is `detail = "Summarising…"` stuffed onto the first carried row, at `crates/baaz/src/handoff.rs:672-676`.

The pill names internal states ("Quiescing", "Checkpointed", "Prepared") that mean nothing to the owner.

**Confidence.** High.

**Fix (library + host).**
1. Give `HandoffCard` a step list:
   - Prepare pack
   - Summarise (optional, with "up to 20 s" or a live elapsed counter)
   - Start <To> session
   - <To> acknowledges
   - Done

   Mark each step done, current, or pending.
2. Add a `summarising: bool` and a `started_at` to the card builder, so the host stops overloading the carried row's detail.
3. Replace the Checkpointed line with "Writing a summary with <model>… (n s)" while summarising.
4. Give the step list an accessible role and label (CLAUDE.md rule).

**Verify.**
- aui: `cargo test -p aui handoff_card_lists_steps_with_the_current_one_marked`.
- baaz: `cargo test -p baaz the_card_says_summarising_in_its_state_line_not_the_carried_row`.
- Plus a Tier V shot entry `handoff-summarising` using the existing `step_handoff` verb with a stubbed client.

## 3. Screen and sidebar "re-render" after handoff

**Root cause.** The destination is a separate, empty session view. Baaz activates it about 5 s before it is given the carried transcript, so the owner sees the transcript swapped out and then rebuilt.

- **Frame A (blank).** Muse `session/start` returns, and `new_session_open` calls `this.open(session_id…)` (`app/lifecycle.rs:1442`). That activates the brand-new Muse view.
  - `land_handoff_destination` then runs (`:1478` → `:4260`). It calls `note_handoff_origin`, which sets a divider with an empty prefix (`session/handoff.rs:101-116`), and the pack bubble is hidden.
  - With `handoff_prefix` empty, the divider grows the "Open the source session" back-link (`session/render.rs:429-437`, `transcript.rs:1706-1712`).
  - That is exactly the near-empty "Handed off from Claude Code to Muse · Open the source session" frame.
- **Frame B (full redraw).** Only on the pack ack (`acknowledge_handoff`, `:4351`) does `show_handoff_prefix` (`:4415`) insert the source's turns above the divider. The whole transcript then redraws, and the back-link disappears.
- **Sidebar, in the same two steps.**
  - At land, `:4325-4342` marks the destination row as running, calls `merge_provider_rows`, then `invalidate_list`. A new row appears next to the source.
  - At activation, `persist_handoff_links` (`:4425`) writes `handoff_from/to`, and the chain collapse folds the source row away.
- **Title flip (side defect).** The auto-title side session starts at t=23.39. That is before the links exist, so the `is_handoff_dest` guard (`app/titles.rs:98`) does not apply yet. The destination therefore earned "Multi Agent Handoff Greeting" (see `sessions.json`), contradicting `titles::tests::a_handoff_destination_never_earns_a_generated_title`. This also spends a model turn.

**Confidence.** High. The code path and the 4.9 s window (t=20.10 → 24.98) match the recording.

**Fix.** Make landing do everything activation does, except the run-state change.

1. In `land_handoff_destination`, before `submit_pack`, snapshot the source (`handoff_snapshot_turns`) and call `show_handoff_prefix` with the final divider.
2. Even better: stash the prefix on `PendingHandoff` and apply it inside `open()` before the first render, so frame A never exists.
3. Call `persist_handoff_links` at land too. Write `handoff_from` into overrides/provider_sessions before `merge_provider_rows`. The row then joins the chain in the same frame, and the title guard sees it.
4. On a Failed or Cancelled run, roll the links back.

The owner's rule ("only a divider is added") then holds:
- The view that becomes active already holds the same turns plus one divider.
- Ideally, keep the source view's element tree and append the divider. That means the scroll position carries over, with `follow` only if it was already following.

**Verify.**
- `cargo test -p baaz land_shows_the_prefix_before_the_pack_submits`. Assert `handoff_prefix` is non-empty, and `handoff_back` is `None`, immediately after `land_handoff_destination`.
- `cargo test -p baaz a_handoff_destination_row_joins_the_chain_at_land`.
- `cargo test -p baaz a_handoff_destination_never_starts_a_title_side_session`. This is a gpui test that counts `title_jobs` after land.

## 4. Claude Code → Muse then "failed"

**Root cause: two separate failures, one Baaz's and one Muse's.**

**4a. Baaz's fault: the destination ran on a Codex model.**
- Muse's log for `01a0fbba-7142…`:
  - `session_start … "provider_id": "muse", "model_id": "gpt-6-astra"`.
  - Then the pack turn: terminal failed, "model `gpt-6-astra` does not exist or you lack access" (t=22.84, non-retryable).
- Baaz still activated the handoff 2 s later (t=24.98). The run treats "`turn/start` accepted" as the acknowledgement (`session/handoff.rs:280-285` → `acknowledge_handoff`, `lifecycle.rs:4351-4361`), and nothing watches the pack turn's terminal outcome.
- So the handoff "succeeded" into a session whose context pack never reached any model. Muse never saw the context.
- Item 5 covers why it was `gpt-6-astra`.

**4b. Muse's fault: backend overload.**
- After the model was reconfigured to `muse-spark-1.3-contributor` (t=128), "hi" ran 10 provider attempts with 60 s backoff, all `API error 503 … The backend is temporarily overloaded`. It failed at t=761 (14:10:36).
- That is the `lastError` stored on the row in `sessions.json`. The owner's "Working… 1m 10s" frame was mid-retry.
- The same 503s hit the summary side session from t=3.45. The title side session at t=23 succeeded in 12.7 s, so the overload was intermittent.
- Not caused by the pack, the resumed session, or Baaz.

One Baaz-side gap remains: the composer says "Working…" for 10 minutes with no sign that Muse is retrying a 503.
- The adapter folds `turn/retryScheduled` into a countdown (`crates/muse-adapter/src/fold.rs:1012-1021`).
- Muse 1.4.2 logged these retries only as task `status` events (`phase: retry_scheduled`).
- Whether it also emits `turn/retryScheduled` on the wire is **unverified**. Check offline with `muse schema generate-json-schema --out <dir>` and grep `retryScheduled`.

**Confidence.** High for both 4a and 4b; the Muse logs state them directly. Medium for the retry-countdown gap.

**Fix.**
- 4a(i): fix item 5.
- 4a(ii): make the pack's acknowledgement depend on the pack turn not failing.
  - Keep `Prepared` until the pack turn reaches `turn/started` with no terminal failure; a first assistant delta also counts.
  - Or, after activation, turn a pack-turn `failed` into `fail_handoff`, so the card and row say "Hand-off failed: model … does not exist". Do not leave a silent Active.
- 4b:
  - If Muse emits `turn/retryScheduled`, confirm the countdown shows on a lane session.
  - If it does not, ask Muse to emit it. This is Muse's to fix; Baaz has the UI ready.

**Verify.**
- `cargo test -p baaz a_pack_turn_that_fails_fails_the_handoff`. Feed `turn/started` and then `turn/completed{status:failed}` on the destination, and assert the run is `Failed` and the card shows the reason.
- Offline, with no turn: a `--replay` fixture of the 7142 pack-failure frames.

## 5. Muse lane wears "Gpt 6 Astra"

**Root cause.** The project's model default is provider-agnostic, but only Muse consumes it.

- A model pick in **any** lane emits `SessionEvent::ModelSelected` (`session/composer.rs:235-238`). That writes `ProjectDefaults.model_id` for the session's project (`app/lifecycle.rs:3765-3769`, via `note_project_default`, `:3509`).
- So a pick of `gpt-6-astra` in a Codex session in harness became harness's default model.
- Every new Muse session in that project, including a handoff destination (`open_handoff_destination` → `new_session` → `projects::start_params`, `projects.rs:545-557`), sends it as `modelId`.
- Muse accepted the unknown id at `session/start`. It recorded `provider_id: "muse", model_id: "gpt-6-astra"` and only failed at the turn.
- Claude Code is immune because it keeps its own store, `claude-code-last-model.json` (`providers.rs:398-448`).

Why the chip and the menu disagreed:
- **Chip.** `model_id()` reads the session's folded model, `gpt-6-astra` (`session.rs:1215-1227`). The catalog has no such row, so `muse_model_label` humanises it to **"Gpt 6 Astra"** (`session.rs:1322-1345`).
- **Menu.** The check comes from the catalog's `is_active` row, falling back to index 0 (`session/composer.rs:88-90`). That is `muse-spark-1.3`, the first catalog row. Two owners for one fact.
- **"muse-spark-1.3-contributor".** This is a real Muse catalog row, Muse's own default. Its `display_label` equals its id (`~/.local/share/muse/model-catalog/6d657461__p746268.json`). It appeared after the reconfigure at t=128. `projects.json` now holds `modelId: muse-spark-1.3-contributor`, which only a menu pick writes. That frame is correct behaviour; the label ugliness is Muse's.
- The handoff card, divider and snapshot also say `gpt-6-astra`, because `run.to_model` is taken from the destination's `model_id()` at land (`lifecycle.rs:4284-4286`).

**Confidence.** High. `projects.json` is the only writer path, the Muse log shows the start params, and the label code is direct.

**Fix.**
1. Key project defaults by provider: `ProjectDefaults.models: BTreeMap<provider, String>` (and the same for effort). Migrate the old `modelId` only to `muse` when it appears in the Muse catalog. `note_project_default` must record the view's `provider_kind()`.
2. Defence in depth: in the Muse start path, drop a `model_id` that the cached `model/list` does not list (log once), so Muse picks its default.
3. Make the chip and the menu read the same source: mark the menu row that equals `model_id()`, or show no check when nothing matches.

**Verify.**
- `cargo test -p baaz projects::tests::a_codex_model_pick_never_becomes_the_muse_start_model`
- `cargo test -p baaz start_params_drops_a_model_the_muse_catalog_does_not_list`
- `cargo test -p baaz the_model_menu_checks_the_row_the_chip_names`
- `cargo test -p baaz a_handoff_to_muse_starts_on_the_muse_default_when_the_project_default_is_foreign`

## Ownership summary

| # | Whose | Severity |
|---|---|---|
| 1 | Baaz (blocking 20 s summary). Muse overload made it hit the cap every time. | High (every handoff with the switch on) |
| 2 | agentic-ui card + Baaz host | Medium |
| 3 | Baaz (prefix and links applied at ack, not at land). Side defect: title side session for the destination. | High (owner-visible on every handoff) |
| 4a | Baaz (foreign model id; pack failure not watched) | High: context silently lost |
| 4b | Muse backend (503 overload, 10 × 60 s retries) | External |
| 5 | Baaz (provider-agnostic project model default) | High |
