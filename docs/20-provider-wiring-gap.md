# 20 — The provider seam is not connected. Written 2026-09-26.

This document exists because three commit messages, `docs/17-providers.md` §S46
and the build board all read as though Baaz can run a session on Claude Code or
Codex. **It cannot.** Until that is either fixed or reworded, this file is the
correction of record.

## What is true

`crates/baaz/src/providers.rs` is a **presentation layer**. It supplies:

    label()  blurb()  composer_placeholder()  hero_subtitle()  icon()
    waiting_headline()  capability_state()

`capability_state` is a `match` over hardcoded arms that **mirrors by hand** what
`provider-muse`, `provider-claude-code` and `provider-codex` declare in their own
`caps.rs`. The file's own doc comment says it "never re-probes". It holds no
adapter, performs no spawn, and opens no connection.

The only connection path in the app is:

    crates/baaz/src/app.rs:1171   conn::connect(&program)
    crates/baaz/src/app.rs:1470   conn::connect(&program)
    crates/baaz/src/conn.rs:109   provider_muse::establish(program, &connect_info())

**There is no second lane.** baaz depends on the two new adapter crates
(`crates/baaz/Cargo.toml:37-38`) but uses them only as parsing libraries:

    provider_codex::child::model_catalog(...)
    provider_codex::child::supported_efforts(...)
    provider_claude_code::argv::reasoning_effort_unavailable_reason()

`ClaudeCodeAdapter` and `CodexAdapter` are **never constructed** in baaz.

## How to see it in thirty seconds

    ./target/debug/baaz --steps 'new;setprovider:claude-code;send:Reply with exactly OK.'

Observed: `routing through echo`; `session/start` issued to **muse**; **no**
`claude` child spawned; the run stalls and never sends.

**A trap while checking this.** `ps aux | grep claude` matches the Claude Code
*desktop app's* own sessions — version 2.1.275, `ccd_*` MCP servers, no
`--mcp-config`. Those are not Baaz's children. A Baaz-spawned child would carry
`--mcp-config` and `--strict-mcp-config` (see `docs/18-claude-code.md`). Reading
those processes as success is an easy and costly mistake.

## What the picker does do, and why that fooled a green gate

Selecting a provider genuinely changes the chip, the model label, the composer
placeholder, the hero subtitle and the capability strip — Claude Code shows
three rows (steer and stop `Unverified`, `Questions` unavailable-in-prose),
Codex shows one. **All of that is real and all of it is presentation.** S46
passed 489 tests and a visual probe on exactly this basis.

The lesson is the one this repo keeps recording, in a new place: **the visible
layer was verified and allowed to stand for the thing it represents.**

## What connecting it means

Routing a session through `ProviderCall` rather than the muse pump. That is
where the danger named in the stage-2 and stage-3 handoffs lives:

> Two state machines now read one event stream. Wiring a session view to both
> the legacy pump and a `ProviderCall` gives two writers to one on-screen state.

`crates/baaz/tests/seam_ratchet.rs` pins `muse_client` coupling at **20 files
and may only fall**. This is architecture, not a fix, and it should be planned
as its own stage rather than folded into a task.

## Until then

Any statement that Baaz "supports three providers" must be qualified: it
**renders** three providers and **runs** one. The adapters are real, tested
against live captures, and ready — they are simply not called.
