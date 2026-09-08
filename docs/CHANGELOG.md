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

## 2026-09-09 — Phase 3: composer controls

The docked composer grew every control spec §5 phase 3 names: the model, effort
and approval-mode pickers, the context meter with compaction, the queued strip
with steering, `@` mentions, the `/` command menu with skills, client-side plan
mode, prompt history and images. Full detail in `docs/03-composer.md`; the
library side landed on agentic-ui `muse-support`.

Spend: **one real `meta` turn** — the plan probe. Every screenshot and every
other exercise ran on `echo` (`HARNESS_PROVIDER=echo`), which leaves four of the
five the spec allows.

Probe findings, from `fixtures/msp/probe_phase3.py` and the two captures it
wrote:

- **`/plan <text>` fires the bundled skill server-side.**
  `fixtures/msp/transcript-plan-probe.jsonl` (one `meta` turn, started in
  `denyUnmatched`) shows `toolCall read_skill {"name":"bundled:plan"}` followed
  by an agent message in the skill's own shape ("**Plan:** … Reply Approve,
  Request changes, or Cancel."). Plan mode therefore sends `/plan <text>` as the
  model-visible input with `displayText` carrying the person's words, and the
  spec's preamble stays only as a fallback behind `HARNESS_PLAN_PREAMBLE=1`.
- **Reasoning effort has no server reflection.** `reasoningEffort` occurs twice
  in `msp.d.ts` — `TurnStartParams` and `TurnSteerParams` — and nowhere else: no
  echo on the `userMessage` item, none on `turn/started`, no `…Changed`
  notification. The chip shows the client's value and says so in
  `docs/03-composer.md`. `high`, `ultra` and `none` were all admitted on echo
  (`fixtures/msp/transcript-phase3.jsonl`), so the picker is live, not disabled.
- **An image part is admitted without being decoded.** `turn/start` answered
  `status: accepted` for a payload whose PNG IDAT CRC was wrong. The wire checks
  the base64 and the media type and nothing else, so the app decodes before it
  sends and refuses what it cannot read. (The probe's own bytes have since been
  replaced with a valid 1×1 PNG.)
- **`session/compact` on a session with no run is rejected `missing_run`.** The
  schema documents the reason; it is confirmed live, and it is a banner rather
  than the `noop` toast.
- **`session/setModel` on an echo session is rejected `invalid_target`**, not
  the `unsupported_route` the phase brief predicted. Either way it is a
  `commandRejected` and goes to the inline banner, which is correct behaviour:
  `docs/images/phase3-banner-*.png`.

Review findings closed this phase:

- **F1** — Muse tool names now map onto `ToolKind::{Read, Write, Edit, Search,
  Web}` in `muse-adapter::tool_shape`, keyed on the name and on which `rawArgs`
  field is present, and the body follows the kind: a read renders as
  `Read <path>` with its line count, a search promotes `path:line:text` output
  to real hits when every line parses, and everything else keeps the raw output
  because a card with no body would hide what the tool said. Fold snapshots
  regenerated.
- **F4** — the `$0.00` cost cell is not drawn when the catalog reports no price,
  which on a subscription catalog is every row.
- **F5** — the initial "Approval mode · Auto" marker is suppressed. A session
  announces its mode at start-up, and that announcement is only worth a row when
  the mode is *not* the default; a change that changes nothing raises none
  either.

F2 (humanized failure reasons) and F3 (live vs backfill fold parity) remain for
Phase 4, as the brief said.

Two smaller decisions, recorded because they are deviations worth knowing:

- **The pickers get no `on_hover` from the app.** The component already lets the
  pointer win the highlight; an app that also wrote the hovered row into
  `selected` gave the selection two owners, and the check — which marks what the
  session is on — followed the mouse. The keyboard owns `selected` now.
- **The caret popovers scroll.** `command_menu` has no height cap of its own and
  the `/` menu lists thirteen commands plus every installed skill, so the app
  caps it at 560 px and scrolls, and offers at most eight skill rows.
