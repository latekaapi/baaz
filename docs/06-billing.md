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

### A real incident during early development

An early login token was on pay-as-you-go, so roughly 110 sessions and 40 turns
across early development were billed as API usage while the project's own docs
described them as subscription turns. A logout and a fresh login put the token
on a subscription plan; nothing else changed, and nothing in the harness could
have told the difference before or after.

---

## 2. The one oracle: the TUI's `/upgrade` card

The `muse` TUI knows. `/upgrade` draws a card that reads either

> You are currently subscribed to the *Muse Code High Usage* usage plan.
> Current 0% used · Resets at 5:17 PM Weekly 2% used · Resets Sep 14 at 5:30 AM

or "you're on pay-as-you-go" / "Subscriptions aren't currently available for
your account".

So `crates/harness/src/tier.rs` drives that card:

1. `openpty`, then `muse --workspace <probe dir> --trust-workspace` on the
   slave side with its own session and the slave as its controlling terminal
   — without which the launcher refuses to draw a TUI at all.
   `--trust-workspace` (added v0.1 prep task 2) matters as of muse 1.3.0:
   an untrusted `--workspace` now opens on a "Do you trust this workspace?"
   gate, and without the flag the probe's blind `/upgrade`-then-Enter
   keystrokes land on that gate instead — Enter accepts its default ("Trust
   and continue"), eating both the command text and the card, so the probe
   read a bare idle composer and reported "not recognised" on every run
   against a fresh `HARNESS_STATE_DIR`.
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
   turned into spaces, whitespace collapsed), and parse — then keep reading
   while the parse is a plan without its percentages (see below). Ceiling for
   the whole probe: 20 s.
6. Two `Ctrl-C`s to ask the TUI to leave; the `Drop` kills it if it declines.

Opening the TUI **writes a session record and makes no model call**, so the
probe costs nothing. It runs in a throwaway workspace under
`~/Library/Application Support/harness/tier-probe`, deliberately not the
window's own, so probe sessions never appear in the sidebar of a real project.
The probe's child is SIGKILLed on every exit — the TUI ignores SIGTERM, which
once left two orphans alive for six hours. Its pid is written to
`tier-probe/probe.pid` so the next probe can sweep a child orphaned by a
force-quit (the sweep checks the command line still names the probe workspace,
never the pid alone). Closing the window or quitting mid-probe runs the same kill plus a bounded 3 s wait, so the interactive exit no longer orphans the child either. The next-probe sweep stays as the backstop for force-quits, which run no exit hook at all.

### Three things the card does that a reasonable person would not expect

- Its sentence is `subscribed to the {plan} usage plan.`, so the plan's name
  arrives glued to the template's own word. `Muse Code High Usage usage` is a
  sentence; `Muse Code High Usage` is the plan, and the parser strips the
  suffix.
- The card's footer follows the second reset with no separator
  (`… 5:30 AM as of 3:27 PM Manage your plan in Account Center <url>`), so the
  reset clauses are cut at `as of` and `Manage` as well as at `·`.
- Under muse 1.2.1 the plan sentence draws **before** the usage windows: an
  early read finds the plan with neither percentage, and accepting it printed
  `Current — / Weekly —` while the card still had ink to lay. A subscription
  missing either percentage reads as "not yet", not as an answer — the probe
  keeps reading while time remains, and only on the deadline settles for the
  plan without a meter. The verbatim 1.2.1 card (URL redacted) is pinned by
  `tier::tests::the_1_2_1_card_parses_with_its_percentages`.
- Under muse 1.3.0, a login whose quota is fully spent draws `Usage currently
  unavailable` in place of both percentages — the plan sentence still names
  the plan, but neither window ever gets a number. This is the card's own
  final word, not a partial draw, so `Tier::Subscription` carries a
  `usage_unavailable` flag the parser sets from that exact phrase, and the
  probe stops waiting for numbers that are not coming rather than spending
  its whole ceiling on them. `/status` and `--print-tier` print "unavailable"
  in place of a percent, never a bare dash pretending nothing was said. The
  verbatim 1.3.0 exhausted card (URL is muse's own public account-center
  link, nothing sensitive) is pinned by
  `tier::tests::the_1_3_0_exhausted_card_names_the_plan_with_no_percentages`.

The first two are pinned by `tier::tests::the_card_this_muse_really_draws_parses`.

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
having to remember to. An entry older than an hour is ignored the same way:
an ordinary boot inside the hour reuses the cache instead of re-driving the
TUI. Beyond that the probe re-runs when the person asks: `/usage`, `/status`,
and the unknown-plan banner's "Check again" always re-probe.

One probe runs at a time across harnesses. The probe takes
`tier-probe/probe.lock` (its pid inside) before opening the TUI; a second
window waits up to 30 s, then reuses the winner's fresh cache when it landed
instead of driving a second TUI at the same workspace, and proceeds without
the lock past the wait rather than failing. A lock whose holder died — or
which outlived any probe by a ceiling and a grace — is stolen; the holder
removes only its own entry. The next-probe sweep of `probe.pid` stays as the
backstop behind it.

Writes go through `crate::store::write_atomic` — a sibling file and a rename —
so a crash mid-write leaves the previous answer rather than half of the next.

---

## 4. What the app does about it

| tier | sidebar footer, name row (the plan sits beside the name) | banner over the composer | sending |
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

While a probe runs, the unknown-plan banner's button reads **Checking…** and
pressing it again is a no-op (a second probe is refused while the first runs).
An asked-for probe toasts its result — "Plan: *Muse Code Power Usage*" or
"Still unknown: *reason*" — and a known subscription clears its own banner.

**A probe that fails never stops the app.** Every failure — no `muse` on the
path, no pseudo-terminal, a card that never arrived — becomes
`Tier::Unavailable`, which draws the quiet banner and blocks nothing.

---

## 5. The API-key lane

A stored API key or `META_API_KEY` is pay-as-you-go by construction: there is
no subscription it could draw on, so there is nothing to probe. When
`account/read` reports the `apiKey` or `envKey` lane the harness sets the tier
to pay-as-you-go directly — the TUI probe is about subscriptions and never
runs — and the footer reads "Pay-as-you-go · API key" without one. The
`/status` lines, the banner and the "Send anyway" guard are the same objects
as on any other pay-as-you-go login.

## 6. Checking it yourself

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

**In Account Center**, check that the subscription is the one you expect and
that no API-usage line is still accruing from a pay-as-you-go period. The
harness cannot see either.

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
