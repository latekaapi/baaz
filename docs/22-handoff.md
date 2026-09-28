# 22 — Handoff between providers, as an honest lossy re-prompt

Locked decision (roadmap): **handoff = a lossy re-prompt, not continuity.**
The destination starts a **fresh** session seeded with a context pack. The
card always shows what crossed over (`Carried`) and what did not
(`Not carried`, never hidden). A same-provider model change is native
`SelectModel`, never a handoff.

Anchors: machine + pack `crates/baaz/src/handoff.rs`, view half
`crates/baaz/src/session/handoff.rs`, app half `crates/baaz/src/app/lifecycle.rs`
(`show_handoff_confirm`, `start_handoff`, `land_handoff_destination`,
`acknowledge_handoff`, `fail_handoff`, `cancel_handoff`), card
`aui::transcript::handoff` (tag `v0.2.9`), confirm `handoff_confirm`.

## 1. State machine

`requested → quiescing → checkpointed → prepared → acknowledged → activated`,
with `refused`, `failed` and `cancelled` off-ramps. One `HandoffRun` per
source session, owned by the application (`Harness::handoffs`); the source
view mirrors the run's card (`append_client_block` on request,
`replace_client_block` on every transition).

| transition | trigger |
|---|---|
| → `requested` | "Hand off to X…" (provider menu, ⌘K `/handoff`, `handoff:<provider>` step). Refuses **immediately** as `refused` with the reason when a question or approval is pending, or the source is mid-turn and cannot be interrupted; else the card appends. A same-provider pick never reaches the machine (menu close; `request` returns `SameProvider`). |
| → `quiescing` | A running turn is interrupted; new sends are refused while the card reads Quiescing / Checkpointed / Prepared. |
| → `checkpointed` | The pack is built from the folded transcript (point-in-time; an interrupt's late deltas may land just after). |
| → `prepared` | The destination opened (`open_on_provider` for Claude Code / Codex, `new_session` for Muse, same project/workspace) and the pack submitted as its first turn — full text model-visible, short summary as `display_text`. |
| → `acknowledged` | The destination's submit ack for that session under the run's epoch. |
| → `activated` | Source card shows Activated with `destination_session`; source composer retires ("Handed off to X — open the new session"); destination row notes "from X"; links persist; view is on the destination (it landed there at open). |
| → `failed` | Any step's error, with the reason. Source stays usable (a Failed card sends again). Destination-open failures fail the run too — never a silent fallback. |
| → `cancelled` | Card Cancel while cancellable (Requested…Prepared). The opened destination shuts down (view closed, child hung up). Past Acknowledged the press is ignored. |

## 2. Owner-epoch fencing

Every request bumps `Harness::handoff_epoch` and carries it. A newer
request for the same source supersedes the in-flight run (aborted, its
destination shut down). A landing applies only under the pending epoch on
the requested lane; an ack advances only a Prepared run — a stale ack
from a cancelled, failed or superseded run matches nothing and is
ignored. Unit test: `epoch_fencing_ignores_a_stale_ack`.

## 3. The context pack (provider-neutral)

Built from the folded transcript only — **no model summary call runs**
(the card says "extractive summary"):

- the original goal (first user prompt, truncated),
- an extractive summary (opening lines of the earliest replies),
- the last turns verbatim, whole turns only, within ~8k tokens (chars/4),
- open todo labels (pending/running, never done),
- files touched (`verb target` from tool cards, deduped, capped at 30),
- the working directory.

Submitted under `Continuing a session handed off from <Provider>. Context
follows.`; the user bubble shows only the short summary.

## 4. Never carried

Tool call internals (outputs, arguments), pending approvals, provider
memory, images (the pack is text-only), reasoning traces. All five sit on
the card's `Not carried` list and in the confirm dialog.

## 5. What survives restart

Provider sides: `provider-sessions.json` (`handoff_to` on the source,
`handoff_from` + `handoff_from_provider` on the destination). Muse sides:
the same halves on the local row (`sessions.json`). The destination keeps
its quiet top marker ("Handed off from X · model" + back-link); the card
and the read-only source do not need the run — they are transcript and
view state.

## 6. What this doc does not prove

How the card looks and whether the destination genuinely understood the
pack. The live runs (§7) are the evidence; report them, do not claim
beyond them.

## 7. Live evidence

### H2b — Claude Code → Codex, 2026-09-27

Scripted baaz run under a scratch `BAAZ_STATE_DIR` (nothing written to
the owner's store): `new` on `claude-code`, two turns about the
fictional harbor town Greyport (three ferry-route names, then a
backstory for Cormorant Crossing), `handoff:codex`, then
"What were we working on?" on the destination.

- The run logged `handoff requested … -> codex`, opened the destination
  on the codex lane, and logged `handoff activated <source> ->
  <destination>` — the transition that replaces the source card with
  the Activated card (carrying `destination_session`) and retires the
  source composer ("Handed off to Codex — open the new session").
  `provider-sessions.json` kept `handoffTo` on the source and
  `handoffFrom` + `handoffFromProvider: claude-code` on the
  destination.
- Destination marker: the destination view opened on
  "Handed off from Claude Code: We are planning a fictional harbor
  town called Greyport…" with the carried turns (both Greyport
  exchanges verbatim) above its first reply.
- Destination's first reply to the pack: "Got it—I'm ready to continue
  with Greyport." (gpt-5.6-sol). Asked "What were we working on?" it
  answered: "We were developing Greyport, a fictional harbor town. We
  named three ferry routes, then chose Cormorant Crossing and wrote its
  two-sentence backstory about lighthouse-island birds and a
  fisherman's informal supply run becoming an official service."
- No fold panic: this is the leg that used to underflow
  `muse-adapter`'s `push_block` (H2b §1) — the source card and the
  destination origin marker both file through the fixed path.
- Not run: the Codex → Muse reverse leg (optional in H2b).

## 8. One session, one row — the handoff chain (owner decision 2026-09-28)

Supersedes the *presentation* in §1 (`activated`) and §5. The machine, the
pack and the fencing are unchanged.

The owner asked for handoff to continue **in the same session**: one sidebar
row, one transcript, with a visible break where the provider changed. A
provider session id is still owned by one lane for life (docs/21) — a Claude
Code thread cannot become a Codex thread — so the destination is still a new
provider session underneath. What changes is what the user sees:

- **Chain.** Sessions linked by `handoff_to` / `handoff_from` form a chain;
  the **head** is the one with no `handoff_to`. The chain is the unit the
  user sees. The sidebar shows **only the head**, under the chain's original
  title, and its turn count is the chain's visible total (§8.4). Selecting any member id
  (search result, back/forward, `--session`, a link) opens the head.
- **Transcript.** The head's view renders, in order: every earlier member's
  turns (read-only), then a `Block::Marker { kind: MarkerKind::HandOff { from,
  to } }` divider reading "Handed off from Claude Code to Codex" (with the
  destination model and time), then its own turns. The pack's user turn is not
  shown as a bubble — the divider stands for it (its short summary may sit in
  the divider's text).
- **Snapshot.** At `activated`, the source view's folded turns (which already
  include its own prefix, so chains of any length compose) are written as JSON
  (`aui_protocol` blocks are serde) to `<state>/handoff/<destination>.json`.
  Reopening the head reads it; the source provider is never respawned just to
  draw history. A chain with no snapshot (pairs made before this change) shows
  the divider with an "Open the earlier conversation" affordance to the
  source's read-only view instead.
- **In place.** Activation swaps the view in the **same slot** — no jump to a
  different row; the composer is live on the destination immediately.
- **Carried-over state kept:** `provider-sessions.json` / `sessions.json`
  links as today; the ledger still records each provider session separately.

### 8.2 Polish (X3c): no frozen card, no pack echo, bounded snapshots

- **No frozen card.** The snapshot never carries the source's handoff card
  (captured while still Prepared, with live Cancel / "Open the new session"
  buttons); `read_snapshot` filters it from files written before this
  change. The divider stands for the handoff. The live source view keeps
  its card while the handoff is in flight, exactly as before.
- **The divider says what crossed over.** "Handed off from Claude Code to
  Codex · gpt-5 · 4 turns carried" — the destination model and the pack's
  verbatim turn count, each omitted when unknown. "Open the source session"
  rides the divider only when no snapshot exists (the fallback divider);
  with the carried turns on screen the back-link has nothing to add.
- **No pack echo.** The pack's last line asks the destination for a
  one-sentence acknowledgement, so its reply ("Context received; I'll wait
  for your next message.") is hidden with the pack bubble — live and on
  reopen. Both turns stay in provider history; their tokens may still count
  in the session meters.
- **First user turn only.** The pack match consults the destination's FIRST
  user turn, so a later message repeating the pack text is real and stays.
- **Bounded snapshots.** Tool outputs and bodies beyond ~200 lines truncate
  with a "… N more lines" line; image and attachment payloads become a
  placeholder chip; screenshots are dropped; at most the last ~300 turns
  keep, with a single "Earlier turns not kept" marker at the top. Bounding
  applies on write and (idempotently) on read; rendering is unchanged.

### 8.1 As built (X3a, transcript half)

- Snapshot I/O, divider text and the pack-bubble match live in
  `crates/baaz/src/handoff_snapshot.rs`. The file is
  `<state>/handoff/<destination id>.json` (same state-dir resolver as the
  other stores, so `BAAZ_STATE_DIR` is honoured; no-op + unread under
  `BAAZ_DETERMINISTIC=1`), written atomically at activation from the source
  view's visible turns. Schema: `version`, `from`/`to` wire ids, `source`
  (the back-link target), `toModel`, `activatedMs`, `packText`,
  `packDisplay`, `turns`, `turnsCarried` (X3c; absent in older files, whose
  divider then omits the count).
- The destination's prefix and divider are view-side (`SessionView` fields
  in `crates/baaz/src/session.rs`, assembled in
  `refresh_render_cache`): the old fold-written origin marker is gone, so
  one handoff draws exactly one divider. The pack's user turn is hidden by
  full text live, by summary after a replay (pre-snapshot pairs fall back
  to the display map); it stays in provider history. (X3c adds the pack's
  acknowledgement beside it, consults only the first user turn, and drops
  the divider's back-link while a snapshot prefix is on screen.)
- Activation (`acknowledge_handoff`) writes the snapshot, shows the prefix
  on the destination, and leaves the window on it with the composer
  focused; reopening (`attach_handoff_prefix`, called from the provider
  open and the muse open/resume paths) reads the snapshot, or the fallback
  divider alone when only `handoff_from` survives.
- The sidebar half (head-only rows, member-id redirect) is separate work;
  `sidebar.rs` is untouched by this change.

### 8.3 As built (X3b, sidebar half)

- **One row per chain.** The collapse is pure (`collapse_handoff_chains`,
  `chain_head`, `chain_members` in `crates/baaz/src/sidebar.rs`, links from
  both stores): a session with a `handoff_to` link whose destination is
  known in either store or the rows is not listed. The head row keeps its
  own `updated`, provider badge and flags, reads the tail member's title
  unless the head was user-renamed, and counts the chain's summed visible
  turns (§8.4).
  Mixed chains work — the links are read from whichever store holds them.
  A dangling `handoff_to` keeps the source listed; a head with no row yet
  waits for the next build; a link cycle lists, never hangs.
- **Member ids open the head.** `resume_inner` resolves through
  `Harness::chain_head` first, so every open path — sidebar/rail clicks,
  both palettes, sidebar search hits, the `open:`/`click:` steps, boot
  `--session`, the handoff card's link — lands on the head, selected
  (the existing activation already points the highlight at it). The X3a
  fallback divider is the one exception: its back-link emits
  `SessionEvent::HandoffOpenSource`, which opens the source through
  `resume_source_read_only`, bypassing the redirect — a retired source
  view keeps its read-only banner there.
- **Archive/unarchive act on the chain** (`chain_members` expansion), so no
  member resurfaces past an archived head; Undo restores the whole chain.
  Rename and pin are untouched — they name the head id the row already
  shows, which is what the row displays.
- **Live.** Activation persists the links, which rebuilds the provider rows
  and collapses in the same build: the source row disappears and the
  destination row is emitted where the chain's first member stands (no
  reorder jump beyond what `updated` would cause anyway — the visible list
  sorts newest-first and the head keeps its own time), with the window
  already on the destination from the lane open.
- **What the gate proves, and does not.** `cargo test -p baaz` proves the
  row list computed from the stores and the id resolution (unit fixtures
  plus `gpui::test`s through the real `Harness`). It does NOT prove what
  the sidebar draws, the selection highlight, any animation, or the live
  swap in the window — those remain unverified here.

### 8.4 As built (X3d, chain-aware search, conditional collapse, honest counts)

- **Search reads a chain as one hit.** `search_session_rows`
  (`crates/baaz/src/app/find.rs`) groups the indexed sessions by chain
  head and emits one `SessionRow` per chain: the head id, the chain's
  display title (the collapsed sidebar row's label when the list has
  one, else the head's own index label), and the head's own
  label/title/first prompt — with every other member's label, title,
  first prompt and transcript text appended to the body. A query
  matching only an early member's turns returns the head. Provider-lane
  members carry no index entry, so they contribute their ack title and
  first prompt. Opening still redirects to the head; that path is
  untouched.
- **Collapse runs only on links.** `sidebar::needs_collapse` is true
  when the touched id carries a handoff link in either direction or is
  the endpoint of another session's link. `rejoin_provider_row` (the
  per-turn settle path) and the override-write `rejoin` collapse only
  then; `merge_provider_rows` collapses only when a merged or removed
  record is link-adjacent. Unlinked settles keep the O(1) row refresh.
  Full list builds (`session/list` replies, fixtures) still collapse
  unconditionally — they already pay the full scan.
- **Counts match the transcript.** The transcript hides the pack
  exchange (the pack's user turn plus its one-sentence acknowledgement)
  while the stores count the acknowledgement as a settled turn, so every
  member with a `handoff_from` link reads one turn high. Both row
  constructors (`SessionEntry::join` for muse rows,
  `SessionEntry::provider_row` for lane rows) subtract it via
  `sidebar::visible_turns`, saturating at zero. The collapsed head row
  is the plain sum of the already-honest member rows, and a lone
  destination row reads the same rule wherever it is shown. E.g.
  claude-code (1 turn) → codex (2 incl. pack) → muse (2 incl. pack)
  shows 3 turns.
- **What the gate proves, and does not.** `cargo test -p baaz` proves
  the index rows, the decision function and the counts. It does NOT
  prove what the palette or the sidebar draw — those remain unverified
  here.

### 8.5 As built (Y2a, one title / one row / one selection)

Supersedes the *presentation* in §8.3's collapse paragraph. Storage now
keeps every member row; the one-row-per-chain view is derived in
`visible_sessions` from the pure `collapse_handoff_chains`
(`crates/baaz/src/sidebar.rs`), so collapsing is idempotent and
order-independent — a second run, in any order, yields the same rows.

- **One title.** At activation the destination records `handoff_title`
  (provider record and/or override), copied from the source's current
  display title — user name first, then its title — and carried forward
  down chains of any length. The title ladder for a member reads: user
  rename of the head, then `handoff_title`, then its own title. The
  collapsed head row derives its label the same way, with the tail
  member's row label as fallback. No handoff turn renames: `should_title`
  is false for any session with `handoff_from`; `title_from_transcript`,
  `first_send_update`, `note_first_prompt` and `maybe_start_title` skip
  the pack turn, its acknowledgement and the destination's first real
  message (the pack match is `sidebar::is_pack_text`: the pack header or
  the "Handed off from …" bubble).
- **One row, one selection.** Header crumb, window title and the
  sidebar's selected-row key all resolve the raw active/pending id
  through `chain_head`; the active/pending head is never filtered as
  empty, so the destination row stays highlighted while the pack runs.
- **Row state.** The pack submit marks the row running and touches
  `updated`, like `ProviderTurnAccepted`; activation copies pinned,
  project, user name and archived from source to destination; the
  collapsed head ORs needs-you attention (and pinned/archived) across
  members, takes the newest time and sums the honest member turns.
- **Restart.** A muse-lane destination exists only as an override until
  the wire answers, so `merge_provider_rows` synthesises its row from
  the local stores (chain-titled, hence kept by the empty filter); lane
  destinations rebuild from their records, whose ladder already prefers
  `handoff_title`.
- **Byline.** `record_last_summary` and `maybe_rewrite_byline` skip the
  pack turn and its acknowledgement, so neither becomes the byline.
- **What the gate proves, and does not.** `cargo test -p baaz` proves
  the derived rows, the title ladder and the skip rules. It does NOT
  prove pixels, highlight colour or real-child timing — only a live
  capture speaks to those, at that moment.
