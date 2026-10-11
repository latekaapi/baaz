# Sidebar v2 — the approved mock as the spec (2026-10-10)

Source of truth: the Design canvas **Baaz sidebar v2** (https://claude.ai/artifact/GACTNjKyXA9necGvwPvovP). The
`*.dc.html` files here are its artboards as of version 28 (2026-10-11: new tab + rename field built; grounds A/B/C kept for the record); `Spec.dc.html` holds the
measurement table. Regenerate PNGs from the canvas (download icon per board) into `png/` when the boards change.

## Board → gallery card (agentic-ui `design-v2`, crate `aui-gallery`)

| Board | Card id | Size | Steps |
|---|---|---|---|
| Main.dc.html · Populated window | `screens/sidebar` | 1440×900 | `hover:<id>`, `select:<id>`, `key:<id>`, `rename:<id>`, `menus`, `leave` |
| States.dc.html | `screens/sidebar-states` | 1432×720 | (five fixed columns: loading, empty, hover, rename+key, menus) |
| BrowseWindow.dc.html | `screens/sidebar-browse` | 1440×900 | `hover:<tab id>` |
| NewTab.dc.html | `screens/sidebar-newtab` | 1440×900 | — |
| Collapsed.dc.html | `screens/sidebar-collapsed` | 1440×900 | — |
| Compact.dc.html | `screens/sidebar-compact` | 260×720 | as `screens/sidebar` |

Sample data: `aui::screens::sidebar::sample_model()` is the mock's `Sidebar.dc.html` data verbatim.

## Measurements → tokens (default / compact)

| Mock | Token |
|---|---|
| list row 32 / 28 | `metrics.control_lg` (`list_row_height`) |
| two-line row 48 / 44 | `control_lg + SP_5` (one Caption line) |
| gutter 8, text axis 40, trailing 244 | `item_px`; leading slot `item_px + icon`; `gap` |
| nested indent 16 | `inset` |
| section title 28 tall, 12 above, label at 16 | `control_md`, `SP_4`, `item_px` |
| title bar 52 | `bar + item_px` |
| toggle at x 84 | `SP_7×2 + SP_5 + SP_2` |
| row title 13/20 | `TextRole::Body` at `Density::Compact` (both densities) |
| byline, counts, ages 12/16 | `TextRole::Caption` |
| chips 11/14, branch mono 12 | `TextRole::Micro`, `TextRole::Mono` compact |
| hover / selected blocks | FluidList overlays (`hover_overlay`, `selected_overlay`), item radius |
| menus | `menu_surface` (+2, item radius, shadow, ring), rows `control_md` |
| page card | `surface(1).fill().shape(Container).ring(true).shadow(true)`, inset `item_px` |

## Gate

```bash
cargo build -p aui-gallery
cargo run -q -p aui-gallery -- --screenshot screens/sidebar /tmp/sidebar.png
python3 -I scripts/visual_diff.py --mock docs/30-sidebar-spec/png/Main.png --capture /tmp/sidebar.png --threshold 0.02
```
(the script lives in agentic-ui `scripts/visual_diff.py`; exit 0 = within threshold, 1 = over, 2 = size mismatch).
