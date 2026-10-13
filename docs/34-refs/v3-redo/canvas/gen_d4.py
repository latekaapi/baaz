#!/usr/bin/env python3
"""D4 — the transcript on the v3 ladder, plus homes for the three homeless controls.

Writes project/D4*.dc.html + preview/D4*.html next to the v3 boards, and prints the canvas.json entries to add.
Every piece of content is lifted from a fixture capture (fixtures/msp/*.jsonl, offscreen renders of 2026-10-13);
nothing is invented. Values: docs/35 ladder, FF roles ×1.1 text scale (the app's), spacing unscaled."""
import os, json, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from gen_canvas import (CSS, VARS, PREVIEW_VARS, BLOBS, LOCAL, ico, I, ib, lights, switch, img, composer_a,
                        muse_mark, actions, FG, MUTED, FAINT, HOVER, SEL, HAIR, EDGE, L0, L1, L2, L2h, L3, WELL)
ROOT = os.path.dirname(os.path.abspath(__file__))
P = ROOT + "/project"; V = ROOT + "/preview"

SUCCESS, WARNING, DANGER = "#86EFAC", "#FCD34D", "#FCA5A5"
KW, TY = "#B88DEA", "#7AA4EF"   # the app's highlighter, sampled from the 2026-10-12 capture

I.update(
 ok='<path d="M3.5 8.5l3 3 6-7"/>',
 x='<path d="M4 4l8 8M12 4l-8 8"/>',
 chevr='<path d="M6 4l4 4-4 4"/>', chevd='<path d="M4 6l4 4 4-4"/>', chevu='<path d="M4 10l4-4 4 4"/>',
 edit='<path d="M11.5 2.5l2 2L6 12H4v-2z"/>', copy='<rect x="5" y="5" width="8" height="8" rx="1.5"/><path d="M3 11V4a1 1 0 0 1 1-1h7"/>',
 resend='<path d="M13 8a5 5 0 1 1-1.5-3.6M13 3v2.5h-2.5"/>', fork='<circle cx="4" cy="4" r="1.5"/><circle cx="12" cy="4" r="1.5"/><circle cx="8" cy="12" r="1.5"/><path d="M4 5.5v1a2 2 0 0 0 2 2h4a2 2 0 0 0 2-2v-1M8 8.5v2"/>',
 file='<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>', play='<path d="M5 3l8 5-8 5z"/>',
 q='<circle cx="8" cy="8" r="6"/><path d="M6.2 6.3a1.8 1.8 0 1 1 2.6 1.6c-.6.3-.8.6-.8 1.1M8 11.2v.1"/>',
 think='<circle cx="8" cy="8" r="6"/><path d="M8 7v4M8 5.2v.1"/>',
 wrap='<path d="M3 4h10M3 8h8a2 2 0 0 1 0 4H9M3 12h3M9 10l-1.5 2L9 14"/>',
 dl='<path d="M8 2v8M4.5 6.5L8 10l3.5-3.5M3 13h10"/>',
 msgplus='<path d="M3 3h10v7H7l-3 3v-3H3z"/><path d="M8 4.8v3.4M6.3 6.5h3.4"/>',
 warn='<path d="M8 2l6.5 11h-13z"/><path d="M8 6.5v3M8 11.5v.1"/>',
 sqr='<rect x="4" y="4" width="8" height="8" rx="1.5" fill="currentColor" stroke="none"/>',
 arrowdn='<path d="M8 3v10M4 9l4 4 4-4"/>',
 term2='<path d="M3 4l4 4-4 4M8 12h5"/>',
)

D4CSS = """
.tx{font-size:14.3px;line-height:22px;color:#F2F2F4;width:880px}
.tx *{box-sizing:border-box}
.bk{display:flex;flex-direction:column;gap:8px}
.gapturn{height:16px}
.u{display:flex;flex-direction:column;align-items:flex-end;gap:4px}
.u .bub{max-width:686px;background:var(--l2);border-radius:12px;box-shadow:var(--raised-sh);padding:9px 13px}
.u .rail{display:flex;align-items:center;gap:2px;height:24px}
.u .age{font-size:12.1px;color:#6E6E78;margin-right:6px}
.ibs{width:28px;height:28px;border-radius:6px;display:flex;align-items:center;justify-content:center;color:#9D9DA6;background:transparent;border:0;padding:0;flex:none}
.ibs.sm{width:24px;height:24px}
.ibs:hover{background:rgba(255,255,255,.05);color:#F2F2F4}
.a p{margin:0 0 10px}.a p:last-child{margin-bottom:0}
.a h2{font-size:17.6px;line-height:24px;font-weight:600;margin:0 0 10px}
.a ul{margin:0 0 10px;padding-left:22px}.a li{margin:0}
.a a{color:#F2F2F4;text-decoration:underline;text-underline-offset:3px;text-decoration-color:#6E6E78}
.ic{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;background:rgba(255,255,255,.07);border-radius:4px;padding:1px 5px}
.arail{display:flex;align-items:center;gap:2px;height:28px;margin-top:8px}
.meta{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;line-height:16px;color:#6E6E78;margin-top:10px;display:flex;gap:10px}
.qrow{display:flex;align-items:center;gap:8px;height:32px;padding:0 8px;margin:0 -8px;border-radius:6px;color:#9D9DA6;white-space:nowrap;background:transparent;border:0;font-size:14.3px;text-align:left;width:calc(100% + 16px)}
.qrow:hover,.qrow.hov{background:rgba(255,255,255,.05);color:#F2F2F4}
.qrow .chev{width:12px;height:12px;flex:none;display:flex;align-items:center;justify-content:center}
.qrow .mono{font-size:12.65px}
.chip{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;display:inline-flex;gap:5px}
.card{background:var(--l2);border-radius:8px;box-shadow:var(--raised-sh);overflow:hidden}
.card.lit{background:var(--l2h)}
.card.need{box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(252,211,77,.7)}
.card.bad{box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(252,165,165,.7)}
.ch{height:34px;display:flex;align-items:center;gap:8px;padding:0 8px 0 12px;white-space:nowrap}
.ch .g{width:14px;height:14px;flex:none;display:flex;align-items:center;justify-content:center}
.ch .verb{color:#F2F2F4}.ch .tgt{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;color:#9D9DA6}
.ch .r{margin-left:auto;display:flex;align-items:center;gap:8px}
.ch .rs{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;color:#6E6E78}
.ch .rs.bad{color:#FCA5A5}
.ch .chev{width:12px;height:12px;color:#9D9DA6;display:flex;align-items:center;justify-content:center;margin-left:4px}
.cb{border-top:1px solid rgba(255,255,255,.07)}
.well{background:var(--well);padding:10px 12px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px;white-space:pre}
.more{height:24px;display:flex;align-items:center;gap:6px;padding:0 10px;font-size:13.2px;color:#9D9DA6;background:var(--well)}
.more .ot{margin-left:auto}
.prev{padding:6px 10px 8px;display:flex;flex-direction:column;gap:6px;font-size:13.2px;color:#9D9DA6;border-top:1px solid rgba(255,255,255,.07)}
.prev div{display:flex;align-items:center;gap:8px}
.prev .mono{font-size:12.1px}
.nest{padding:0 8px 8px;display:flex;flex-direction:column;gap:8px}
.code{background:var(--l2);border-radius:8px;box-shadow:var(--raised-sh);overflow:hidden}
.code .h{height:32px;background:var(--l2h);display:flex;align-items:center;gap:8px;padding:0 6px 0 12px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;color:#9D9DA6}
.code .h .r{margin-left:auto;display:flex;gap:2px}
.code .b{padding:10px 12px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px;white-space:pre;display:grid;grid-template-columns:22px 1fr;column-gap:8px}
.code .n{color:#6E6E78;text-align:right}
.tbl{background:var(--l2);border-radius:8px;box-shadow:var(--raised-sh);overflow:hidden;width:100%;border-collapse:separate;border-spacing:0}
.tbl th{background:var(--l2h);font-weight:600;text-align:left;padding:10px 14px}
.tbl td{padding:10px 14px;border-top:1px solid rgba(255,255,255,.07)}
.diff{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px;white-space:pre;padding:6px 0}
.diff div{display:grid;grid-template-columns:36px 20px 1fr;column-gap:8px;padding:0 12px 0 8px}
.diff .n{color:#6E6E78;text-align:right}
.diff .add{background:rgba(134,239,172,.10)}.diff .del{background:rgba(252,165,165,.10)}
.hd{display:flex;align-items:flex-start;gap:10px;padding:10px 12px}
.tile{width:26px;height:26px;border-radius:7px;background:var(--l2h);box-shadow:var(--raised-sh);display:flex;align-items:center;justify-content:center;color:#9D9DA6;flex:none}
.hd .t{font-weight:600}.hd .s{font-size:13.2px;line-height:18px;color:#9D9DA6}
.tag{font-size:13.2px;line-height:18px;padding:2px 8px;border-radius:6px;background:rgba(255,255,255,.07);color:#9D9DA6;white-space:nowrap}
.tag.warn{background:rgba(252,211,77,.12);color:#FCD34D}
.cmd{margin:0 12px 10px;background:var(--well);border-radius:6px;padding:8px 10px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px}
.foot{display:flex;align-items:center;gap:8px;padding:8px 12px;border-top:1px solid rgba(255,255,255,.07)}
.foot .hint{font-size:13.2px;color:#6E6E78;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px}
.foot .sp{flex:1}
.ctl2{height:28px;padding:0 10px;border-radius:6px;display:inline-flex;align-items:center;gap:6px;color:#9D9DA6;font-size:14.3px;white-space:nowrap;background:transparent;border:0}
.ctl2:hover{background:rgba(255,255,255,.05);color:#F2F2F4}
.ctl2.sec{background:var(--l2h);box-shadow:var(--raised-sh);color:#F2F2F4}
.ctl2.pri{background:#F2F2F4;color:#121215;font-weight:600}
.opt{display:flex;align-items:flex-start;gap:10px;padding:8px 10px;margin:0 8px;border-radius:6px}
.opt:hover,.opt.hov{background:rgba(255,255,255,.05)}
.opt.sel{background:rgba(255,255,255,.09)}.opt.sel .l{font-weight:600}
.radio{width:16px;height:16px;border-radius:50%;box-shadow:inset 0 0 0 1.5px #9D9DA6;flex:none;margin-top:3px;position:relative}
.radio.on{box-shadow:inset 0 0 0 1.5px #F2F2F4}.radio.on::after{content:"";position:absolute;inset:4px;border-radius:50%;background:#F2F2F4}
.opt .d{font-size:13.2px;line-height:18px;color:#9D9DA6}
.key{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;color:#6E6E78;background:rgba(255,255,255,.07);border-radius:4px;padding:1px 6px;line-height:16px}
.disc{display:flex;align-items:center;gap:6px;padding:4px 10px 4px 18px;font-size:13.2px;color:#9D9DA6}
.th{height:32px;display:flex;align-items:center;gap:8px;padding:0 8px 0 12px}
.bar{height:3px;background:rgba(255,255,255,.07)}.bar i{display:block;height:3px;background:#9D9DA6}
.trow{height:30px;display:flex;align-items:center;gap:10px;padding:0 12px;border-top:1px solid rgba(255,255,255,.07)}
.mk{width:15px;height:15px;border-radius:50%;flex:none;position:relative}
.mk.p{box-shadow:inset 0 0 0 1.5px #9D9DA6}
.mk.r{box-shadow:inset 0 0 0 1.5px #F2F2F4}.mk.r::after{content:"";position:absolute;inset:4.5px;border-radius:50%;background:#F2F2F4}
.mk.d{background:#86EFAC}
.trow.done span{color:#9D9DA6;text-decoration:line-through}
.dl{display:grid;grid-template-columns:92px 1fr;row-gap:3px;column-gap:12px;padding:0 12px 12px;font-size:13.2px;line-height:18px}
.dl .k{color:#9D9DA6}
.st{height:28px;display:flex;align-items:center;gap:8px;font-size:13.2px;color:#9D9DA6;white-space:nowrap}
.spin{width:12px;height:12px;border-radius:50%;border:1.5px solid rgba(157,157,166,.35);border-top-color:#9D9DA6;flex:none}
.jump{height:28px;border-radius:999px;background:var(--l3);box-shadow:var(--float-sh);display:inline-flex;align-items:center;gap:6px;padding:0 12px 0 10px;font-size:13.2px;color:#F2F2F4}
.jump b{font-weight:400;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:10.5px;background:rgba(255,255,255,.09);border-radius:999px;padding:2px 6px;line-height:12px}
.toast{height:36px;border-radius:10px;background:var(--l3);box-shadow:var(--float-sh);display:inline-flex;align-items:center;gap:8px;padding:0 6px 0 12px;white-space:nowrap;max-width:480px}
.toast .dot{width:8px;height:8px;border-radius:50%;flex:none}
.toast .t{font-weight:600}.toast .b{color:#9D9DA6;overflow:hidden;text-overflow:ellipsis}
.toast .n{font-size:13.2px;color:#6E6E78}
.fb{height:36px;border-radius:10px;background:var(--l3);box-shadow:var(--float-sh);display:inline-flex;align-items:center;gap:4px;padding:0 4px}
.fb .cnt{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;color:#6E6E78;padding:0 4px}
.lbl{position:absolute;width:224px;font-size:12px;line-height:17px;color:#9D9DA6}
.lbl b{display:block;color:#F2F2F4;font-weight:600;font-size:13px;margin-bottom:2px}
.sb{position:absolute;left:0;top:44px;width:256px;font-size:14.3px;line-height:22px}
.sb .r{height:32px;display:flex;align-items:center;gap:12px;padding:0 20px;color:#F2F2F4}
.sb .r .k{margin-left:auto;color:#9D9DA6}
.sb .r svg{color:#9D9DA6}
.sb .grp{height:30px;display:flex;align-items:flex-end;padding:0 16px 4px;font-size:13.2px;color:#9D9DA6}
.sb .sess{margin:0 8px;height:42px;border-radius:6px;background:rgba(255,255,255,.09);display:flex;align-items:center;gap:10px;padding:0 10px 0 8px}
.sb .sess .dot{width:7px;height:7px;border-radius:50%;box-shadow:inset 0 0 0 1px #9D9DA6;flex:none}
.sb .sess .dot.need{background:#FCD34D;box-shadow:none}
.sb .sess .tt{font-weight:600;line-height:18px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.sb .sess .by{font-size:13.2px;line-height:17px;color:#9D9DA6;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.sb .sess .when{margin-left:auto;font-size:13.2px;color:#9D9DA6;flex:none}
.sbfoot{position:absolute;left:0;bottom:0;width:256px;height:48px;display:flex;align-items:center;gap:10px;padding:0 16px;font-weight:600;font-size:14.3px}
.sbfoot .av{width:24px;height:24px;border-radius:6px;background:#C9444C;color:#fff;font-size:12px;display:flex;align-items:center;justify-content:center}
.bfade{position:absolute;left:0;right:0;bottom:0;height:128px;background:linear-gradient(rgb(from var(--l1) r g b / 0),var(--l1) 32px);pointer-events:none}
"""

def g(name, size=12, sw="1.5", color=None):
    s = f' style="color:{color}"' if color else ''
    return f'<span{s} style="display:inline-flex;{"color:"+color+";" if color else ""}">{ico(I[name], sw=sw, size=size)}</span>'

def glyph(kind):
    return {'ok': g('ok', 14, "1.8", SUCCESS), 'bad': g('x', 14, "1.8", DANGER), 'spin': '<span class="spin"></span>'}[kind]

def ibs(name, label, sm=False):
    return f'<button class="ibs{" sm" if sm else ""}" aria-label="{label}">{ico(I[name], size=12)}</button>'

# ---------------- turns ----------------
def user_turn(text, age=None, w=None, hover=False):
    """At rest only the bubble. On hover (the turn's own box) the rail: edit · copy, then the age in faint mono."""
    rail = f'<div class="rail">{ibs("edit","Edit")}{ibs("copy","Copy")}{"<span class=\"age mono\">"+age+"</span>" if age else ""}</div>' if hover else '<div class="rail"></div>'
    bub = f'<div class="bub"{" style=\"width:"+str(w)+"px\"" if w else ""}>{text}</div>'
    return f'<div class="u">{bub}{rail}</div>'

def assistant_turn(html, footer=None, rail=True, hover=False):
    """At rest only the prose. On hover: one copy button and the footer meta under the last block."""
    out = f'<div class="a">{html}</div>'
    if rail and hover:
        out += f'<div class="arail">{ibs("copy","Copy")}</div>'
        if footer:
            out += '<div class="meta">' + '<span>·</span>'.join(f'<span>{p}</span>' for p in footer) + '</div>'
    elif rail:
        out += '<div class="arail"></div>'
    return out

def quiet_row(glyph_html, text, open=False, mono=None, extra='', hov=False):
    chev = ico(I['chevd' if open else 'chevr'], size=12)
    return (f'<button class="qrow{" hov" if hov else ""}"><span class="chev">{chev}</span>{glyph_html}<span>{text}</span>'
            f'{"<span class=\"mono\">"+mono+"</span>" if mono else ""}{extra}</button>')

def fold_row(text, open=False, diff=None, hov=False):
    extra = f'<span class="chip" style="margin-left:4px"><span style="color:{SUCCESS}">+{diff[0]}</span><span style="color:{DANGER}">−{diff[1]}</span></span>' if diff else ''
    return quiet_row('', text, open=open, extra=extra, hov=hov)

def think_row(text, summary=None, open=False):
    return quiet_row(g('think', 14, "1.5", MUTED), text, open=open, extra=(f'<span>· {summary}</span>' if summary else ''))

def think_body(text):
    return f'<div style="padding:0 0 4px 32px;font-size:13.2px;line-height:20px;color:{MUTED}">{text}</div>'

def resolved_row(kind, label, mono=None, tail=None):
    extra = f'<span class="mono" style="color:{FAINT}">{tail}</span>' if tail else ''
    return f'<div class="qrow" style="cursor:default">{glyph(kind)}<span>{label}</span>{"<span class=\"mono\">"+mono+"</span>" if mono else ""}{extra}</div>'

def live_row(verb, cmd, earlier=None, elapsed=None):
    e = f'<span style="color:{FAINT}">+{earlier} earlier</span>' if earlier else ''
    t = f'<span class="mono" style="color:{FAINT}">{elapsed}</span>' if elapsed else ''
    return f'<div class="qrow">{glyph("spin")}<span>{verb}</span><span class="mono">{cmd}</span>{e}{t}</div>'

# ---------------- cards ----------------
def tool_header(kind, verb, target, right='', open=True, lit=False):
    return (f'<div class="ch"><span class="g">{glyph(kind)}</span><span class="verb">{verb}</span><span class="tgt">{target}</span>'
            f'<span class="r">{right}<span class="chev">{ico(I["chevd" if open else "chevr"], size=12)}</span></span></div>')

def rs(text, bad=False): return f'<span class="rs{" bad" if bad else ""}">{text}</span>'
def run_in_terminal(label="Run in terminal"):
    return f'<button class="ctl2" style="height:24px;font-size:13.2px;padding:0 8px;gap:5px">{ico(I["play"], size=11)}{label}</button>'

def shell_card(verb, cmd, out, kind='ok', right='', open=True, lit=False, more=None):
    body = f'<div class="cb"><div class="well">{out}</div>' + (f'<div class="more">{ico(I["chevd"],size=11)}{more} more lines</div>' if more else '') + '</div>' if open else ''
    return f'<div class="card{" lit" if lit else ""}">{tool_header(kind, verb, cmd, right, open)}{body}</div>'

def read_card(path, lines, open=False, body=None, lit=False):
    b = f'<div class="cb"><div class="well">{body}</div></div>' if open and body else ''
    return f'<div class="card{" lit" if lit else ""}">{tool_header("ok", "Read", path, rs(lines), open)}{b}</div>'

def edit_card(path, diff, dur, rows, open=True):
    chip = f'<span class="chip"><span style="color:{SUCCESS}">+{diff[0]}</span><span style="color:{DANGER}">−{diff[1]}</span></span>'
    body = '<div class="cb"><div class="diff">' + ''.join(
        f'<div class="{cls}"><span class="n">{o}</span><span class="n">{n}</span><span>{t}</span></div>' for cls, o, n, t in rows) + '</div></div>' if open else ''
    return f'<div class="card">{tool_header("ok", "Edited", path, chip + rs(dur), open)}{body}</div>'

def group_card(title, preview, count_more=None, open=False, calls=None):
    """One surface for the whole group. Closed: a caption preview of the first two calls. Open: one row per call
    (the tool header's own layout, no card of its own) over 7 % rules; a row that is open shows its output on the
    Well right under it. Nothing nests."""
    if open:
        rows = ''
        for kind, verb, target, right, out in calls:
            chev = ico(I["chevd" if out else "chevr"], size=12)
            rows += (f'<div class="ch" style="border-top:1px solid rgba(255,255,255,.07)"><span class="g">{glyph(kind)}</span><span class="verb">{verb}</span><span class="tgt">{target}</span>'
                     f'<span class="r">{right}<span class="chev">{chev}</span></span></div>')
            if out:
                rows += f'<div class="well">{out}</div>'
        body = rows
    else:
        rows = ''.join(f'<div>{glyph(k)}<span class="verb">{v}</span><span class="mono" style="font-family:\'Geist Mono\',ui-monospace,Menlo,monospace">{c}</span></div>' for k, v, c in preview)
        if count_more: rows += f'<div style="color:{FAINT}">+{count_more} more</div>'
        body = f'<div class="prev">{rows}</div>'
    return (f'<div class="card"><div class="ch"><span class="g">{glyph("ok")}</span><span class="verb">{title}</span>'
            f'<span class="r"><span class="chev">{ico(I["chevd" if open else "chevr"], size=12)}</span></span></div>{body}</div>')

def code_block(lang, lines, hover=False, more=None):
    rows = ''.join(f'<span class="n">{i+1}</span><span>{l}</span>' for i, l in enumerate(lines))
    acts = ibs("copy","Copy",True) if hover else ''
    fold = f'<div class="more" style="background:var(--l2);border-top:1px solid rgba(255,255,255,.07)">{ico(I["chevd"],size=11)}{more} more lines</div>' if more else ''
    return (f'<div class="code"><div class="h">{ico(I["file"], size=12)}code {lang}<span class="r">{acts}</span></div>'
            f'<div class="b">{rows}</div>{fold}</div>')

def table(head, rows):
    return ('<table class="tbl"><thead><tr>' + ''.join(f'<th>{h}</th>' for h in head) + '</tr></thead><tbody>' +
            ''.join('<tr>' + ''.join(f'<td>{c}</td>' for c in r) + '</tr>' for r in rows) + '</tbody></table>')

def approval_card(cmd, state='pending'):
    if state == 'pending':
        return (f'<div class="card need"><div class="hd"><span class="tile">{ico(I["term2"], size=14)}</span><div><div class="t">Allow this command?</div><div class="s">Muse wants to run a shell command in acme-web</div></div></div>'
                f'<div class="cmd">{cmd}</div>'
                f'<div class="foot"><span class="hint">⏎ allow · ⎋ deny</span><span class="sp"></span><button class="ctl2">Deny</button><button class="ctl2">Always allow</button><button class="ctl2 pri">Allow</button></div></div>')

def question_card(header, q, opts, auto=None, sel=None):
    o = ''
    for i, (label, desc, key) in enumerate(opts):
        on = sel == i
        o += (f'<div class="opt{" sel" if on else ""}"><span class="radio{" on" if on else ""}"></span><div style="flex:1"><div class="l">{label}</div><div class="d">{desc}</div></div><span class="key">{key}</span></div>'
              f'<div class="disc">{ico(I["chevr"], size=11)}Preview</div>')
    tag = f'<span class="tag warn" style="margin-left:auto">{auto}</span>' if auto else ''
    return (f'<div class="card need"><div class="hd"><span class="tile">{ico(I["q"], size=14)}</span><div><div class="s">{header}</div><div class="t">{q}</div></div>{tag}</div>'
            f'<div style="padding-bottom:6px">{o}</div>'
            f'<div class="foot"><span class="hint">↑↓ move · ⏎ continue</span><span class="sp"></span><button class="ctl2">Skip</button><button class="ctl2 sec">Explain instead</button><button class="ctl2 pri">Continue</button></div></div>')

def tasks_card(done, total, items, open=True):
    rows = ''.join(f'<div class="trow{" done" if s=="d" else ""}"><span class="mk {s}"></span><span>{t}</span></div>' for s, t in items) if open else ''
    pct = int(100 * done / total)
    return (f'<div class="card"><div class="th">{g("list", 14, "1.5", MUTED)}<span style="font-weight:600">Tasks</span><span class="mono" style="font-size:12.1px;color:{FAINT}">{done} / {total}</span>'
            f'<span class="chev" style="margin-left:auto;color:{MUTED};display:flex">{ico(I["chevd" if open else "chevr"], size=12)}</span></div><div class="bar"><i style="width:{pct}%"></i></div>{rows}</div>')

def goal_card(title, status, pct, rows):
    dl = ''.join(f'<span class="k">{k}</span><span>{v}</span>' for k, v in rows)
    return (f'<div class="card"><div class="hd" style="align-items:center"><div class="t">{title}</div><span class="tag" style="margin-left:auto">{status}</span></div>'
            f'<div style="display:flex;align-items:center;gap:10px;padding:0 12px 10px"><div class="bar" style="flex:1"><i style="width:{min(pct,100)}%"></i></div><span class="mono" style="font-size:12.1px;color:{FAINT}">{pct}%</span></div>'
            f'<div class="dl">{dl}</div></div>')

def error_card(title, lines, action='Retry now'):
    body = ''.join(f'<div style="font-size:13.2px;line-height:18px;color:{MUTED}">{l}</div>' for l in lines)
    return (f'<div class="card bad"><div class="hd" style="align-items:center">{g("x", 16, "1.8", DANGER)}<div style="flex:1"><div class="t">{title}</div>{body}</div>'
            f'<button class="ctl2 sec">{action}</button></div></div>')

def status_row(text, key=None, tail=None, spin=True):
    k = f'<span>·</span><span class="key">{key[0]}</span><span>{key[1]}</span>' if key else ''
    t = f'<span style="color:{FAINT}">{tail}</span>' if tail else ''
    return f'<div class="st">{"<span class=\"spin\"></span>" if spin else ""}<span>{text}</span>{k}{t}</div>'

def jump_pill(n): return f'<span class="jump">{ico(I["arrowdn"], size=12)}Jump to latest<b>{n}</b></span>'

def toast(kind, title, body, action=None, queued=0):
    col = {'ok': SUCCESS, 'warn': WARNING, 'neutral': MUTED}[kind]
    q = f'<span class="n">+{queued}</span>' if queued else ''
    a = f'<button class="ctl2" style="height:24px;padding:0 8px;font-size:13.2px">{action}</button>' if action else ''
    return f'<div class="toast"><span class="dot" style="background:{col}"></span><span class="t">{title}</span><span class="b">{body}</span>{q}{a}{ibs("x","Dismiss",True)}</div>'

def feedback_slot(active=False, count=None):
    if not active:
        return f'<div class="fb">{ibs("msgplus","Feedback mode")}</div>'
    return f'<div class="fb"><button class="ibs" aria-label="Feedback mode" style="color:{FG};background:rgba(255,255,255,.09)">{ico(I["msgplus"], size=12)}</button><span class="cnt">{count}</span><button class="ctl2 pri" style="height:24px;padding:0 8px;font-size:13.2px">Done</button><button class="ctl2" style="height:24px;padding:0 8px;font-size:13.2px">Discard</button></div>'

# ---------------- shell (drawn sidebar + sheet) ----------------
def sidebar_drawn(title, byline, when='now', need=False):
    rows = [('search', 'Search', ''), ('plus', 'New session', ''), ('folder', 'Add project', ''), ('slash', 'Skills', '0'), ('bolt', 'Automations', 'Soon')]
    I.setdefault('folder', '<path d="M2 4a1 1 0 0 1 1-1h3l2 2h5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/>')
    I.setdefault('slash', '<path d="M10 3L6 13"/>'); I.setdefault('bolt', '<path d="M9 2L4 9h4l-1 5 5-7H8z"/>')
    r = ''.join(f'<div class="r">{ico(I[ic], size=16)}<span>{t}</span><span class="k">{k}</span></div>' for ic, t, k in rows)
    sess = (f'<div class="sess"><span class="dot{" need" if need else ""}"></span><div style="min-width:0;flex:1"><div class="tt">{title}</div><div class="by">{byline}</div></div><span class="when">{when}</span></div>')
    return (f'{lights()}<button class="ib" aria-label="Hide sidebar" style="position:absolute;left:84px;top:8px;z-index:4">{ico(I["side"])}</button>'
            f'<div style="position:absolute;left:184px;top:10px;z-index:4">{switch("s")}</div>'
            f'<div class="sb">{r}<div class="grp">Today</div>{sess}</div>'
            f'<div class="sbfoot"><span class="av">R</span><span>Replay</span><span style="margin-left:auto;color:{MUTED};display:flex">{ico(I["chevu"], size=12)}</span></div>'
            f'<div class="contact"></div><div class="lfade"></div>')

def screen(sidebar_html, title, transcript_html, below_composer='', overlay='', stop=False, status=''):
    comp = composer_a(408, 18)
    if stop:
        comp = comp.replace(ico(I["up"], sw="1.8", size=14), ico(I["sqr"], size=12))
    b = f'<div class="w" style="width:1440px;height:900px">{sidebar_html}'
    b += '<div class="sheet" style="left:262px;top:6px;right:6px;bottom:6px">'
    b += f'<div class="tx bk" style="position:absolute;left:146px;top:80px">{transcript_html}</div>'
    b += '<div class="bfade"></div>'
    if status: b += f'<div style="position:absolute;left:146px;bottom:104px">{status}</div>'
    b += f'<div class="fade"></div><div class="row" style="padding-left:16px"><span class="title">{title}</span><span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{actions()}</span></div>'
    b += overlay + '</div>' + comp + below_composer + '</div>'
    return b

# ---------------- content (from the fixtures) ----------------
MD_ANSWER = ('<h2>Validation summary</h2><p>CA and GB now branch per country, with focused tests over both.</p>'
             '<ul><li>CA outward codes accept the space form</li><li>GB outward codes still fail on the space (next)</li>'
             '<li>Full suite stays green: <span class="ic">cargo test --workspace</span></li></ul>')
RUST = [f'<span style="color:{KW}">fn</span> validate(code: <span style="color:{TY}">&amp;str</span>, country: <span style="color:{TY}">Country</span>) -&gt; <span style="color:{TY}">bool</span> {{',
        f'    <span style="color:{KW}">match</span> country {{',
        f'        <span style="color:{TY}">Country</span>::Ca =&gt; <span style="color:{TY}">CA_RE</span>.<span style="color:{TY}">is_match</span>(code),',
        f'        <span style="color:{TY}">Country</span>::Gb =&gt; <span style="color:{TY}">GB_RE</span>.<span style="color:{TY}">is_match</span>(code),',
        '    }', '}']
MD_TAIL = '<p>Notes live in <a href="#">the plan</a> and the harness doc at <a href="#">docs/02-app.md</a>.</p>'

def b_answer(src):
    tx = (user_turn('Show me the validation summary with code and links.') + '<div class="gapturn"></div>' +
          assistant_turn(MD_ANSWER, rail=False) + code_block('rust', RUST) + table(['country', 'cases', 'result'], [['CA', '14', 'pass'], ['GB', '11', '1 failing']]) +
          assistant_turn(MD_TAIL, footer=['muse-spark-1.3', '2.3 s']))
    return screen(sidebar_drawn('Show me the validation su…', 'Notes live in the plan and…'), 'Show me the validation summary with code and links.', tx)

def b_work(src):
    children = ''
    tx = (user_turn('Build it and check the configs.') + '<div class="gapturn"></div>' +
          fold_row('Worked · read 2 files, ran 5 commands', open=True) +
          group_card('Ran 3 commands', [('ok', 'Ran', 'cargo build --workspace'), ('ok', 'Ran', 'cargo test --workspace')], 1) +
          think_row('Thought', 'Checking the plan') +
          group_card('Read 2 files', [('ok', 'Read', '/tmp/synthetic/harness/notes.txt'), ('ok', 'Read', '/tmp/synthetic/harness/todo.txt')]) +
          resolved_row('ok', 'Allowed once', 'cargo run -- --no-connect', '· exit 0 · 0.0 s') +
          shell_card('Ran', 'cargo clippy -- --deny warnings', 'error: warnings denied', kind='bad', right=rs('exit 2', True)) +
          '<div class="gapturn"></div>' + user_turn('Run the checks.') + '<div class="gapturn"></div>' +
          fold_row('Worked for 24 s · ran 3 commands') +
          assistant_turn('<p>All three checks pass: tests, lint and build.</p>', footer=['muse-spark-test', '24.0 s']))
    return screen(sidebar_drawn('Build it and check the co…', 'All three checks pass: tests…'), 'Build it and check the configs.', tx)

def b_ask(src):
    tx = (user_turn('Describe one of the two files, and ask me which.') + '<div class="gapturn"></div>' +
          question_card('File', 'Which of the listed files do you want described?',
                        [('README.md', "The project's own introduction", '1'), ('notes.txt', 'Scratch notes from the last session', '2')], auto='Auto-resolves in 2 m 00 s'))
    st = status_row('Waiting for your answer…', key=('esc', 'to interrupt'))
    return screen(sidebar_drawn('Describe one of the two…', 'Asked: “Which of the listed…', need=True), 'Describe one of the two files, and ask me which.', tx, stop=True, status=st)

def b_progress(src):
    tx = (user_turn('Tighten address validation for CA and GB.') + '<div class="gapturn"></div>' +
          goal_card('Ship the checkout fix', 'wrapping up', 120, [('Doing now', 'Re-running the focused tests'), ('Next', 'Summarise the change')]) +
          tasks_card(3, 5, [('d', 'Read validators and the form'), ('d', 'Branch validation per country'), ('d', 'Add CA and GB tests'), ('r', 'Run the focused tests'), ('p', 'Summarise the change')]) +
          assistant_turn('<p>Done: CA and GB now validate, and the tests cover both.</p>', footer=['muse-echo', '41.0 s']))
    return screen(sidebar_drawn('Tighten address validati…', 'Done: CA and GB now valid…'), 'Tighten address validation for CA and GB.', tx)

def b_homes_sessions(src):
    tx = (user_turn('Rename the helper and update its callers.') + '<div class="gapturn"></div>' +
          fold_row('Worked for 2.1 s · edited 1', diff=(10, 3)) +
          assistant_turn('<p>Renamed across two files.</p>', footer=['muse-spark-test', '2.1 s', '1.3k tokens', '12 reasoning']))
    # the corner stack: toast above the composer's top (8), feedback slot in the sheet's corner (12 / 12)
    ov = (f'<div style="position:absolute;right:12px;bottom:104px;display:flex;flex-direction:column;align-items:flex-end;gap:8px">'
          f'{toast("neutral", "File not found", "docs/02-app.md is no longer in this workspace.")}{feedback_slot(True, "3 notes")}</div>')
    return screen(sidebar_drawn('Rename the helper and…', 'Renamed across two files.'), 'Rename the helper and update its callers.', tx, overlay=ov)

def browse_control_row(src):
    # redrawn over the captured row: back · forward · reload · new tab … downloads · ✦ · ⋯
    return (f'<div style="position:absolute;left:0;top:92px;width:256px;height:36px;background:{L0};display:flex;align-items:center;gap:2px;padding:0 12px">'
            f'{ib("back","Back")}<span class="ib" style="color:{FAINT}">{ico(I["fwd"])}</span>{ib("reload","Reload")}{ib("plus","New tab")}'
            f'<span style="margin-left:auto;display:flex;gap:2px"><button class="ib" aria-label="Downloads, 1 in progress" style="position:relative">{ico(I["dl"])}<i style="position:absolute;right:5px;top:5px;width:6px;height:6px;border-radius:50%;background:{FG}"></i></button>{ib("spark","Ask")}{ib("dots","Page menu")}</span></div>')

def b_homes_browse(src):
    b = f'<div class="w" style="width:1440px;height:900px">'
    b += f'<div style="position:absolute;left:0;top:0">{img("sb-browse",254,900,src)}</div><div style="position:absolute;left:0;top:0;width:256px;height:44px;background:{L0}"></div>'
    b += f'{lights()}<button class="ib" aria-label="Hide sidebar" style="position:absolute;left:84px;top:8px;z-index:4">{ico(I["side"])}</button><div style="position:absolute;left:184px;top:10px;z-index:4">{switch("b")}</div>'
    b += browse_control_row(src)
    b += f'<div style="position:absolute;left:0;top:852px">{img("sb-footer",254,48,src)}</div><div class="contact"></div><div class="lfade"></div>'
    b += f'<div class="sheet" style="left:262px;top:6px;right:6px;bottom:6px;background:#FAFAFA"><div style="position:absolute;left:0;top:0">{img("page",1168,881,src)}</div>'
    b += (f'<div class="tx" style="position:absolute;right:12px;bottom:12px;width:auto;display:flex;flex-direction:column;align-items:flex-end;gap:8px">'
          f'{toast("ok", "Downloaded", "fluid-functionalism-tokens.json · 24 KB", action="Show in Finder")}{feedback_slot(False)}</div>')
    b += '</div></div>'
    return b

# ---------------- component sheets ----------------
def sheet_board(title, sub, items, h):
    b = f'<div class="w" style="width:1440px;height:{h}px;background:{L1}">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">{title}</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;color:{MUTED};width:1360px">{sub}</div>'
    b += '<div style="position:absolute;left:40px;top:96px;width:1360px;display:flex;flex-direction:column;gap:28px">'
    for label, note, html, _ in items:
        b += (f'<div style="display:flex;gap:36px;align-items:flex-start"><div class="lbl" style="position:static;flex:none"><b>{label}</b>{note}</div>'
              f'<div class="tx bk">{html}</div></div>')
    return b + '</div></div>'

def inline(*parts):
    """Inline pieces (pills, chips) laid out in a wrapping row, so the column does not stretch them."""
    return '<div style="display:flex;flex-wrap:wrap;gap:8px;align-items:flex-start">' + ''.join(parts) + '</div>'

def b_blocks(src):
    items = [
     ('User turn', 'Raised · r 12 · the edge · 9 / 13 · body 13 → 14.3 / 22 · max 78 % of the 880 measure, right-aligned. At rest nothing else. Hovering the turn reveals edit · copy (28, muted) and the age in faint mono, 4 under the bubble; the row’s height is always reserved so nothing shifts.',
      user_turn('Run the shell command <span class="ic">ls</span> in the workspace, then use your question tool to ask me which of the listed files I want described. Do not do anything else.') + user_turn('Check the workspace, tidy the todos, then estimate the tidy.', age='just now', hover=True), 112),
     *_items_round6(src),
     ('Fold row · closed', 'No surface. 32 px · chevron 12 on the left · body text muted; foreground + 5 % overlay (r 6, bleeding 8 px) on hover. The +N −M chip in mono, success / danger.',
      fold_row('Worked for 2.1 s · edited 1', diff=(10, 3)) + fold_row('Worked for 17 s · ran 1 command', hov=True), 80),
     ('Fold row · open', 'The same row with the chevron turned; its cards stack under it on the measure, 8 apart. Nothing indents.',
      fold_row('Worked · ran 1 command', open=True) + shell_card('Ran', 'ls', 'README.md\nnotes.txt', right=''), 136),
     ('Thinking', 'The same quiet row with the thinking glyph; summary after a middle dot. Open: the trace in caption muted, 32 in from the measure (under the text), a 34 px fade while streaming.',
      think_row('Thought for 0.0 s', 'Plan the sweep') + think_row('Thought for 0.0 s', open=True) + think_body('Consider the workspace layout first, then list files.'), 110),
     ('Tool card · shell', 'Raised · r 8 · the edge. Header 34: glyph 14 · verb · target in mono muted · right: result + duration in faint mono, chevron. No actions: the header toggles the body. Body under a 7 % rule, on the Well.',
      shell_card('Ran', 'ls -la', 'README.md\nnotes.txt', right=rs('exit 0') + rs('0.1 s')), 110),
     ('Tool card · failed', 'The glyph and the exit code carry the failure; the card itself stays Raised, no ring (a failure in the past needs nothing from you).',
      shell_card('Ran', 'cargo clippy -- --deny warnings', 'error: warnings denied', kind='bad', right=rs('exit 2', True)), 92),
     ('Tool card · long output', 'The output folds at six lines; the “N more lines” row sits on the Well and expands in place; the whole output is in the right pane’s full-output view as today.',
      shell_card('Ran', 'cargo build --workspace', '[test] running 42 tests\n[test] .... ok\n[lint] clippy clean\n[doc] building docs\n[done] finished in 12.4 s\n[done] 0 warnings', right=rs('exit 0') + rs('12.4 s'), more=4), 196),
     ('Read · Edited', 'Read: header only, the line count faint; opens to the file on the Well. Edited: the +N −M chip in the header, the unified diff on Raised with 10 % success / danger line tints, gutters faint.',
      read_card('/tmp/synthetic/notes.txt', '3 lines') + edit_card('/tmp/synthetic/notes.txt', (10, 3), '0.1 s', [('', '1', '1', 'line one'), ('del', '2', '', 'line two'), ('add', '', '2', 'line two, renamed'), ('', '3', '3', 'line three')]), 190),
     ('Tool group', 'Consecutive calls fold into one surface: closed, the first two as a caption preview with “+k more” faint. Open, every call is a row in the same card (the tool header’s layout over a 7 % rule) and a row’s output opens on the Well right under it. Nothing nests, no card inside a card.',
      group_card('Ran 3 commands', [('ok', 'Ran', 'cargo build --workspace'), ('ok', 'Ran', 'cargo test --workspace')], 1) +
      group_card('Read 2 files', [], open=True, calls=[('ok', 'Read', '/tmp/synthetic/harness/notes.txt', rs('2 lines'), 'alpha\nbeta'), ('ok', 'Read', '/tmp/synthetic/harness/todo.txt', rs('1 lines'), None)]), 230),
     ('Assistant turn', 'Prose on the sheet at the full measure: body 14.3 / 22, paragraphs 10 apart, h2 as title 17.6 / 24 semibold, inline code mono 12.65 on a 7 % tint (r 4). Links underlined, the underline faint.',
      assistant_turn(MD_ANSWER + MD_TAIL, rail=False), 150),
     ('Code block', 'Raised · r 8 · the edge. Header 32 on Raised-lit: file glyph, “code rust” in mono muted; one copy button (24, muted) appears on hover. Body 10 / 12, gutter 22 faint, mono 12.65 / 20.',
      code_block('rust', RUST, hover=True), 190),
     ('Code block · long', 'A block past twelve lines folds at twelve: a 24 px “N more lines” row on the same Raised ground, under a 7 % rule, expands it in place; the header never toggles (the code is the answer, not a step). Content: fixtures/msp/synthetic-stress-hetero.jsonl.',
      code_block('rust', [f'<span style="color:{KW}">fn</span> main_1() {{', f'    <span style="color:{KW}">let</span> value_0 = compute_thing(0, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_1 = compute_thing(1, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_2 = compute_thing(2, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_3 = compute_thing(3, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_4 = compute_thing(4, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_5 = compute_thing(5, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_6 = compute_thing(6, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_7 = compute_thing(7, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_8 = compute_thing(8, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_9 = compute_thing(9, &amp;state).unwrap_or_default();', f'    <span style="color:{KW}">let</span> value_10 = compute_thing(10, &amp;state).unwrap_or_default();'], more=14), 320),
     ('Table', 'Raised · r 8 · the edge. Header row on Raised-lit, semibold; cells 10 / 14; rules 7 % white; row hover 5 %.',
      table(['country', 'cases', 'result'], [['CA', '14', 'pass'], ['GB', '11', '1 failing']]), 150),
     ('Turn end', 'At rest nothing under the prose. Hovering the turn reveals one copy button (28, muted) 8 below and the footer in faint mono 12.1, 10 below: model · time · tokens · reasoning · age. Retry and fork are gone: a retry is a typed ask, a fork lives in the session menu.',
      assistant_turn('<p>You selected <b>README.md</b>. Let me read and describe it.</p>', footer=['muse-spark-1.3-contributor', '17.3 s', '59.2k tokens', '419 reasoning', 'just now'], hover=True), 110),
    ]
    return sheet_board('D4 · Transcript blocks', 'Everything inside the sheet is Raised, Raised-lit or the Well, with the one edge; rows that only report (Worked for…, Thought…, Allowed, Answered) carry no surface at all. No standing actions: a turn shows copy (and edit on yours) only while hovered; cards carry no buttons but the choices a provider minted. Text is the FF roles at the app’s 1.1 scale; spacing is unscaled.', items, H["D4Blocks.dc.html"])

def b_cards(src):
    items = [
     ('Approval · pending', 'Raised · r 8; the ring becomes warning at 70 % (the only marked edge: it needs you). Tile 26 r 7 Raised-lit; title semibold, sub muted; the command on the Well (r 6, 8 / 10); action row: key hints faint, spacer, text controls, one primary.',
      approval_card('echo hi &amp;&amp; ls'), 170),
     ('Approval · running · resolved', 'Once answered the card goes quiet: a surface-less row with the status glyph, the verdict, the command in mono and the result faint. Running shows the spinner.',
      live_row('Approving…', 'echo hi &amp;&amp; ls') + resolved_row('ok', 'Allowed once', 'cargo run -- --no-connect', '· exit 0 · 0.0 s'), 90),
     ('Question · pending', 'The same frame and ring. Header: tile, the question’s header in caption muted over the question semibold, the auto-resolve tag (warning tint) right. Options: rows 8 / 10 in an 8 px inset, radio 16, label 400 (semibold when picked), description caption muted, the key in a faint mono cap; “Preview” as a caption disclosure. Footer: key hint, spacer, then only the choices the provider minted (Skip, Explain instead, Continue): no count for a single choice.',
      question_card('File', 'Which of the listed files do you want described?', [('README.md', "The project's own introduction", '1'), ('notes.txt', 'Scratch notes from the last session', '2')], auto='Auto-resolves in 2 m 00 s', sel=0), 290),
     ('Question · answered', 'A quiet row: the ok glyph, “Answered”, the pick as inline code.',
      resolved_row('ok', 'Answered', None) .replace('</span></div>', '</span><span class="ic">README.md</span></div>') + resolved_row('ok', 'Clarified', None).replace('</span></div>', '</span><span>Neither — describe the checkout form instead.</span></div>'), 80),
     ('Tasks', 'Raised · r 8. Header 32: list glyph, “Tasks” semibold, “n / m” faint mono, chevron. A 3 px bar (7 % track, muted fill). Rows 30 / 12 over 7 % rules; marks 15: pending ring, running ring + dot (foreground), done success fill; done text muted + strike.',
      tasks_card(1, 3, [('r', 'Sweep the workspace'), ('p', 'Update the changelog'), ('d', 'File the notes')]), 150),
     ('Goal', 'Raised · r 8. Title semibold with the provider’s status as a 7 % tag; the bar clamps at 100 % while the number stays what the provider said; a two-column list in caption.',
      goal_card('Ship the checkout fix', 'wrapping up', 120, [('Doing now', 'Re-running the focused tests'), ('Next', 'Summarise the change')]), 130),
     ('Failure', 'Raised · r 8 · ring danger 70 %: this one needs you. Danger glyph 16, title semibold, the lines in caption muted, one secondary action at the right.',
      error_card('Model error', ['the provider closed the stream before the reply finished', 'The turn was orphaned when the session’s process was lost.', '<span class="mono">resume_reconcile:orphaned_by_process_loss</span>']), 120),
     ('Live rows', 'While a turn runs: the activity row (spinner, verb, target, “+k earlier” faint, elapsed) in the transcript, and the status row (28 px, caption muted) 8 above the composer, with the key cap.',
      live_row('Running', 'cargo test --workspace', earlier=2, elapsed='8.1 s') + status_row('Finishing up…', key=('esc', 'to interrupt'), tail='memory reminders') + status_row('Attempt 2/5 <span class="mono">retrying in 8 s</span>', tail='· rate limited by the provider'), 110),
     ('Jump to latest', 'Floating · full radius · the edge + the float drop; 28 px; centred over the measure, 8 above the composer; the count in a 9 % cap.',
      inline(jump_pill(3)), 60),
     ('Toast chip', 'Floating · r 10 · the edge + the float drop; 36 px; dot 8 in the status colour, title semibold, body muted (truncates), “+n” faint, an optional text action, ✕. Max 480.',
      inline(toast('neutral', 'File not found', 'docs/02-app.md is no longer in this workspace.')) + inline(toast('ok', 'Downloaded', 'fluid-functionalism-tokens.json · 24 KB', action='Show in Finder', queued=1)), 100),
     ('Feedback slot', 'The same floating pill: the toggle (28); active: the toggle lit (9 %), the note count faint mono, Done as the primary, Discard as text.',
      inline(feedback_slot(False), feedback_slot(True, '3 notes')), 100),
     ('Downloads', 'In the Browse sidebar’s control row, before ✦ and ⋯: a 28 px icon button with a 6 px foreground dot while a download runs; it exists only while downloads list. Its popover is the menu, at Floating.',
      f'<div style="width:256px;height:36px;background:{L0};border-radius:8px;position:relative">{browse_control_row(src).replace("top:92px","top:0").replace("position:absolute;left:0;","position:absolute;left:0;border-radius:8px;")}</div>', 70),
    ]
    return sheet_board('D4 · Cards, rows and the three homes', 'Cards that need you (approval, question, failure) carry a 70 % ring in their status colour instead of the white ring; everything else keeps the one edge. The toast, the feedback slot and the downloads button are drawn where the owner placed them (docs/40 §3).', items, H["D4Cards.dc.html"])

# ---------------- write ----------------
def page(name, title, body, w, h):
    html = f'''<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{title}</title>
<script src="./support.js"></script>
</head>
<body>
<x-dc>
<helmet>
<link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Geist+Mono:wght@400&display=swap">
<style>{CSS}{D4CSS}</style>
</helmet>
{body(BLOBS).replace('class="w" style="', 'class="w" style="' + VARS, 1)}
</x-dc>
<script type="text/x-dc" data-dc-script data-props='{{"$preview":{{"width":{w},"height":{h}}}}}'>
class Component extends DCLogic {{
  renderVals() {{
    return {{}};
  }}
}}
</script>
</body>
</html>
'''
    assert '{{' not in html.split('<script type="text/x-dc"')[0].split('</helmet>')[1], name
    open(f"{P}/{name}.dc.html", "w").write(html)
    prev = f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Geist+Mono:wght@400&display=swap"><style>{CSS}{D4CSS}body{{background:#000}}</style></head><body>{body(LOCAL).replace('class="w" style="', 'class="w" style="' + PREVIEW_VARS, 1)}</body></html>'
    open(f"{V}/{name}.html", "w").write(prev)

H = {}
BOARDS = {
 "D4Blocks.dc.html": ("D4 · Blocks", b_blocks, 1440, 3011, 0, 4040),
 "D4Cards.dc.html": ("D4 · Cards + homes", b_cards, 1440, 1923, 1520, 4040),
 "D4Answer.dc.html": ("D4 · Answer", b_answer, 1440, 900, 0, 6500),
 "D4Work.dc.html": ("D4 · Work, fold open", b_work, 1440, 900, 1520, 6500),
 "D4Ask.dc.html": ("D4 · Question pending", b_ask, 1440, 900, 0, 7520),
 "D4Progress.dc.html": ("D4 · Goal + tasks", b_progress, 1440, 900, 1520, 7520),
 "D4HomeSessions.dc.html": ("Homes · Sessions", b_homes_sessions, 1440, 900, 0, 8660),
 "D4HomeBrowse.dc.html": ("Homes · Browse", b_homes_browse, 1440, 900, 1520, 8660),
}
# ======================= round 3 (owner, 2026-10-13 17:57): reading, composer states, the remaining cards =======================
D4CSS += """
.rd{font-size:14.3px;line-height:23px}
.rd p{margin:0 0 12px}.rd h2{font-size:17.6px;line-height:24px;font-weight:600;margin:20px 0 8px}.rd h2:first-child{margin-top:0}
.rd h3{font-size:15.4px;line-height:22px;font-weight:600;margin:16px 0 6px}
.rd ul,.rd ol{margin:0 0 12px;padding-left:24px}.rd li{margin:0 0 2px}.rd ol li::marker{color:#9D9DA6}
.rd blockquote{margin:0 0 12px;padding:2px 0 2px 14px;border-left:2px solid rgba(255,255,255,.12);color:#9D9DA6}
.rd hr{border:0;height:1px;background:rgba(255,255,255,.07);margin:16px 0}
.rd .img{background:var(--l2);border-radius:8px;box-shadow:var(--raised-sh);overflow:hidden;margin:0 0 12px}
.rd .img img{display:block;width:100%;max-height:480px;object-fit:cover}
.rd .img .cap{padding:8px 12px;font-size:13.2px;line-height:18px;color:#9D9DA6}
.rd .imgph{display:flex;gap:10px;align-items:center;padding:10px 12px}
.rd .tbl{margin:0 0 12px}
.cp{background:var(--l2);border-radius:12px;box-shadow:var(--raised-sh);padding:8px;display:flex;flex-direction:column;gap:4px;width:880px}
.cp .fld{font-size:15.4px;line-height:22px;padding:8px;color:#F2F2F4}.cp .fld.ph{color:#9D9DA6}
.cp .bar{display:flex;align-items:center;gap:4px;height:auto;background:none}
.att{display:flex;gap:6px;padding:0 8px;flex-wrap:wrap}
.att .c{height:32px;border-radius:6px;background:var(--l2h);box-shadow:var(--raised-sh);display:inline-flex;align-items:center;gap:6px;padding:0 4px 0 1px;font-size:13.2px;white-space:nowrap;position:relative;overflow:hidden}
.att .c.f{padding-left:8px}
.att .c .th{width:30px;height:30px;border-radius:5px;background:linear-gradient(135deg,#2A2A30,#3a3a44);flex:none;overflow:hidden}.att .c .th img{width:30px;height:30px;object-fit:cover;display:block}
.att .c .m{color:#9D9DA6;font-size:12.1px}.att .c .x{width:20px;height:20px;border-radius:4px;display:flex;align-items:center;justify-content:center;color:#9D9DA6}
.att .c.up .th{opacity:.5}.att .c .sw{position:absolute;left:0;bottom:0;height:2px;width:35%;background:#F2F2F4}
.att .c.bad{color:#FCA5A5}
.pop{width:520px;background:var(--l3);border-radius:10px;padding:6px;box-shadow:var(--float-sh)}
.pop .sec{height:24px;display:flex;align-items:flex-end;padding:0 10px 4px;font-size:12.1px;color:#9D9DA6}
.pop .r{height:36px;display:flex;align-items:center;gap:10px;padding:0 10px;border-radius:6px;font-size:14.3px}
.pop .r.sel{background:rgba(255,255,255,.09)}.pop .r .d{color:#9D9DA6;font-size:13.2px;margin-left:auto}.pop .r .k{color:#6E6E78;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px}
.pop .pcmd{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:13.2px;width:120px}
.pop .ft{display:flex;gap:12px;padding:6px 10px 2px;margin-top:4px;border-top:1px solid rgba(255,255,255,.07);font-size:12.1px;color:#6E6E78}
.qd{height:32px;border-radius:6px;background:var(--l2h);box-shadow:var(--raised-sh);display:flex;align-items:center;gap:8px;padding:0 6px 0 10px;font-size:13.2px}
.qd .b{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:10.5px;line-height:16px;height:16px;color:#9D9DA6;background:rgba(255,255,255,.09);border-radius:999px;padding:0 6px;display:inline-flex;align-items:center}
.dropov{position:relative;height:220px;border-radius:8px;background:var(--l1)}
.dropov .ring{position:absolute;inset:8px;border:2px dashed rgba(255,255,255,.2);border-radius:8px}
.dropov .tile{width:40px;height:40px;border-radius:8px;background:var(--l3);box-shadow:var(--float-sh);display:flex;align-items:center;justify-content:center;color:#F2F2F4;margin:0 auto 10px}
.nyb{display:flex;align-items:center;gap:10px;padding:8px 10px 8px 12px;border-radius:8px;background:var(--l2h);box-shadow:var(--raised-sh)}
.mkr{display:flex;align-items:center;gap:10px;font-size:13.2px;color:#9D9DA6;margin:10px 0}.mkr::before,.mkr::after{content:"";flex:1;height:1px;background:rgba(255,255,255,.07)}
.caret{display:inline-block;width:2px;height:16px;background:#F2F2F4;vertical-align:-3px;margin-left:1px}
.shim{background:linear-gradient(90deg,#9D9DA6 0%,#F2F2F4 50%,#9D9DA6 100%);-webkit-background-clip:text;background-clip:text;color:transparent}
.hero{display:flex;flex-direction:column;align-items:center;gap:8px;padding:40px 0;color:#9D9DA6}
"""

def img_block(src, caption, w=880):
    return f'<div class="img"><img src="{src}" alt="" style="height:300px"><div class="cap">{caption}</div></div>'

def b_reading(src):
    body = ('<h2>Why these values</h2>'
            '<p>A seven-unit RGB step between near-black greys is about 3 L*, and the eye needs four to five to see a level without a line. Fluid Functionalism\'s ladder therefore needed its rings to read, and the rings read as borders. The v3 ladder starts darker (Claude Desktop\'s sidebar is #111111) and steps nine units, so fill alone separates; the edge is only a hint.</p>'
            '<p>Two to three units of blue in every grey turn a dead grey alive without ever reading as blue. Claude Desktop\'s side panel is #131215 and Linear\'s chrome sits in the same place. Only two kinds of depth exist: a surface that sits in another (fill plus a top highlight) and a surface that floats (one soft drop).</p>'
            '<blockquote>Too much dark border and shadows.</blockquote>'
            '<p>That one line, from the second round, is the whole brief: the stacked 18 % drops and black outer rings were what read as heavy. The owner answered the four tone questions "all of the above" and picked Claude Desktop as the composer reference.</p>'
            '<h3>The loop for every screen</h3>'
            '<ol><li>Capture the real app offscreen with <span class="ic">BAAZ_DETERMINISTIC=1</span>, a fresh state dir and a timeout around every capture.</li>'
            '<li>Mock on the v3 canvas from the captures, at the quality of the four screens. The canvas is the spec.</li>'
            '<li>Owner approval, as canvas comments.</li><li>Build in agentic-ui (component and gallery card), then Baaz adopts.</li>'
            '<li>Gates in both repos, then the 18-entry probe.</li><li>Drive the real app yourself before any handover.</li></ol>'
            + img_block(src["page"], 'The sidebar peeking over a live page, captured 2026-10-13') +
            '<hr>' + table(['Level', 'Hex', 'Used for'], [['Frame', '#121215', 'window background, the sidebar, every gap'], ['Sheet', '#1B1B1F', 'the work area'], ['Raised', '#232328', 'composer, user turn, code block, cards'], ['Floating', '#2C2C32', 'menus, pills, popovers, toasts']]) +
            '<p>The right pane, Settings and the command palette inherit the ladder: the right pane is a second sheet 6 px to the right, the palette sits at the dialog level, a toast floats. Light is not designed yet; when it is, the frame goes to a warm off-white, the sheet to white, and the drop shadows carry the depth the way Fluid Functionalism\'s light rule says.</p>')
    missing = f'<div class="img"><div class="imgph">{g("file", 16, "1.5", MUTED)}<div><div>The sidebar peeking over a live page, captured 2026-10-13</div><div class="mono" style="font-size:12.1px;color:{FAINT}">capture-peek.png · not available</div></div></div></div>'
    items = [
     ('Reading rhythm', 'A long answer on the 880 measure. Body 14.3 / 23 (1.6), paragraphs 12 apart. h2 17.6 / 24 semibold with 20 above and 8 below; h3 15.4 / 22 with 16 / 6. Lists indent 24, items 2 apart, ordered markers muted. Quote: a 2 px 12 % rule, text muted. Rule: 1 px 7 %, 16 above and below. Image: a Raised block, r 8, the picture edge to edge (max 480 tall), its caption in caption muted inside. Table as on Blocks. Content: fixtures/msp/synthetic-reading.jsonl (real project prose).',
      f'<div class="a rd">{body}</div>' + assistant_turn('', footer=['muse-spark-1.3', '2.3 s'], hover=True), 1700),
     ('Image not available', 'When the picture cannot be loaded the block keeps its place: file glyph, the alt text, the path faint. No broken-image icon, no empty box.',
      f'<div class="a rd">{missing}</div>', 90),
    ]
    return sheet_board('D4 · Reading', 'Walls of rich text: the markdown blocks an answer can hold, set for reading rather than scanning. Nothing here is a card except the code block, the table and an image.', items, H["D4Reading.dc.html"])

def composer(field, chips='', ph=False, stop=False, above=''):
    send = ico(I["sqr"], size=12) if stop else ico(I["up"], sw="2" if (not ph or chips) else "1.8", size=14)
    go = ' go' if (stop or not ph or chips) else ''
    return (f'{above}<div class="cp">{chips}<div class="fld{" ph" if ph else ""}">{field}</div>'
            f'<div class="bar">{ib("plus","Attach or insert")}<button class="ctl on">{muse_mark()}Muse{ico(I["down"],size=12)}</button>'
            f'<span class="mono" style="font-size:11px;color:{FAINT};margin-left:4px">0 tokens</span>'
            f'<span style="margin-left:auto;display:flex;align-items:center;gap:4px"><button class="ctl">Muse Spark 1.3{ico(I["down"],size=12)}</button><button class="ctl">Auto{ico(I["down"],size=12)}</button><button class="ctl">Default{ico(I["down"],size=12)}</button>'
            f'<button class="send{go}" aria-label="{"Stop" if stop else "Send"}" style="margin-left:4px">{send}</button></span></div></div>')

def chip_img(src, name, state=''):
    return f'<span class="c {state}"><span class="th"><img src="{src}" alt=""></span>{name}<span class="x">{ico(I["x"], size=11)}</span>{"<i class=\"sw\"></i>" if state=="up" else ""}</span>'
def chip_file(name, meta, state=''):
    return f'<span class="c f {state}">{g("file", 13, "1.5", MUTED)}{name}<span class="m">{meta}</span><span class="x">{ico(I["x"], size=11)}</span></span>'

def b_composer(src):
    draft = 'Tighten the GB regex so the space form passes too, then rerun the focused tests and show me the diff before committing anything.'
    chips = f'<div class="att">{chip_img(src["thumb"], "transcript-real.png")}{chip_file("synthetic-markdown.jsonl", "JSONL · 2.4 KB")}</div>'
    chips_states = f'<div class="att">{chip_img(src["thumb"], "transcript-real.png", "up")}{chip_file("notes.pdf", "PDF · 1.1 MB")}{chip_file("archive.zip", "couldn’t be read", "bad")}</div>'
    mention = ('<div class="pop"><div class="sec">Symbols</div>'
               f'<div class="r sel">{g("file",14,"1.5",MUTED)}<span>validateAddress</span><span class="d mono">validators.ts:12</span></div>'
               '<div class="sec">Worktrees and more</div>'
               f'<div class="r"><span class="spin"></span><span>checkout-flow-v2</span><span class="d">transcript of another worktree</span></div>'
               f'<div class="r">{g("globe",14,"1.5",MUTED)}<span>localhost:3000/checkout</span><span class="d">open browser tab</span></div>'
               '<div class="ft"><span>↑↓ move</span><span>⏎ insert</span><span>esc close</span></div></div>')
    command = ('<div class="pop"><div class="sec">Commands</div>'
               '<div class="r sel"><span class="pcmd">/review</span><span>Review the current diff for bugs</span><span class="k" style="margin-left:auto">↩</span></div>'
               '<div class="r"><span class="pcmd">/resume</span><span>Resume a paused session</span></div>'
               '<div class="r"><span class="pcmd">/rewind</span><span>Restore files to an earlier checkpoint</span></div>'
               '<div class="sec">Skills</div><div class="r"><span class="pcmd">/report</span><span>Draft a report from this session</span><span class="d">skill</span></div>'
               '<div class="sec">Project</div><div class="r"><span class="pcmd">/release-notes</span><span class="mono" style="font-size:12.65px;color:#9D9DA6">.claude/commands/release-notes.md</span><span class="d">project</span></div>'
               '<div class="ft"><span>↑↓ move</span><span>⏎ run</span><span>esc close</span></div></div>')
    queued = f'<div class="qd" style="width:880px;margin-bottom:8px"><span class="b">queued</span><span style="flex:1">Also update the changelog once the tests pass.</span>{ibs("x","Remove",True)}</div>'
    drop = (f'<div class="dropov"><div class="ring"></div><div style="position:absolute;inset:0;display:flex;flex-direction:column;align-items:center;justify-content:center">'
            f'<div class="tile">{ico(I["up"], sw="1.8", size=16)}</div><div style="font-size:15.4px;font-weight:600">Drop to attach to this turn</div><div style="font-size:13.2px;color:{MUTED}">images become screenshots the agent can see</div></div></div>')
    items = [
     ('Empty', 'As approved on Composers: Raised, r 12, the edge, 8 around; the field text at 16 in subtitle 15.4 / 22; text controls below; the round send at Floating.',
      composer('Ask Muse, or type / for commands', ph=True), 110),
     ('Typed', 'The field grows with the draft (min 1 row, max 8, then it scrolls). Nothing else changes.',
      composer(draft), 130),
     ('With attachments', 'A chip row above the field, 6 apart, inset 8: Raised-lit, r 6, the edge, 32 tall. An image chip is its 30 px thumbnail + name; a file chip is the file glyph + name + type and size in faint. ✕ at the end of each. Captured from the app’s own attach steps.',
      composer(draft.split(',')[0] + '.', chips=chips), 150),
     ('With a quotation', 'A Text attachment: the quote glyph and the first words of the selected text; sent, it becomes the quote block in the user turn.',
      composer('Try that again.', chips=f'<div class="att"><span class="c f">{g("edit", 13, "1.5", MUTED)}“the provider closed the stream before…”<span class="m">quote</span><span class="x">{ico(I["x"], size=11)}</span></span></div>'), 150),
     ('Attachment states', 'Uploading: the thumbnail at 50 % with a 2 px sweep along the chip’s bottom. Failed: the name in danger with the reason as its meta; ✕ removes it. No retry button: drop it again.',
      composer('Here is the capture and the fixture.', chips=chips_states), 150),
     ('Drop over the sheet', 'Dragging files over the window: a 2 px dashed ring (white 20 %) 8 in from the sheet, a 40 px Floating tile with the arrow, title subtitle semibold, hint caption muted. The sheet itself does not dim. Captured.',
      drop, 240),
     ('@ mention', 'The caret popover, Floating, r 10, 6 px padding, up to 520 wide, 8 above the composer: sections in caption muted, rows 36 with a 14 px glyph, the name, the detail muted (mono for a symbol), the selected row at 9 %. Footer: the three keys faint.',
      mention + composer('@val'), 330),
     ('/ command', 'Same panel: the command in mono (120 col), its description, the key or source tag at the right. Sections Commands · Skills · Project.',
      command + composer('/re'), 380),
     ('Queued', 'A message sent while a turn runs waits above the composer: Raised-lit, r 6, 32 tall, a 16 px “queued” cap in faint mono, the text, ✕.',
      queued + composer('Ask Muse, or type / for commands', ph=True, stop=True), 160),
     ('Running', 'While a turn runs the send button becomes stop (a 12 px square), and the status row sits 8 above the card.',
      f'<div style="margin-bottom:8px">{status_row("Working…", key=("esc","to interrupt"), tail="12 s")}</div>' + composer('Ask Muse, or type / for commands', ph=True, stop=True), 150),
    ]
    return sheet_board('D4 · Composer states', 'Every state the composer can be in, on the approved card. Attachments and the drop overlay are drawn from the app’s own attach captures; the two caret menus from the library’s sample data.', items, H["D4Composer.dc.html"])

def b_more(src):
    plan = (f'<div class="card"><div class="hd" style="align-items:center"><div class="t">Proposed plan</div><span class="tag" style="margin-left:auto">Plan mode</span></div>'
            '<ol style="margin:0;padding:0 12px 10px 30px;font-size:14.3px;line-height:22px">'
            '<li>Branch <span class="ic">validateAddress</span> per country; keep regexes small.</li><li>Return a structured <span class="ic">{ ok, field }</span> instead of boolean.</li>'
            '<li>Add CA / GB / empty-country cases to the test file.</li><li>Run focused tests, then lint touched files.</li></ol>'
            '<div class="foot"><span class="hint">⏎ accept · e edit · ⎋ reject</span><span class="sp"></span><button class="ctl2">Reject</button><button class="ctl2">Edit</button><button class="ctl2 pri">Accept plan</button></div></div>')
    def frow(status, path, a, r):
        col = SUCCESS if status == 'A' else MUTED
        return f'<div style="display:flex;align-items:center;gap:8px;height:24px;font-size:13.2px"><span class="mono" style="color:{col};font-size:11px;width:14px">{status}</span><span class="mono" style="font-size:12.65px">{path}</span><span class="chip" style="margin-left:auto;font-size:12.1px"><span style="color:{SUCCESS}">+{a}</span><span style="color:{DANGER}">−{r}</span></span></div>'
    summary = (f'<div class="card"><div class="hd" style="align-items:center">{glyph("ok")}<div class="t">Done · address validation tightened</div><span class="mono" style="margin-left:auto;font-size:12.1px;color:{FAINT}">4 m 12 s · $0.31</span></div>'
               f'<div style="padding:0 12px 8px;display:flex;flex-direction:column;gap:2px">{frow("M","src/checkout/validators.ts",8,3)}{frow("A","src/checkout/validators.test.ts",41,0)}{frow("M","src/checkout/AddressForm.tsx",2,1)}</div>'
               f'<div style="display:flex;gap:12px;padding:0 12px 10px;font-size:13.2px;color:{MUTED}"><span>{g("ok",12,"1.8",SUCCESS)} 27 tests pass</span><span>{g("ok",12,"1.8",SUCCESS)} lint clean</span><span>{g("ok",12,"1.8",SUCCESS)} typecheck</span></div>'
               '<div class="foot"><span class="sp"></span><button class="ctl2">Review diff</button><button class="ctl2">Commit…</button><button class="ctl2 sec">Create PR</button></div></div>')
    search = (f'<div class="card">{tool_header("ok","Searched","validateAddress", rs("2 hits")+rs("0.2 s"))}<div class="cb" style="padding:6px 10px;display:flex;flex-direction:column;gap:2px;font-size:13.2px;line-height:20px">'
              f'<div><span class="mono" style="font-size:12.65px;color:{MUTED}">src/checkout/AddressForm.tsx:38</span>  <span class="mono" style="font-size:12.65px">validateAddress(values)</span></div>'
              f'<div><span class="mono" style="font-size:12.65px;color:{MUTED}">src/checkout/validators.ts:12</span>  <span class="mono" style="font-size:12.65px">export function validateAddress</span></div></div></div>')
    web = (f'<div class="card">{tool_header("ok","Searched web","canadian postal code regex", rs("1.8 s"))}<div class="cb" style="padding:6px 10px;display:flex;flex-direction:column;gap:6px;font-size:13.2px">'
           f'<div style="display:flex;gap:8px;align-items:center"><span style="width:14px;height:14px;border-radius:3px;background:rgba(255,255,255,.12)"></span><span>Postal codes in Canada</span><span style="color:{MUTED}">wikipedia.org</span></div>'
           f'<div style="display:flex;gap:8px;align-items:center"><span style="width:14px;height:14px;border-radius:3px;background:rgba(255,255,255,.12)"></span><span>Validating Canadian postal codes</span><span style="color:{MUTED}">canada.ca</span></div>'
           f'<div style="color:{FAINT}">+3 more</div></div></div>')
    browser = (f'<div class="card">{tool_header("ok","Browser","localhost:3000/signup", rs("3 actions")+rs("2.4 s"))}<div class="cb"><div style="margin:8px 10px;height:70px;border-radius:6px;overflow:hidden;position:relative;background:#FAFAFA">'
               f'<img src="{src["page"]}" alt="" style="width:100%;height:70px;object-fit:cover;object-position:top"><span style="position:absolute;left:38%;top:55%;width:18px;height:18px;border:2px solid {FG};border-radius:50%;transform:translate(-9px,-9px);box-shadow:0 0 0 1px rgba(0,0,0,.4)"></span>'
               f'<span style="position:absolute;left:10px;bottom:8px;padding:2px 6px;border-radius:4px;background:rgba(18,18,21,.85);font-size:12.1px;color:{FG}">click “Try free”</span></div></div></div>')
    sub = (f'<div class="card">{tool_header("ok","Delegated","postcode-research", rs("21 s"))}<div class="cb" style="padding:10px 12px;display:flex;flex-direction:column;gap:6px;font-size:13.2px;line-height:20px">'
           f'<div style="display:flex;gap:10px"><span style="color:{MUTED};width:64px;flex:none">You</span><span>Find the canonical CA and GB postcode formats.</span></div>'
           f'<div style="display:flex;gap:10px"><span style="color:{MUTED};width:64px;flex:none">haiku 4.6</span><span>CA is A1A 1A1 with an optional space; GB is an outward and an inward part, e.g. SW1A 1AA.</span></div></div></div>')
    mcp = (f'<div class="card">{tool_header("ok","linear · get_issue","BAZ-214", rs("0.4 s"))}<div class="cb" style="padding:8px 12px;display:grid;grid-template-columns:92px 1fr;row-gap:2px;font-size:13.2px;line-height:20px">'
           f'<span style="color:{MUTED}">id</span><span class="mono" style="font-size:12.65px">BAZ-214</span><span style="color:{MUTED}">fields</span><span class="mono" style="font-size:12.65px">title, state, assignee</span></div></div>')
    ushell = shell_card('$', 'echo hi &amp;&amp; ls', 'tool failed: environment failure: managed shell sandbox is unavailable\nThe execution environment is broken: the command was never started and every later command will fail the same way.', kind='bad', right=rs('0.0 s'))
    skill = quiet_row(g('spark', 14, "1.5", MUTED), 'Loaded skill', mono='release-notes', extra=f'<span style="color:{FAINT}">· project</span>')
    handoff = (f'<div class="card"><div class="hd" style="align-items:center"><span style="display:inline-flex;align-items:center;gap:6px">{muse_mark(16)}<span style="color:{MUTED}">→</span><span style="width:16px;height:16px;border-radius:4px;background:#D97757;color:#fff;font-size:10px;font-weight:700;display:inline-flex;align-items:center;justify-content:center">C</span></span>'
               f'<div><div class="t">Handed off to Claude Code · opus 4.6</div><div class="s">from Muse · muse-spark-1.3 · checkpointed</div></div></div>'
               f'<div style="padding:0 12px 10px;display:grid;grid-template-columns:1fr 1fr;gap:12px;font-size:13.2px;line-height:20px">'
               f'<div><div style="color:{MUTED};margin-bottom:2px">Carried</div><div>Conversation summary <span style="color:{FAINT}">· the thread so far, condensed</span></div><div>Recent turns <span style="color:{FAINT}">· last 12 turns verbatim</span></div><div>Open todos <span style="color:{FAINT}">· 3 of 7 done</span></div><div>Touched files <span style="color:{FAINT}">· src/checkout/validators.ts +2</span></div></div>'
               f'<div><div style="color:{MUTED};margin-bottom:2px">Not carried</div><div>Tool state <span style="color:{FAINT}">· running shells stop here</span></div><div>Pending approvals</div><div>Provider memory <span style="color:{FAINT}">· the old agent’s own notes</span></div></div></div>'
               f'<div class="foot"><span class="hint">pack 18 KB</span><span class="sp"></span><button class="ctl2 sec">Open the new session</button></div></div>')
    markers = (f'<div class="mkr">{g("clock",12,"1.5",MUTED) if "clock" in I else ""}<span>Today · 09:14 · session started · Claude Code v2.1.174</span></div>'
               f'<div class="mkr"><span>Context compacted · 41k → 12k tokens · <u>view summary</u></span></div>'
               f'<div class="mkr">{g("shield",12,"1.5",WARNING)}<span>Permission mode changed to <b style="color:{FG};font-weight:600">Bypass</b> for this session</span></div>'
               f'<div class="mkr"><span style="display:inline-flex;align-items:center;gap:6px">{muse_mark(14)}<span>→</span><span style="width:14px;height:14px;border-radius:4px;background:#D97757;color:#fff;font-size:9px;font-weight:700;display:inline-flex;align-items:center;justify-content:center">C</span></span><span>Handed off to Claude Code · opus 4.6 · <u>Open the new session</u></span></div>')
    need = (f'<div class="nyb">{g("warn",14,"1.5",WARNING)}<div style="flex:1"><span style="font-weight:600">Muse is waiting for you.</span> <span style="color:{MUTED}">One approval and one question above.</span></div><button class="ctl2" style="height:24px;padding:0 8px;font-size:13.2px">Jump {ico(I["chevu"],size=11)}</button></div>')
    stream = ('<div class="a"><p>CA and GB now branch per country, with focused tests over both. The GB regex accepts the space form as of<span class="caret"></span></p></div>'
              f'<div class="qrow">{glyph("spin")}<span class="shim">Running</span><span class="mono">cargo test --workspace</span><span class="mono" style="color:{FAINT}">8.1 s</span></div>')
    generic = (f'<div class="card"><div class="ch"><span class="g">{glyph("ok")}</span><span class="verb">reminderChild</span><span class="tgt">completed</span><span class="r"><span class="chev">{ico(I["chevd"],size=12)}</span></span></div>'
               f'<div class="cb" style="padding:8px 12px;font-size:14.3px;line-height:22px">Reminder from the child session.</div></div>')
    hero = (f'<div class="hero"><span style="width:48px;height:48px;border-radius:12px;background:var(--l2);box-shadow:var(--raised-sh);display:flex;align-items:center;justify-content:center">{muse_mark(20)}</span>'
            f'<div style="font-size:17.6px;font-weight:600;color:{FG}">acme-web</div><div style="font-size:14.3px">Ask Muse, or type / for commands</div></div>')
    items = [
     ('Plan card', 'Raised · r 8 · the edge. “Proposed plan” semibold with the Plan mode tag; the steps as an ordered list at body size; the action row: key hints faint, spacer, Reject · Edit as text, Accept as the primary. The choices are the moment’s: a plan exists to be accepted or not.', plan, 230),
     ('Summary card', 'The end-of-work rollup: ok glyph, headline semibold, time and cost in faint mono; files with their status letter and +N −M; the checks as a muted line; then the three next steps as text controls (Review diff · Commit… · Create PR) with none primary: they are peers.', summary, 220),
     ('Search · Web', 'The same tool header. Search: one line per hit, path:line in mono muted, the snippet in mono. Web: a 14 px favicon tile, the title, the domain muted, “+n more” faint. No open-in-browser buttons: the title is the link.', search + web, 230),
     ('Browser · Delegated · MCP', 'Browser: a 70 px screenshot strip (r 6, 8 / 10 margins) with the click ring (18, 2 px foreground) and the action label in a dark cap. Delegated: the sub-agent’s transcript as two-column lines (who · text) in caption, inside the card, no nested bubbles. MCP: server · tool as the verb, the parameters as a two-column list.', browser + sub + mcp, 350),
     ('Your own command', 'A command you typed with $: the same shell card, the verb is “$”, failure as on any card.', ushell, 110),
     ('Skill loaded · markers', 'Loading a skill is a quiet row (spark glyph, “Loaded skill”, the name in mono, its source faint). Markers are one centred caption line between two 7 % rules, 10 above and below: session started, context compacted (with its one link), permission mode changed (warning glyph), and the hand-off: both marks, the destination, one link to the new session. No hand-off card: what crossed over is shown once, in the confirm dialog before it happens.', skill + markers, 190),
     ('Needs you', 'A banner that appears above the composer when an approval or question is out of view: Raised-lit, r 8, the edge; warning glyph, headline semibold + detail muted, one Jump control. It leaves as soon as the card is on screen.', need, 70),
     ('Streaming', 'Prose streams with a 2 px caret at the end of the last line. The live activity row’s verb shimmers (muted → foreground sweep, 1.6 s) while the call runs; nothing else animates.', stream, 90),
     ('Unknown kind · child reminder', 'An item kind this build does not model shows the kind, its status and the provider’s own fallback text, folded by default. Honest and literal, never a guessed richer card.', generic, 110),
     ('Empty session', 'Before the first turn: the project mark on a Raised tile, the project name as title, the composer’s own hint. No suggestion chips, no walkthrough.', hero, 200),
    ]
    return sheet_board('D4 · The remaining cards and rows', 'Everything the library and Baaz can put in a transcript that the fixtures did not exercise, drawn from the library’s own sample data. Same rules: one edge, nothing nests, no standing actions beyond a card’s own choices.', items, H["D4More.dc.html"])

I.setdefault('clock', '<circle cx="8" cy="8" r="6"/><path d="M8 5v3l2 1.5"/>')
BOARDS.update({
 "D4Reading.dc.html": ("D4 · Reading", b_reading, 1440, 1494, 0, 9800),
 "D4Composer.dc.html": ("D4 · Composer states", b_composer, 1440, 2146, 1520, 9800),
 "D4More.dc.html": ("D4 · More cards", b_more, 1440, 2158, 3040, 9800),
})

# ======================= round 4 (owner, 2026-10-13 18:01): approval, error and a grilling session as screens =======================
D4CSS += """
.qsec{border-top:1px solid rgba(255,255,255,.07)}
.qsec .qh{display:flex;align-items:center;gap:10px;padding:10px 12px}
.qsec .num{width:20px;height:20px;border-radius:6px;background:rgba(255,255,255,.07);font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:11px;color:#9D9DA6;display:flex;align-items:center;justify-content:center;flex:none}
.qsec.done .num{background:rgba(134,239,172,.12);color:#86EFAC}
.qsec .qh .t{font-weight:600}.qsec .qh .s{font-size:13.2px;color:#9D9DA6}
.qsec.pend .qh .t{font-weight:400;color:#9D9DA6}
.cbox{width:16px;height:16px;border-radius:4px;box-shadow:inset 0 0 0 1.5px #9D9DA6;flex:none;margin-top:3px;position:relative}
.cbox.on{background:#F2F2F4;box-shadow:none}.cbox.on::after{content:"";position:absolute;left:5px;top:2px;width:4px;height:8px;border:solid #121215;border-width:0 1.5px 1.5px 0;transform:rotate(45deg)}
.other{margin:0 8px 8px;height:32px;border-radius:6px;background:var(--well);display:flex;align-items:center;gap:8px;padding:0 10px;font-size:14.3px;color:#9D9DA6}
"""

def b_approve(src):
    tx = (user_turn('Run the tests, then commit if they pass.') + '<div class="gapturn"></div>' +
          fold_row('Worked for 8.1 s · read 2 files, ran 1 command') +
          approval_card('cargo test --workspace &amp;&amp; git commit -am "Tighten CA and GB validation"'))
    st = status_row('Waiting for your approval…', key=('esc', 'to interrupt'))
    return screen(sidebar_drawn('Run the tests, then com…', 'Allow this command?', need=True), 'Run the tests, then commit if they pass.', tx, stop=True, status=st)

def b_error(src):
    tx = (user_turn('Summarise what changed on this branch.') + '<div class="gapturn"></div>' +
          error_card('Model error', ['the provider closed the stream before the reply finished', 'The turn was orphaned when the session’s process was lost.', '<span class="mono">resume_reconcile:orphaned_by_process_loss</span>']) +
          '<div class="gapturn"></div>' + user_turn('Try that again.'))
    st = status_row('Attempt 2/5 <span class="mono">retrying in 8 s</span>', tail='· rate limited by the provider')
    return screen(sidebar_drawn('Summarise what change…', 'Try that again.'), 'Summarise what changed on this branch.', tx, stop=True, status=st)

def qsec(n, header, title, state, body='', answer=None):
    cls = {'done': 'done', 'active': '', 'pend': 'pend'}[state]
    chev = ico(I["chevd" if state == 'active' else "chevr"], size=12)
    right = f'<span style="margin-left:auto;display:flex;align-items:center;gap:8px;color:{MUTED}">{"<span class=\"ic\">"+answer+"</span>" if answer else ""}<span style="display:flex">{chev}</span></span>'
    return (f'<div class="qsec {cls}"><div class="qh"><span class="num">{"✓" if state=="done" else n}</span><div><div class="s">{header}</div><div class="t">{title}</div></div>{right}</div>{body}</div>')

def opt_row(kind, label, desc, key, on=False):
    mark = f'<span class="{"cbox" if kind=="c" else "radio"}{" on" if on else ""}"></span>'
    return f'<div class="opt{" sel" if on else ""}">{mark}<div style="flex:1"><div class="l">{label}</div>{"<div class=\"d\">"+desc+"</div>" if desc else ""}</div><span class="key">{key}</span></div>'

def b_grill(src):
    q2 = ('<div style="padding-bottom:4px">' +
          opt_row('c', 'US ZIP and ZIP+4', '', '1', True) + opt_row('c', 'Canadian postal', '', '2', True) +
          opt_row('c', 'UK postcode', '', '3') +
          f'<div class="other">{g("edit", 13, "1.5", MUTED)}Other…</div></div>')
    # a wizard: one question at a time, a 3 px progress bar under the header, Back / Next in the footer
    card = (f'<div class="card need"><div class="hd" style="align-items:center"><span class="tile">{ico(I["q"], size=14)}</span><div><div class="t">Address validation</div><div class="s">Muse needs four answers before editing the validator</div></div>'
            f'<span style="margin-left:auto;display:flex;align-items:center;gap:10px"><span class="mono" style="font-size:12.1px;color:{FAINT}">2 of 4</span><span class="tag warn">Auto-resolves in 2 m 00 s</span></span></div>'
            f'<div class="bar"><i style="width:25%"></i></div>'
            f'<div class="hd" style="padding-bottom:4px"><div><div class="s">Formats · pick all that apply</div><div class="t">Which postcode formats should validate?</div></div></div>'
            + q2 +
            f'<div class="foot"><button class="ctl2" style="padding-left:6px">{ico(I["back"], size=12)}Back</button><span class="hint">↑↓ move · space pick · ⏎ next</span><span class="sp"></span><button class="ctl2">Skip</button><button class="ctl2 sec">Explain instead</button><button class="ctl2 pri">Next</button></div></div>')
    tx = user_turn('Tighten address validation for CA and GB; ask me anything you need first.') + card
    st = status_row('Waiting for your answers…', key=('esc', 'to interrupt'))
    return screen(sidebar_drawn('Tighten address validati…', 'Asked: 4 questions', need=True), 'Tighten address validation for CA and GB; ask me anything you need first.', tx, stop=True, status=st)

BOARDS.update({
 "D4Approve.dc.html": ("D4 · Approval pending", b_approve, 1440, 900, 0, 12280),
 "D4Error.dc.html": ("D4 · Error + retry", b_error, 1440, 900, 1520, 12280),
 "D4Grill.dc.html": ("D4 · Several questions", b_grill, 1440, 900, 3040, 12280),
})

# ======================= round 5 (owner, 2026-10-13 18:02): the composer's pickers; send / stop emphasis =======================
D4CSS += """
.send.go{background:#F2F2F4;color:#121215;box-shadow:none}
.pk{width:360px;background:var(--l3);border-radius:10px;padding:6px;box-shadow:var(--float-sh);font-size:14.3px}
.pk .h{height:28px;display:flex;align-items:center;padding:0 10px;font-size:12.1px;color:#9D9DA6}
.pk .r{display:flex;align-items:center;gap:10px;padding:6px 10px;border-radius:6px;min-height:36px}
.pk .r.sel{background:rgba(255,255,255,.09)}.pk .r.hov{background:rgba(255,255,255,.05)}
.pk .r .t{font-weight:400}.pk .r.sel .t{font-weight:600}.pk .r .d{font-size:13.2px;line-height:18px;color:#9D9DA6}
.pk .r .m{margin-left:auto;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;color:#6E6E78;white-space:nowrap}
.pk .r .b{font-size:11px;line-height:16px;padding:0 6px;border-radius:999px;background:rgba(255,255,255,.09);color:#9D9DA6}
.pk .sep{height:1px;background:rgba(255,255,255,.07);margin:4px -6px}
.pk .mark{width:16px;height:16px;border-radius:4px;color:#fff;font-size:10px;font-weight:700;display:inline-flex;align-items:center;justify-content:center;flex:none}
.pk .chk{margin-left:auto;color:#F2F2F4;display:flex}
.sub{position:absolute;left:100%;top:0;margin-left:4px}
.sl{padding:10px 12px 12px}
.sl .track{position:relative;height:4px;border-radius:2px;background:rgba(255,255,255,.12);margin:14px 8px 10px}
.sl .fill{position:absolute;left:0;top:0;height:4px;border-radius:2px;background:#F2F2F4}
.sl .tick{position:absolute;top:-3px;width:2px;height:10px;margin-left:-1px;background:rgba(255,255,255,.18);border-radius:1px}
.sl .thumb{position:absolute;top:-6px;width:16px;height:16px;margin-left:-8px;border-radius:50%;background:#F2F2F4;box-shadow:0 0 0 1px rgba(0,0,0,.4)}
.sl .lbls{display:flex;justify-content:space-between;font-size:11px;color:#6E6E78;padding:0 2px}
"""

def pk_row(title, detail=None, sel=False, hov=False, meta=None, badge=None, mark=None, chev=False):
    m = f'<span class="mark" style="background:{mark[1]}">{mark[0]}</span>' if mark else ''
    right = ''
    if meta: right += f'<span class="m">{meta}</span>'
    if badge: right += f'<span class="b" style="margin-left:{"6px" if meta else "auto"}">{badge}</span>'
    if sel: right += f'<span class="chk">{ico(I["ok"], size=14, sw="1.8")}</span>'
    if chev: right += f'<span class="chk" style="color:{MUTED}">{ico(I["chevr"], size=12)}</span>'
    return f'<div class="r{" sel" if sel else ""}{" hov" if hov else ""}">{m}<div><div class="t">{title}</div>{"<div class=\"d\">"+detail+"</div>" if detail else ""}</div>{right}</div>'

def picker(header, rows_html, w=360, extra=''):
    return f'<div class="pk" style="width:{w}px;position:relative"><div class="h">{header}</div>{rows_html}{extra}</div>'

MUSE, CLAUDE, CODEX = ('M', '#3B6DF2'), ('C', '#D97757'), ('O', '#2C2C32')

def b_pickers(src):
    provider = picker('Provider',
        pk_row('Muse', sel=True, mark=MUSE) + pk_row('Claude Code', mark=CLAUDE) + pk_row('Codex', mark=CODEX), w=240)
    model = picker('Model',
        pk_row('muse-spark-1.3', sel=True, meta='1.0M ctx', badge='active') +
        pk_row('muse-spark-1.3-contributor', meta='1.0M ctx', badge='default') +
        pk_row('muse-spark-1.2', meta='1.0M ctx') +
        pk_row('muse-echo', 'Replays the last answer; costs nothing.', meta='—'))
    nested_sub = ('<div class="pk sub" style="width:300px"><div class="h">Reasoning effort · muse-spark-1.3</div>' +
                  pk_row('Default', 'Leave it to the provider.', sel=True) + pk_row('Low', 'A short budget.') + pk_row('Medium', 'The middle budget, and the usual default.', hov=True) + pk_row('High', 'A long budget.') + pk_row('Max', 'Longer than extra high, below ultra.') + '</div>')
    nested = picker('Model',
        pk_row('muse-spark-1.3', sel=True, meta='1.0M ctx', badge='active', chev=True) +
        pk_row('muse-spark-1.3-contributor', meta='1.0M ctx', badge='default', chev=True) +
        pk_row('muse-spark-1.2', meta='1.0M ctx', chev=True), extra=nested_sub)
    effort = picker('Reasoning effort',
        pk_row('Default', 'Leave it to the provider; the field is omitted.', sel=True) + pk_row('None', 'No reasoning at all.') + pk_row('Minimal', 'The smallest budget the provider offers.') +
        pk_row('Low', 'A short budget.') + pk_row('Medium', 'The middle budget, and the usual default.') + pk_row('High', 'A long budget.') +
        pk_row('Extra high', 'Longer than high.') + pk_row('Max', 'Longer than extra high, below ultra.') + pk_row('Ultra', 'The largest budget MSP accepts.'))
    stops = ['None', 'Minimal', 'Low', 'Medium', 'High', 'Extra high', 'Max', 'Ultra']
    ticks = ''.join(f'<i class="tick" style="left:{i*100/7:.2f}%"></i>' for i in range(8))
    slider = (f'<div class="pk" style="width:360px"><div class="h">Reasoning effort</div><div class="sl"><div style="display:flex;align-items:baseline;gap:8px"><span style="font-weight:600">High</span><span style="font-size:13.2px;color:{MUTED}">A long budget.</span></div>'
              f'<div class="track"><div class="fill" style="width:{4*100/7:.2f}%"></div>{ticks}<i class="thumb" style="left:{4*100/7:.2f}%"></i></div>'
              f'<div class="lbls"><span>None</span><span>Medium</span><span>Ultra</span></div></div>'
              f'<div class="sep"></div>{pk_row("Default", "Leave it to the provider; the field is omitted.")}</div>')
    mode = picker('Approval mode',
        pk_row('Full access', 'Nothing is gated; every action runs.') + pk_row('Auto', 'Policy decides; unmatched actions ask only when the agent requests it.', sel=True) +
        pk_row('Ask', 'Anything without a matching rule raises an approval card.') + pk_row('Read-only', 'Anything without a matching rule is refused.'))
    send_states = (f'<div style="display:flex;gap:40px;align-items:center;font-size:13.2px;color:{MUTED}">'
                   f'<span style="display:flex;align-items:center;gap:10px"><button class="send">{ico(I["up"], sw="1.8", size=14)}</button>empty · Floating, muted glyph</span>'
                   f'<span style="display:flex;align-items:center;gap:10px"><button class="send go">{ico(I["up"], sw="2", size=14)}</button>ready · foreground fill, frame glyph</span>'
                   f'<span style="display:flex;align-items:center;gap:10px"><button class="send go">{ico(I["sqr"], size=12)}</button>running · the same fill, a 10 px square</span></div>')
    items = [
     ('Send · Stop', 'The one primary action gets the one solid fill in the composer. Empty: the round 28 px button at Floating with the edge and a muted glyph, as before. Ready (a draft, an attachment, or a queued line): foreground fill #F2F2F4, the arrow in the frame colour, no edge. Running: the same fill with a 10 px stop square. The change is a cross-fade of the fill (80 ms) and the glyph swap on spring.fast (0.6 → 1 scale + opacity); nothing moves.',
      send_states + '<div style="height:12px"></div>' + composer('Tighten the GB regex so the space form passes too.').replace('class="send" aria-label="Send"', 'class="send go" aria-label="Send"').replace(ico(I["up"], sw="1.8", size=14), ico(I["up"], sw="2", size=14)), 180),
     ('Provider', 'Every picker is the one dropdown: Floating #2C2C32, r 10, the edge + the float drop, 6 px padding (the highlight keeps 6 px of panel on every side), a caption header, rows r 6 at 36 with the selected row at 9 % and semibold, hover 5 %. Just the providers: mark (16, r 4) and name, the current one checked. Picking another on a session that already has turns opens the hand-off confirm (the session cannot switch provider in place); “Hand off to…” also lives in the session’s ⋯ menu. New-session choices are not here: that is the sidebar’s New session.',
      inline(provider), 300),
     ('Model', 'Rows: the model id, its context window in faint mono, badges as 9 % caps (active, default), the check on the current one. Any number of rows may wear a badge; the check is the only truth.',
      inline(model), 220),
     ('Nested', 'A row with a chevron opens its submenu inside the menu’s own box, 4 px to the right, top-aligned with the row (FF’s rule: a submenu laid out outside the box turned every pick into an outside press). Here: a model’s effort levels. Hover opens it after 150 ms; the arrow keys enter it.',
      inline(nested), 300),
     ('Effort · list', 'The nine levels the wire allows, each with its one-line meaning, the current one checked. This is what the app has today.',
      inline(effort), 420),
     ('Effort · slider', 'The same levels as a slider: 4 px track at 12 %, filled to the current stop in foreground, 8 ticks, a 16 px thumb; the name and meaning above, None / Medium / Ultra under the ends and the middle; Default stays a row beneath the rule (it is not a level, it is “leave it”). Drag or arrow keys snap to a stop; the chip label updates live.',
      inline(slider), 200),
     ('Approval mode', 'Four rows with their meaning: Full access, Auto, Ask, Read-only. The chip shows the short name.',
      inline(mode), 240),
    ]
    return sheet_board('D4 · Pickers and the primary action', 'The composer’s four pickers on the one dropdown, a nested submenu, the effort as a slider, and the send / stop button given the composer’s only solid fill. Rows and meanings from the app’s own pickers (captured) and the library’s sample models.', items, H["D4Pickers.dc.html"])

# the screens: a running turn shows the emphasised stop; a draft the emphasised send
_screen_prev = screen
def screen(sidebar_html, title, transcript_html, below_composer='', overlay='', stop=False, status=''):
    out = _screen_prev(sidebar_html, title, transcript_html, below_composer, overlay, stop, status)
    if stop:
        out = out.replace('class="send" aria-label="Send"', 'class="send go" aria-label="Stop"')
    return out

BOARDS.update({"D4Pickers.dc.html": ("D4 · Pickers + send", b_pickers, 1440, 2190, 0, 13300)})

# ======================= round 6 (owner, 2026-10-13 18:22): sent attachments, annotations, quotations =======================
D4CSS += """
.u .atts{display:flex;gap:6px;justify-content:flex-end;flex-wrap:wrap;max-width:686px}
.u .atts .im{width:64px;height:64px;border-radius:6px;overflow:hidden;background:var(--l2);box-shadow:var(--raised-sh);position:relative}
.u .atts .im img{width:64px;height:64px;object-fit:cover;display:block}
.u .atts .im .play{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;color:#F2F2F4;background:rgba(0,0,0,.25)}
.u .atts .im .dur{position:absolute;right:4px;bottom:4px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:10px;line-height:14px;padding:0 4px;border-radius:3px;background:rgba(18,18,21,.85)}
.u .atts .fc{height:30px;border-radius:6px;background:var(--l2);box-shadow:var(--raised-sh);display:inline-flex;align-items:center;gap:6px;padding:0 10px 0 6px;font-size:13.2px;white-space:nowrap}
.u .atts .fc .m{color:#9D9DA6;font-size:12.1px}
.u .bub .qt{margin:0 0 8px;padding:1px 0 1px 12px;border-left:2px solid rgba(255,255,255,.18);color:#9D9DA6;font-size:13.2px;line-height:19px;display:-webkit-box;-webkit-line-clamp:3;-webkit-box-orient:vertical;overflow:hidden}
.u .bub .qsrc{font-size:12.1px;color:#6E6E78;margin:-4px 0 8px 14px}
.ann{margin:0 0 8px}
.ann .shot{position:relative;height:120px;border-radius:6px;overflow:hidden;background:#FAFAFA}
.ann .shot img{width:100%;height:120px;object-fit:cover;object-position:top;display:block}
.ann .pin{position:absolute;width:18px;height:18px;border-radius:50%;background:#F2F2F4;color:#121215;font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;transform:translate(-9px,-9px);box-shadow:0 0 0 1px rgba(0,0,0,.4)}
.ann .ln{display:flex;gap:8px;font-size:13.2px;line-height:19px;margin-top:6px}
.ann .ln .n{width:18px;height:18px;border-radius:50%;background:rgba(255,255,255,.09);font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;flex:none;margin-top:1px}
.ann .ln .sel{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.1px;color:#9D9DA6}
.ann .hd{font-size:12.1px;color:#9D9DA6;margin:6px 0 0}
"""

def user_turn_rich(inner, atts='', w=None):
    style = f' style="width:{w}px"' if w else ''
    bub = f'<div class="bub"{style}>{inner}</div>'
    a = f'<div class="atts">{atts}</div>' if atts else ''
    return f'<div class="u">{a}{bub}<div class="rail"></div></div>'

def _items_round6(src):
    atts = (f'<span class="im"><img src="{src["thumb"]}" alt=""></span><span class="im"><img src="{src["page"]}" alt=""><span class="play">{ico(I["play"], size=16)}</span><span class="dur">0:42</span></span>'
            f'<span class="fc">{g("file", 13, "1.5", MUTED)}synthetic-markdown.jsonl<span class="m">JSONL · 2.4 KB</span></span>')
    quote = ('<div class="qt">the provider closed the stream before the reply finished</div><div class="qsrc">from the reply above</div>Try that again.')
    ann = (f'<div class="ann"><div class="shot"><img src="{src["page"]}" alt=""><span class="pin" style="left:20%;top:30%">1</span><span class="pin" style="left:38%;top:55%">2</span><span class="pin" style="left:74%;top:55%">3</span></div>'
           f'<div class="hd">3 annotations · localhost:3000/pricing</div>'
           f'<div class="ln"><span class="n">1</span><div><span class="sel">h1 · “Simple pricing”</span><div>Headline is fine; subtitle needs the free-trial length.</div></div></div>'
           f'<div class="ln"><span class="n">2</span><div><span class="sel">div.card.starter</span><div>Make the Starter card stand out; it is the plan most people pick.</div></div></div>'
           f'<div class="ln"><span class="n">3</span><div><span class="sel">div.card.enterprise</span><div>Drop the “Contact us” button here; link the text instead.</div></div></div></div>')
    return [
     ('User turn · attachments', 'Sent attachments sit above the bubble, right-aligned, 6 apart, 4 above it: images and video as 64 px tiles (r 6, the edge; a video tile gets the play glyph over a 25 % scrim and its length in a cap — the agent gets its frames, not a player); files as 30 px chips (file glyph, name, type and size muted). Click a tile to open it in the right pane. Nothing on hover but the turn’s own edit · copy.',
      user_turn_rich('Here is the capture and the fixture.', atts), 150),
     ('User turn · quotation', 'Select text in any answer and the selection’s rail offers Quote; it lands in the composer as a Text attachment (a chip with the quote glyph and the first words) and is sent as a quote block at the top of the bubble: 2 px rule at 18 %, caption muted, clamped to three lines, “from the reply above” faint under it, then your words. Clicking the quote scrolls to its source.',
      user_turn_rich(quote, w=520), 130),
     ('User turn · annotations', 'A batch from the Browse annotator (pins on elements, notes beside the page) is one user turn: the page screenshot (120 px strip, r 6) with the numbered pins, a caption line with the count and the address, then one line per note: its number, the element selector in mono muted, the note. The agent gets the same batch with the selectors and boxes.',
      user_turn_rich(ann, w=620), 330),
    ]


if __name__ == "__main__":
    entries = {}
    for f, (t, fn, w, h, x, y) in BOARDS.items():
        H[f] = h
        page(f[:-8], t, fn, w, h)
        entries[f] = {"x": x, "y": y, "w": w, "h": h, "title": t}
    json.dump(entries, open(ROOT + "/d4_boards.json", "w"), indent=1)
    print("wrote", list(BOARDS))




