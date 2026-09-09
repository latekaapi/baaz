# Billing: the two tiers, and the guard

Phase 5 decision A1. What a turn costs is not decided by the provider you pick,
by the model, or by anything on the wire. It is decided by the **login token**,
and until this phase the harness had no way to tell you which kind you were
holding.

Read `docs/04-approvals.md` §0 first: it is the other half of this, and it says
what makes a model call at all.

---

## 1. The two tiers

Muse issues credentials on one of two tiers.

| tier | what a turn costs |
|---|---|
| **Subscription** | drawn from the plan's current and weekly windows |
| **Pay-as-you-go** | billed as API usage, per turn, per token |

Both look identical from inside the harness. `auth.json` carries the mechanism
(`oauth`), the storage, the API base and the person's name and email, and says
nothing about entitlement. `initialize` carries no account object. `model/list`
carries no plan field. The session log records a `credential_backend` and not a
tier. A pay-as-you-go token signs in, lists models, starts sessions and answers
turns exactly as a subscribed one does — it simply sends the bill somewhere
else.

### What happened in Phases 1–4

The owner's login token, taken on 2026-09-08, was on pay-as-you-go, so roughly
110 sessions and 40 turns across Phases 1 to 4 were billed as API usage while
every document in this repository — including this one's predecessors — described
them as subscription turns. A logout and a fresh login on 2026-09-09 14:46 put
the token on the **Muse Code High Usage** plan; nothing else changed, and nothing
in the harness could have told the difference before or after.

---

## 2. The one oracle: the TUI's `/upgrade` card

The `muse` TUI knows. `/upgrade` draws a card that reads either

> You are currently subscribed to the *Muse Code High Usage* usage plan.
> Current 0% used · Resets at 5:17 PM Weekly 2% used · Resets Sep 14 at 5:30 AM

or "you're on pay-as-you-go" / "Subscriptions aren't currently available for
your account".

So `crates/harness/src/tier.rs` drives that card:

1. `openpty`, then `muse --workspace <probe dir>` on the slave side with its own
   session and the slave as its controlling terminal — without which the
   launcher refuses to draw a TUI at all.
2. **Answer the cursor-position query.** The TUI opens with `ESC [ 6 n` and
   paints nothing until something replies; the driver answers `ESC [ 1;1 R`
   every time it sees one.
3. Let it settle (4 s), type `/upgrade`, wait for the palette (0.7 s), press
   Enter.
4. **Throw away everything read so far.** The slash palette's own row for
   `/upgrade` contains the phrase "pay-as-you-go", so a matcher that ran over
   the palette would report the opposite of the truth on a subscribed account.
   Only what the card draws after the Enter is parsed.
5. Read for 6 s, flatten (escapes dropped, box drawing and control characters
   turned into spaces, whitespace collapsed), and parse once. Ceiling for the
   whole probe: 20 s.
6. Two `Ctrl-C`s to ask the TUI to leave; the `Drop` kills it if it declines.

Opening the TUI **writes a session record and makes no model call**, so the
probe costs nothing. It runs in a throwaway workspace under
`~/Library/Application Support/harness/tier-probe`, deliberately not the
window's own, so probe sessions never appear in the sidebar of a real project.
The probe's child is SIGKILLed on every exit — the TUI ignores SIGTERM, which
once left two orphans alive for six hours. Its pid is written to
`tier-probe/probe.pid` so the next probe can sweep a child orphaned by a
force-quit (the sweep checks the command line still names the probe workspace,
never the pid alone).

### Two things the card does that a reasonable person would not expect

- Its sentence is `subscribed to the {plan} usage plan.`, so the plan's name
  arrives glued to the template's own word. `Muse Code High Usage usage` is a
  sentence; `Muse Code High Usage` is the plan, and the parser strips the
  suffix.
- The card's footer follows the second reset with no separator
  (`… 5:30 AM as of 3:27 PM Manage your plan in Account Center <url>`), so the
  reset clauses are cut at `as of` and `Manage` as well as at `·`.

Both are pinned by `tier::tests::the_card_this_muse_really_draws_parses`.

### The rule the module keeps

**The raw terminal output is never logged.** The card's footer carries a URL,
and a TUI that decided to show a login flow would carry a code. Only the parsed
plan name, the two percentages and the reset clauses ever leave the module.
`HARNESS_TIER_DEBUG=1` prints how many bytes were read and which of a fixed list
of harmless words appeared, and never the bytes.

---

## 3. The cache

`~/Library/Application Support/harness/tier.json`, keyed by the modification
time of `auth.json` in whole seconds:

```json
{ "authMtime": 1757423160, "tier": { "tier": "subscription",
  "plan": "Muse Code High Usage", "currentPct": 0, "weeklyPct": 2,
  "resets": ["Resets at 5:17 PM", "Resets Sep 14 at 5:30 AM"] } }
```

A cached entry whose `authMtime` no longer matches the `auth.json` on disk is
ignored, which is what makes a logout and a re-login re-probe without anyone
having to remember to. Beyond that the probe re-runs when the person asks:
`/usage`, `/status`, and the unknown-plan banner's "Check again".

Writes go through `crate::store::write_atomic` — a sibling file and a rename —
so a crash mid-write leaves the previous answer rather than half of the next.

---

## 4. What the app does about it

| tier | sidebar footer, third row | banner over the composer | sending |
|---|---|---|---|
| Subscription | `Muse Code High Usage · 2% this week`, quiet ink | none | normal |
| Pay-as-you-go | `Pay-as-you-go`, **warning ink** | warning: "This login is on pay-as-you-go: every turn bills API usage. Sign out and back in after subscribing, or send anyway." · **Sign out** · **Send anyway** | **refused** until "Send anyway" |
| Unknown | `Plan unknown`, warning ink | info: "Muse did not say which plan this login is on…" · **Check again** | normal |

The refusal lives in `SessionView::submit`, which is the single funnel every
send reaches — the composer's Enter, `--send`, and a `send:` step alike. A
refused draft goes straight back into the composer; nothing is lost and nothing
is queued behind the person's back. "Send anyway" lasts **one app run**: someone
who accepted the bill this morning is asked again tomorrow.

`/status` and `/usage` lead with the plan, both percentages and the reset times,
above everything the session knows about itself.

**A probe that fails never stops the app.** Every failure — no `muse` on the
path, no pseudo-terminal, a card that never arrived — becomes
`Tier::Unavailable`, which draws the quiet banner and blocks nothing.

---

## 5. Checking it yourself

```bash
cargo run -p harness -- --print-tier      # probe, print, exit; costs nothing
```

```
Subscription: Muse Code High Usage
Current: 0% used
Weekly: 2% used
Resets at 5:17 PM
Resets Sep 14 at 5:30 AM
```

It refreshes the same cache the window reads, and exits non-zero when the plan
could not be determined.

**In Account Center**, the owner should check that the subscription is the one
they expect and that no API-usage line is still accruing from the pay-as-you-go
period. The harness cannot see either.

### Scripting the states

`--tier subscription|payg|unknown` skips the probe and pretends it said that.
It fakes nothing else: the footer row, the banner and `/status` all read the
same `Tier` the real probe returns, so a screenshot of the guard is a screenshot
of the guard.

```bash
cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl \
  --tier payg --theme dark --screenshot docs/images/phase5-tier-payg-dark.png
```

| image | run |
|---|---|
| `phase5-tier-plan-{light,dark}` | `--tier subscription` — the footer's plan row, no banner |
| `phase5-tier-payg-{light,dark}` | `--tier payg` — the warning footer row and the blocking banner |
| `phase5-tier-banner-{light,dark}` | `--tier unknown` — the quiet banner and its "Check again" |

All six are `--replay` runs. They spend nothing.
