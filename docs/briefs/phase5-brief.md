# Phase 5 brief — the billing guard, session operations, polish, docs, CI (Harness)

You are the single lead for Phase 5, the last phase of the Muse Code chat slice. You own the
work end to end: library changes in `/Users/latekaapi/Projects/agentic-ui` (branch
`muse-support`, NEVER `main`) and app changes in `/Users/latekaapi/Projects/harness` (branch
`main`). Do not touch `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.

## Spend rule — there is NO free provider (read `docs/04-approvals.md` §0 first)

On this signed-in machine `providerId: "echo"` is routed to the real model and every turn is
billed. Phase 5 spends **zero** turns: never run `live_echo`, `live_backfill_parity`,
`harness-probe`, the Python probes, `--send`, or a `--steps` list containing `send:`/`steer:`.
Free: `--replay <capture>`, `--no-connect`, starting a session, `session/userShell` (`!cmd`,
`shell:` steps), `session/fork`, `session/list`, `model/list`, and the `muse` TUI opened
without a prompt. Every screenshot comes from `--replay` or `--no-connect` unless the state
genuinely needs a live session with no turn (a fresh empty session, a shell-only approval).
The owner counts turns from `~/.local/share/muse/sessions/*/session.jsonl`
(`grep -c runtime.user_intent.accepted`), not from your report.

## Billing finding this phase exists to guard against

Muse has two credential tiers, **Pay-as-you-go** and **Subscription**, decided from the login
token. The owner's login token from 2026-09-08 was on pay-as-you-go, so Phases 1–4 (~110
sessions, ~40 turns) were billed as API usage although `auth.json` said `mechanism: oauth`.
A logout and re-login on 2026-09-09 14:46 put the token on the "Muse Code High Usage" plan.
Nothing on the wire exposes the tier: `initialize` and `model/list` carry no account or plan
field, `auth.json` carries only mechanism/storage/obtained_via/api_base_url/name/email, and
the session logs record only `credential_backend`. The one oracle is the TUI: run `muse`
under a pty with no prompt, answer its cursor-position query (`\x1b[6n` → `\x1b[1;1R`),
type `/upgrade` and Enter, and the card reads either "You are currently subscribed to the
<name> plan. Current N% used · Resets at … Weekly N% used · Resets …" or "you're on
pay-as-you-go" / "Subscriptions aren't currently available for your account". A working
pty driver is in `fixtures/msp/tui_slash.py`; strip SGR and DEC sequences before matching.
Opening the TUI starts a session record but makes no model call.

## Read first, in this order

1. `docs/00-spec.md` §1, §3.2, §3.7, §3.9, §3.10, §4, §5 item 5, §6.
2. `docs/04-approvals.md` §0 and §6 (`--replay`), `docs/CHANGELOG.md` (Phase 4 entry
   in full), `docs/02-app.md` §4 (auth), `docs/03-composer.md` §5 (`/` commands), `docs/05-handoff.md`.
3. `agentic-ui/docs/00-agent-brief.md`, `agentic-ui/docs/04-design-rules.md`,
   `agentic-ui/.github/workflows/ci.yml`, `agentic-ui/docs/08-getting-started.md`.
4. `agentic-ui/docs/10-muse-research.md` §3 (auth), §4.3 (session naming, the index), §4.4
   (`~/.local/share/muse` on disk).
5. Code: `crates/harness/src/{auth.rs,app.rs,session.rs,sidebar.rs,index.rs,overlays.rs,
   transcript.rs,main.rs,history.rs}`, `crates/muse-adapter/src/fold.rs` (approval_resolved),
   `agentic-ui/crates/aui/src/nav/{sidebar.rs,session_row.rs}`, `agentic-ui/crates/aui/src/
   feedback/{banner.rs,toast.rs}`, `agentic-ui/crates/aui/src/overlay/dialog.rs`,
   `agentic-ui/crates/aui-motion/src/lib.rs` (the springs), `agentic-ui/crates/aui/src/keys.rs`.

## Decisions (do not re-litigate; record deviations in CHANGELOG)

### A1 — the billing guard. Do this FIRST and commit it on its own before anything else.

- `crates/harness/src/plan_tier.rs` (name it `tier.rs` if you prefer): `probe(muse: &str) ->
  Result<Tier, String>` drives the TUI in a pty on a background thread as described above,
  with a 20 s ceiling, and returns `Tier::Subscription { plan, current_pct, weekly_pct,
  resets: Vec<String> }`, `Tier::PayAsYouGo`, or `Tier::Unavailable(reason)`. Never log the
  raw TUI output. Cache the result in `~/Library/Application Support/harness/tier.json`
  keyed by the mtime of `~/.config/muse/auth.json`; re-probe when the mtime changes, after a
  login, and on `/usage`.
- Sidebar footer: the identity line gains a third row — "High Usage · 2% this week" or a
  warning-tinted "Pay-as-you-go" or "Plan unknown". `aui::nav::SidebarFooter` gets whatever
  slot it lacks (`.plan(text, warning: bool)`), gallery updated.
- **Pay-as-you-go blocks sending.** A warning banner over the composer ("This login is on
  pay-as-you-go: every turn bills API usage. Sign out and back in after subscribing, or send
  anyway.") with actions "Sign out" and "Send anyway"; the composer refuses to send until
  "Send anyway" is pressed once per app run. `Unavailable` shows a quieter banner with
  "Check again" and does not block. A tier probe that fails must never stop the app booting.
- `/usage` and `/status` show the plan, both percentages and the reset times.
- Scripting: `--tier subscription|payg|unknown` fakes the probe for screenshots; say so in
  the doc. Screenshots: `phase5-tier-payg-*`, `phase5-tier-plan-*` (footer), `phase5-tier-banner-*`.
- Write `docs/06-billing.md`: the two tiers, how the probe works, the cache, what the owner
  should check in Account Center, and the Phase 1–4 history in two sentences.

### A2 — session operations not on the wire (spec §3.7)

- `~/Library/Application Support/harness/sessions.json`: `{ "<session_id>": { "name":
  Option<String>, "hidden": bool } }`, written atomically.
- `/name <text>` renames the active session (empty text clears); the sidebar row's rename
  affordance (`aui::nav::session_row` gains a pencil action and an inline edit slot the app
  owns — same slot pattern as the composer's editor). The index's own `session_name`/`title`
  are shown when no override exists.
- **Titles for shell-only sessions (F10).** A session with no user prompt shows "New session"
  in Phase 4's screenshots, fourteen in a row. Derive the title in this order: sessions.json
  name → index `session_name` → index `title` → index `first_user_prompt` → the first
  `userShell` command text from a `session/read` of the head (cache it in sessions.json under
  `derived_title`) → "New session".
- Hide: a row action and `/hide`; the row disappears and a toast "Session hidden" with Undo
  (8 s) restores it. Hidden sessions never load. "Show hidden" is a sidebar footer toggle.
- Search: ⌘⇧F focuses a search field at the top of the sidebar (`aui::nav` gains
  `sidebar_search` — a field slot with clear button); filtering over name + title + first
  prompt + `search_text`, case-insensitive subsequence, highlighting not required. Esc clears
  and returns focus.
- `/resume` opens a picker (the command palette primitive) listing sessions with the same
  titles; Enter opens. Remove the "Not in this build yet" toast for `/name` and `/resume`.

### A3 — Phase 4 review findings

- **F9** `docs/images/phase4-approval-stage1-{light,dark}.png` were captured before the
  approval card arrived (they show only the shell card). Retake both via `--steps` on a
  fresh session with `shell:echo hi && ls;wait:1500` under `promptUnmatched` (free), and
  make the screenshot path wait for a pending approval when a `shell:` step was given.
- **F11** A policy-resolved approval card reads "Rule echo hi && ls": the fold uses
  `subject.command` as the rule (`fold.rs` `approval_resolved`). Use `amendment.rulePreview`
  when present, else the gated item's `failureReason`/`visibleOutput` reason
  ("deny_unmatched: no policy rule allows this action"), else the approval mode's name. The
  card's line becomes "Denied by policy · no policy rule allows this action". Snapshots
  regenerated; read the diff.
- Any `Block::Generic` still reached by a capture is a bug; check every snapshot.

### A4 — polish (spec §5 item 5)

- Motion: every enter/exit/morph goes through `aui_motion`'s springs; audit the transcript,
  the toasts, the banners, the dialogs, the queue strip and the pickers for anything that
  pops. Only the newest turn animates; a replayed transcript does not animate at all.
- Focus: focus rings on every actionable element under keyboard use (`aui::keys::track_pointer`
  keeps the window-wide flag honest — the app has its own root, so call the helper); tab
  order composer → queue strip → pending card → sidebar; Esc always returns to the composer;
  ⌘K palette lists every `/` command and the session operations.
- Empty states and first-run copy: no session (the workspace name, "⌘N to start"), a fresh
  session (three suggested prompts as chips, from a small list), no sessions match the
  search, hidden-only, logged-out. Copy in the library's voice (short, declarative).
- Window title = the session title; the sidebar rail (⌘B) keeps the account footer.
- Every string a person reads is reviewed once for "Claude" leftovers (`grep -rn Claude
  crates/`) — the harness speaks about Muse.

### A5 — docs and CI

- `README.md` (what it is, screenshot, how to run, the spend rule in one paragraph),
  `docs/07-architecture.md` (crates, entities, thread model, the fold, `--replay`; reuse the
  diagrams already in 01/02), `docs/08-keymap.md` (spec §3.9 as built, plus the card keys),
  `docs/05-handoff.md` rewritten as a maintenance handoff (the slice is complete).
- `.github/workflows/ci.yml` matching agentic-ui's: build, test, clippy `-D warnings`,
  rustdoc `-D warnings`, on macOS, with agentic-ui checked out beside it on `muse-support`
  and the path dependencies resolving. The ignored live tests stay ignored in CI.
- CHANGELOG Phase 5 entry; spend statement (zero turns, how you verified).

### A6 — gates and commits

Library: `cargo build --workspace`, all-features build (`--features aui-webview/wry,
aui-terminal/pty,aui-terminal/tui`), `cargo test --workspace`, clippy `--all-targets -D
warnings`, rustdoc `-D warnings`, `python3 scripts/api-doc.py`; gallery entries for the
footer plan row, the rename affordance, the sidebar search. Harness: build, test, clippy,
rustdoc, one `gpui-pre` and one `gpui-kit` in `cargo tree -d`, snapshots regenerated and
read. Commits: A1 first as its own harness commit (plus its library commit if the footer
slot needs one), then one commit per repo for the rest. Messages end with
`Co-Authored-By: Claude Fable 5.1 <noreply@anthropic.com>`. No commits on agentic-ui `main`.

## Deliverables

1. The commits above, both trees clean.
2. Screenshots `docs/images/phase5-*.png`, light and dark: tier footer (plan), tier banner
   (pay-as-you-go), rename in progress, hidden toast with Undo, search filtering, `/resume`
   picker, the four empty states, the retaken approval stage 1, the corrected policy card.
3. `docs/06-billing.md`, `README.md`, `docs/07-architecture.md`, `docs/08-keymap.md`,
   CHANGELOG, handoff.
4. Final report under 400 words: what the tier probe returned on this machine (plan name and
   percentages only), what round-trips and how verified, gate evidence, confirmation of zero
   turns, files touched, screenshot paths, anything not verified.
