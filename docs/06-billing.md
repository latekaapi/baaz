# Billing: the two tiers, and the guard

Phase 5 decision A1. What a turn costs is not decided by the provider you pick,
by the model, or by anything on the wire. It is decided by the **login token**,
and until this phase Baaz had no way to tell you which kind you were
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

Both look identical from inside Baaz. `auth.json` carries the mechanism
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
on a subscription plan; nothing else changed, and nothing in Baaz could
have told the difference before or after.

---

## 2. The one oracle: the TUI's `/upgrade` card

The `muse` TUI knows. `/upgrade` draws a card that reads either

> You are currently subscribed to the *Muse Code High Usage* usage plan.
> Current 0% used · Resets at 5:17 PM Weekly 2% used · Resets Sep 14 at 5:30 AM

or "you're on pay-as-you-go" / "Subscriptions aren't currently available for
your account".

So `crates/baaz/src/tier.rs` drives that card:

1. `openpty`, then `muse --workspace <probe dir> --trust-workspace` on the
   slave side with its own session and the slave as its controlling terminal
   — without which the launcher refuses to draw a TUI at all.
   `--trust-workspace` (added v0.1 prep task 2) matters as of muse 1.3.0:
   an untrusted `--workspace` now opens on a "Do you trust this workspace?"
   gate, and without the flag the probe's blind `/upgrade`-then-Enter
   keystrokes land on that gate instead — Enter accepts its default ("Trust
   and continue"), eating both the command text and the card, so the probe
   read a bare idle composer and reported "not recognised" on every run
   against a fresh `BAAZ_STATE_DIR`.
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
`~/Library/Application Support/baaz/tier-probe`, deliberately not the
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

### The wire is primary now; the scrape is the fallback that cannot be deleted

Since muse 1.3.0 the card's numbers are on the wire, typed, and Baaz reads
them first:

- `usage/read` is issued with every tier probe (`probe_tier`, on connect and
  on every asked-for re-probe). On `Some(usage)` made after the current
  credential was installed the tier is built directly from it — plan from
  `tier`, both percentages verbatim, reset clauses rendered from the two
  `resets_at_ms` stamps in the card's own register (`Resets at …` /
  `Resets <date> at …`, local time) — and cached through `tier::remember`
  exactly as a probe answer is. Anything older belongs to a previous login
  (see below) and falls through to the scrape.
- `usage/changed` is folded into the tier in place on every notification, so
  the sidebar footer meter tracks the window as the person works instead of
  freezing at a boot-time snapshot. The update reaches `push_tier`, so the
  banner follows too.

The scrape stays, demoted to fallback: on `{}` (a cold host, before any turn
has observed a provider response) or any read failure, the pty probe runs
unchanged. It cannot be deleted, because it answers the one question the wire
never does — **pay-as-you-go vs not known**. `usage/read` returns a
`SubscriptionUsage` when a subscription observation exists and nothing
otherwise; absence is "not known yet", never pay-as-you-go. Only the
`/upgrade` card distinguishes `Tier::PayAsYouGo` from `Tier::Unavailable`,
and `PayAsYouGo` is what raises the blocking banner that stops the person
being billed API rates by surprise. Deleting the scrape would silently turn
that protection into "Plan unknown", which does not block.

`observed_at_ms` stamps when the host received the observation, not now — and
the wire carries no account or credential identity, so "the last thing seen"
may belong to a previous login: signing out and back in reuses the same
`muse serve` connection, and `usage/read` keeps serving the previous
account's window. An observation is therefore trusted only when it was made
after the current credential was installed — `observed_at_ms` (epoch
milliseconds, converted down to whole seconds) strictly after `auth.json`'s
modification time in whole seconds (`tier::auth_mtime`), with no tolerance
for skew: both stamps come from this machine's clock, and the tie goes to
distrust (a false fall-through costs a scrape; a false trust bills the person
with no warning). Anything older falls through to the scrape, and a
`usage/changed` stamped before the installed credential never updates the
tier. `used_percent` may exceed 100 (over-quota is valid): the meter clamps
to `0..=1`, `/status` and `/usage` print the true number.

### The rule the module keeps

**The raw terminal output is never logged.** The card's footer carries a URL,
and a TUI that decided to show a login flow would carry a code. Only the parsed
plan name, the two percentages and the reset clauses ever leave the module.
`BAAZ_TIER_DEBUG=1` prints how many bytes were read and which of a fixed list
of harmless words appeared, and never the bytes.

---

## 3. The cache

`~/Library/Application Support/baaz/tier.json`, keyed by the modification
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

One probe runs at a time across Baaz processes. The probe takes
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
`account/read` reports the `apiKey` or `envKey` lane Baaz sets the tier
to pay-as-you-go directly — the TUI probe is about subscriptions and never
runs — and the footer reads "Pay-as-you-go · API key" without one. The
`/status` lines, the banner and the "Send anyway" guard are the same objects
as on any other pay-as-you-go login.

## 6. Checking it yourself

```bash
cargo run -p baaz -- --print-tier      # probe, print, exit; costs nothing
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
Baaz cannot see either.

### Scripting the states

`--tier subscription|payg|unknown` skips the probe and pretends it said that.
It fakes nothing else: the footer row, the banner and `/status` all read the
same `Tier` the real probe returns, so a screenshot of the guard is a screenshot
of the guard.

```bash
cargo run -p baaz -- --replay fixtures/msp/transcript-approve.jsonl \
  --tier payg --theme dark --screenshot /tmp/phase5-tier-payg-dark.png
```

| image | run |
|---|---|
| `phase5-tier-plan-{light,dark}` | `--tier subscription` — the footer's plan row, no banner |
| `phase5-tier-payg-{light,dark}` | `--tier payg` — the warning footer row and the blocking banner |
| `phase5-tier-banner-{light,dark}` | `--tier unknown` — the quiet banner and its "Check again" |

All six are `--replay` runs. They spend nothing.

---

## 7. Usage history (`baaz.db`)

Baaz keeps a ledger of finished turns in its own
`~/Library/Application Support/baaz/baaz.db` — one row per finished turn,
written when a turn's terminal event folds (and backfilled from `view/page`
when a session is opened, so turns that ran while Baaz was closed are still
recorded). This is the ledger; rendering it is a later stage.

**The key is the turn.** Each row is keyed on
`(session_id, turn_id)`, and every insert is `ON CONFLICT DO NOTHING`.

An earlier schema keyed on `(session_id, view_cursor)` — the cursor of the
turn's terminal event — and a real database proved that wrong: one session
came back as 16 rows over 16 distinct cursors but only 15 distinct turns,
because the two write paths disagree about which cursor keys a turn. The
live path (`SessionView::record_usage`) takes one cursor for a whole batch
of finished turns and stamps every one with it; the backfill path (the
`view/page` handler) walks events one at a time and stamps each turn with
its own event's cursor. Under the cursor key the `ON CONFLICT` clause never
fired for that turn and both rows landed, over-counting every `SUM` over the
ledger by one turn. `turn_id` is the identity both paths already agree on,
so keying on it makes the live write and the backfill safe to overlap no
matter which cursor each chose: backfilling a session that was already
recorded live adds only the new rows and changes none of the old.

**The cursor is still stored.** `view_cursor` remains a column on every row
— it says where the turn sat in the view — but it is data now, not
identity, and two rows never differ only by cursor.

**Schema version 2, migrated in place.** The re-key bumped the ledger's
`meta.schema_version` from 1 to 2. Opening a version-1 database rebuilds
`usage_turns` under the new key and carries every existing row over, with
one deliberate exception: a turn the old key stored twice under two cursors
collapses to a single row (the smallest cursor wins). No other history is
dropped.

**The migration is all-or-nothing.** The rename, rebuild, copy, drop and the
version stamp run inside a single SQLite transaction, so an interruption at
any statement boundary leaves either the untouched version-1 database or the
finished version-2 one — never a half-migrated file, and never an empty
version-2 stamped over orphaned history. A `usage_turns_v1` left beside the
live table by an interrupted attempt is finished on the next open rather than
erroring: its rows merge into `usage_turns` (the live table wins key
conflicts; the backup only fills gaps — nothing in either table is lost),
the backup is dropped, and the stamp lands in the same transaction.

**An unreadable `schema_version` is never taken for a fresh install.** When
the stamp is missing or does not parse and a `usage_turns` table already
exists, the table's real primary key decides: a version-1 key migrates, a
version-2 key is re-stamped, and anything else fails loudly with the stamp
untouched. Stamping a version-1 table as version-2 is what once left the
ledger silently dead — every later write failing on the untouched v1 key
while the file claimed health — and that outcome is now unreachable.

Crash coverage is at statement boundaries on scratch copies only: the tests
interrupt the migration after each of its four statements and reopen, which
proves the all-or-nothing shape above — not a real mid-write process kill,
and not two Baaz windows racing `open_at` on one file. Never exercised
against a real `baaz.db`.

**`NULL` is not zero.** `cache_read_tokens` and `cache_write_tokens` are
nullable, and a `NULL` means "the provider never told us" — the whole reason
`TurnMeta` keeps them as `Option`. A reporting query may `COALESCE` them to
zero, but the write never flattens the distinction: unknown stays unknown.

**Baaz does not own the transcript.** Muse stays the system of record; the
ledger holds token counts, durations and cost beside it, never the words.
A locked database, a missing directory and a schema this build has never seen
are all ordinary: the write logs one line and carries on, and nothing the
person does ever blocks on it.

The one index is on `finished_at_ms`, which is what a "last N days" query
ranges over. Backfilled rows carry the time they were recorded, not the time
the turn ran — a page carries a turn's duration but no wall-clock for its
terminal, so the recording time is the honest stamp.

---

## The migration tie-break (2026-09-24)

When the ledger was re-keyed from `(session_id, view_cursor)` to
`(session_id, turn_id)`, a turn the old key had stored twice had to collapse to
one row. The original rule was `ORDER BY session_id, view_cursor` — **the
lexicographically smallest cursor won.**

That was unsafe, and it is now replaced by an ordering on the data:

    ORDER BY session_id, turn_id, (tokens_in = 0) ASC, finished_at_ms DESC, view_cursor ASC

1. a row with non-zero `tokens_in` beats a zero one — a zero row is a write that
   never learned the figure;
2. then the later `finished_at_ms` — chronology, if both carry real figures;
3. then `view_cursor`, only for determinism. It decides nothing on its own.

**Why the old rule was unsafe.** `view_cursor` is an opaque, server-issued
string. Lexicographic order means something only while both cursors share a
digit width — `:9` sorts *after* `:10`. It is also unrelated to chronology and
unrelated to which write was live rather than backfill.

**And it mattered.** The owner's real database held two duplicate pairs, and the
second was not a harmless twin: **92,278 tokens at cursor `:69` against 0 at
`:76`.** Which copy survived decided whether the ledger lost 22,633 tokens or
114,911. It kept `:69` and went the right way — by luck, not by design.

Four tests in `usage.rs` pin this, built from that exact pair. Three of them
fail against the old ordering; the fourth is the real orientation, which the old
rule also satisfied and which must keep passing.

