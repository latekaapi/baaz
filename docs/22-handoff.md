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
  title, and its turn count is the chain's total. Selecting any member id
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
