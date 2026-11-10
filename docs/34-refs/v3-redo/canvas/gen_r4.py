#!/usr/bin/env python3
"""R4 · The daily driver — the four surfaces nothing draws yet (PROPOSAL, 2026-11-11, not built).

harness docs/77 §3 and docs/27 §5/§7/§8. Owner answers 2026-11-11: ⇧-click opens Peek, ⌥-click opens Split (both are
in the agentic-ui gallery, wired not redrawn); Chrome import is tested on bookmarks + history only; boards first for
bookmarks, the Chrome import sheet, Spaces with own sign-ins, and the footer's downloads/extensions.

History in the Field needs no board: the Field already draws history rows (aui `FieldHistory`); R4 feeds them from
browser.db. They appear here only where bookmarks sit beside them.

The sidebar copies the `browse-sidebar` capture (256 px, address line 32 r 6, nav row, Search, rows 32, footer).
Run: python3 gen_r4.py (writes project/R4Daily.dc.html + preview/R4Daily.html)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d6 as g
import gen_d5p as d5
from gen_d6 import ic, mini, grid_board, I

I.update(
    star='<path d="M8 2.2l1.75 3.6 3.95.55-2.87 2.77.7 3.93L8 11.2l-3.53 1.85.7-3.93L2.3 6.35l3.95-.55z"/>',
    starf='<path fill="currentColor" d="M8 2.2l1.75 3.6 3.95.55-2.87 2.77.7 3.93L8 11.2l-3.53 1.85.7-3.93L2.3 6.35l3.95-.55z"/>',
    folder='<path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/>',
    key='<circle cx="5.5" cy="10.5" r="2.5"/><path d="M7.3 8.7L13 3M11 5l1.5 1.5M9.5 6.5L11 8"/>',
    puzzle='<path d="M3 5h3V3.8a1.3 1.3 0 0 1 2.6 0V5H12v3.2h-1.2a1.3 1.3 0 0 0 0 2.6H12V13H3z"/>',
    import_='<path d="M8 2.5v7M5 6.5l3 3 3-3"/><path d="M3 10.5V13h10v-2.5"/>',
    finder='<rect x="2.5" y="3" width="11" height="10" rx="2"/><path d="M8 3v10M5.5 6.5v1M10.5 6.5v1"/>',
    pause='<path d="M5.5 4v8M10.5 4v8"/>',
    hist='<path d="M2.8 8a5.2 5.2 0 1 0 1.5-3.7"/><path d="M2.5 2.8v2.2h2.2M8 5v3.2l2 1.3"/>',
    cmd='<path d="M6 6V4.5A1.5 1.5 0 1 0 4.5 6H11.5A1.5 1.5 0 1 0 10 4.5V11.5A1.5 1.5 0 1 0 11.5 10H4.5A1.5 1.5 0 1 0 6 11.5z"/>',
    tab='<rect x="2.5" y="3.5" width="11" height="9" rx="1.5"/><path d="M2.5 6h11"/>',
)
btn = d5.btn

CSS = """
.r4w{position:absolute;inset:0;display:flex;background:#0E0E10}
.r4sb{width:256px;flex:none;background:#121214;position:relative;display:flex;flex-direction:column;padding:8px 8px 0;color:#E9E9EC;font-size:14px}
.r4lights{height:28px;display:flex;align-items:center;gap:8px;padding-left:8px}
.r4lights i{width:12px;height:12px;border-radius:6px;background:#3A3A40;display:block}
.r4addr{height:32px;border-radius:6px;background:#1A1A1D;box-shadow:inset 0 0 0 1px rgba(255,255,255,.06);display:flex;align-items:center;gap:9px;padding:0 8px;margin-top:8px;color:#F2F2F4;font-size:14px}
.r4addr .h{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r4addr .h span{color:#8E8E98}
.r4addr .sg{color:#8E8E98;display:inline-flex}
.r4addr .st{color:#F2F2F4;display:inline-flex}
.r4nav{height:34px;display:flex;align-items:center;gap:16px;padding:0 8px;color:#8E8E98}
.r4nav .sp{flex:1}
.r4row{height:32px;border-radius:6px;display:flex;align-items:center;gap:10px;padding:0 8px;color:#E9E9EC;font-size:14px;white-space:nowrap}
.r4row .t{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis}
.r4row.on{background:#26262A}
.r4row .fv{width:16px;height:16px;border-radius:4px;background:#2A2A2F;color:#C8C8CF;font-size:9px;font-weight:600;display:inline-flex;align-items:center;justify-content:center;flex:none}
.r4row .m{color:#8E8E98;font-size:13px}
.r4sec{height:28px;display:flex;align-items:center;gap:6px;padding:0 8px;margin-top:10px;color:#C8C8CF;font-size:13px}
.r4new{height:32px;border-radius:6px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08);display:flex;align-items:center;gap:10px;padding:0 8px;color:#C8C8CF;margin-top:2px}
.r4new .k{margin-left:auto;font-size:12px;color:#8E8E98}
.r4grow{flex:1}
.r4foot{height:44px;display:flex;align-items:center;gap:6px;margin:0 -8px;padding:0 10px;box-shadow:inset 0 1px 0 rgba(255,255,255,.07)}
.r4foot .sp{flex:1}
.r4gb{width:28px;height:28px;border-radius:6px;display:inline-flex;align-items:center;justify-content:center;color:#9D9DA6;position:relative}
.r4gb.on{background:rgba(255,255,255,.09);color:#F2F2F4}
.r4gb .d{position:absolute;right:4px;top:4px;width:6px;height:6px;border-radius:3px;background:#F2F2F4}
.r4gb .ring{position:absolute;inset:4px;border-radius:10px;box-shadow:inset 0 0 0 2px rgba(255,255,255,.12)}
.r4chip{height:26px;border-radius:7px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.12);display:inline-flex;align-items:center;gap:7px;padding:0 8px;font-size:13px;color:#E9E9EC}
.r4chip.on{background:rgba(255,255,255,.09)}
.r4sw{width:9px;height:9px;border-radius:5px;flex:none}
.r4acct{height:44px;display:flex;align-items:center;gap:10px;padding:0 6px;color:#E9E9EC;font-size:14px}
.r4acct .av{width:22px;height:22px;border-radius:5px;background:#D97757;color:#fff;font-size:11px;font-weight:600;display:inline-flex;align-items:center;justify-content:center}
.r4pg{flex:1;position:relative;margin:6px 6px 6px 0;border-radius:6px;background:#F3F3F5;color:#18181B;padding:28px 34px;overflow:hidden;font-size:14px;line-height:21px}
.r4pg h3{margin:0 0 8px;font-size:24px;line-height:30px;font-weight:700}
.r4pg p{margin:0 0 12px;color:#52525B}
.r4pg .bar{height:9px;border-radius:4px;background:#E2E2E6;margin:0 0 10px}
.r4pg .dim{position:absolute;inset:0;background:rgba(0,0,0,.28)}
/* popovers + menus (Floating) */
.r4pop{position:absolute;border-radius:10px;background:#2A2A2F;box-shadow:inset 0 0 0 1px rgba(255,255,255,.11),0 16px 40px -12px rgba(0,0,0,.7);color:#F2F2F4;font-size:13.5px;overflow:hidden;z-index:5}
.r4pop .hd{display:flex;align-items:center;gap:8px;padding:12px 12px 8px 14px;font-weight:600;font-size:14px}
.r4pop .hd .sp{flex:1}
.r4pop .lb{font-size:12px;color:#9D9DA6;padding:4px 14px 4px}
.r4pop .fld{margin:0 12px 8px;height:30px;border-radius:6px;background:#1E1E22;box-shadow:inset 0 0 0 1px rgba(255,255,255,.10);display:flex;align-items:center;gap:8px;padding:0 9px;font-size:13.5px;color:#F2F2F4}
.r4pop .fld.on{box-shadow:inset 0 0 0 1px rgba(255,255,255,.28)}
.r4pop .fld .sp{flex:1}
.r4pop .fld .m{color:#8E8E98}
.r4pop .act{display:flex;align-items:center;gap:8px;padding:6px 12px 12px}
.r4pop .act .sp{flex:1}
.r4pop .r{min-height:34px;margin:0 4px;border-radius:6px;display:flex;align-items:center;gap:10px;padding:0 10px;font-size:13.5px}
.r4pop .r.hl{background:rgba(255,255,255,.08)}
.r4pop .r .n{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r4pop .r .n small{display:block;font-size:12px;line-height:16px;color:#9D9DA6}
.r4pop .r .m{color:#8E8E98;font-size:12.5px;white-space:nowrap}
.r4pop .r .ic{color:#B4B4BC;display:inline-flex;flex:none}
.r4pop .sep{height:1px;background:rgba(255,255,255,.08);margin:4px 0}
.r4pop .ft{box-shadow:inset 0 1px 0 rgba(255,255,255,.08);padding:4px 0}
.r4bar{height:4px;border-radius:2px;background:rgba(255,255,255,.12);overflow:hidden;margin-top:5px}
.r4bar i{display:block;height:100%;background:#F2F2F4;border-radius:2px}
.r4tg{width:28px;height:16px;border-radius:8px;background:#4A4A50;position:relative;flex:none}
.r4tg:after{content:"";position:absolute;left:2px;top:2px;width:12px;height:12px;border-radius:6px;background:#C8C8CF}
.r4tg.on{background:#F2F2F4}.r4tg.on:after{left:14px;background:#18181B}
.r4kbd{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:11px;color:#8E8E98}
.r4ext{width:18px;height:18px;border-radius:4px;display:inline-flex;align-items:center;justify-content:center;font-size:9px;font-weight:700;color:#fff;flex:none}
/* the Field, centred near the top (R3 position) */
.r4field{position:absolute;left:50%;top:56px;transform:translateX(-50%);width:520px;border-radius:12px;background:#26262B;box-shadow:inset 0 0 0 1px rgba(255,255,255,.11),0 24px 60px -16px rgba(0,0,0,.7);color:#F2F2F4;overflow:hidden;z-index:6}
.r4field .q{height:48px;display:flex;align-items:center;gap:10px;padding:0 14px;font-size:15px;box-shadow:inset 0 -1px 0 rgba(255,255,255,.08)}
.r4field .q .ph{color:#8E8E98}
.r4field .q .cur{display:inline-block;width:1.5px;height:17px;background:#F2F2F4;vertical-align:-3px;margin-left:1px}
.r4field .q .scope{height:22px;border-radius:5px;background:rgba(255,255,255,.10);padding:0 7px;display:inline-flex;align-items:center;gap:5px;font-size:12.5px;color:#E4E4E7}
.r4field .h{font-size:12px;color:#8E8E98;padding:10px 16px 4px}
.r4field .r{height:40px;margin:0 6px;border-radius:7px;display:flex;align-items:center;gap:11px;padding:0 10px;font-size:14px}
.r4field .r.hl{background:rgba(255,255,255,.08)}
.r4field .r .ic{color:#9D9DA6;display:inline-flex;flex:none;width:16px;justify-content:center}
.r4field .r .n{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r4field .r .n .u{color:#8E8E98;margin-left:8px;font-size:13px}
.r4field .r .n mark{background:rgba(252,211,77,.18);color:inherit;border-radius:2px;padding:0 1px}
.r4field .r .m{color:#8E8E98;font-size:12.5px;white-space:nowrap}
.r4field .ft{display:flex;gap:14px;padding:8px 16px 10px;box-shadow:inset 0 1px 0 rgba(255,255,255,.08);font-size:12px;color:#8E8E98;margin-top:6px}
/* sheets (settings-style, as P7) */
.r4set{position:absolute;inset:0;background:#1B1B1F;padding:28px 0 0 40px;color:#F2F2F4}
.r4set h2{margin:0 0 6px;font-size:19.8px;font-weight:600}
.r4set .in{font-size:14.3px;color:#9D9DA6;margin:0 0 14px}
.r4set .sh{font-size:13.2px;color:#9D9DA6;margin:14px 0 6px}
.r4grp{width:584px;border-radius:6px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.09)}
.r4sr{display:flex;align-items:center;gap:12px;padding:9px 12px;min-height:46px}
.r4sr+.r4sr{box-shadow:inset 0 1px 0 rgba(255,255,255,.07)}
.r4sr .t{flex:1;min-width:0}
.r4sr .t b{display:block;font-weight:500;font-size:14.3px;line-height:20px}
.r4sr .t span{display:block;font-size:13.2px;line-height:18px;color:#9D9DA6}
.r4scrim{position:absolute;inset:0;background:rgba(0,0,0,.45);z-index:4}
.r4sht{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);width:500px;border-radius:12px;background:#1F1F23;box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.08),0 24px 60px -16px rgba(0,0,0,.7);overflow:hidden;z-index:5;color:#F2F2F4}
.r4sht .hd{height:44px;display:flex;align-items:center;gap:10px;padding:0 8px 0 16px;box-shadow:inset 0 -1px 0 rgba(255,255,255,.07)}
.r4sht .hd b{font-size:14.3px;font-weight:600;flex:1}
.r4sht .in{padding:12px 16px 0;font-size:13.2px;line-height:19px;color:#9D9DA6}
.r4sht .list{margin:10px 16px 0;border-radius:8px;background:#232328;box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06);overflow:hidden}
.r4sht .lr{display:flex;align-items:center;gap:10px;min-height:44px;padding:6px 12px;font-size:13.2px}
.r4sht .lr+.lr{box-shadow:inset 0 1px 0 rgba(255,255,255,.07)}
.r4sht .lr input{margin:0;width:14px;height:14px;accent-color:#F2F2F4;flex:none}
.r4sht .lr .n{flex:1;min-width:0}
.r4sht .lr .n small{display:block;color:#9D9DA6;font-size:12.5px;line-height:17px}
.r4sht .lr .c{color:#9D9DA6;font-variant-numeric:tabular-nums;white-space:nowrap}
.r4sht .lr.dim .n{color:#9D9DA6}
.r4sht .fn{padding:10px 16px 0;font-size:12.5px;line-height:18px;color:#6E6E78}
.r4sht .ac{display:flex;align-items:center;gap:8px;padding:14px 16px 16px}
.r4sht .ac .sp{flex:1}
.r4sht .pbar{margin:16px 16px 0;height:6px;border-radius:3px;background:#232328;box-shadow:inset 0 0 0 1px rgba(255,255,255,.06);overflow:hidden}
.r4sht .pbar i{display:block;height:100%;background:#F2F2F4;opacity:.75}
.r4sht .big{padding:16px 16px 0;font-size:14.3px;line-height:21px}
.r4sht .big .m{color:#9D9DA6}
.r4sht .lab{font-size:12.5px;color:#9D9DA6;padding:12px 16px 6px}
.r4sht .fld{margin:0 16px;height:32px;border-radius:6px;background:#17171A;box-shadow:inset 0 0 0 1px rgba(255,255,255,.22);display:flex;align-items:center;padding:0 10px;font-size:14px}
.r4sht .sws{display:flex;gap:8px;padding:0 16px}
.r4sht .sws i{width:22px;height:22px;border-radius:11px;display:block}
.r4sht .sws i.on{box-shadow:0 0 0 2px #1F1F23,0 0 0 4px #F2F2F4}
.r4seg{display:flex;flex-direction:column;gap:0;margin:0 16px;border-radius:8px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.09);overflow:hidden}
.r4opt{display:flex;gap:10px;padding:10px 12px;align-items:flex-start;font-size:13.5px}
.r4opt+.r4opt{box-shadow:inset 0 1px 0 rgba(255,255,255,.07)}
.r4opt.on{background:rgba(255,255,255,.05)}
.r4opt .t{flex:1}
.r4opt .t small{display:block;color:#9D9DA6;font-size:12.5px;line-height:18px}
.r4rad{width:14px;height:14px;border-radius:7px;box-shadow:inset 0 0 0 1.5px #6E6E78;flex:none;margin-top:3px}
.r4rad.on{box-shadow:inset 0 0 0 1.5px #F2F2F4,inset 0 0 0 4px #1F1F23,inset 0 0 0 7px #F2F2F4}
.r4toast{position:absolute;left:50%;bottom:18px;transform:translateX(-50%);border-radius:8px;background:#2A2A2F;box-shadow:inset 0 0 0 1px rgba(255,255,255,.11),0 12px 30px -10px rgba(0,0,0,.6);padding:8px 10px 8px 12px;display:flex;align-items:center;gap:10px;font-size:13.2px;white-space:nowrap;color:#F2F2F4;z-index:6}
"""

SPACES = {'Personal': '#8B93F8', 'Work': '#F59E0B', 'Trip': '#34D399'}


# ------------------------------------------------------------------ pieces
def fv(letter):
    return f'<span class="fv">{letter}</span>'


def sidebar(addr=('rust-lang.org', '/learn'), starred=False, rows=None, space='Personal', own=False,
            dl=None, ext_on=False, chip_on=False, pinned=True):
    st = f'<span class="st">{ic("starf", 14)}</span>' if starred else ''
    rows = rows if rows is not None else [('N', 'New tab', False), ('R', 'Learn Rust · rust-lang.org', True)]
    rws = ''.join(f'<div class="r4row{" on" if on else ""}">{fv(l)}<span class="t">{t}</span></div>' for l, t, on in rows)
    pins = (f'<div class="r4sec">Pinned</div><div style="display:flex;gap:6px;padding:0 8px">'
            f'<span class="r4row" style="width:36px;height:36px;padding:0;justify-content:center;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08)">{fv("G")}</span>'
            f'<span class="r4row" style="width:36px;height:36px;padding:0;justify-content:center;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08)">{fv("M")}</span></div>') if pinned else ''
    dlb = ''
    if dl == 'running':
        dlb = f'<span class="r4gb" aria-label="Downloads">{ic("dl", 15)}<span class="d"></span></span>'
    elif dl == 'open':
        dlb = f'<span class="r4gb on" aria-label="Downloads">{ic("dl", 15)}</span>'
    elif dl == 'idle':
        dlb = f'<span class="r4gb" aria-label="Downloads">{ic("dl", 15)}</span>'
    exb = f'<span class="r4gb{" on" if ext_on else ""}" aria-label="Extensions">{ic("puzzle", 15)}</span>'
    key = f'<span style="color:#9D9DA6;display:inline-flex">{ic("key", 12)}</span>' if own else ''
    chip = (f'<span class="r4chip{" on" if chip_on else ""}"><span class="r4sw" style="background:{SPACES.get(space, "#8B93F8")}"></span>'
            f'{space}{key}{ic("chevd", 11)}</span>')
    return (f'<div class="r4sb"><div class="r4lights"><i></i><i></i><i></i></div>'
            f'<div class="r4addr"><span class="sg">{ic("shield", 14)}</span><span class="h">{addr[0]}<span>{addr[1]}</span></span>{st}</div>'
            f'<div class="r4nav">{ic("back", 14)}{ic("fwd", 14)}{ic("reload", 14)}{ic("plus", 14)}<span class="sp"></span>{ic("spark", 14)}{ic("dots", 14)}</div>'
            f'<div class="r4row" style="color:#C8C8CF">{ic("search", 14)}<span class="t">Search</span></div>'
            f'{pins}<div class="r4sec">Today <span style="color:#8E8E98">{len(rows)}</span></div>{rws}'
            f'<div class="r4new">{ic("plus", 14)}New tab<span class="k">⌘T</span></div><div class="r4grow"></div>'
            f'<div class="r4foot">{dlb}{exb}<span class="sp"></span>{chip}</div>'
            f'<div class="r4acct"><span class="av">C</span>Claude Code<span style="flex:1"></span>{ic("chevu", 12)}</div></div>')


def page(title='Learn Rust', host='rust-lang.org', dim=False, body=None):
    b = body or ('<p>Get started with Rust. Affectionately nicknamed “the book,” The Rust Programming Language will give '
                 'you an overview of the language from first principles.</p>'
                 + ''.join(f'<div class="bar" style="width:{w}%"></div>' for w in (88, 72, 93, 64, 81, 90, 58, 76)))
    return f'<div class="r4pg"><h3>{title}</h3>{b}{"<div class=dim></div>" if dim else ""}</div>'


def win(sb, pg, over=''):
    return f'<div class="r4w">{sb}{pg}</div>{over}'


def pop(x, y, w, inner, bottom=None):
    v = f'bottom:{bottom}px' if bottom is not None else f'top:{y}px'
    return f'<div class="r4pop" style="left:{x}px;{v};width:{w}px">{inner}</div>'


def bookmark_pop():
    return pop(14, 92, 300,
               f'<div class="hd">{ic("starf", 14)}Bookmarked<span class="sp"></span><span class="r4kbd">⌘D</span></div>'
               '<div class="lb">Name</div><div class="fld on">Learn Rust<span class="sp"></span></div>'
               f'<div class="lb">Folder</div><div class="fld">{ic("folder", 13)}Bookmarks bar<span class="sp"></span><span class="m">{ic("chevd", 11)}</span></div>'
               f'<div class="act">{btn("Remove", "txt")}<span class="sp"></span>{btn("Done", "pri")}</div>')


def field(q, rows, scope=None, foot=True):
    sc = f'<span class="scope">{ic("starf", 11) if scope == "Bookmarks" else ic("hist", 11)}{scope}<span style="color:#8E8E98">×</span></span>' if scope else ''
    qq = q if q else '<span class="ph">Search bookmarks</span>'
    body = ''
    for r in rows:
        if r[0] == 'h':
            body += f'<div class="h">{r[1]}</div>'
            continue
        _, icn, name, url, meta, hl = r
        u = f'<span class="u">{url}</span>' if url else ''
        body += (f'<div class="r{" hl" if hl else ""}"><span class="ic">{ic(icn, 14)}</span><span class="n">{name}{u}</span>'
                 f'<span class="m">{meta}</span></div>')
    ft = ('<div class="ft"><span>↩ open</span><span>⌘↩ new tab</span><span>⇥ only bookmarks</span>'
          '<span>⌘⌫ remove</span></div>') if foot else ''
    return f'<div class="r4field"><div class="q">{ic("search", 15)}{sc}<span>{qq}</span><span class="cur"></span></div>{body}{ft}</div>'


# ------------------------------------------------------------------ row 1 · bookmarks
def b1a():
    return mini(win(sidebar(starred=True), page(), bookmark_pop()), 664, 470, label='1a · ⌘D bookmarks the page',
                note='⌘D (or the Field command “Bookmark this page”) saves it and opens this popover under the address '
                     'line: name, folder, Remove, Done. ↩ or a click away keeps it. The address line keeps a filled ★ '
                     'while the page is bookmarked; ⌘D again opens the same popover. Folders are the ones you import '
                     'from Chrome plus “Bookmarks bar” and “Other”; there is no bookmarks bar drawn anywhere.')


def b1b():
    rows = [('h', 'Tabs'),
            ('r', 'tab', 'Learn Rust', 'rust-lang.org/learn', 'switch', False),
            ('h', 'Bookmarks'),
            ('r', 'starf', 'The Rust Programming Language', 'doc.rust-lang.org/book', 'Bookmarks bar', True),
            ('r', 'starf', 'Rust by Example', 'doc.rust-lang.org/rust-by-example', 'Other › Learning', False),
            ('h', 'History'),
            ('r', 'hist', 'Tokio tutorial · Spawning', 'tokio.rs/tokio/tutorial/spawning', '2 days ago', False),
            ('r', 'hist', 'Pin and suffering', '<mark>rust</mark> · “…self-referential futures in <mark>Rust</mark> need Pin…”', 'last week', False),
            ('h', 'Commands'),
            ('r', 'starf', 'Bookmarks', '', 'all, by folder', False)]
    return mini(win(sidebar(), page(dim=True), field('rust', rows)), 664, 640,
                label='1b · The Field: bookmarks and history beside tabs',
                note='Groups in this order: open tabs (↩ switches), bookmarks (★, folder at the end), history (clock, '
                     'when), commands. History matches title, address and words on the page (local, browser.db FTS5, '
                     'pages you stayed on ≥ 10 s); a words match shows the line it came from. At most 3 rows a group '
                     'until you press ⇥ to see only that group. ⌘⌫ removes a history row or a bookmark.')


def b1c():
    rows = [('r', 'folder', 'Bookmarks bar', '', '24', True),
            ('r', 'folder', 'Other', '', '112', False),
            ('r', 'folder', 'Imported from Chrome', '', '412', False),
            ('h', 'Recently added'),
            ('r', 'starf', 'Learn Rust', 'rust-lang.org/learn', 'just now', False),
            ('r', 'starf', 'Hacker News', 'news.ycombinator.com', 'yesterday', False)]
    return mini(win(sidebar(), page(dim=True), field('', rows, scope='Bookmarks')), 664, 640,
                label='1c · Bookmarks, browsed in the Field',
                note='The Bookmarks command (or ⇥ on the Bookmarks group) scopes the Field: the scope pill, folders '
                     'first, then recently added. ↩ on a folder opens it in place, ⌫ on an empty query goes up. Typing '
                     'filters inside the scope. History has the same scoped view (days as headings, “Clear history…” '
                     'as its last row). No separate bookmarks window or page.')


# ------------------------------------------------------------------ row 2 · Chrome import
def settings_import():
    return ('<div class="r4set"><h2>Browser</h2><div class="in">How Baaz browses: the assistant, tabs, search, downloads.</div>'
            '<div class="sh">Your data</div><div class="r4grp">'
            f'<div class="r4sr"><div class="t"><b>Import from Chrome</b><span>Bookmarks, history and passwords. Chrome '
            f'is only read; nothing in it changes.</span></div>{btn("Import…", "sec")}</div>'
            f'<div class="r4sr"><div class="t"><b>History</b><span>18,204 pages · kept 90 days · never in a private '
            f'window</span></div>{btn("Clear…", "txt")}</div>'
            f'<div class="r4sr"><div class="t"><b>Don’t remember these sites</b><span>bank.example, mail.example</span>'
            f'</div>{btn("Edit…", "txt")}</div></div>'
            '<div class="sh">Assistant · Agents · Search · Downloads · Default browser</div>'
            '<div style="font-size:12px;color:#6E6E78">… unchanged from R3.</div></div>')


def import_sheet(state):
    hd = f'<div class="hd"><b>Import from Chrome</b><span class="r4gb">{ic("x", 13)}</span></div>'
    if state == 'pick':
        body = ('<div class="in">Found Chrome on this Mac. Baaz reads a copy; Chrome is never changed.</div>'
                '<div class="lab">Profile</div><div style="margin:0 16px"><div class="r4pop" style="position:relative;'
                f'box-shadow:inset 0 0 0 1px rgba(255,255,255,.10);background:#17171A"><div class="r" style="margin:0;padding:0 10px">'
                f'<span class="r4ext" style="background:#4F7DF3;border-radius:10px">B</span><span class="n">Person 1 · your main profile</span>'
                f'<span class="m">{ic("chevd", 11)}</span></div></div></div>'
                '<div class="list">'
                '<div class="lr"><input type="checkbox" checked><div class="n">Bookmarks<small>Into a folder “Imported from Chrome”</small></div><span class="c">412</span></div>'
                '<div class="lr"><input type="checkbox" checked><div class="n">History<small>The last 90 days. Page words are not in Chrome’s history, so only titles and addresses search until you visit again.</small></div><span class="c">18,204</span></div>'
                '<div class="lr"><input type="checkbox"><div class="n">Passwords<small>Through Chromium’s own importer into Baaz’s password manager; macOS asks for your password first. NEEDS A SPIKE: if CEF’s password manager is off, this row is left out.</small></div><span class="c">—</span></div>'
                '</div><div class="fn">Cookies and sign-ins are not imported: Chrome locks them to itself. You sign in to each site once.</div>'
                f'<div class="ac"><span class="sp"></span>{btn("Cancel", "sec")}{btn("Import", "pri")}</div>')
    elif state == 'run':
        body = ('<div class="big">Importing history… <span class="m">9,140 of 18,204</span></div>'
                '<div class="pbar"><i style="width:50%"></i></div>'
                '<div class="list" style="margin-top:14px">'
                f'<div class="lr"><span style="color:#86EFAC;display:inline-flex">{ic("check", 13)}</span><div class="n">Bookmarks</div><span class="c">412</span></div>'
                '<div class="lr"><span class="r4gb" style="width:13px;height:13px"><span class="ring" style="inset:0"></span></span><div class="n">History</div><span class="c">9,140</span></div>'
                '</div><div class="fn">You can close this; the import carries on and a toast says when it’s done.</div>'
                f'<div class="ac"><span class="sp"></span>{btn("Close", "sec")}</div>')
    else:
        body = ('<div class="big">Imported 412 bookmarks and 18,204 pages of history.<br><span class="m">Bookmarks are in '
                '“Imported from Chrome”. Type in the Field (⌘L) to find any of them.</span></div>'
                f'<div class="ac">{btn("Undo import", "txt")}<span class="sp"></span>{btn("Done", "pri")}</div>')
    return f'<div class="r4scrim"></div><div class="r4sht">{hd}{body}</div>'


def b2a():
    return mini(settings_import(), 664, 470, label='2a · Settings › Browser › Your data',
                note='One new group on the existing Browser page: Import from Chrome…, History (size, how long it is '
                     'kept, Clear…) and the sites history never records. The same Import… also appears once as a line '
                     'in the Field the first time Baaz is the default browser and Chrome is found.')


def b2b():
    return mini(settings_import() + import_sheet('pick'), 664, 560, label='2b · The import sheet',
                note='One sheet. The Chrome profile (a menu only when there are several), three ticks with counts read '
                     'from a copy of Chrome’s files. Passwords are off by default and say what will happen. Cookies are '
                     'stated as not imported. Import runs in the background.')


def b2c():
    return mini(settings_import() + import_sheet('run'), 664, 470, label='2c · Importing',
                note='Bookmarks first (instant), then history in batches. Close keeps it running.')


def b2d():
    return mini(settings_import() + import_sheet('done'), 664, 470, label='2d · Done, with Undo',
                note='Undo removes exactly what this import added (the folder and the rows it wrote), never your own '
                     'bookmarks or history. Running the import again adds only what is new.')


# ------------------------------------------------------------------ row 3 · spaces with own sign-ins
def space_menu():
    return pop(96, None, 210,
               '<div style="height:6px"></div>'
               f'<div class="r"><span class="r4sw" style="background:#8B93F8"></span><span class="n">Personal</span><span class="m">⌃1</span></div>'
               f'<div class="r hl"><span class="r4sw" style="background:#F59E0B"></span><span class="n">Work</span>'
               f'<span class="ic" style="color:#9D9DA6">{ic("key", 12)}</span><span class="m">⌃2</span><span class="ic">{ic("check", 12)}</span></div>'
               f'<div class="r"><span class="r4sw" style="background:#34D399"></span><span class="n">Trip</span><span class="m">⌃3</span></div>'
               '<div class="sep"></div>'
               f'<div class="r"><span class="ic">{ic("plus", 13)}</span><span class="n">New space…</span></div>'
               f'<div class="r"><span class="ic">{ic("edit", 13)}</span><span class="n">Edit “Work”…</span></div>'
               '<div style="height:4px"></div>', bottom=94)


def space_sheet():
    return ('<div class="r4scrim"></div><div class="r4sht" style="width:460px">'
            f'<div class="hd"><b>New space</b><span class="r4gb">{ic("x", 13)}</span></div>'
            '<div class="lab">Name</div><div class="fld">Work</div>'
            '<div class="lab">Colour</div><div class="sws"><i style="background:#8B93F8"></i><i class="on" style="background:#F59E0B"></i>'
            '<i style="background:#34D399"></i><i style="background:#F472B6"></i><i style="background:#60A5FA"></i><i style="background:#A1A1AA"></i></div>'
            '<div class="lab">Sign-ins</div><div class="r4seg">'
            '<div class="r4opt"><span class="r4rad"></span><div class="t">Shared with your other spaces<small>One set of cookies and '
            'logins. Signed in once, signed in everywhere.</small></div></div>'
            f'<div class="r4opt on"><span class="r4rad on"></span><div class="t">Its own {ic("key", 12)}<small>Separate cookies, '
            'logins and site data. Sign in to a second Google or GitHub account here. Saved passwords stay shared.</small></div></div>'
            '</div><div class="fn">You can change this later. Switching a space back to shared signs it out of everything it '
            'signed in to on its own.</div>'
            f'<div class="ac"><span class="sp"></span>{btn("Cancel", "sec")}{btn("Create", "pri")}</div></div>')


def b3a():
    sb = sidebar(addr=('mail.google.com', '/mail/u/0'), space='Work', own=True, chip_on=True, dl='idle',
                 rows=[('G', 'Inbox · work@acme.com · Gmail', True), ('L', 'Linear · Sprint 42', False)], pinned=False)
    return mini(win(sb, page('Inbox', body='<p>Sprint 42 planning · 3 new</p>' + ''.join(f'<div class=bar style="width:{w}%"></div>' for w in (90, 70, 84, 66)))) + space_menu(), 664, 520, label='3a · The space menu',
                note='The footer chip opens it: each space with its colour and ⌃1–9; a key marks a space with its own '
                     'sign-ins. ✓ is the one shown. Switching swaps the tab list (tabs, folders, pins) and the page; a '
                     'space with its own sign-ins also swaps cookies. Task folders show in every space.')


def b3b():
    sb = sidebar(space='Personal')
    return mini(win(sb, page(dim=True)) + space_sheet(), 664, 560, label='3b · New space (and Edit)',
                note='Name, colour, and one choice: shared sign-ins (the default, as today) or its own. Its own = a '
                     'separate Chromium request context with its own cache folder; Edit opens the same sheet with '
                     'Delete space at the bottom left.')


def b3c():
    sb = sidebar(addr=('accounts.google.com', '/signin'), space='Work', own=True, rows=[('G', 'Sign in – Google Accounts', True)], pinned=False)
    body = ('<p style="color:#18181B;font-size:16px">Choose an account</p>'
            '<div style="border-radius:8px;box-shadow:inset 0 0 0 1px #DADADE;padding:12px 14px;width:320px;margin-bottom:10px">'
            'Use another account</div>'
            + ''.join(f'<div class="bar" style="width:{w}%"></div>' for w in (60, 44)))
    return mini(win(sb, page('Sign in', 'accounts.google.com', body=body)), 664, 470,
                label='3c · A space with its own sign-ins, first visit',
                note='Nothing else marks it: the chip shows the key, and sites see a browser that has never signed in. '
                     'The address line, the Field and the agent behave as in any space; an agent tab opened from this '
                     'space uses this space’s sign-ins.')


# ------------------------------------------------------------------ row 4 · footer downloads + extensions
def dl_pop():
    rows = (f'<div class="r"><span class="ic">{ic("file", 15)}</span><div class="n">rustup-init.dmg<small>12.4 of 18 MB · 6 s left</small>'
            f'<div class="r4bar"><i style="width:68%"></i></div></div><span class="r4gb">{ic("pause", 13)}</span><span class="r4gb">{ic("x", 12)}</span></div>'
            f'<div class="r hl"><span class="ic">{ic("file", 15)}</span><div class="n">invoice-1042.pdf<small>212 KB · Done</small></div>'
            f'<span class="r4gb">{ic("finder", 13)}</span></div>'
            f'<div class="r"><span class="ic">{ic("img", 15)}</span><div class="n">western-ghats.jpg<small>3.1 MB · yesterday</small></div></div>'
            f'<div class="r"><span class="ic" style="color:#FCA5A5">{ic("warn", 15)}</span><div class="n">dataset.zip<small style="color:#FCA5A5">Failed · network</small></div>'
            f'<span class="r4gb">{ic("reload", 13)}</span></div>')
    return pop(8, None, 300, f'<div class="hd">Downloads<span class="sp"></span>{btn("Clear", "txt")}</div>{rows}'
                            f'<div class="ft"><div class="r"><span class="ic">{ic("finder", 13)}</span><span class="n">Open Downloads folder</span></div></div>', bottom=94)


def ext_pop(empty=False):
    if empty:
        inner = ('<div class="hd">Extensions</div><div style="padding:0 14px 10px;color:#9D9DA6;font-size:13px;line-height:19px">'
                 'No extensions yet. Baaz runs Chrome extensions from the Chrome Web Store.</div>'
                 f'<div class="ft"><div class="r"><span class="ic">{ic("ext", 13)}</span><span class="n">Chrome Web Store</span></div></div>')
        return pop(36, None, 280, inner, bottom=94)
    rows = (f'<div class="r hl"><span class="r4ext" style="background:#2F6EEA">1P</span><span class="n">1Password</span><span class="m">click to open</span></div>'
            f'<div class="r"><span class="r4ext" style="background:#C2410C">uB</span><span class="n">uBlock Origin Lite</span><span class="r4tg"></span></div>'
            f'<div class="r"><span class="r4ext" style="background:#0F766E">Df</span><span class="n">Dark Reader</span><span class="r4tg on"></span></div>')
    return pop(36, None, 280, f'<div class="hd">Extensions</div>{rows}<div class="ft">'
                            f'<div class="r"><span class="ic">{ic("puzzle", 13)}</span><span class="n">Manage extensions</span></div>'
                            f'<div class="r"><span class="ic">{ic("ext", 13)}</span><span class="n">Chrome Web Store</span></div></div>', bottom=94)


def b4a():
    return mini(win(sidebar(dl='open'), page()) + dl_pop(), 664, 520, label='4a · Downloads, from the footer',
                note='The downloads button moves from the control row to the footer, left of extensions; a dot while '
                     'anything runs, and the button is always there (no more appearing and vanishing). The popover '
                     'is the existing one at Floating, anchored above the button: progress with pause and cancel, '
                     'Show in Finder, Retry on a failure, Clear, the folder at the foot.')


def b4b():
    return mini(win(sidebar(dl='idle', ext_on=True), page()) + ext_pop(), 664, 520, label='4b · Extensions',
                note='Chromium’s own extensions, listed by Baaz. Click a row = the extension’s action (its popup opens '
                     'over the page as Chrome draws it). The toggle turns one off. Manage opens chrome://extensions in '
                     'a tab; the Web Store link opens the store, whose Add to Chrome installs here. NEEDS A SPIKE: stock CEF lists no '
                     'extensions today (chrome.rs); if CEF 154 cannot install or run them, this becomes 4c and I come back to you.')


def b4c():
    return mini(win(sidebar(dl='idle', ext_on=True), page()) + ext_pop(empty=True), 664, 520, label='4c · No extensions yet',
                note='The empty state: one sentence and the store. Ad blocking is Baaz’s own (adblock-rust, on by '
                     'default, a per-site “Allow ads here” in the address line’s menu) and needs no extension.')


def b_r4(src):
    rows = [[b1a(), b1b()], [b1c(), b2a()], [b2b(), b2c(), ], [b2d(), b3a()], [b3b(), b3c()], [b4a(), b4b()], [b4c()]]
    return grid_board('R4 · The daily driver — bookmarks, Chrome import, Spaces with own sign-ins, footer — PROPOSAL '
                      '(2026-11-11, not built)',
                      'Your 11 Nov answers: ⇧-click Peek, ⌥-click Split (both already drawn in the gallery and wired, '
                      'not redrawn here); import tested on bookmarks and history only. Four new things here, each '
                      'built from parts you already use: ⌘D and a ★ in the address line; the Field gains bookmark '
                      'and history groups and a scoped view (no bookmarks window or bar); one import sheet; a '
                      'sign-ins choice on a space; the footer gets downloads and extensions popovers.', rows, 4760)


if __name__ == "__main__":
    g.D6CSS += d5.CSS + CSS
    g.page6("R4Daily", "R4 · Daily driver (proposal)", b_r4, 1440, 4760)
    print("wrote R4Daily")
