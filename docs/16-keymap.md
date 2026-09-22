# 16 — The keymap

How keystrokes reach actions, why they stopped working, and what a
user-editable keymap will look like. Written 2026-09-22 after the owner
reported *"the keyboard shortcuts are not wired properly — it works once, but
after that it doesn't work."*

## 1. What was wrong

Three causes, not one. All three were confirmed against the code and the first
against a failing test.

### 1.1 A context-scoped binding dies when focus is lost

`crates/baaz/src/app.rs` gives the root `div` both
`.key_context(aui::keys::ROOT_CONTEXT)` and `.track_focus(&self.focus_root)`.
**Nothing ever focused `focus_root`** — the handle had three references in the
whole crate: the field, its creation, and that `track_focus`.

gpui resolves a keystroke like this (`gpui::Window::dispatch_key_event`, and
`focus_node_id_in_rendered_frame` beneath it): it looks up the focused node,
and **when nothing is focused it falls back to the dispatch tree's root node**.
The dispatch root is above our `div`, so the path then contains no `AuiRoot`
context at all. Every binding scoped to `AuiRoot` stops matching.

The sequence the owner hit: press a shortcut → it opens an overlay → the
overlay closes → its focused element unmounts → focus is null → the next press
of that same shortcut reaches nothing. Works once, then never.

### 1.2 The asymmetry that proves it

Bindings registered with context `None` are handled by
`cx.global_action_listeners`, which gpui runs in the **capture phase, before**
it walks the window's dispatch path — so they fire regardless of focus. Those
are the ones that never broke:

| Binding | Context | Behaviour |
|---|---|---|
| `⌘B` sidebar, `⌘K` palette, `⌘,` settings | `None` | always worked |
| `⌘N`, `⌘⇧F`, `⌃\``, `⌘⌥B` | `AuiRoot` | worked once |

This is also the fix's shape: genuinely app-wide commands belong on global
listeners, not on a context.

### 1.3 `⌘\` dispatched into nothing

`aui::keys` binds `cmd-\` to **its own** `ToggleRightPane`. Baaz declares a
**different** `ToggleRightPane` in its own `actions!` block and never handled
the library's. Two same-named action types from two crates: the keystroke was
delivered, the action had no listener, nothing happened, no error.

### 1.4 One trap that turned out not to apply

The `caret` prototype (`~/Sandbox/caret`) records, in a comment paid for in
real debugging time, that on macOS Option remaps the produced character —
`⌥B` yields `∫` — so a webview matching `event.key` against `"b"` silently
never fires. **That does not apply here.** gpui's macOS layer recomputes the
key name as `chars_for_modified_key(keyCode, CMD_MOD)`, asking the layout what
the key produces with *Cmd only*, deliberately excluding Option. `cmd-alt-b`
resolves to `b` correctly. The trap is a webview problem; it is written down
so nobody re-derives it.

## 2. What the references do

Three systems were read before designing ours.

**Claude Code** (`~/.claude/keybindings.json`, `/keybindings` to open it) —
JSON only, no remapping UI. Grouped by context, then keystroke→action, where
actions are `namespace:action` strings and `null` unbinds. Contexts are a flat
enumerated list (`Global`, `Chat`, `Settings`, …), and per-component override
order is hardcoded rather than declared. Its real strength is **load-time
validation**: unknown action names, invalid contexts and duplicates all warn,
an unknown action *keeps the default* rather than killing the key, and a
reserved list (Ctrl+C, Ctrl+D, Enter, Escape, Tab…) refuses to be rebound.
Chords are space-separated with a 3-second timeout.

**Synara** (`~/.synara/userdata/keybindings.json`) — a flat ordered array of
`{key, command, when?}`, VS Code's shape, with both a settings panel and the
file. Conflicts resolve purely by order: last match wins. Their issue #969 is
a cautionary tale we should take seriously — their shortcut *documentation*
drifted from the app, with missing entries and a wrong key combo.

**Zed** — the one that matters, because it is built on the same gpui. Keymap is
a JSON array of `{context, bindings}` blocks where `context` is a real
predicate language (`&&`, `||`, `!`, parens, `==`, and `>` for
ancestor-descendant). Contexts form a **tree mirroring the element tree**, so
depth disambiguates. `Keymap::bindings_for_input` collects every enabled
binding with its depth and sorts by depth then definition order — so a user
keymap loaded *after* the defaults wins for free, with no special override
mechanism. `NoAction` unbinding is source-aware. Chords hold a pending prefix
and — the good part — **replay** the swallowed keystrokes if the sequence turns
out not to complete. It ships a keymap editor UI that writes back to the JSON.
Its weakness is that conflicts are resolved silently and never reported.

## 3. What we will build

### 3.1 Now — correctness and one source of truth

- Restore focus so `AuiRoot` bindings survive; and move genuinely app-wide
  commands to global listeners, which do not depend on focus at all.
- Make `⌘\` reach a real handler.
- Replace the 34 hand-written `KeyBinding::new` calls with **one table** —
  `(action, keystroke, context, category, human_label)` — so there is a single
  place that answers "what are this app's shortcuts". `category` and
  `human_label` are unused today and exist because §3.2 needs them beside the
  binding rather than in a second list that drifts (Synara #969).
- A test rejecting duplicate `(keystroke, context)` pairs. That is conflict
  detection, and it is the thing 34 scattered calls cannot give.

### 3.2 Later — user-editable

- `~/Library/Application Support/baaz/keymap.json`, beside `layout.json`, in
  Zed's shape: an array of `{context, bindings}` blocks, `null` to unbind.
  **The file is the source of truth and the UI writes to it**, never a second
  store.
- Load order does the work: defaults first, user file second, so user bindings
  win by gpui's existing depth-then-order rule. No override mechanism to write.
- **Load-time validation, from Claude Code rather than Zed**: unknown action →
  warn and keep the default; invalid context → warn; duplicates → warn. Surface
  them where the app already surfaces diagnostics.
- A **reserved list** that refuses to be rebound: ⌘Q, ⌘W, ⌘H, and the text
  editing keys. Publish it; a keymap whose failure is invisible is worse than
  one that refuses.
- Settings gains a Shortcuts section. **This needs a new library component** —
  today `aui`'s settings dialog offers switch rows only, and there is no way to
  express "record a keystroke". That component, not the storage, is the bulk of
  the work.
- The shortcut sheet and `docs/08-keymap.md` are **generated from the same
  table** the keymap validates against, so they cannot drift.

### 3.3 Deliberately not doing

- **No base-keymap presets** (VS Code, JetBrains, …). Zed can afford eight
  because people arrive at an editor with muscle memory; for a session harness
  the payoff is small and the maintenance permanent. The `category`/source tag
  on each binding keeps the option open.
- **No predicate language at first.** Our contexts are already a small tree.
  Adopt Zed's `&&`/`!` syntax only when a binding genuinely needs it.

## 4. Where this sits in the roadmap

**§3.1 is S1.4** — remedial. The shortcuts are broken *now*, in shipped Stage 1
work, and a customisation layer over a broken focus model would only make 34
broken bindings configurable.

**§3.2 is S2.4**, in Stage 2 (Foundation). It belongs there and not earlier for
a concrete reason: Stage 2 already builds the app's persistence and settings
foundation (`baaz.db`, the capability spec), and the Shortcuts UI needs a new
library component plus a validated store. Doing it in Stage 1 would mean
building that component twice.
