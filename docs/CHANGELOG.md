# Harness changelog

## 2026-09-08
- Spec frozen: docs/00-spec.md. Wire captures and schema exports from muse 1.0.3 added under fixtures/msp.

## 2026-09-08 — Phase 1: transport and fold

Two crates, no UI. `crates/muse-client` (the `muse serve` child, NDJSON JSON-RPC,
a typed Rust surface for all 186 types in `msp.d.ts`) and `crates/muse-adapter`
(`MuseFold`: MSP view events → `aui_protocol::Delta` + `SideState`), plus the
`aui-protocol` extensions on agentic-ui's `muse-support` branch. Every capture in
`fixtures/msp/*.jsonl` replays through the fold to a checked-in snapshot with no
`Generic` fallbacks; `live_echo` drives a real `muse serve` on the free echo
provider. Full detail in `docs/01-transport.md`.

Discrepancies recorded per the spec's precedence rule (capture wins):

- **`muse serve --no-session-log` emits no view events.** The spec names it as
  the flag tests should use. Under an ephemeral host a `turn/start` is accepted
  and `session/started` is the only notification that ever arrives — no
  `turn/started`, no items, no `turn/completed`. Verified through `muse-client`
  and independently through `fixtures/msp/probe.py`. The probe and `live_echo`
  therefore run **durable**; `HARNESS_PROBE_EPHEMERAL=1` reproduces the silence.
- **An approval's `itemId` is its own id, not the gated item's.** The research
  doc §7.2 implies otherwise. The `userShell` item it gates has a different id
  and no back-pointer; only `toolCall.approvalId` joins the two, and only for
  model tool calls.
- **An approval's `turnId`, for a user shell, is the shell item's `commandId`**
  even though the item's own `turnId` is `null`. Nothing in `msp.d.ts` says this;
  the fold relies on it to file the shell card and its approval in one turn.
- **Five schema-optional fields are always present on the wire**, sometimes as
  `null`: `Item.turnId`, `SessionTokenUsageParams.modelId`,
  `SessionBranchChangedParams.branch`, `SessionModelChangedParams.providerId`,
  `SessionGoalChangedParams.goal`. Modelled as required-nullable.
- `UnframedViewNotificationParams` needs a `serde(flatten)` catch-all or every
  `view/page` event loses its payload.
- Confirmations of the research doc, not contradictions: `onRequest` raises no
  approval for a model-issued `ls`; a turn can bill reasoning tokens and emit no
  `reasoning` item; `session/started`, `approval/request` and `userInput/request`
  are all on the wire and all absent from the published method index.

Spend: zero real-provider turns. Everything ran on `echo`.

## 2026-09-09 — Phase 2: shell, sessions, streaming

`crates/harness`, the gpui app. It boots as `aui/examples/minimal.rs` does, opens
one 1440×900 window titled "Harness", and drives a real `muse serve` through
`muse-client` and `MuseFold`. Auth probe and device-code login screen, the
sessions sidebar filtered to the workspace and enriched from the local index,
resume with full `view/page` backfill, a real streaming turn with tool cards and
the per-turn token footer, stop with retract, the reconnect procedure, and the
error banner/dialog split of §3.8. Full detail in `docs/02-app.md`.

Spend: **two real `meta` turns** (of the five the spec allows), both in
`docs/images/phase2-turn-*.png`. Everything else ran on `echo`
(`HARNESS_PROVIDER=echo`).

Findings and decisions worth keeping:

- **`session/list` filters `workspaceRoot` on exact string equality.** A session
  started in `/tmp/x` and one started in `/private/tmp/x` are two different
  workspaces to the wire, even though the index records the same
  `workspace_key`. The app therefore canonicalizes `--workspace` once at start-up.
- **A newly started session is not in `session/list` immediately.** The listing
  is index-derived and the index is written when the session log flushes, so the
  sidebar refreshes on `turn/completed` rather than only after `session/start`.
- **History comes from `view/page`, not from `session/resume`.** Resume runs with
  `excludeItems: true` and the transcript is paged forward from the beginning of
  the view. It is the one path that is contiguous, ordered and bounded, and it
  never replays `item/delta`, so a backfilled message arrives whole.
- **The credential is ambient, so a successful login needs a fresh child.**
  `muse serve` picks up the credential at spawn; the app respawns and re-probes
  after `Signed in.` rather than reusing the connection that inherited none.
- **`muse login` prints the device code bold through `tput`.** The stderr parser
  strips SGR before matching, and neither the URL nor the code is ever logged.
- The spec's §4 name for the composer is `aui::composer::docked_composer`; the
  editable docked composer is `aui::composer::composer(...).docked(true)` —
  `aui::shell::docked_composer` is the design card's non-editable placeholder.
  The app uses the editable one.
