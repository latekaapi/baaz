#!/usr/bin/env python3
"""Baaz v3 canvas boards. Reads blobs.json {name: "/_blob/<id>"}; writes project/*.dc.html + canvas.json,
and preview/*.html (same boards with local image paths and no support.js) for a look before publishing."""
import os, json
ROOT = os.path.dirname(os.path.abspath(__file__))
P = ROOT + "/project"; V = ROOT + "/preview"
os.makedirs(P, exist_ok=True); os.makedirs(V, exist_ok=True)
BLOBS = json.load(open(ROOT + "/blobs.json"))
LOCAL = {k: f"../assets/{k}.png" for k in BLOBS}

# ---------------- the v3 ladder (cool-neutral, bigger steps at the bottom) ----------------
LIT = dict(l0="#121215", l1="#1B1B1F", l2="#232328", l2h="#2A2A30", l3="#2C2C32", l4="#35353C", well="#151518")
L0, L1, L2, L2h, L3, L4, WELL = "var(--l0)", "var(--l1)", "var(--l2)", "var(--l2h)", "var(--l3)", "var(--l4)", "var(--well)"
FG, MUTED, FAINT = "#F2F2F4", "#9D9DA6", "#6E6E78"
HOVER, SEL = "rgba(255,255,255,.05)", "rgba(255,255,255,.09)"
HAIR = "rgba(255,255,255,.07)"
EDGE = "inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06)"  # the one edge
# depth recipes: nested surfaces get fill + a top highlight; only floating things get a dark ring + diffuse shadow
SHEET = "var(--sheet-sh)"
RAISED = "var(--raised-sh)"
FLOAT = "var(--float-sh)"
DIALOG = "var(--dialog-sh)"

def ico(p, sw="1.5", size=16, fill=False):
    if fill: return f'<svg width="{size}" height="{size}" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">{p}</svg>'
    return f'<svg width="{size}" height="{size}" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{p}</svg>'
I = dict(
 side='<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M6 3v10"/>',
 right='<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M10 3v10"/>',
 list='<path d="M3 4h10M3 8h10M3 12h7"/>',
 globe='<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
 term='<path d="M3 4l4 4-4 4M8 12h5"/>',
 dots='<circle cx="3" cy="8" r="1.3"/><circle cx="8" cy="8" r="1.3"/><circle cx="13" cy="8" r="1.3"/>',
 back='<path d="M13 8H3M7 4L3 8l4 4"/>', fwd='<path d="M3 8h10M9 4l4 4-4 4"/>',
 reload='<path d="M13 8a5 5 0 1 1-1.5-3.6M13 3v2.5h-2.5"/>',
 spark='<path d="M8 2l1.4 3.6L13 7l-3.6 1.4L8 12l-1.4-3.6L3 7l3.6-1.4z"/>',
 shield='<path d="M8 2l5 2v4c0 3-2.2 5.2-5 6-2.8-.8-5-3-5-6V4z"/>',
 plus='<path d="M8 3v10M3 8h10"/>', check='<path d="M3.5 8.5l3 3 6-7"/>',
 down='<path d="M4 6l4 4 4-4"/>', up='<path d="M8 13V3M4 7l4-4 4 4"/>',
 search='<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
)
def ib(icon, label): return f'<button class="ib" aria-label="{label}">{ico(I[icon], fill=(icon=="dots"))}</button>'
def lights(x=12, y=16): return (f'<span style="position:absolute;left:{x}px;top:{y}px;display:flex;gap:8px;z-index:4" aria-hidden="true">'
    '<i style="width:12px;height:12px;border-radius:999px;background:#FF5F57;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#FEBC2E;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#28C840;display:block"></i></span>')
def lights_inline(): return ('<span style="display:flex;gap:8px;margin:0 8px 0 10px" aria-hidden="true">'
    '<i style="width:12px;height:12px;border-radius:999px;background:#FF5F57;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#FEBC2E;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#28C840;display:block"></i></span>')
def switch(on):
    a='class="on" aria-selected="true"'; b='aria-selected="false"'
    return (f'<div class="modes" role="tablist" aria-label="Sidebar mode">'
            f'<button role="tab" {a if on=="s" else b} aria-label="Sessions">{ico(I["list"])}</button>'
            f'<button role="tab" {a if on=="b" else b} aria-label="Browse">{ico(I["globe"])}</button></div>')
def img(name, w, h, src):
    return f'<img src="{src[name]}" alt="" style="width:{w}px;height:{h}px;display:block">'

CSS = f"""
body{{margin:0}}
a{{color:{FG}}}a:hover{{color:#fff}}
.w{{font-family:"Inter",-apple-system,"SF Pro Text","Helvetica Neue",sans-serif;color:{FG};-webkit-font-smoothing:antialiased;font-size:13px;line-height:20px;position:relative;overflow:hidden;background:{L0}}}
.w *{{box-sizing:border-box}}
.mono{{font-family:"Geist Mono",ui-monospace,Menlo,monospace}}
.ib{{width:28px;height:28px;border-radius:6px;display:flex;align-items:center;justify-content:center;color:{MUTED};background:transparent;border:0;padding:0;flex:none}}
.ib:hover{{background:{HOVER};color:{FG}}}
.modes{{display:flex;height:24px;padding:2px;border-radius:6px;background:{HOVER}}}
.modes button{{width:28px;height:20px;border:0;border-radius:4px;background:transparent;color:{MUTED};padding:0;display:flex;align-items:center;justify-content:center}}
.modes button.on{{background:{L3};color:{FG};box-shadow:{EDGE}}}
.sheet{{position:absolute;background:{L1};border-radius:8px;box-shadow:{SHEET};overflow:hidden}}
.fade{{position:absolute;left:0;right:0;top:0;height:64px;background:linear-gradient({L1} 20%,rgb(from {L1} r g b / 0));pointer-events:none}}
.row{{position:absolute;left:0;right:0;top:0;height:32px;display:flex;align-items:center;z-index:2}}
.title{{font-weight:600;color:{FG};white-space:nowrap}}
.pill{{position:absolute;height:32px;border-radius:16px;background:{L3};box-shadow:{FLOAT};z-index:3}}
.contact{{position:absolute;left:242px;top:6px;bottom:6px;width:20px;background:linear-gradient(90deg,rgba(0,0,0,0),rgba(0,0,0,.16));z-index:1}}
.lfade{{position:absolute;left:0;top:44px;width:256px;height:24px;background:linear-gradient({L0},rgb(from {L0} r g b / 0));z-index:2}}
.comp{{position:absolute;background:{L2};border-radius:12px;box-shadow:{RAISED};padding:8px;display:flex;flex-direction:column;gap:4px}}
.ph{{font-size:14px;line-height:20px;color:{MUTED};padding:8px}}
.ctl{{height:28px;padding:0 8px;border-radius:6px;display:inline-flex;align-items:center;gap:6px;color:{MUTED};font-size:13px;white-space:nowrap;background:transparent;border:0}}
.ctl:hover{{background:{HOVER};color:{FG}}}
.ctl.on{{color:{FG}}}
.send{{width:28px;height:28px;border-radius:999px;background:{L3};color:{MUTED};display:flex;align-items:center;justify-content:center;border:0;box-shadow:{EDGE}}}
.menu{{position:absolute;background:{L3};border-radius:10px;padding:4px;box-shadow:{FLOAT};width:240px;z-index:4}}
.menu .mr{{height:36px;display:flex;align-items:center;padding:0 10px;border-radius:6px;gap:8px;color:{FG}}}
.menu .mr.sel{{font-weight:600}} .menu .mr.hov{{background:{HOVER}}}
.menu .mh{{height:28px;display:flex;align-items:center;padding:0 10px;color:{MUTED};font-size:12px}}
.menu .sep{{height:1px;background:{HAIR};margin:4px -4px}}
.addr{{display:flex;align-items:center;gap:8px;height:28px;padding:0 10px;border-radius:6px;color:{MUTED};white-space:nowrap}}
.addr b{{font-weight:600;color:{FG}}}
"""

VARS = "--l0:#121215;--l1:#1B1B1F;--l2:#232328;--l2h:#2A2A30;--l3:#2C2C32;--l4:#35353C;--well:#151518;--sheet-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06);--raised-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06);--float-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 8px 24px -10px rgba(0,0,0,.4);--dialog-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 24px 56px -16px rgba(0,0,0,.5);"
PREVIEW_VARS = "--l0:#121215;--l1:#1B1B1F;--l2:#232328;--l2h:#2A2A30;--l3:#2C2C32;--l4:#35353C;--well:#151518;--sheet-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06);--raised-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06);--float-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 8px 24px -10px rgba(0,0,0,.4);--dialog-sh:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 24px 56px -16px rgba(0,0,0,.5);"
PROPS_SURFACE = '"edge":{"editor":"enum","options":["light","dark"],"default":"light","section":"Surfaces"},"shadow":{"editor":"range","min":0,"max":1,"step":0.05,"default":0.5,"section":"Surfaces"},"ring":{"editor":"range","min":0,"max":0.2,"step":0.01,"default":0.06,"section":"Surfaces"}'
PROPS_COLOUR = '"scheme":{"editor":"enum","options":["cool","neutral","ff"],"default":"cool","section":"Colour"},"contrast":{"editor":"range","min":0.6,"max":1.5,"step":0.05,"default":1,"section":"Colour"}'
LOGIC = """class Component extends DCLogic {
  renderVals() {
    const p = this.props;
    const scheme = p.scheme ?? 'cool', k = p.contrast ?? 1, edge = p.edge ?? 'light', s = p.shadow ?? 0.5, r = p.ring ?? 0.06;
    const ladders = {
      cool: [[18,18,21],[27,27,31],[35,35,40],[42,42,48],[44,44,50],[53,53,60],[21,21,24]],
      neutral: [[18,18,18],[27,27,27],[35,35,35],[42,42,42],[44,44,44],[53,53,53],[21,21,21]],
      ff: [[23,23,23],[30,30,30],[37,37,37],[42,42,42],[44,44,44],[51,51,51],[20,20,20]]
    };
    const base = ladders[scheme] || ladders.cool, b0 = base[0];
    const hex = c => '#' + c.map(v => Math.max(0, Math.min(255, Math.round(v))).toString(16).padStart(2, '0')).join('');
    const lv = i => hex(base[i].map((v, j) => b0[j] + (v - b0[j]) * k));
    const hi = 'inset 0 1px 0 rgba(255,255,255,.07)';
    const ring = edge === 'dark' ? ('0 0 0 1px rgba(0,0,0,' + (0.15 + 0.6 * r).toFixed(2) + ')') : ('inset 0 0 0 1px rgba(255,255,255,' + r.toFixed(2) + ')');
    return {
      l0: lv(0), l1: lv(1), l2: lv(2), l2h: lv(3), l3: lv(4), l4: lv(5), well: lv(6),
      sheetSh: hi + ', ' + ring, raisedSh: hi + ', ' + ring,
      floatSh: hi + ', ' + ring + ', 0 8px 24px -10px rgba(0,0,0,' + (0.8 * s).toFixed(2) + ')',
      dialogSh: hi + ', ' + ring + ', 0 24px 56px -16px rgba(0,0,0,' + s.toFixed(2) + ')'
    };
  }
}"""

def page(name, title, body, w, h, colour=True):
    props = ''
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
<style>{CSS}</style>
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
    open(f"{P}/{name}.dc.html", "w").write(html)
    prev = f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Geist+Mono:wght@400&display=swap"><style>{CSS}body{{background:#000}}</style></head><body>{body(LOCAL).replace('class="w" style="', 'class="w" style="' + PREVIEW_VARS, 1)}</body></html>'
    open(f"{V}/{name}.html", "w").write(prev)
    return (w, h)

# ---------------- pieces ----------------
def muse_mark(size=14):
    return f'<span style="width:{size}px;height:{size}px;border-radius:4px;background:#3B6DF2;color:#fff;font-size:{size-5}px;font-weight:700;display:inline-flex;align-items:center;justify-content:center;line-height:1">M</span>'

def composer_a(x, bottom, w=880):
    """Direction A: one raised card; the field above, quiet text controls below. (Claude Desktop's shape.)"""
    return (f'<div class="comp" style="left:{x}px;bottom:{bottom}px;width:{w}px">'
            f'<div class="ph">Ask Muse, or type / for commands</div>'
            f'<div style="display:flex;align-items:center;gap:4px">'
            f'{ib("plus","Attach or insert")}'
            f'<button class="ctl on">{muse_mark()}Muse{ico(I["down"],size=12)}</button>'
            f'<span class="mono" style="font-size:11px;color:{FAINT};margin-left:4px">0 tokens</span>'
            f'<span style="margin-left:auto;display:flex;align-items:center;gap:4px">'
            f'<button class="ctl">Muse Spark 1.3{ico(I["down"],size=12)}</button>'
            f'<button class="ctl">Auto{ico(I["down"],size=12)}</button>'
            f'<button class="ctl">Default{ico(I["down"],size=12)}</button>'
            f'<button class="send" aria-label="Send" style="margin-left:4px">{ico(I["up"],sw="1.8",size=14)}</button></span></div></div>')

def composer_b(x, bottom, w=824):
    """Direction B: a single-line field; options as one quiet line beneath it."""
    return (f'<div style="position:absolute;left:{x}px;bottom:{bottom}px;width:{w}px">'
            f'<div style="height:48px;border-radius:24px;background:{L2};box-shadow:{RAISED};display:flex;align-items:center;padding:0 8px 0 8px;gap:8px">'
            f'<button class="send" aria-label="Attach or insert" style="width:32px;height:32px;background:{L3}">{ico(I["plus"],size=14)}</button>'
            f'<span style="font-size:14px;color:{MUTED};flex:1">Ask Muse, or type / for commands</span>'
            f'<button class="send" aria-label="Send" style="width:32px;height:32px">{ico(I["up"],sw="1.8",size=14)}</button></div>'
            f'<div style="display:flex;align-items:center;gap:2px;padding:6px 10px 0">'
            f'<button class="ctl on" style="gap:7px;padding-left:4px">{muse_mark()}Muse{ico(I["down"],size=12)}</button>'
            f'<button class="ctl">Muse Spark 1.3{ico(I["down"],size=12)}</button><button class="ctl">Auto{ico(I["down"],size=12)}</button><button class="ctl">Default{ico(I["down"],size=12)}</button>'
            f'<span class="mono" style="font-size:11px;color:{FAINT};margin-left:auto">0 tokens</span></div></div>')

def actions():
    return f'{ib("term","Terminal")}{ib("dots","Session menu")}{ib("right","Show right pane")}'

TITLE = "Show me the validation summary with code and links."

def chrome(mode):
    # The chrome row (2026-10-13, round 3): lights (12,16), the toggle at x 84, the switch ending 12 before the
    # sidebar's right edge — the sidebar column's own top row when open, the pill's contents when closed.
    return (f'{lights()}'
            f'<button class="ib" aria-label="Hide sidebar" style="position:absolute;left:84px;top:8px;z-index:4">{ico(I["side"])}</button>'
            f'<div style="position:absolute;left:184px;top:10px;z-index:4">{switch(mode)}</div>')

def pill(mode, reveal_html="", right_html=""):
    # Closed: the row becomes a pill — 32 tall, 6 in from the corner, Floating + the edge — holding exactly the
    # lights, the toggle and the switch at the same x and y. Hovered, it extends to the right with the title (Browse:
    # the page controls and address) on spring.moderate and the actions pill fades in at the top right; both leave
    # 250 ms after the pointer goes. The boards show the hovered state so the reveal is visible.
    w = 244 + (420 if reveal_html else 0)
    b = f'<div class="pill" style="left:6px;top:6px;width:{w}px"></div>{chrome(mode)}'
    if reveal_html:
        b += f'<div style="position:absolute;left:260px;top:0;height:44px;display:flex;align-items:center;z-index:4;white-space:nowrap">{reveal_html}</div>'
    if right_html:
        b += f'<div class="pill" style="right:6px;top:6px;padding:0 4px;display:flex;align-items:center;gap:2px">{right_html}</div>'
    return b

def sidebar_sessions(src):
    return (f'{chrome("s")}'
            f'<div style="position:absolute;left:0;top:44px">{img("sb-sessions-rows",254,804,src)}</div>'
            f'<div style="position:absolute;left:0;top:852px">{img("sb-footer",254,48,src)}</div><div class="contact"></div><div class="lfade"></div>')

# ---------------- boards ----------------
def b_sessions_open(src):
    cw = 1440-262-6
    b = f'<div class="w" style="width:1440px;height:900px">{sidebar_sessions(src)}'
    b += f'<div class="sheet" style="left:262px;top:6px;right:6px;bottom:6px">'
    b += f'<div style="position:absolute;left:{(cw-892)//2}px;top:56px;width:892px;height:648px">{img("tx-open",892,648,src)}</div>'
    b += f'<div class="fade"></div><div class="row" style="padding-left:16px"><span class="title">{TITLE}</span><span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{actions()}</span></div></div>'
    b += composer_a(262 + (cw-880)//2, 18)
    return b + '</div>'

def b_sessions_closed(src):
    b = f'<div class="w" style="width:1440px;height:900px">'
    b += f'<div class="sheet" style="left:6px;top:6px;right:6px;bottom:6px">'
    b += f'<div style="position:absolute;left:268px;top:76px;width:892px;height:648px">{img("tx-closed",892,648,src)}</div>'
    b += f'<div class="fade" style="height:92px"></div></div>'
    b += composer_a((1440-880)//2, 18)
    b += pill("s", f'<span class="title">{TITLE}</span>', actions())
    return b + '</div>'

def b_browse_open(src):
    b = f'<div class="w" style="width:1440px;height:900px"><div style="position:absolute;left:0;top:0">{img("sb-browse",254,900,src)}</div>'
    # one chrome for both modes: cover the captured top row and footer with the drawn ones Sessions uses
    b += f'<div style="position:absolute;left:0;top:0;width:256px;height:44px;background:{L0}"></div>{chrome("b")}'
    b += f'<div style="position:absolute;left:0;top:852px">{img("sb-footer",254,48,src)}</div><div class="contact"></div><div class="lfade"></div>'
    b += f'<div class="sheet" style="left:262px;top:6px;right:6px;bottom:6px;background:#FAFAFA;box-shadow:0 0 0 1px rgba(0,0,0,.5)"><div style="position:absolute;left:0;top:0">{img("page",1168,881,src)}</div></div>'
    return b + '</div>'

def b_browse_closed(src):
    page_chrome = (f'{ib("back","Back")}{ib("fwd","Forward")}{ib("reload","Reload")}'
                   f'<span class="addr" style="padding:0 10px 0 8px">{ico(I["shield"],size=14)}<b>Fluid Functionalism</b>www.fluidfunctionalism.com</span>')
    b = f'<div class="w" style="width:1440px;height:900px">'
    b += f'<div class="sheet" style="left:6px;top:6px;right:6px;bottom:6px;background:#FAFAFA;box-shadow:0 0 0 1px rgba(0,0,0,.5)"><div style="position:absolute;left:0;top:0">{img("page-closed",1424,848,src)}</div></div>'
    b += pill("b", page_chrome, ib("spark","Ask")+ib("dots","Page menu")+ib("plus","New tab"))
    return b + '</div>'

# ---- Tone study: the same miniature in three ladders ----
def mini(l0, l1, l2, l3, fg, muted, label, note, x):
    sel = "rgba(255,255,255,.09)"
    return (f'<div style="position:absolute;left:{x}px;top:72px;width:420px;height:300px;border-radius:10px;background:{l0};overflow:hidden;box-shadow:0 0 0 1px rgba(255,255,255,.06)">'
            f'<div style="position:absolute;left:12px;top:14px;display:flex;gap:6px"><i style="width:9px;height:9px;border-radius:9px;background:#FF5F57;display:block"></i><i style="width:9px;height:9px;border-radius:9px;background:#FEBC2E;display:block"></i><i style="width:9px;height:9px;border-radius:9px;background:#28C840;display:block"></i></div>'
            f'<div style="position:absolute;left:10px;top:40px;width:104px;font-size:10px;line-height:14px;color:{muted}">'
            f'<div style="padding:4px 8px">Search</div><div style="padding:4px 8px">New session</div><div style="padding:4px 8px;border-radius:5px;background:{sel};color:{fg};font-weight:600">Token audit</div><div style="padding:4px 8px">Relay pass</div></div>'
            f'<div style="position:absolute;left:124px;top:6px;right:6px;bottom:6px;border-radius:7px;background:{l1};box-shadow:inset 0 1px 0 rgba(255,255,255,.06),inset 0 0 0 1px rgba(255,255,255,.035)">'
            f'<div style="position:absolute;right:12px;top:28px;padding:5px 9px;border-radius:7px;background:{l2};font-size:10px;color:{fg}">Apply the review, then rerun.</div>'
            f'<div style="position:absolute;left:14px;top:66px;width:180px;font-size:10px;line-height:14px;color:{fg}">Fixes 1 and 2 landed.<div style="margin-top:6px;height:46px;border-radius:6px;background:{l2};box-shadow:inset 0 1px 0 rgba(255,255,255,.05)"></div></div>'
            f'<div style="position:absolute;left:14px;right:14px;bottom:12px;height:48px;border-radius:9px;background:{l2};box-shadow:inset 0 1px 0 rgba(255,255,255,.06),inset 0 0 0 1px rgba(255,255,255,.03)"><div style="padding:8px 10px;font-size:10px;color:{muted}">Ask Muse</div></div>'
            f'<div style="position:absolute;left:150px;top:118px;width:120px;border-radius:7px;background:{l3};padding:3px;box-shadow:{FLOAT}"><div style="padding:5px 8px;font-size:10px;color:{fg};font-weight:600;border-radius:4px">Muse Spark 1.3</div><div style="padding:5px 8px;font-size:10px;color:{fg};background:rgba(255,255,255,.05);border-radius:4px">Muse Spark 1.2</div><div style="padding:5px 8px;font-size:10px;color:{fg}">Muse Sonar</div></div>'
            f'</div></div>'
            f'<div style="position:absolute;left:{x}px;top:388px;width:420px"><div style="font-weight:600">{label}</div><div style="color:{MUTED};font-size:12px;line-height:17px;margin-top:4px">{note}</div>'
            f'<div style="display:flex;gap:6px;margin-top:10px">' + "".join(f'<span style="width:72px;height:40px;border-radius:6px;background:{c};box-shadow:inset 0 0 0 1px rgba(255,255,255,.08);font-size:9px;color:{MUTED};display:flex;align-items:flex-end;padding:4px" class="mono">{c}</span>' for c in (l0,l1,l2,l3)) + '</div></div>')

def b_tone(src):
    b = f'<div class="w" style="width:1440px;height:520px;background:#0B0B0D">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">Tone: the same screen in three ladders</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;color:{MUTED}">Frame · sheet · raised · floating. Depth reads from the fill step first; an edge is only a hint.</div>'
    b += mini("#171717","#1E1E1E","#252525","#2C2C2C","#F5F5F5","#A3A3A3","A · Fluid Functionalism as shipped","Seven units per step, neutral. On a Mac at 2× the steps read, but the frame and the sheet sit close enough that the sheet needs its ring to be seen.",40)
    b += mini("#121212","#1B1B1B","#232323","#2C2C2C","#F5F5F5","#A3A3A3","B · Neutral, darker frame, nine-unit steps","Same structure, the frame drops to #121212 (Claude Desktop's sidebar is #111111) and each step grows. The sheet reads without its ring.",510)
    b += mini(LIT["l0"],LIT["l1"],LIT["l2"],LIT["l3"],FG,MUTED,"C · Cool-neutral, same steps (chosen)","B with two to three units of blue in each grey: the chrome stops looking like dead grey without ever reading as blue. Claude Desktop's panel #131215 and Linear's chrome sit here.",980)
    return b + '</div>'

# ---- Depth study ----
def b_depth(src):
    b = f'<div class="w" style="width:1440px;height:620px;background:{L0}">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">Depth: four recipes, nothing else</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;color:{MUTED}">Light comes from above. A surface that sits in another gets a fill step and a 1 px highlight on its top edge. Only a surface that floats casts a shadow: one soft drop at 40 %. Every surface shares one edge: a 7 % top highlight and a 6 % white ring.</div>'
    # sheet sample
    b += f'<div class="sheet" style="left:40px;top:96px;width:300px;height:200px"><div style="padding:14px 16px;font-size:12px;color:{MUTED}">Sheet on frame<br><span class="mono" style="color:{FAINT};font-size:11px">{LIT["l1"]} on {LIT["l0"]}<br>inset 0 1px 0 white 7 %<br>inset 0 0 0 1px white 6 %</span></div></div>'
    # raised on sheet
    b += f'<div class="sheet" style="left:380px;top:96px;width:300px;height:200px"><div class="comp" style="left:16px;top:40px;width:268px"><div class="ph" style="padding-bottom:4px">Raised on sheet</div><div class="mono" style="font-size:11px;color:{FAINT}">{LIT["l2"]} on {LIT["l1"]}<br>inset 0 1px 0 white 7 % · ring white 6 %</div></div></div>'
    # floating
    b += f'<div class="sheet" style="left:720px;top:96px;width:300px;height:200px"><div class="menu" style="left:30px;top:36px;width:240px"><div class="mh">Floating: menus, pills, popovers</div><div class="mr sel">{LIT["l3"]}<span style="margin-left:auto;color:{MUTED}">{ico(I["check"])}</span></div><div class="mr hov">ring white 6 % + one 8/24 drop at 40 %</div></div></div>'
    # dialog
    b += f'<div class="sheet" style="left:1060px;top:96px;width:340px;height:200px"><div style="position:absolute;inset:0;background:rgba(0,0,0,.45)"></div><div style="position:absolute;left:30px;top:34px;width:280px;background:{L4};border-radius:10px;box-shadow:{DIALOG};padding:16px 18px"><div style="font-size:14px;font-weight:600">Dialog</div><div style="font-size:12px;color:{MUTED};margin-top:4px">{LIT["l4"]} over a 45 % scrim, one deep soft drop.</div><div style="margin-top:14px;display:flex;justify-content:flex-end;gap:8px"><button class="ctl" style="background:{L3};box-shadow:{EDGE};color:{FG}">Cancel</button><button class="ctl" style="background:{FG};color:{L0};font-weight:600">Archive</button></div></div></div>'
    # rules
    rules = [("Frame", L0, LIT["l0"], "window, sidebar, the gaps"), ("Sheet", L1, LIT["l1"], "the work area; transcript and page live here"), ("Raised", L2, LIT["l2"], "composer, user turn, code block, cards in the transcript"), ("Floating", L3, LIT["l3"], "menus, pills, popovers, toasts, the peek sidebar"), ("Dialog", L4, LIT["l4"], "over a 45 % scrim"), ("Well", WELL, LIT["well"], "inputs and wells cut into a raised surface")]
    b += f'<div style="position:absolute;left:40px;top:330px;display:flex;gap:16px">' + "".join(
        f'<div style="width:216px"><div style="height:56px;border-radius:8px;background:{c};box-shadow:inset 0 0 0 1px rgba(255,255,255,.07)"></div><div style="margin-top:8px;font-weight:600">{n} <span class="mono" style="color:{FAINT};font-weight:400;font-size:11px">{lit}</span></div><div style="font-size:12px;color:{MUTED};line-height:17px">{d}</div></div>' for n,c,lit,d in rules) + '</div>'
    b += f'<div style="position:absolute;left:40px;top:500px;font-size:12px;color:{MUTED};line-height:18px;width:1360px">Overlays stay surface-relative: hover white 5 %, selected 9 %, pressed 12 %. Text is {FG} or {MUTED}, never an opacity. Lines inside a surface (table rules, menu separators) are white 7 %. Radii: sheet 8, raised 12 (composer, bubbles), menus and pills 10 / full, controls 6. One edge for every surface: a 7 % top highlight and a 6 % white ring. The white page sheet is the one exception: a 50 % black ring, since a white highlight cannot show on white.</div>'
    return b + '</div>'

def b_composers(src):
    b = f'<div class="w" style="width:1440px;height:360px;background:{L1}">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">Composer: two directions on the sheet</div>'
    b += f'<div style="position:absolute;left:40px;top:60px;width:620px;font-size:12px;color:{MUTED};line-height:17px"><b style="color:{FG}">A · one raised card (chosen for the screens).</b> The field on top, a quiet row of text controls below: provider, then model, mode and effort as text with a chevron, lit on hover. Token count sits in the faint mono. One send button. No bordered chips: the card is the only edge.</div>'
    b += composer_a(40, 40, w=620)
    b += f'<div style="position:absolute;left:740px;top:60px;width:620px;font-size:12px;color:{MUTED};line-height:17px"><b style="color:{FG}">B · a single-line field.</b> A 48 px pill field with attach and send inside it; the options become one quiet line underneath, outside the field. Lighter on an empty session, cramped once the draft grows to three lines.</div>'
    b += composer_b(740, 40, w=620)
    return b + '</div>'

boards = {
 "Main.dc.html": ("Tone", b_tone, 1440, 520, 0, 0),
 "Depth.dc.html": ("Depth", b_depth, 1440, 620, 0, 640),
 "Composers.dc.html": ("Composer", b_composers, 1440, 360, 0, 1380),
 "SessionsOpen.dc.html": ("Sessions · open", b_sessions_open, 1440, 900, 0, 2000),
 "SessionsClosed.dc.html": ("Sessions · closed", b_sessions_closed, 1440, 900, 1520, 2000),
 "BrowseOpen.dc.html": ("Browse · open", b_browse_open, 1440, 900, 0, 3020),
 "BrowseClosed.dc.html": ("Browse · closed", b_browse_closed, 1440, 900, 1520, 3020),
}
index = {"v": 3, "createdOnFiles": {"v": 1, "at": "2026-10-12T15:50:00Z"}, "title": "Baaz v3", "launch": {"view": "canvas"}, "pages": [], "boards": {}, "order": [], "notes": {}, "designSystems": []}
for f, (t, fn, w, h, x, y) in boards.items():
    page(f[:-8], t, fn, w, h, colour=(f in ("Main.dc.html","Depth.dc.html","Composers.dc.html")))
    index["boards"][f] = {"x": x, "y": y, "w": w, "h": h, "title": t}
    index["order"].insert(0, f)
index["notes"] = {
 "n6": {"x": 3040, "y": 2480, "w": 420, "fill": "blue", "text": "Polish pass 2026-10-12 (rhythm per FF), approved: one edge on every surface, the send button and the switch segment included (no private drops; the switch track is the 5 % overlay). Weights only 400 / 600: the pill title is 600, like the open title row. The open title row is centred on the lights' line (y 22), title 16 in, mirroring the action glyphs. The composer sits on the transcript's 880 measure (flush with the code block and table), FF InputMessage padding (8 around, text at 16, 4 between field and controls), 18 from the window bottom, the pill 32 tall, 6 from the corner, end caps 14. Browse open uses the same drawn chrome and account footer as Sessions."},
 "t1": {"x": 0, "y": -240, "kind": "title1", "maxW": 2960, "text": "Baaz v3 — FINAL 2026-10-12: tone, depth, composer, then the four screens"},
 "t2": {"x": 0, "y": 1760, "kind": "title1", "maxW": 2960, "text": "The four screens — Sessions and Browse, open and closed"},
 "n1": {"x": 1520, "y": 0, "w": 420, "fill": "blue", "text": "Why the previous rounds read flat: a seven-unit step between near-black greys is about 3 L* and the eye needs 4 to 5 to see a level without a line. Drawing the line back in made it look outlined. So: a darker frame, nine-unit steps, highlight on the top edge instead of a border, and a slight cool bias so the greys are not dead."},
 "n2": {"x": 1520, "y": 640, "w": 420, "fill": "blue", "text": "FINAL (owner, 2026-10-12): the cool-neutral ladder, light edge (7 % top highlight + 6 % white ring on every surface), floating drop 8/24 at 40 %, dialog drop 24/56 at 50 %. Only two kinds of depth exist: a surface that sits in another (fill + top highlight) and a surface that floats (a faint ring + one soft drop). FF's stacked small drops at 18 % are replaced by one diffuse drop; FF's structure (substrate + offset, surface-relative overlays, the springs) stays."},
 "n3": {"x": 1520, "y": 1380, "w": 420, "fill": "blue", "text": "A is used on the screens. The chips lose their borders; the composer card is the single edge at the bottom of the sheet. Provider stays visible as a mark, model and mode as text controls that light on hover."},
 "n4": {"x": 3040, "y": 2000, "w": 420, "fill": "blue", "text": "Revised 2026-10-13 (owner round 3): the sidebar column full height on the frame with its chrome row on top — lights (12,16), Hide/Show sidebar at x 84, the switch ending 12 before the column's edge; the sheet 6 px in with its own 32 px title row (title at 16, actions right) centred on the lights' line. Closed: only the column leaves (ease-out 260 / 220 ms, the sheet's content laid out once); the row stays as a PILL — 32 tall, 6 from the corner, Floating + the edge — holding exactly lights · toggle · switch at the same x and y. Hovered (this board), the pill extends right with the title on spring.moderate and the actions pill fades in at the top right; both leave 250 ms after the pointer. A 150 ms rest on the toggle or the left edge peeks the sidebar as a Floating card behind the pill. Depth: the sheet's contact shadow beside the open sidebar (black 16 % → 0 over 20 px); rows fade under the row (24 px)."},
 "n5": {"x": 3040, "y": 3020, "w": 420, "fill": "blue", "text": "Browse: the page is the sheet, white on the frame. Open, the sidebar's own header holds the address and the page controls. Closed: the pill holds lights · toggle · switch; hovered (this board) it extends with back, forward, reload and the address at rest, and the actions pill brings Ask, the page menu and new tab. No tab strip: tabs are one hover away in the peek. The pills and the peek over the native page are holes cut through the page window."},
}
json.dump(index, open(f"{P}/canvas.json", "w"), indent=1)
print("wrote", list(boards))
