# Architecture

Three crates, two entities, one fold, and one rule about threads. Everything
here is already described somewhere else in more detail; this is the map.

---

## 1. The crates

```
                    ┌──────────────────────────────────────────┐
   muse serve ─────►│ muse-client                              │
   (child, NDJSON)  │   frame.rs   one JSON value per line     │
                    │   schema.rs  every MSP shape, typed      │
                    │   client.rs  reader + writer threads,    │
                    │              blocking requests, events   │
                    └───────────────┬──────────────────────────┘
                                    │ MuseEvent
                    ┌───────────────▼──────────────────────────┐
                    │ muse-adapter                             │
                    │   fold.rs    MuseFold: MSP → aui deltas  │
                    │   side.rs    SideState: what aui has no  │
                    │              room for (requirementIds,   │
                    │              pending approvals, queue)   │
                    │   failure.rs humanized turn failures     │
                    └───────────────┬──────────────────────────┘
                                    │ aui_protocol::{Session, Delta}
                    ┌───────────────▼──────────────────────────┐
                    │ harness (gpui)                           │
                    │   app.rs      the window, sessions, shell│
                    │   session.rs  one open session           │
                    │   login.rs    the login screen, account/*│
                    │   steps.rs    --steps / --login-steps    │
                    │   wire.rs     call, then update          │
                    │   + tier, store, sessions, index,        │
                    │     overlays, transcript, sidebar, shot  │
                    └──────────────────────────────────────────┘
                                    │
                              aui, aui-protocol, aui-motion,
                              aui-tokens, aui-icons  (path deps)
```

`muse-client` knows the wire and nothing about the UI. `muse-adapter` knows
both protocols and nothing about gpui — it has no gpui dependency at all, which
is what lets `crates/muse-adapter/tests/fixtures.rs` replay every checked-in
capture with no window. `harness` knows gpui and the library.

## 2. The entities

Two gpui entities own state, and a third owns everything that floats.

| entity | file | owns |
|---|---|---|
| `Harness` | `app.rs` | the one `MuseClient`, the wire account state and the login flow, the session list, the billing tier, the sidebar's search and rename fields, the active session |

Three of `Harness`'s concerns keep their fields there but live in their own
modules, each reached through one call per seam (C1, 2026-09-12):

| module | owns |
|---|---|
| `login.rs` | the login screen's state, the `account/*` notifications, the device-code and API-key flows, sign-out, and `render_login` (`docs/diagnosis/login.md` §4, D22–D28) |
| `steps.rs` | the whole scripting surface: one parser, one verb table per scope (window, session, login), the two runners, and the cost notes |
| `wire.rs` | `WireCall`: run a blocking request on the background executor, then return through `update` / `update_in` — the shape every wire call in the app has |
| `SessionView` | `session.rs` | one Muse session: its `MuseFold`, the composer draft, the scroll position, the folded cards, the running turn, the pending questions' clocks |
| `Overlays` | `overlays.rs` | **state only**: the modal, the open menu and its selection, the palette, the toasts, and the two lists the menus are built from |

`Overlays` holds no elements. The dialog, the palette and the toast stack are
rendered by `Harness`; the composer's chip menus and caret popovers are rendered
by `SessionView`, because a picker is anchored to the chip that opened it and
only the composer knows where its chips are. Keeping the *state* in one place is
what makes "Escape closes whatever is open, in order" a function rather than a
negotiation between two views.

## 3. Threads

```
  ┌── muse-client's reader thread ── NDJSON in ──┐
  │                                              │  crossbeam channel
  │   muse serve (child process)                 │       ↓
  │                                              │  one bridging thread
  └── muse-client's writer thread ── NDJSON out ─┘       ↓
                    ▲                            futures::mpsc
                    │                                    ↓
              background_spawn                    one foreground task
            (every request blocks)                       ↓
                    │                            SessionView::apply
              ┌─────┴───────────────────────────────────┴─────┐
              │              the UI thread                    │
              └───────────────────────────────────────────────┘
```

Two directions cross the boundary, and each has exactly one shape.

- **Events in.** `conn::connect` returns a `futures` receiver fed by one
  bridging thread. A single foreground task drains it and calls
  `SessionView::apply`, so folding happens **on the UI thread in wire order**
  and every frame renders a consistent transcript. `apply` notifies only when
  the fold changed or view state changed (2026-09-10): unchanged streaming
  deltas used to rebuild the whole transcript per chunk, and the turn ticker
  with them at 250 ms. The ticker is 1 Hz now and notifies only when the
  displayed second changes.
- **Commands out.** Every `muse-client` request blocks, so every one of them
  runs on `background_spawn` and comes back through `update`. The UI thread
  issues intents and never waits.

The one blocking thing that is not a request follows the same rule: the
billing probe drives a pseudo-terminal on the background executor with a 20 s
ceiling (`tier.rs`). Sign-in is requests all the way down — `account/*` on the
background executor like everything else — and `auth.rs` only reads
`auth.json` for the two display strings.

## 4. The fold

`MuseFold` is the whole adapter. MSP events go in; `aui_protocol::Delta`s come
out, already applied to the fold's own `Session`, so a caller can either
re-apply them or read `MuseFold::session`.

Three rules earned their place the hard way:

- **A block is placed by its item's log sequence, not by arrival.** The same
  `sourceRange.first.sequence` appears on `item/started` and on
  `item/completed`, so a live stream and a backfilled `view/page` put the same
  block in the same place. Before this, a session read a second time did not say
  what it said the first time (finding F3, `docs/04-approvals.md` §9).
- **Nothing is cached that belongs to a stage.** An approval's choices belong to
  its current stage and the stage moves; the card re-renders from
  `approval/updated` and `approval/resolved` and from nothing else.
- **What decided an approval is not what it asked about.** A policy resolution
  names the amendment it installed, else the reason the gated item carries, else
  the approval mode — never the command (finding F11).

What `aui_protocol` has no room for lives in `SideState`: the MSP
`requirementId` per stage, the pending approval and user-input requests, the
queued turns, the effective approval mode, and the prompt text of a command in
flight (the wire never gives a prompt back, so a retraction has to be able to
hand it over).

## 5. The harness's own storage

Muse owns `~/.config/muse` and `~/.local/share/muse`, and the harness never
writes to either. It reads the session index (`index.rs`, read-only, and every
failure mode is an empty map). Its own state lives under
`~/Library/Application Support/harness`, written atomically by `store.rs`:

| file | what |
|---|---|
| `tier.json` | the last billing probe, keyed by `auth.json`'s mtime (`docs/06-billing.md`) |
| `sessions.json` | per-session name, hidden flag and derived title (`sessions.rs`) |
| `history.json` | the prompt history, per workspace (`history.rs`) |

## 6. `--replay`

`--replay <capture.jsonl>` folds a capture's `<-- ` lines through `MuseFold`
exactly as the fixture test does, renders the result, and lets `--steps` and
`--screenshot` work on it. There is no child process and no server, so it costs
nothing and is reproducible to the byte. Commands issued against a replayed
session are refused with a banner rather than silently dropped, and the sidebar
labels the row by the **file**, because a replayed session is not one this host
ever ran.

Almost every screenshot in `docs/images/` is one of these. The ones that are not
are named in the document that owns them, with the command that took them.
