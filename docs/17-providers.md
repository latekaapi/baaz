# Providers: the capability spec

The provider seam (C1) refuses unsupported commands with a typed
`ProviderError::Unsupported`. This is what the UI needs *before* it asks: a
provider's declared capabilities, so a button that cannot work is not
offered in the first place.

## The four states

- **native** — the provider does this itself.
- **emulated** — Baaz does it on the provider's behalf, and the result is
  not identical to native. A person is entitled to know which they are
  getting, so the state carries what differs.
- **unavailable** — this provider cannot do it, by design or by version.
  Carries the human reason; without one, whoever renders the missing button
  cannot say why it is gone. `Unavailable` and `Emulated` take the reason
  as a constructor field, so the reasonless form is unrepresentable.
- **unverified** — nobody has checked. Honest ignorance: it is attempted,
  never refused. Not a synonym for `Unavailable`, and it must never be
  quietly collapsed into one.

In code: `Capability`, `CapabilityState`, `CapabilitySet` in
`crates/provider/src/capability.rs`. The list is coarser than `Command`
(several commands share one UI question), except the seven the planned
providers genuinely differ on — forking a session, steering a running turn,
reasoning traces, sub-agent turns, a session-scoped shell, client-side
tools, compacting — which each stand alone. `ClientTools`,
`ReasoningTraces`, and `SubagentTurns` have no commanding command; they
describe the handshake and the transcript.

## Floor, never a pin

Wherever a capability depends on the provider's version, the comparison is
a numeric **minimum** (`muse_version_supported`, `capabilities_for_version`
in `crates/provider-muse/src/caps.rs`). The floors are written down:
the seam supports muse ≥ **1.2.1**, and client-side tools
(`sessionMcp`) need muse ≥ **1.3.0** (1.3.0 grants it; 1.2.1 never did).

One sentence for why: an earlier implementation pinned one provider's CLI
version with exact string equality, so a routine upgrade on the owner's
own machine made half the features fail closed and no test caught it,
because a shell stub stood in for the real CLI.

## The invariant, and where it is enforced

A command whose capability is `Unavailable` must never return `Ok` — and
that is a mechanism, not a convention (`crates/provider/src/traits.rs`).
The trait exposes only the raw dispatch (`dispatch`, a required method that
cannot refuse on its own); the enforced `send` is an **inherent method on
`Provider`**, the wrapper every caller holds instead of a bare
`Box<dyn ProviderAdapter>`. `Provider::send` looks up the command's
`required_capability` in `capabilities()` first and refuses there, before
the adapter's dispatch runs. An adapter that forgets to refuse still
refuses, because its dispatch is unreachable except through the gate.

Why this shape holds where a provided `send` did not: a provided trait
method is a name any `impl ProviderAdapter` block can shadow, and Rust then
dispatches to the shadow without ever falling back to the default — the
gate gone, silently, for that adapter. An inherent method on `Provider`
cannot be shadowed that way: an adapter `impl` block has no `send` to
override (the trait has none), and coherence forbids any adapter crate from
adding methods to `Provider`. There is no accessor for the inner adapter,
so no caller can obtain the bare trait object and reach `dispatch`
directly. Sealing the trait was rejected as the fix: it narrows *who* can
implement (a future second provider implements it from another crate)
without stopping an implementor from overriding — the wrapper narrows
*what any caller can reach*, which is the property that holds.

The old override is now un-expressible, not merely untested: there is no
`send` on the trait to override, so no test adapter can demonstrate the
bypass — the closest attack, an adapter overriding everything the trait
still exposes plus its own inherent `send`, is pinned in
`crates/provider/tests/capabilities.rs` (`SneakyAdapter`) and still
refuses through `Provider::send`.

The gate is a floor, not a ceiling: adapters may still refuse anything
further with `Unsupported` (e.g. partial support inside one capability).

`Unverified` is attempted everywhere: only `Unavailable` refuses
(`CapabilityState::allows_attempt`).

## What muse declares, and on what evidence

From live fixtures and the recording puppet: every command-backed
capability except the shell is `Native`; client tools are `Native` at
≥ 1.3.0 and `Unavailable` below. From this brief, not from a probe: the
1.3.0 `sessionMcp` grant and the 1.2.1 never-granted observation.
`Unverified`: reasoning traces and sub-agent turns — no live probe in this
task — and the session shell. The shell's `userShell` grant *is* on record
at 1.0.3, 1.1.1, and 1.2.1 (`grantedCapabilities:["userShell"]` in
`fixtures/msp/transcript-echo.jsonl`, `transcript-account.jsonl`, and
`transcript-1.2.1-shapes.jsonl`), but every recorded `session/userShell`
execution there ends with the item in status `failed` — the sandbox was
unavailable, so the command was never started. A grant without a single
clean run is not the hard evidence `Native` needs, and 1.3.0+ was never
probed for `userShell` at all. A guess stated as `Native` is worse than
`Unverified`: the declaration stops guessing while `allows_attempt` keeps
the shell sending.

The gate proves the crates compile, clippy is clean, and the invariant
holds for the adapters in the tests. It does **not** prove the declared
set is true of a real muse — nothing here talks to a live server. Every
entry is a claim until a live probe checks it. Nor does any UI consume the
set yet — `baaz` is untouched by this task.

## Mutation check

Two checks, both required before trusting the gate:

1. In a scratch copy, neuter the refusal inside `Provider::send` (call
   `dispatch` directly) and run the refusal test
   (`crates/provider/tests/capabilities.rs`): it must FAIL — the forgetful
   adapter's `Ok` leaks through. Restore. If it still passes, the test is
   pinning the adapter's good behaviour rather than the guarantee.
2. The `SneakyAdapter` test in the same file is the second mutation made
   permanent: an adapter overriding everything the trait still exposes,
   answering `Ok` for an `Unavailable` capability, still cannot reach the
   caller's `send`. If that test ever fails, the wrapper has grown a path
   around the gate and the task is not done.

## S46 — the wiring: a second provider in the switcher, approvals that reach it

`baaz` is no longer untouched: `crates/baaz/src/providers.rs` is the
registry, and the session view reads it every frame.

### The registry

Three entries — `muse`, `claude-code`, `codex` — each exposing its
capability map, mirrored cell-for-cell from the adapter crate that owns
the evidence (`provider-muse`, `provider-claude-code`, `provider-codex`
`caps.rs`). The registry never re-probes; it repeats what those files
declare, so a fixture that upgrades a cell upgrades the mirror with it.
One deliberate deviation is recorded, not hidden: `docs/19-codex.md` §5
proposes `Questions: Native` for Codex, but `provider-codex/src/caps.rs`
declares `Unverified` (the request shape was read, never executed), and
the registry mirrors the crate, not the proposal. The proposal is a
sentence; the crate is the evidence.

Selecting a provider for a **new** session is enough: the composer's
provider chip seeds from the command line and every `session/start`
path reads the pick. The control belongs in the composer action row,
immediately left of the model chip (`ComposerChipAnchor::Provider`,
`ComposerIntent::Provider`) — that is where `aui` always put it
(`composer(id, state, provider, model)` takes the provider as a required
argument), and the chip shows the session's own provider, derived from
the session like every other piece of session chrome.

An earlier revision of this section recorded a provider switcher on the
empty-state hero (`Harness::render_provider_picker`) as though it were
the design. It was not: the S46 brief said "selecting a provider for a
new session is enough" and never said *where* the control belonged, so
the lane invented a hero picker and this doc wrote the invention down.
The hero control is gone — the empty state is hero, project name, hint,
**New session**, nothing else — and this paragraph is corrected so the
accident stops surviving review as an intention.

The chip's menu behaves by whether the session has run anything, because
there is still no way to switch a live session's provider — the view
reads its lane off the one `provider_id` field the host fixed at
construction, with no setter. That is the structural answer to the
two-writers danger; where structure runs out, `check_single_lane` is
the loud detector: both lanes claiming one session returns a violation
as text (logged and bannered) instead of corrupting the screen silently.
So: a session with no turns yet swaps to the pick (nothing written, no
lane committed — the empty session is retired and a new one starts on
the pick), while a session with turns offers the others as **"New
session on \<Provider\>"**, each row carrying the reason in its detail
line the way an `Unavailable` capability explains itself. A turn in
flight counts as turns. Every row acts on click; none goes dead.

The pick is remembered in Baaz's own store (`provider.json` beside the
projects file): a new session starts on the last provider chosen, across
relaunches. An explicit `--provider` / `BAAZ_PROVIDER` still wins for
the run it names.

### The gate, visible

`providers::gate` is what the screen reads. `Unavailable` refuses, so
the control is disabled with the provider's own reason (`steer_text`
and `interrupt` banner and keep the words rather than send a command
the seam would refuse). `Unverified` is attempted everywhere — the seam
never refuses it — so it stays offered but visibly marked. The
capability strip above the composer is the same screen rendering
differently per provider: Claude Code shows steering and interruption
as unverified and questions as unavailable-in-prose; Codex shows
steering as native; muse shows no strip at all. If the strip ever reads
the same for two providers, the spec is decoration and the task is not
done.

### Approvals on the existing surface

Both new providers' approval requests park in `session/approvals.rs`
beside the fold's, under the surface's one rule: **the card is never
ahead of the server**. A press records exactly one `DecideApproval`
onto the outbox the provider lane drains, shows "sent, waiting for the
server", and offers no second press; only the server's notification
(`resolve_external_approval`) settles the card. Claude Code's
`can_use_tool` carries its `permission_suggestions` as the card's
"don't ask again" note; Codex's five request kinds each carry the
model-written `reason` sentence. `decline` ("Deny": no, do something
else — the turn continues) and `cancel` ("Deny and stop": no, stop —
the turn is interrupted) ride as visibly different buttons, because
they are different answers and the old surface did not draw the
distinction.

### The disagreement, not papered over

`docs/19-codex.md` §3, proven live: Codex auto-approves inside its
sandbox profile and only asks when an action escapes it (`echo` ran
unprompted under `:read-only`; a write to `/tmp` prompted). Claude
Code asks about everything not pre-allowed. These are genuinely
different models, and Baaz does **not** average them: each session
presents its own backend's behaviour, labelled as such. A Codex
session's approvals arrive rarely and each carries the model's reason
for why *this* action escaped the profile; a Claude Code session's
arrive for everything unallowed, each with the "don't ask again"
affordance that shrinks future asking. The choice is per-session
honesty over one averaged fiction, for one reason: an averaged
approvals model would teach the person exactly the wrong rhythm —
either waving through a Claude Code prompt that always fires, or
ignoring a Codex prompt that fires only when something actually
escaped. The strip and the card always name which backend is asking,
so the person learns two rhythms and trusts both.

## S46fix — retired with the hero switcher

S46fix once pinned the hero switcher's layout contract in
`Harness::render_provider_picker`: found by driving the app, the
switcher row was one centred flex unit
(`Muse | Claude Code | Codex | <caption>`), so each provider's
different-length caption re-centred the whole row — Codex sat 78px away
from itself between picks, and a second click aimed at it landed on
Claude Code — and the same row hard-clipped the caption at the pane
boundary in a ~520px centre column. The fix put the caption on its own
ellipsised line below a fixed button row, pinned by
`provider_buttons_hold_still_and_caption_stays_inside`.

P2 removed the hero switcher outright (see §S46 above), and the contract
died with the control: the function, its call sites and the geometry pin
are all gone. What remains is `hero_has_no_provider_picker` in
`crates/baaz/src/app.rs`: a `#[gpui::test]` that draws both empty states
and asserts the picker left no measured bounds in either — the pin now
says the hero carries no provider control at all. The history is kept
here because the 78px jump is still the reason no one rebuilds a
caption-carrying switcher on the hero: a control that moves when its own
label changes is a misclick waiting for a second press.
