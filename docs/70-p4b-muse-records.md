# 70 — P4-B: Muse sessions get Baaz records

30 Oct 2026. The design note docs/69 §3 S1 asks for before building. Not an owner gate; it is on the tracker.

## 1. What is two paths today

| Path | Claude Code / Codex | Muse |
|---|---|---|
| Start | `open_new_on_provider` → `record_provider_open` writes a `ProviderSessionRecord` | same open, but `record_provider_open` returns early and pushes a local list row instead |
| "Is this a fresh draft?" | `!provider_sessions.contains_key(id)` | `!sessions.iter().any(id)` |
| Reopen | `resume_inner` → record found → `reopen_provider(record)` | no record → `reopen_muse(id)`: its own `ResumeSession { metadata_only: true }`, bridge registration, lease/stale-history handling, `backfill_lane` |
| Row on turn start | view's `ProviderTurnAccepted` | ignored there; the hub's `TurnRunning` in `on_hub_event` |
| Row on turn end | view's `ProviderTurnFinished`: ledger row, record `turns`, one-row `rejoin_provider_row`, byline | ledger row only; the hub's `TurnSettled` → `load_index` + **`load_sessions` (a listing and a 2,661-row join, 64–144 ms)** |
| Start parameters | none | `muse_open()` → `MuseOpen { model_provider, mode }`; `start_model` checks Muse's catalog |

`ProviderId::parse` falls back to `Muse` for an unknown id, which is why `reopen_provider` refuses a "Muse" record
today: it is really refusing an unknown provider.

## 2. The record

No new shape. A Muse session's record is a `ProviderSessionRecord` with `provider: "muse"` in the same
`provider-sessions.json`. Muse fills the same fields: `workspace`, `project`, `created_ms`/`updated_ms`, `turns`,
`title` (Muse's own title from its list row), `first_prompt`, the handoff fields, `display_texts`.

An unknown provider id stops parsing as Muse: `ProviderId::parse_known` (or equivalent) returns `None`, and
`reopen_provider` refuses that, not Muse.

## 3. Where records come from, and the migration trigger

- **New sessions:** `record_provider_open` writes the record for all three (the Muse early return goes).
- **Existing Muse sessions (the owner's 2,661):** a record is written **the first time Baaz opens that session** —
  a sidebar or rail click, ⌘K Resume, a search hit, an `open:` step, boot `--session`, a fork or a handoff that
  names it. `resume_inner` finds no record, finds the session in Muse's list (`self.sessions` / the saved list),
  writes a record from that row (workspace, project, turns, title, first prompt, created/updated), then goes
  through `reopen_provider` like the others.
- **Trigger, named precisely (for the checklist):** *the first time you open an old Muse session in Baaz.* Not at
  launch, not on list arrival. A session you never open never gets a record and needs none: its row is still
  Muse's list row, and the list keeps listing it.
- On every Muse reopen the record's `turns` and `title` are refreshed from the newest list row first (the muse
  CLI may have added turns elsewhere), so the "no turns, open fresh" rule in `reopen_provider` never drops a real
  Muse session.

**Why not "on first sight in `session/list`"** (docs/69's wording): writing 2,661 records buys nothing a list row
does not already say, and turns `provider-sessions.json` into a ~1 MB file that today is rewritten synchronously
on the UI thread on every turn start and settle. The record exists to route an open and to hold Baaz's own facts;
an unopened session needs neither. If P7 or search later wants every session in one store, that is a one-line
change to the trigger plus the off-thread writer below.

Records are still written on the UI thread today. S1 moves `provider_sessions::write` onto the same
generation-ordered background writer `sessions.json` already uses (docs/51 #10), so the file can grow without
costing a frame.

## 4. What dies

- `reopen_muse` and its private `ResumeSession`: Muse reopens through `reopen_provider`. What it did that the
  others lack becomes generic or a registry property:
  - starting the shared child first (`ensure_muse_started`) → the factory's job for a provider that shares a child;
  - `metadata_only: true` + `backfill_lane` after the ack → `providers::pages_history` (already a property);
  - the terminal bridge registration and its refusal memo → a property (`resume_carries_bridge`) plus the hub's memo;
  - `HeldElsewhere` → read-only with the lease notice, `StaleHistory` → page anyway: handled by **error hint** in
    the one reopen landing, for any provider whose adapter raises them.
- `record_provider_open`'s Muse early return and the `fresh` fork in `finish_provider_open`.
- The `ProviderTurnAccepted` / `ProviderTurnFinished` Muse early returns: Muse rows, records, titles and drafts
  follow the same view events as Codex. The ledger row stays untagged for Muse only if the ledger needs it
  (`provider_sessions::ledger_row` takes the provider id).
- In `on_hub_event`, `TurnRunning` / `TurnSettled` keep only the work for a Muse session **with no view in this
  window** (run from the muse CLI or another window): `sync_row_live` for that one row. The `load_sessions`
  after every settle goes; the one-row update replaces it, which should close speed follow-up 2 (measure again
  in S4 before doing anything more). `load_index` stays: it is fingerprinted and cheap.
- `muse_open()` / `MuseOpen` and the Muse branch in `start_model`: the model a session asks for comes from the
  provider's catalog through one function; the `model_provider` extra becomes a property of the open command.
- The boot catalog warm-up's Muse branch, if it is only a "Muse needs its catalog early" special case: a
  registry property (`warms_catalog_at_boot`) or nothing.

What stays Muse-specific on purpose, as named registry properties, not forks: `lists_sessions` (Muse's list is
where sessions made outside Baaz come from), `row_mark`, and everything in `providers.rs`, `login.rs`, `tier.rs`,
`billing.rs`, `provider_status.rs`, account usage and Settings content.

## 5. The ratchet

`crates/baaz/tests/seam_ratchet.rs` gains an **absolute** check: no `== ProviderId::Muse`, `!= ProviderId::Muse`
or `ProviderId::Muse =>` match arm in the open/row paths — `app/lifecycle.rs`, `app.rs`, `app/titles.rs`,
`right_changes.rs`, `session/lane.rs`, `session/commands.rs` — outside `#[cfg(test)]` modules. Constructing the
value (`ProviderId::Muse.as_str()`, a fixture row) is not a comparison and stays allowed.

## 6. Done when

- The ratchet passes at zero.
- A Muse session opens, reopens and updates its row through `open_new_on_provider`, `reopen_provider`,
  `ProviderTurnAccepted` / `ProviderTurnFinished` — the code Codex uses.
- Tests: a Muse reopen with no record writes one from the list row and goes through `reopen_provider`; a second
  reopen reads the record; a held-elsewhere and a stale-history Muse resume land as before; a Muse turn updates
  one row and does not relist.
- Live (no Muse turns): reopen an old owner Muse session (record written, history pages in, model and context
  read right) and one created before the migration; quit and reopen Baaz and reopen it again from its record.

## 7. As built (30 Oct 2026)

Built as §1–§6 say, on `main`. The ratchet (`tests/seam_ratchet.rs`,
`the_open_and_row_paths_never_compare_against_muse`) went from **30** comparisons to **0**; it strips
`#[cfg(test)] mod` blocks with a literal-aware brace matcher, counts any path to the variant
(`crate::providers::ProviderId::Muse` too), and treats `==`/`!=` on either side, match arms (`=>`, `|`),
`matches!(…)` and `contains(&…)` as comparisons. A self-test pins what it sees.

**Registry properties added** (`crate::providers::ProviderId`, all Muse-only today): `parse_known`,
`shares_one_child` (start the window's child first; the lane never hangs it up on a lost connection; the window's
reconnect banner speaks for it; the hub's resume-bridge memo applies), `lane_survives_lost_child`
(Claude Code and Muse), `scripted_draft_is_local`, `unfiled_starts_in_default_workspace`,
`open_routes_model_vendor`, `completes_in_side_session`, `catalog_rows_carry_efforts`,
`writes_skills_by_prompt`, `external_approval_kind(approval_id)`. Existing ones now used for it:
`lists_sessions`, `pages_history` (now also sets `ResumeSession.metadata_only`), `has_billing_tier`
(the account-usage refresh), `catalog_flags_session_model` (`start_model`, `load_models`).

**Died:** `reopen_muse` and its `MuseReopen`; `MuseOpen` / `muse_open` (→ `OpenExtras` / `open_extras`);
`muse_turn_started`; the Muse early returns in `record_provider_open`, `ProviderTurnAccepted`,
`ProviderTurnFinished`; the `fresh` fork; `reopen_provider`'s "Muse means unknown" refusal (now
`parse_known` → `None`); `SessionView::new_lane_reopen` and `Harness::open`'s `reopen` argument (only scripted
chrome opens a lane-less view now); `SessionView::first_prompt_text` (its one caller went).

**Off-thread writer:** `provider_sessions::write_ordered(generation, …)` + `next_write_generation()`, the
`sessions.json` shape; `Harness::persist_provider_sessions` snapshots on the UI thread and writes on the background
executor, and `flush_right_save` (the quit/close durability point) writes synchronously while one is in flight.
`provider_sessions::write` stays as the synchronous form in the same generation line.

Where the code moved a point, the closest faithful option, and why:

1. **Starting the shared child is not the factory's job.** The factory runs on the background executor and
   cannot start the window's connection (`connect` needs the window's context). Instead every open and reopen
   calls `ensure_provider_started(id)`, a no-op unless `shares_one_child`; the factory's lane already waits
   while the child starts.
2. **The bridge property is `shares_one_child` + the hub's memo** (`resume_bridge_refused`), not a separate
   `resume_carries_bridge`: every provider's resume carries the bridge; only the shared child can refuse it.
3. **A Muse record never builds a sidebar row.** Muse's list owns its rows (labels from the list/index ladder,
   bylines, branch); `merge_provider_rows` skips records of a provider that `lists_sessions`, and
   `rejoin_provider_row` only moves such a row's counters (turns, time) — that is the "one row" a settle updates.
   A fresh Muse session still gets its local row from the open (`ensure_listed_row`) until the list shows it.
4. **`TurnSettled` keeps two more things than §4 lists:** `record_turn_outcome` (the failed-turn state; only the
   hub stream carries the outcome, so dropping it would lose Muse's "Failed" rows — Claude Code and Codex have the
   same gap, which P6 should close) and `load_index` (which §4 says stays). A viewless row still stands down on
   the idle `statusChanged`, as before (docs/64 §4 item 4). A turn started **elsewhere** on a session that has a
   view here no longer flips its row to running (the view hears no `ProviderTurnAccepted` for it) — the same as
   Codex; rare enough to leave.
5. **"No turns, open fresh" for a listed session** needs its list row to confirm zero turns
   (`record_holds_no_history`): boot `--session <id>` before the list lands writes a record with nothing known,
   and must not drop a real session. A Muse draft the list confirms empty now reopens fresh, like Codex's.
6. **A parked view with no record** (a provider draft still opening under its request id) activates before any
   record is synthesised, so no bogus Muse record is written for it.
7. **Handoffs to Muse open like the others:** `start_handoff` calls `open_on_provider(to, …)` in the source's
   workspace for every destination. Before, a Muse destination went through `new_session` (it could reuse a
   project draft, switched the new-session default to Muse, and in scripted chrome opened a local view). Every
   handoff test passed unchanged.
8. **Reopen UX moves to `reopen_provider`'s:** the previous view stays (Q4b/Q4c) until the lane lands, instead of
   a loading view drawn at the click; a refusal other than held-elsewhere/stale-history lands the inline failure
   with Retry, not a dialog. `ProviderTurnAccepted` gained what `muse_turn_started` did generically: it clears
   `unsent` and re-arms the reveal when the first send makes the row visible (Codex gains both).
9. **The Muse ledger row is tagged `"muse"`:** nothing queries untagged rows (`usage.rs` documents the tag).
10. **Not moved off the UI thread:** `SessionView::remember_display_text` still reads and writes the file
    synchronously (a view-side copy, pre-existing, rare). It now takes a generation in the same line.
11. **Scripted chrome** (`--no-connect` / `--replay`) writes no records (`provider_sessions::read` is empty and
    `write` is a no-op under `BAAZ_DETERMINISTIC=1`, and the trigger runs live only), so captures are unchanged.
12. **No boot catalog warm-up branch** existed in these files any more; nothing to convert.

Tests added: `a_muse_reopen_without_a_record_writes_one_from_its_row_then_reads_it`,
`a_held_elsewhere_muse_reopen_lands_read_only_under_the_lease_notice`, `a_stale_history_muse_reopen_pages_in_anyway`,
`a_refused_muse_reopen_fails_inline`, `a_muse_turn_moves_one_row_and_never_relists`, `a_records_provider_parses_strictly`,
plus the two ratchet tests. Changed: `deleting_a_provider_session_forgets_it_entirely` waits for the background write
before reading the file; four tests drop `open`'s removed argument. None deleted.
