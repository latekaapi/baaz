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
that is not left to each adapter's good behaviour. `ProviderAdapter::send`
is a **provided method** (`crates/provider/src/traits.rs`): it looks up the
command's `required_capability` in `capabilities()` first and refuses there,
before the adapter's real dispatch (`send_inner`, the required method)
runs. An adapter that forgets to refuse still refuses, because its dispatch
is unreachable except through `send`; the app calls only `send`. The
deliberate hole — overriding the provided `send` itself — is out of
contract, and no in-tree adapter does it. The gate is a floor, not a
ceiling: adapters may still refuse anything further with `Unsupported`
(e.g. partial support inside one capability).

`Unverified` is attempted everywhere: only `Unavailable` refuses
(`CapabilityState::allows_attempt`).

## What muse declares, and on what evidence

From live fixtures and the recording puppet: every command-backed
capability is `Native`; the session shell is `Native` (`userShell`
granted 1.0.3–1.3.0); client tools are `Native` at ≥ 1.3.0 and
`Unavailable` below. From this brief, not from a probe: the 1.3.0
`sessionMcp` grant and the 1.2.1 never-granted observation. `Unverified`:
reasoning traces and sub-agent turns — no live probe in this task.

The gate proves the crates compile, clippy is clean, and the invariant
holds for the adapters in the tests. It does **not** prove the declared
set is true of a real muse — nothing here talks to a live server. Every
entry is a claim until a live probe checks it. Nor does any UI consume the
set yet — `baaz` is untouched by this task.

## Mutation check

In a scratch copy, remove the refusal from the provided `send` (call
`send_inner` directly) and run the refusal test
(`crates/provider/tests/capabilities.rs`): it must FAIL — the forgetful
adapter's `Ok` leaks through. Restore. If it still passes, the test is
pinning the adapter's good behaviour rather than the guarantee.
