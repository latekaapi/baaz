# Keymap, as built

Spec §3.9, plus everything the cards added. Nothing here is configurable yet;
`aui::init` installs the library's half and `crate::app::bind_keys` installs the
harness's, so a fork can rebind either by calling `cx.bind_keys` afterwards.

A key means one thing at a time because the **context** it is bound in is a
fact about the frame, not a guess. `HarnessComposer` is on the composer's
holder; `menu`, `field`, `histup` and `histdown` join it as the frame changes;
`AuiMenu` belongs to whatever overlay has the keyboard; `AuiApproval` is added
only while a pending card is focused and the draft is empty.

---

## Composing

| Key | What it does | Context |
|---|---|---|
| Enter | Send, or queue when a turn is running | `HarnessComposer`, no menu, no field |
| Enter | Run the highlighted `/` or `@` row | `HarnessComposer && menu` |
| Enter | Send an open approval-feedback or question-clarify field | `HarnessComposer && field` |
| Enter | Commit the sidebar row's inline rename | `HarnessRename` |
| ⇧Enter | New line | matches nothing, so the editor gets it |
| ⌘Enter | Steer the running turn | `HarnessComposer` |
| ⇧Tab | Plan mode on / off | `HarnessComposer` |
| ⌘V | Paste; an image on the clipboard becomes an attachment | `HarnessComposer` |
| ↑ / ↓ | Prompt history, only on the draft's first / last line | `histup` / `histdown` |
| ↑ / ↓ | Move the open menu's highlight | `HarnessComposer && menu` |
| `/` | The command menu, at a line start only | — |
| `@` | The mention picker, wherever a word starts | — |
| `!` | The shell escape: a command, not a turn | — |

## Stopping

| Key | What it does |
|---|---|
| ⌃C | Stop the running turn and hand its prompt back |
| Esc | Close the topmost overlay; then the card's open field; then the rename; then the search; then, on an empty composer, stop the turn |

Escape's order is one list because every overlay's state lives in one place
(`crate::overlays::Overlays`), so "close whatever is open" is a function rather
than a negotiation.

## Cards

| Key | What it does |
|---|---|
| `1`–`9` | The n-th choice of the pending approval, **in the server's order** |
| Enter | Confirm an open feedback or clarify field |
| Esc | Close the field, then collapse the card |
| Tab | Between the card and the composer |

A provider mints its own choices, so there is no fixed `y`/`a`/`n` to bind:
the keys are digits and the payload is an index (`aui::keys::ChooseNth`). A card
with fewer choices than the digit pressed does nothing. When a card arrives and
the draft is **empty**, focus moves to it; when the draft is not empty focus
stays where the person was typing and the needs-you banner is the way over.

## The window

| Key | What it does |
|---|---|
| ⌘B | Sidebar ↔ collapsed rail |
| ⌘N | New session in this workspace |
| ⌘K | The command palette: every `/` command and every session operation |
| ⌘⇧F | The sidebar's search field |
| ⌘⇧M / ⌘⇧E / ⌘⇧P | Model / reasoning effort / approval mode |
| ⌘\\ | The right pane (wired, and the pane is empty) |
| Tab / ⇧Tab | The next / previous tab stop, and it arms the focus ring |

## Focus rings

gpui has no `:focus-visible`, so the library keeps one window-wide flag: a key
arms it, a mouse press disarms it, and every focusable control draws its accent
ring only while it is armed. The harness has its own root element, so it calls
`aui::keys::track_pointer` on it — without that call the flag would never be
disarmed and every control would wear a ring after the first key press.

## Deliberately not bound

- **No ⌘1–⌘9 for sessions.** The digits belong to the approval card, and a key
  that means two things depending on where you are looking is a key that means
  neither.
- **No ⌘F.** The search is the sidebar's, not the transcript's, and ⌘⇧F says so.
- **No ⌘W / ⌘Q overrides.** They are the platform's.
