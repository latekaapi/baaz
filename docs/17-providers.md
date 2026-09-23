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
