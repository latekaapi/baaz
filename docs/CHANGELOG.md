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
