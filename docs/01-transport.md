# Transport and fold — what the wire actually does

Phase 1 of the spec (`docs/00-spec.md` §5). Two crates, no UI:

```
crates/muse-client    the `muse serve` child, NDJSON JSON-RPC, typed MSP schema
crates/muse-adapter   MuseFold: MSP view events -> aui_protocol::Delta + SideState
```

The research doc (`../agentic-ui/docs/10-muse-research.md`) is the reference for
*what* MSP is. This document records what building against it taught us: the
framing rules, the ordering rule, the reconnect procedure, the places where the
captures contradict the schema, and how to run the probe and the live test.

---

## 1. Framing

`muse serve` speaks **JSON-RPC 2.0 as newline-delimited JSON over stdio**. One
compact JSON value per line, no `Content-Length`, no `\r\n`. `stderr` carries no
protocol.

- **Write** `serde_json::to_string(&frame)` followed by `'\n'`. Never pretty
  JSON — a newline inside a frame ends it.
- **Read** with `BufReader::lines()`. Every server frame is exactly one line.
- **`params` is omitted entirely when it is empty**, never sent as `null`.
  `frame::request_line` enforces this.
- **Results are always objects**, possibly `{}`, never a bare scalar. That is
  what lets every result grow additive-optional members, and it is why
  `MuseClient::request` returns a `Value` that is always an object.

### Classification is on shape, not on the id

Both directions send requests. The server issues `approval/request` and
`userInput/request` as real JSON-RPC requests carrying **its own** id, from an id
space that has nothing to do with ours. So a line is classified like this:

| `id` | `method` | `result` / `error` | frame |
|---|---|---|---|
| yes | yes | — | `ServerRequest` |
| yes | no | `result` | `Response` |
| yes or `null` | no | `error` | `ErrorResponse` |
| no | yes | — | `Notification` |

A server request is **never** answered with a JSON-RPC result. It is settled with
`approval/decide` or `userInput/answer|cancel|clarify`. Because the same payload
also arrives as the sibling `…/requested` notification, the fold de-duplicates on
`approvalId` / `userInputId` and folds it exactly once.

`id: null` appears only on an unrecoverable parse error. No request owns it, so
the client surfaces it as an event rather than settling a waiter.

### Command ids

Every command carries a **client-minted UUIDv7** (`new_command_id()`). This is
enforced — a v4 uuid comes back as `invalidParams`. The id matters beyond
idempotency:

- for a fresh `turn/start`, `turnId == commandId`;
- `turn/unqueued` and `turn/retracted` echo the `commandId` back, and that is the
  **only** handle the client has for restoring the prompt text. The fold keeps
  `commandId → composer text` in `SideState::command_text` for exactly this.

---

## 2. The ordering rule

**Ack ≠ first, and ack ≠ outcome.**

View events for a command can arrive *before* its response. This is not a race we
tolerate, it is the observed norm: in `transcript-echo.jsonl` the
`session/branchChanged` for a turn lands before the `turn/start` result, and in
`transcript-echo-try.jsonl` so does the whole first half of the turn.

So:

- **Nothing gates folding on an ack.** The fold consumes notifications in wire
  order and never waits for a request to settle.
- **The ack is admission, not outcome.** `status: "accepted"` means the command
  was admitted. The authority is always the view event: `turn/completed`,
  `approval/resolved`, `userInput/settled`, `turn/unqueued`.
- The one thing the ack *is* authoritative for is identity —
  `TurnStartResult.turnId`. Never derive a turn id.

Ordering within the stream is `viewCursor`: opaque, strictly monotonic, observed
as `v:<sessionId>:<n>`. **Do not parse it.** The client compares cursors only for
equality (gap-fill overlap) and stores the last one for reconnects.

---

## 3. `view/gap` and the reconnect procedure

### `view/gap`

`view/gap {sessionId, after, next}` says the transcript has a hole and delivery
continues at `next`. `muse-client` performs the sanctioned splice-fill itself,
because event-stream completeness is a transport concern:

1. Park every live event for that session.
2. `view/page {sessionId, cursor: after, limit: 1000, direction: "forward"}`,
   paging on `nextCursor` until it runs out.
3. Emit the paged events, remembering their cursors.
4. Release the parked events, dropping any whose cursor the page already covered.

The page request runs on its own thread: the reader thread is what completes it,
so issuing it inline would deadlock.

The fold additionally draws a `MarkerKind::ViewGap` row, so the person sees that
something was missed rather than a silent discontinuity.

Note `view/page` **never replays `item/delta`** — a backfilled `agentMessage`
arrives whole on its `item/completed`. The fold therefore has to accept an
`item/completed` for an `itemId` it never saw start, and it does.

### Reconnect

On child exit the reader emits `MuseEvent::Closed(code)` and fails every in-flight
request with `MuseError::Closed`, so nothing waits on a dead pipe. The app then:

1. respawns `muse serve` with the same flags;
2. `initialize` + `initialized` (comparing the schema fingerprint, warning only);
3. for each open session, `session/resume { sessionId, cursor: <last observed
   viewCursor>, history: "auto" }`. A cursor suffix serves
   `history.mode: "none"` and only the suffix streams;
4. `approval/listPending` to recover anything that was awaiting a decision — a
   resume also re-issues those as server requests, and both paths de-duplicate on
   the same ids.

`SideState::last_cursor` is the value step 3 needs.

---

## 4. Where the captures contradict the schema or the research doc

Per spec §0: when the research doc and a capture disagree, the capture wins; when
the schema and a capture disagree, the capture wins and it is written down.

1. **An approval's `itemId` is the approval's own id, not the gated item's.**
   The research doc §7.2 suggests joining an approval to the item it gates
   through `itemId`. In `transcript-approve.jsonl` and `transcript-wire.jsonl`
   the `approval/requested` reports `approvalId == taskId == itemId ==
   66de002c-…`, while the `userShell` item it gates is `caec247d-…`. The join
   that *does* work is `toolCall.approvalId` (model tool calls only); a
   `session/userShell` item has no back-pointer at all.

2. **An approval's `turnId` for a user shell is the shell command's
   `commandId`.** The `userShell` item carries `turnId: null` — it is the one
   kind outside a turn — but the approval it raises reports
   `turnId: 01a081ef-25bf-…`, which is that item's `commandId`. The fold relies
   on this: a `userShell` item is filed under its own `commandId`, so the shell
   card and its approval land in the same turn. Nothing in `msp.d.ts` states this
   identity.

3. **`onRequest` is not "ask for everything".** `transcript-real.jsonl` runs a
   model-issued `ls` under `onRequest` and **no `approval/*` fires at all**. The
   approval captures come from `session/userShell` under `promptUnmatched`
   and `denyUnmatched`, which makes no model call and so spends nothing.

4. **A billed reasoning budget does not imply a `reasoning` item.**
   `transcript-real.jsonl` reports `reasoningTokens: 171` and emits no `reasoning`
   item whatsoever. A reasoning UI must tolerate "thought for N tokens, no text".

5. **`session/started` and the `…/request` server requests are not in the
   published method index.** `MspNotification` omits `session/started`; `MspMethod`
   omits `approval/request` and `userInput/request`. All three are on the wire.
   The index is not exhaustive and is not treated as such.

6. **~~`ReasoningEffort` has `ultra` and no `max`~~ — retired by muse 1.1.1;
   ~~the picker omits `max`~~ — retired: the picker now offers it.**
   Under 1.0.3 the MSP enum had `ultra` and no `max` while the CLI and the
   on-disk catalog both advertised `max`, so sending `max` was `invalidParams`.
   The 1.1.1 schema adds `max` between `xhigh` and `ultra`
   (`docs/10-msp-1.1.1-diff.md`), so the wire accepts what the CLI and the
   catalog advertise — and the effort picker now offers the whole enum:
   the library's `aui_protocol::ReasoningEffort` gained `Max`, the picker's
   tiers list it between `xhigh` and `ultra`, and the wire map sends
   `Wire::Max`.

7. **`cost` is `null` on every catalog row** on this subscription, and
   `model/list` ignores `providerId` — an echo session is still served the meta
   catalog. `TurnMeta::cost_usd` is therefore always `0.0` in phase 1.

8. **`muse serve --no-session-log` emits no view events.** This one contradicts
   the spec, which names `--no-session-log` as the flag tests should use. Under
   an ephemeral host a `turn/start` is *accepted* — `disposition: "started"`,
   a real `turnId` — and then `session/started` is the **only** notification
   that ever arrives. No `turn/started`, no items, no `session/tokenUsage`, no
   `turn/completed`, ever. Verified twice, once through `muse-client` and once
   through the reference Python probe in `fixtures/msp/probe.py`, so it is the
   server's behaviour and not this client's:

   ```
   ['--trust-workspace', '--no-session-log'] durability: ephemeral
     turn: started
     notifications: ['session/started']
   ['--trust-workspace']                     durability: durable
     turn: started
     notifications: ['session/started', 'turn/started', 'item/completed',
                     'item/started', 'item/delta', 'item/delta',
                     'item/completed', 'session/tokenUsage',
                     'session/contextUsage', 'turn/completed']
   ```

   So **anything that needs a transcript runs durable**: the probe and
   `live_echo` both do, and both leave a real (tiny, echo-provider) session in
   `~/.local/share/muse/sessions`. `MuseConfig::no_session_log` still exists,
   with the finding written on it; `HARNESS_PROBE_EPHEMERAL=1` makes the probe
   reproduce the silence.

9. **Five fields the schema calls optional are always on the wire, sometimes as
   `null`.** `msp.d.ts` declares them `foo?: X` (absent when unset), but the
   captures carry the key on every frame. They are modelled as
   *required-nullable* — `Option<T>` that still serializes as `null` — because
   `skip_serializing_if` would drop the key and break the round-trip test:

   | field | evidence |
   |---|---|
   | `Item.turnId` | present on 12/12 `item/completed`, 8/8 `item/started`, 1/1 `item/updated`, 3/3 `view/page` events and 2/2 `session/read` history items; explicitly `null` on every `userShell` |
   | `SessionTokenUsageParams.modelId` | present 5/5, `null` on unpriced legs |
   | `SessionBranchChangedParams.branch` | present 7/7; `null` is a detached-HEAD *fact* |
   | `SessionModelChangedParams.providerId` | not exercised; the doc says `null` means "the selection names none" |
   | `SessionGoalChangedParams.goal` | not exercised; the doc says `null` **clears** — dropping the key would mean "unchanged", the opposite |

   `UnframedViewNotificationParams` also needed a `#[serde(flatten)]` catch-all:
   the schema declares three members and says the type stays open, and without it
   every `view/page` event loses its payload.

10. **A `Turn::User` needs the item id, not the turn id.** `turnId == commandId`
   for a fresh turn, so using the MSP `turnId` for both the user turn and the
   assistant turn would give two turns the same id and
   `Session::turn_mut` would find the wrong one. The user turn is keyed on the
   `userMessage` **item id**.

---

## 5. What the fold does with the shape mismatch

MSP models the person's message as an *item inside* a turn. `aui_protocol` models
it as its own `Turn::User`. So one MSP `turnId` folds into up to two library
turns:

- the `userMessage` item becomes a `Turn::User` whose **id is the item id**;
- every other item becomes a block of a `Turn::Assistant` whose **id is the MSP
  `turnId`**.

The assistant turn is created **lazily, by its first block** rather than on
`turn/started`, because `turn/started` arrives *before* the `userMessage` item and
creating it eagerly would put the reply above the prompt.

Markers, the todo list and the goal card all need a host turn (they are blocks,
not turns), so a session-level fact lands on the newest assistant turn, or on a
synthetic `session:<id>` turn when the transcript has none yet.

`request_user_input` tool calls render as their question card and **not** as a
tool card: the `toolCall` item and the `userInput/*` request share an id, so the
question is the better rendering of the same fact.

Unknown item kinds get `Block::Generic { kind, status, text }` — the rendering MSP
mandates. Across all five captures nothing reaches it.

---

## 6. Running things

Every command needs the project's PATH prefix:

```sh
export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"
```

### The probe

Spawns `muse serve --trust-workspace`, initializes, starts an **echo** session
in a temp workspace, lists models, sends one turn, and prints every folded
`Delta` as JSON on stdout (progress goes to stderr):

```sh
cargo run -p muse-adapter --bin harness-probe
cargo run -p muse-adapter --bin harness-probe -- "say something else"
```

### The tests

```sh
cargo test -p muse-client -p muse-adapter          # replay + snapshots, no child process
cargo test -p muse-client -- --ignored live_echo   # one real echo turn
UPDATE_SNAPSHOTS=1 cargo test -p muse-adapter      # regenerate the fold snapshots
```

`live_echo` lives in `muse-client` because that is where the spec's gate command
points, and reaches the fold through a **dev-dependency** on `muse-adapter`.
Cargo permits that cycle precisely because it is dev-only: the `muse-client`
library target still has no path to `aui`.

Snapshots live in `crates/muse-adapter/tests/snapshots/<capture>.json`. A changed
snapshot is a changed transcript — read the diff.

### The gates

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
RUSTDOCFLAGS=-D warnings cargo doc --workspace --no-deps
```

### Spend

**Correction (phase 4). `echo` is not a free provider.** Phase 1 recorded that
it was, and research §2.4 says the same; both are wrong on a machine that is
signed in. Read the truth out of the session log:

* `~/.local/share/muse/sessions/<y>/<m>/<d>/<id>/session.jsonl` opens with a
  `command_intake` record carrying `provider_id: echo` — the route that was
  asked for.
* A later **metadata** record in the same file names what actually served the
  turn: `provider_id: meta`, `model_id: muse-spark-1.3-contributor`.
* `~/.local/share/muse/session-index.db` follows the *metadata* record, not the
  intake, so the index reports `meta` for a session started as `echo`.

Turns routed through `echo` bill reasoning tokens (`fixtures/msp/transcript-echo.jsonl`
carries a `session/tokenUsage` with `reasoningTokens: 94`), carry provider
response ids, and answer with varied real text rather than one canned line.
`--provider` picks a route, not a bill: **every turn on every provider is a real
subscription turn**, and the cap of five per phase covers all of them.

What actually costs nothing:

| free | why |
|---|---|
| `--replay <capture.jsonl>` | folds a checked-in capture; no server, no session, no turn |
| `--no-connect` | draws the chrome without spawning `muse serve` |
| `session/start` | opens a session; no model call |
| `session/userShell` (the `!` path) and the whole approval flow it raises | no model call |
| `session/fork`, `approval/*`, `session/list`, `view/page` | no model call |

Anything that reaches `turn/start` — including `--send` and a `--steps` list
containing `send:` or `steer:` — spends a turn.
