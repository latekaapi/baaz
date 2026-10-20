#!/usr/bin/env python3
"""D6 follow-up — one bar per right-pane screen (proposal, 2026-10-21).

Owner, 2026-10-21 (annotated screenshot of the Page): "Is the separate header bar called Page with a dropdown
really required? Can that and the address bar not be merged into a single bar/chrome? … design first, get it
approved from me and then implement. We need to do same for the other panes such as File etc."

What this proposes (nothing here is built):
- Page: the pane row and the nav row become ONE 32 px bar. The screen's name leaves (the lit Page toggle in the
  transcript title row already names it); Recent pages moves from the title into a chevron at the address field's
  right end; the screen's actions and ✕ stay at the right, after a hairline.
- Files: the filter no longer opens a second row: the search action turns the bar itself into the filter field.
- Changes and the pushed views (file preview, output, image) are one row already; they stay as they are.

Built from gen_d6's own pieces (header_tabs, nav_row, recent_menu, view_files, page_img) so the "Now" frames are
the locked boards and the "Proposed" frames change only the bars.
Run: python3 gen_d6_bar.py   (writes project/D6OneBar.dc.html + preview/D6OneBar.html)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d6 as g
from gen_d6 import ic, ib2, mini, grid_board, view_page, view_files, page_img, recent_menu, header_page_menu, nav_row, file_rows, LIGHT

HAIR = '<span aria-hidden="true" style="width:1px;height:16px;background:var(--line);margin:0 4px;flex:none"></span>'


def one_bar_page(w, menu_open=False, chev_hov=False, loading=False, annot=False):
    """The Page's one bar: ← → ↻ · address (lock, host, path, Recent pages chevron) · annotate · screenshot ·
    open in Browse · hairline · ✕. 32 tall, opaque L1, padding 0 6, gap 2 — the pane row's own metrics."""
    narrow = w < 300
    nav = '' if narrow else ib2('back', 'Back') + ib2('fwd', 'Forward', 'off')
    nav += ib2('reload', 'Reload')
    chev_bg = 'var(--sel)' if menu_open else ('var(--hov)' if chev_hov else 'transparent')
    chev = (f'<button aria-label="Recent pages" aria-haspopup="menu" aria-expanded="{"true" if menu_open else "false"}" '
            f'style="margin-right:-6px;width:22px;height:22px;border:0;border-radius:4px;background:{chev_bg};'
            f'color:var(--muted);display:inline-flex;align-items:center;justify-content:center;flex:none;padding:0">{ic("down", 12)}</button>')
    addr = (f'<div class="addr2" role="textbox" aria-label="Address"><span style="color:var(--muted);display:flex">{ic("lock", 12)}</span>'
            f'<span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap"><span class="h">localhost:3000</span><span class="p">/pricing</span></span>{chev}</div>')
    right = ib2('pin', 'Annotate the page', 'on' if annot else '') + ib2('cam', 'Attach a screenshot to the composer') + ib2('ext', 'Open in Browse')
    prog = '<div class="prog" style="width:38%;top:30px"></div>' if loading else ''
    return (f'<div class="ph2" style="position:relative">{nav}{addr}{right}{HAIR}{ib2("x", "Close the pane", size=12)}{prog}</div>')


def view_page_one(src, w, menu_open=False, chev_hov=False, loading=False):
    pg = page_img(src, w)
    menu = ''
    if menu_open:
        # The dropdown hangs from the address field: 2 px under the bar, its left edge on the field's.
        left = 6 + (0 if w < 300 else 56) + 30
        menu = recent_menu(w, 1).replace('left:6px;top:34px', f'left:{left}px;top:34px')
    return one_bar_page(w, menu_open, chev_hov, loading) + f'<div class="pgw" style="top:32px">{pg}</div>' + menu


def view_page_now(src, w, menu_open=False):
    pg = page_img(src, w)
    menu = recent_menu(w, 1) if menu_open else ''
    return header_page_menu(open_=menu_open) + nav_row(w) + f'<div class="pgw" style="top:68px">{pg}</div>' + menu


FILTER_ROWS = [(0, 'do', 'crates', ''), (1, 'do', 'baaz', ''), (2, 'do', 'src', ''), (3, 'rs', 'right.rs', 'M'),
               (2, 'do', 'tests', ''), (3, 'rs', 'right_pane.rs', ''), (0, 'do', 'docs', ''), (1, 'md', '42-handoff-d6-right-pane.md', '')]


def view_files_filter_one(w):
    """Files with the filter open, in the bar: the search glyph, the query, the count, then ✕ (which closes the
    filter first, the pane on a second press — as Esc does today)."""
    field = (f'<div class="inp focus" style="flex:1;min-width:0;display:flex;align-items:center;gap:8px;height:28px;min-height:28px;padding:0 8px;font-size:13.2px">'
             f'<span style="color:var(--muted);display:flex">{ic("search", 12)}</span><span>right</span>'
             f'<span style="margin-left:auto;font-size:12.1px;color:var(--faint)" class="mono">3 of 1,204</span></div>')
    bar = f'<div class="ph2" style="gap:6px">{field}{HAIR}{ib2("x", "Close the filter", size=12)}</div><div class="pfade"></div>'
    body = f'<div class="rows" role="tree" aria-label="Files" style="padding-top:4px">{file_rows(FILTER_ROWS, sel="right.rs")}</div>'
    return bar + f'<div class="pb">{body}</div>'


def b_one_bar(src):
    page_note = ('One 32 px bar, the pane row’s metrics (opaque L1, padding 0 6, gap 2, controls 28): back, forward, '
                 'reload; the address on the Well (28, r 6, edge) with a 22 px chevron button at its right end — '
                 '“Recent pages”, the same menu as today, hung 2 px under the bar from the field’s left edge; then '
                 'annotate, screenshot, open in Browse; a 1 × 16 hairline; ✕. The name “Page” leaves the pane: the '
                 'lit globe toggle in the transcript’s title row already says which screen this is. The page starts '
                 'right under the bar, 36 px higher than today; no fade (it never drew over the page).')
    rows = [[
        mini(view_page_now(src, 400), 400, 360, label='Page · now (locked D6)', note='Two rows: the pane row (glyph, “Page ⌄”, ✕) over the 36 px nav row. 68 px of chrome before the page.'),
        mini(view_page_one(src, 400), 400, 360, label='Page · proposed: one bar', note=page_note),
        mini(view_page_one(src, 400, menu_open=True), 400, 360, label='Recent pages, open', note='The chevron is lit (9 %) while the menu is open; rows, order and check as the locked Recent pages board.'),
    ], [
        mini(view_page_one(src, 280), 280, 300, label='Narrow (280)', note='Under 300 back and forward leave, as today; the address keeps ~120 px.'),
        mini(view_page_one(src, 400, loading=True), 400, 300, label='Loading', note='The 2 px muted bar runs along the bar’s foot instead of under the nav row.'),
        mini(view_page_one(src, 400), 400, 300, theme=LIGHT, label='Light'),
    ], [
        mini(view_files(400, sel='right.rs', filt='right'), 400, 360, label='Files · now, filter open', note='The search action opens a second, 32 px field row under the pane row.'),
        mini(view_files_filter_one(400), 400, 360, label='Files · proposed: the filter in the bar', note='At rest the bar is unchanged (folder glyph, “Files”, search, ✕). The search action — or ⌘F, / — turns the bar into the field: the Well (28, edge 22 % while focused), search glyph 12, the query, the count faint mono at the right; a hairline; ✕ closes the filter (Esc too), and a second ✕ closes the pane.'),
        mini(view_files(400, sel='right.rs'), 400, 360, label='Files · at rest (unchanged)', note='Changes and the pushed views (file preview, output, image) are one row already and stay as built.'),
    ]]
    return grid_board('D6 · One bar per screen — PROPOSAL (2026-10-21, not built)',
                      'Owner 2026-10-21: “Is the separate header bar called Page with a dropdown really required? Can that and the address bar not be merged into a single bar? … We need to do the same for the other panes such as Files.” Left: the locked D6 boards. Right: the proposal. Only the bars change; everything below them is as built.',
                      rows, 1440)


if __name__ == "__main__":
    g.page6("D6OneBar", "D6 · One bar per screen (proposal)", b_one_bar, 1440, 1440)
    print("wrote D6OneBar")
