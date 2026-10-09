# 28 · Iterating the design in agentic-ui, and getting it into Baaz

*Written 2026-10-09, before the browser integration (R1/R2). Measured against baaz `feedback-mode` 830aa00 and aui v0.3.21.*

## The short answer

Yes, with one condition. Baaz gets its look from the library at three layers, and a design iteration carries over
automatically only at the first two:

| Layer | Where it lives | Carries into Baaz by | Measured today |
|---|---|---|---|
| **Tokens** (colour, type, spacing, radii, density, motion) | `aui-tokens` (`theme.rs`, `density.rs`, `fonts.rs`, `styled.rs`) | a tag bump, no code change | Baaz has **0** hard-coded colours; every colour is `p.ink`, `p.surface_2`, `p.line`… (≈230 uses) |
| **Components** (rows, buttons, menus, cards, transcript blocks, the shell, overlays, nav) | `aui::{shell,nav,transcript,overlay,data,composer,feedback,screens,workbench}` | a tag bump plus compile fixes where a signature moved | Baaz calls into these ≈260 times |
| **Screens** (how a whole page is composed) | partly `aui::screens` (login, assistant, providers, connect, browse), **mostly Baaz itself** | does NOT carry over; the screen is Baaz code | Baaz draws **556** `div()/h_flex/v_flex` of its own, concentrated in `app.rs` 160, `settings.rs` 89, `skills_page.rs` 79, `sidebar_view.rs` 65, `feedback/ui.rs` 32, `session/render.rs` 29, `right.rs` 26, `dialogs.rs` 24, `transcript.rs` 21 |

So: a new palette, type scale, spacing rhythm, corner radius, or motion curve reaches every Baaz screen through one
tag bump. A redesigned row, button, menu, card or transcript block reaches every place Baaz already uses it. A
re-composed *page* (a new Settings layout, a new sidebar structure, a new session header) does not reach Baaz unless
either the page is moved into `aui::screens` and Baaz adopts it, or the change is made in Baaz's own file.

This mechanism is proven: 41 tags so far, roughly twenty of them design changes consumed by a repin.

## The method that keeps integration cheap

1. **Tokens first.** Any change that can be expressed as a token value goes into `aui-tokens`. Never a one-off colour
   or size in a component. Check with `grep -rn "rgb(0x\|hsla(" crates/aui/src` — it should stay at zero.
2. **Components second, in the gallery.** Every component change lands with its gallery card updated and a screenshot
   taken (`cargo run -p aui-gallery -- --screenshot <card> out.png`). The card is the contract Baaz will see.
3. **Screens third, as `aui::screens`.** When a page's composition changes, build the page in the library as a
   screen (the way `screens/browse` was), with the gallery's steps harness for its states. Baaz then *adopts* the
   screen instead of drawing its own. This is exactly the R1 step planned for the browser; apply it to every page.
4. **One tag per iteration round.** Tag only when the gallery is green and the cards have been looked at. Baaz pins by
   tag, so a half-done round never leaks.

## Integration into Baaz, step by step

```bash
# in harness
sed -i '' 's/tag = "v0.3.21"/tag = "vX.Y.Z"/' Cargo.toml   # all five aui-* lines
cargo build -p baaz 2>&1 | grep -E "^error" -A6             # fix the signature moves
cargo test --workspace
python3 ~/.claude/skills/relay/scripts/relay_visual.py --repo . --entry login-choose … --entry settings-archived
```

The visual probe (CLAUDE.md lists the 18 entries) is the step that matters: it shows what the repin changed on each
Baaz screen. Read the images. Regenerate `design/baseline/` **only on main, after merge**, in its own commit.

Compile breaks after a repin are expected and are the complete list of places Baaz must adapt: the library is pre-1.0,
signature moves are the integration work, and there is no hidden second list.

## Where it will NOT carry over, and what to do there

The 556 lines Baaz draws itself re-colour with tokens but keep their shape. For a design round that changes how pages
are composed, pick one of two paths per page, up front:

- **Move the page into `aui::screens`** (recommended for Settings, the skills page, the sidebar, the session header,
  dialogs). Cost: one relay lane per page, like the browse screen. Gain: the page becomes designable in the gallery
  forever, with a steps harness and screenshots.
- **Restyle in Baaz** (acceptable for `app.rs` chrome glue). Cost: design iteration happens in Baaz, verified by the
  Tier V probe instead of the gallery.

Do not do a third thing: re-drawing a library component inside Baaz. Every instance of that today is a place the next
design round will miss.

## What the gallery does not yet mirror

Gallery screens today: login, assistant, providers, connect, browse. Not mirrored: **Settings** (general, providers,
shortcuts, archived), **Skills page**, **the sidebar with sessions and projects**, **the right pane** (browser, diff,
git, files, changes). A design round that touches those must either add gallery screens for them first (so there is
something to iterate on) or iterate in Baaz.

## The recommended path (decided 2026-10-09): move every page into the library, by redesigning it

Do not refactor first and design second. Each Baaz-drawn page moves into `aui::screens` **at the moment it is
redesigned**, one relay lane per page, so every lane ends with something the owner can look at.

A library screen has the shape Browse already has: a plain **view model in** (the data to show), **intents out** (what
the person did). Baaz keeps all state, stores, sessions and providers; it becomes the thin host that fills the view
model and handles the intents. The gallery shows the screen with mock data and a steps harness for every state.

Order (most visible first, and where the browser integration will land):
1. Sidebar with sessions and projects (the Sessions ↔ Browse toggle changes it anyway)
2. Session header + transcript chrome
3. Settings ×4
4. Right pane ×5
5. Skills page, dialogs
6. App chrome last (glue; may stay in Baaz)

Each lane: capture the Baaz page with the Tier V probe as the honest reference → build the library screen (view model,
gallery card, steps harness, screenshots) → iterate the design in the gallery on the owner's notes → Baaz adopts
(delete the local drawing, map state → view model, intents → handlers) → tag, repin, probe, compare images.

Cost guess: the Browse run built one screen plus its library in ≈4 h of relay time with reviews; nine pages on existing
components ≈ 2–3 days of runs across however many rounds of owner notes. End state: Baaz draws nothing of its own, the
gallery IS the app's design, every later design round is a tag bump, and the browser's R1 step is already half done.

## Prompt for the session that runs the design round

> Read docs/28-design-iteration.md, "The recommended path". We are moving Baaz's pages into agentic-ui as screens,
> redesigning each as it moves. Library work in `~/Projects/agentic-ui` on a branch cut from the merge of
> `relay/browse-proto`; Baaz work in `~/Projects/harness`. Rules: view model in, intents out, Baaz keeps state; tokens
> before components before screens; every change lands with its gallery card, steps harness and screenshot; no
> hard-coded colour or size. Start with the sidebar: capture Baaz's sidebar with the Tier V probe as the reference,
> build `screens/sidebar` in the gallery, then take my design notes on it, then have Baaz adopt it, tag, repin, run
> the 18-entry probe and show me before/after. Then the next page in the doc's order. Use relay for the lanes.

## Related

- docs/27-browser-redesign.md §8 — R1 (adopt browse components into Baaz) is the same adoption step as above.
- `~/Projects/agentic-ui/.relay/HANDOFF.md` — state of the `relay/browse-proto` branch (unmerged, unpushed).
- CLAUDE.md — the visual probe, the 18 entries, the baseline rule.
