# Keymap, as built

Spec §3.9, plus everything the cards added. Nothing here is configurable yet;
`aui::init` installs the library's half and `crate::app::bind_keys` installs Baaz's,
so a fork can rebind either by calling `cx.bind_keys` afterwards.

A key means one thing at a time because the **context** it is bound in is a
fact about the frame, not a guess. `BaazComposer` is on the composer's
holder; `menu`, `field`, `histup` and `histdown` join it as the frame changes;
`AuiMenu` belongs to whatever overlay has the keyboard; `AuiApproval` is added
only while a pending card is focused and the draft is empty.

---

## Composing

| Key | What it does | Context |
|---|---|---|
| Enter | Send, or queue when a turn is running | `BaazComposer`, no menu, no field |
| Enter | Run the highlighted `/` or `@` row | `BaazComposer && menu` |
| Enter | Send an open approval-feedback or question-clarify field | `BaazComposer && field` |
| Enter | Commit the sidebar row's inline rename | `BaazRename` |
| ⇧Enter | New line | matches nothing, so the editor gets it |
| ⌘Enter | Steer the running turn | `BaazComposer` |
| ⇧Tab | Plan mode on / off | `BaazComposer` |
| ⌘V | Paste; an image on the clipboard becomes an attachment | `BaazComposer` |
| ⌘U | Attach a file or photo (the `+` menu's first row) | `BaazComposer` |
| ↑ / ↓ | Prompt history, only on the draft's first / last line | `histup` / `histdown` |
| ↑ / ↓ | Move the open menu's highlight | `BaazComposer && menu` |
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
| ⌘⌥B | The right pane ↔ closed; reopening restores the last-shown kind (Browser, Diff review, Changes, Files) |
| ⌘N | New session in this workspace |
| ⌘, | The Settings dialog (File → Settings…; its Sidebar section owns the three group flags) |
| ⌘K | The command palette: every `/` command and every session operation — the six window commands among them act with no session open |
| ⌘⇧F | The full-text search palette (`docs/12-search.md`); its empty query lists recent sessions, which is what the old sidebar filter did |
| ⌘⇧O | The Projects palette: adopted projects to switch to, recent Muse workspaces to adopt |
| ⌘⇧M / ⌘⇧E / ⌘⇧P | Model / reasoning effort / approval mode |
| ⌘W | Close the window (File → Close Window): probe cleanup, then the app hides; the Dock icon or ⌘-Tab brings the same window and session back |
| ⌘Q | Quit (Baaz → Quit Baaz; probe cleanup first; asks first when a terminal command is running, naming it) |
| ⌘M | Minimize the window |
| Tab / ⇧Tab | The next / previous tab stop, and it arms the focus ring |

## Terminal

| Key | What it does | Context |
|---|---|---|
| ⌃` | Toggle the terminal dock, focusing it on open | `AuiRoot` |
| Every other key | To the pty | `BaazTerminal` |
| ⌃C | SIGINT to the active tab | `BaazTerminal` |
| ⌘C | Copy the grid selection, if any | `BaazTerminal` |
| ⌘K | The command palette (gains "Toggle the terminal dock" and "Open a new terminal tab", with the four right-pane commands beside them) | `AuiRoot` |
| ⌘B | Sidebar ↔ collapsed rail | `AuiRoot` |
| ⌘W | Close the window | `AuiRoot` |
| ⌘Q | Quit (asks first when a terminal command is running, naming it) | `AuiRoot` |
| ⌘N | New session in this workspace | `AuiRoot` |

The grid runs under `BaazTerminal`: every key reaches the pty except the
rows above. The composer's Enter, paste, history and ⇧Tab keys, and the
turn's ⌃C, are nulled in `BaazTerminal` so they never fire while the dock
holds the keyboard.

## Play buttons

No binding — both are clicks, and no click means no execution. Every shell
tool card (Muse's shell and the `!` userShell) carries **Run in terminal**
in its header; a runnable fenced code block (`sh`, `bash`, `zsh`, `shell`,
`terminal`, `console`, or an untagged `$ `-prompt block) offers **Run**
under its turn. A click opens the dock, picks the project's idle active
tab (else a new one), bracketed-pastes the whole command and sends Enter;
⌥-click pastes without Enter. Entirely local: no turn, no wire, and it
works under `--replay`.

## Focus rings

gpui has no `:focus-visible`, so the library keeps one window-wide flag: a key
arms it, a mouse press disarms it, and every focusable control draws its accent
ring only while it is armed. Baaz has its own root element, so it calls
`aui::keys::track_pointer` on it — without that call the flag would never be
disarmed and every control would wear a ring after the first key press.

## Native menus

The menu bar is real (`crate::app::set_menus`, called after `bind_keys` in
`main.rs`): Baaz (About, Services, Quit ⌘Q), File (Add Project ⌘⇧O, New
⌘N, Settings ⌘,, Close ⌘W),
Edit (the standard six, each carrying its `OsAction` for OS recognition),
View (sidebar, palette ⌘K, search ⌘⇧F, theme), Window (Minimize ⌘M, Zoom),
Help (Baaz Documentation reveals `docs/` in Finder). A menu item's shortcut displays
from the keymap, so an item without a binding shows none — which is why the
Edit items show none: ⌘X/⌘C/⌘V/⌘A/⌘Z belong to the focused field and are not
rebound globally. ⌘W and the red dot both hide the app (`cx.hide()`) after the
tier-probe cleanup (`tier::cleanup_probes`), so the window and the `muse serve`
child survive and the Dock icon and ⌘-Tab bring the same session back;
`on_reopen` re-activates, or rebuilds the window through the shared
`open_shell_window` if it was removed some other way. ⌘Q quits through
`cx.quit()` after the same cleanup — unless a terminal tab holds a running
block, in which case it asks first through the dialog, naming the command
(`docs/14-terminal.md` D52). Closing a busy tab asks the same way; an idle
tab closes outright.

## Deliberately not bound

- **No ⌘1–⌘9 for sessions.** The digits belong to the approval card, and a key
  that means two things depending on where you are looking is a key that means
  neither.
- **No ⌘F.** Search is the palette's (⌘⇧F), not the transcript's.
- **No global ⌘X/⌘C/⌘V/⌘A/⌘Z.** The Edit menu names them for the OS, but their
  keys stay with the focused field; binding them globally would steal them
  from the composer.
- **⌘W / ⌘Q are bound, once.** They used to be left to the platform; now
  File → Close Window and Baaz → Quit Baaz own them, because closing or
  quitting mid-probe must run the probe cleanup.
