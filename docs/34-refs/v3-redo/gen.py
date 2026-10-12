#!/usr/bin/env python3
"""v3 surfaces — candidate boards composed from captures of the built app.
Writes html/*.html + img/*.png (crops). Render with render.sh."""
import os, subprocess
from PIL import Image

ROOT = os.path.dirname(os.path.abspath(__file__))
CAP = ROOT + "/cap"
IMG = ROOT + "/img"; HTML = ROOT + "/html"
for d in (IMG, HTML): os.makedirs(d, exist_ok=True)

# ---------- FF tokens (docs/31a §0) ----------
S = {1:"#171717",2:"#1E1E1E",3:"#252525",4:"#2C2C2C",5:"#333333",6:"#3A3A3A",7:"#414141",8:"#484848"}
FG, MUTED = "#F5F5F5", "#A3A3A3"
BORDER = "rgba(245,245,245,.12)"      # FF --border (foreground 12 %)
BORDER60 = "rgba(245,245,245,.072)"   # FF border/60 (menu separators)
D1="0 1px 1px -0.5px rgba(0,0,0,.18)"; D3="0 3px 3px -1.5px rgba(0,0,0,.18)"
D6="0 6px 6px -3px rgba(0,0,0,.18)"; D12="0 12px 12px -6px rgba(0,0,0,.18)"
SH = {
 1: "inset 0 0 0 1px rgba(255,255,255,.02)",
 2: f"inset 0 1px 0 rgba(255,255,255,.01),inset 0 0 0 1px rgba(255,255,255,.02),{D1}",
 3: f"inset 0 1px 0 rgba(255,255,255,.02),inset 0 0 0 1px rgba(255,255,255,.02),0 0 0 1px rgba(0,0,0,.12),{D1},{D3}",
 4: f"inset 0 1px 0 rgba(255,255,255,.02),inset 0 0 0 1px rgba(255,255,255,.04),0 0 0 1px rgba(0,0,0,.14),{D1},{D3},{D6}",
 5: f"inset 0 1px 0 rgba(255,255,255,.04),inset 0 0 0 1px rgba(255,255,255,.04),0 0 0 1px rgba(0,0,0,.16),{D1},{D3},{D6},{D12}",
}
# edge treatments for decision 1 (what separates a card from its substrate)
EDGE = {
 "ring":  dict(border="1px solid transparent", shadow=SH[2]),                 # FF exact: shadow-2's 2 % ring
 "b60":   dict(border=f"1px solid {BORDER60}", shadow=SH[2]),                # FF border/60 + shadow-2
 "b100":  dict(border=f"1px solid {BORDER}", shadow=SH[2]),                  # FF --border + shadow-2
}

# ---------- crops from the 2× captures ----------
def crop(src, box, name):
    im = Image.open(f"{CAP}/{src}.png").convert("RGB")
    x0,y0,x1,y1 = [v*2 for v in box]
    im.crop((x0,y0,x1,y1)).save(f"{IMG}/{name}.png")
    return name, x1-x0, y1-y0  # 2× px

def find_card(src, y=450):
    im = Image.open(f"{CAP}/{src}.png").convert("RGB")
    xs = [x for x in range(520, 2880) if im.getpixel((x, y*2)) != (23,23,23)]
    ys = [yy for yy in range(8, 1800) if im.getpixel((1400, yy)) != (23,23,23)]
    return xs[0]//2, ys[0]//2, xs[-1]//2+1, ys[-1]//2+1

crop("T-open", (0,0,254,848), "sb-sessions")          # sessions sidebar, lights+toggle+switch row included, no edge line
crop("T-open", (0,852,254,900), "sb-footer")          # footer row (below its rule)
crop("T-open", (402,52,1294,700), "tx-open")          # transcript column as laid out when open
crop("T-closed", (280,52,1172,700), "tx-closed")      # … when closed (re-centred)
crop("T-closed", (300,822,1150,892), "composer-inner") # placeholder, chips, tokens, send — as built
crop("browse-open", (0,0,254,900), "sb-browse")
bx0,by0,bx1,by1 = find_card("browse-open")
imc=Image.open(CAP+"/browse-closed.png").convert("RGB")
PT=[y for y in range(0,1800) if imc.getpixel((1400,y))==(250,250,250)][0]//2
crop("browse-open", (bx0+2,by0+2,bx1-2,by1-2), "page")
crop("sessions-right", (1042,0,1440,900), "pane")
crop("browse-closed", (8,PT,1432,892), "page-closed")
# U1: the transcript inside an s2 card — recolour the base pixels of the crop to s2
im=Image.open(IMG+"/tx-open.png").convert("RGB"); px=im.load()
for y in range(im.height):
  for x in range(im.width):
    if px[x,y]==(23,23,23): px[x,y]=(30,30,30)
im.save(IMG+"/tx-open-s2.png")
im=Image.open(IMG+"/tx-closed.png").convert("RGB"); px=im.load()
for y in range(im.height):
  for x in range(im.width):
    if px[x,y]==(23,23,23): px[x,y]=(30,30,30)
im.save(IMG+"/tx-closed-s2.png")
crop("T-open", (0,44,254,848), "sb-rows")
im=Image.open(IMG+"/composer-inner.png").convert("RGB"); px=im.load()
for y in range(im.height):
  for x in range(im.width):
    if px[x,y]==(23,23,23): px[x,y]=(37,37,37)
im.save(IMG+"/composer-inner-s3.png")
print("browse card", (bx0,by0,bx1,by1))

# ---------- pieces ----------
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
 plus='<path d="M8 3v10M3 8h10"/>', x='<path d="M4 4l8 8M12 4l-8 8"/>',
 check='<path d="M3.5 8.5l3 3 6-7"/>', down='<path d="M4 6l4 4 4-4"/>',
)
def ib(icon, label, extra=""):
    return f'<button class="ib {extra}" aria-label="{label}">{ico(I[icon], fill=(icon=="dots"))}</button>'
LIGHTS = ('<span class="lights" aria-hidden="true"><i style="background:#FF5F57"></i><i style="background:#FEBC2E"></i><i style="background:#28C840"></i></span>')
def switch(on):
    a='class="on" aria-selected="true"'; b='aria-selected="false"'
    return (f'<div class="modes" role="tablist" aria-label="Sidebar mode">'
            f'<button role="tab" {a if on=="s" else b} aria-label="Sessions">{ico(I["list"])}</button>'
            f'<button role="tab" {a if on=="b" else b} aria-label="Browse">{ico(I["globe"])}</button></div>')
def img(name, w, h, style=""):
    return f'<img src="../img/{name}.png" alt="" style="width:{w}px;height:{h}px;display:block;{style}">'

CSS = f"""
body{{margin:0;background:#000}}
.w{{font-family:Inter,-apple-system,"SF Pro Text",sans-serif;color:{FG};-webkit-font-smoothing:antialiased;font-size:13px;line-height:20px;width:1440px;height:900px;background:{S[1]};position:relative;overflow:hidden}}
.w *{{box-sizing:border-box}}
.lights{{position:absolute;left:13px;top:16px;display:flex;gap:8px}} .lights i{{width:12px;height:12px;border-radius:999px;display:block}}
.ib{{width:28px;height:28px;border-radius:6px;display:flex;align-items:center;justify-content:center;color:{MUTED};background:transparent;border:0;padding:0;flex:none}}
.ib.sm{{width:24px;height:24px}}
.modes{{display:flex;height:24px;padding:2px;border-radius:6px;background:{S[2]}}}
.modes button{{width:28px;height:20px;border:0;border-radius:4px;background:transparent;color:{MUTED};padding:0;display:flex;align-items:center;justify-content:center}}
.modes button.on{{background:{S[4]};color:{FG};box-shadow:{SH[4]}}}
.row{{position:absolute;left:0;right:0;top:0;height:44px;display:flex;align-items:center}}
.title{{font-weight:600;color:{FG};white-space:nowrap}} .crumb{{color:{MUTED};margin-left:10px;white-space:nowrap}}
.fade{{position:absolute;left:0;right:0;top:44px;height:48px;background:linear-gradient({S[1]},rgba(23,23,23,0));pointer-events:none}}
.card{{background:{S[2]};border-radius:10px;overflow:hidden;position:absolute}}
.menu{{position:absolute;background:{S[4]};border-radius:10px;padding:4px;box-shadow:{SH[3]};width:288px}}
.menu .mr{{height:36px;display:flex;align-items:center;padding:0 8px;border-radius:6px;gap:8px;color:{FG}}}
.menu .mr.sel{{font-weight:600}} .menu .mr.hov{{background:rgba(255,255,255,.06)}}
.menu .mh{{height:28px;display:flex;align-items:center;padding:0 8px;color:{MUTED};font-size:12px}}
.menu .sep{{height:1px;background:{BORDER60};margin:4px -4px}}
.scrim{{position:absolute;inset:0;background:rgba(0,0,0,.4)}}
.dialog{{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);width:440px;background:{S[5]};border-radius:10px;box-shadow:{SH[5]};padding:20px}}
.btn{{height:32px;padding:0 12px;border-radius:6px;display:inline-flex;align-items:center;gap:6px;font-size:13px;border:1px solid {BORDER};background:transparent;color:{FG}}}
.btn.primary{{background:{FG};color:{S[1]};border-color:transparent;font-weight:600}}
.sel{{height:32px;padding:0 10px;border-radius:6px;display:inline-flex;align-items:center;gap:8px;border:1px solid {BORDER};color:{FG};background:transparent}}
.tip{{position:absolute;background:{FG};color:{S[1]};font-size:12px;line-height:16px;padding:4px 8px;border-radius:6px;white-space:nowrap}}
.toast{{position:absolute;right:16px;bottom:16px;height:40px;padding:0 8px 0 14px;display:flex;align-items:center;gap:14px;background:{S[3]};border-radius:10px;box-shadow:{SH[3]}}}
.addr{{display:flex;align-items:center;gap:8px;height:28px;padding:0 10px;border-radius:6px;color:{MUTED};white-space:nowrap}}
.addr b{{font-weight:600;color:{FG}}}
.pill{{position:absolute;top:10px;height:32px;border-radius:16px;background:{S[2]};display:flex;align-items:center;gap:2px;padding:0 6px}}
"""

def page(name, body, edge="b60", extra_css=""):
    e = EDGE[edge]
    css = CSS + f".card{{border:{e['border']};box-shadow:{e['shadow']}}}" + extra_css
    html = f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&display=swap"><style>{css}</style></head><body><div class="w">{body}</div></body></html>'
    open(f"{HTML}/{name}.html","w").write(html)

def composer(x, w=824, inside=False, picker=False):
    clipw = min(850, w-24)
    lvl = S[3] if inside else S[2]
    extra = "" if not inside else f"background:{lvl};box-shadow:{SH[3]};"
    body = f'<div class="card" style="left:{x}px;width:{w}px;bottom:{20 if inside else 12}px;height:84px;{extra}">' \
           f'<div style="position:absolute;left:12px;top:6px;width:{clipw}px;overflow:hidden">{img("composer-inner-s3" if inside else "composer-inner",850,70)}</div></div>'
    if picker:
        body += (f'<div class="menu" style="left:{x+140}px;bottom:104px;width:240px">'
                 f'<div class="mh">Muse · model</div>'
                 f'<div class="mr sel">Muse Spark 1.3<span style="margin-left:auto;color:{MUTED}">{ico(I["check"])}</span></div>'
                 f'<div class="mr hov">Muse Spark 1.2</div><div class="mr">Muse Sonar</div>'
                 f'<div class="sep"></div><div class="mr">Effort<span style="margin-left:auto;color:{MUTED};font-size:12px">Medium ›</span></div></div>')
    return body

def actions(right=False):
    return f'<span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{ib("term","Terminal")}{ib("dots","Session menu")}{ib("right","Show right pane", "on" if right else "")}</span>'

TITLE = "Show me the validation summary with code and links."

# ---- U2: the base is the room; cards for page / pane / composer / peek ----
def u2_sessions_open(edge="b60", name="U2-SessionsOpen", footer_rule=True, pane=False, picker=False, hairline=False):
    tx_x = 402 if not pane else 268
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-sessions",254,848)}</div>'
    body += (f'<div style="position:absolute;left:0;top:848px;width:254px;height:52px">'
             f'{"<div style=\"height:1px;background:%s;margin:0 8px\"></div>" % BORDER if footer_rule else ""}'
             f'<div style="position:absolute;left:0;top:4px">{img("sb-footer",254,48)}</div></div>')
    cw = 1440 if not pane else 1440-8-400-8
    body += f'<div class="row" style="left:256px;right:{0 if not pane else 416}px;padding-left:10px"><span class="title">{TITLE}</span>{actions(pane)}</div>'
    body += f'<div class="fade" style="left:256px;right:{0 if not pane else 416}px"></div>'
    body += f'<div style="position:absolute;left:{tx_x}px;top:52px;width:892px;height:648px;overflow:hidden;{"transform:scale(.84);transform-origin:0 0" if pane else ""}">{img("tx-open",892,648)}</div>'
    cw_ = min(824, cw-256-48); cx = 256 + ((cw-256)-cw_)//2
    if hairline: body += f'<div style="position:absolute;left:255px;top:0;bottom:0;width:1px;background:{BORDER}"></div>'
    body += composer(cx, w=cw_, picker=picker)
    if pane:
        body += f'<div class="card" style="right:8px;top:8px;bottom:8px;width:400px"><div style="position:absolute;left:-1px;top:-1px">{img("pane",398,900)}</div></div>'
    page(name, body, edge)

def u2_sessions_closed(edge="b60", name="U2-SessionsClosed", peek=False, depth=False):
    body = LIGHTS
    body += f'<div class="row" style="padding-left:85px;gap:0"><span style="display:flex;align-items:center;gap:6px">{ib("side","Show sidebar")}{switch("s")}</span><span class="title" style="margin-left:14px">{TITLE}</span>{actions()}</div>'
    body += '<div class="fade"></div>'
    body += f'<div style="position:absolute;left:280px;top:52px;width:892px;height:648px">{img("tx-closed",892,648)}</div>'
    body += composer((1440-824)//2)
    if peek:
        body += (f'<div class="card" style="left:8px;top:44px;bottom:8px;width:256px;box-shadow:{SH[5]}">'
                 f'<div style="position:absolute;left:0;top:4px">{img("sb-rows",254,804)}</div>'
                 f'<div style="position:absolute;left:0;right:0;bottom:0">{img("sb-footer",254,48)}</div></div>')
    if depth:
        body += '<div class="scrim"></div>'
        body += (f'<div class="dialog"><div style="font-size:16px;line-height:22px;font-weight:600">Archive this session?</div>'
                 f'<div style="margin-top:8px;color:{MUTED}">It leaves the sidebar and stops its agent. You can find it again under Show archived.</div>'
                 f'<div style="margin-top:16px;display:flex;align-items:center;gap:8px"><span style="color:{MUTED};font-size:12px">Worktree</span>'
                 f'<span class="sel">Keep{ico(I["down"],size=14)}</span></div>'
                 f'<div class="menu" style="left:20px;top:132px;width:200px;background:{S[7]}"><div class="mr sel">Keep<span style="margin-left:auto;color:{MUTED}">{ico(I["check"])}</span></div><div class="mr hov">Remove</div></div>'
                 f'<div style="margin-top:24px;display:flex;justify-content:flex-end;gap:8px"><button class="btn">Cancel</button><button class="btn primary">Archive</button></div></div>')
        body += f'<div class="tip" style="left:1296px;top:46px">Terminal <span style="opacity:.6;margin-left:4px">⌘J</span></div>'
        body += f'<div class="toast">Session archived<button class="btn" style="height:28px;padding:0 10px">Undo</button></div>'
    page(name, body, edge)

def p_sessions_closed(edge="b60", name="P-SessionsClosed"):
    # the refined pill variant: lights inside the pill, 32 tall, title regular
    body = (f'<div class="pill" style="left:12px;padding-left:12px;gap:4px;border:1px solid {BORDER60};box-shadow:{SH[2]}">'
            f'<span style="display:flex;gap:8px;margin-right:10px"><i style="width:12px;height:12px;border-radius:999px;background:#FF5F57;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#FEBC2E;display:block"></i><i style="width:12px;height:12px;border-radius:999px;background:#28C840;display:block"></i></span>'
            f'{ib("side","Show sidebar","sm")}{switch("s")}<span style="margin-left:8px;margin-right:6px;white-space:nowrap">{TITLE}</span></div>')
    body += (f'<div class="pill" style="right:12px;border:1px solid {BORDER60};box-shadow:{SH[2]}">{ib("term","Terminal","sm")}{ib("dots","Session menu","sm")}{ib("right","Show right pane","sm")}</div>')
    body += '<div class="fade" style="top:0;height:64px"></div>'
    body += f'<div style="position:absolute;left:280px;top:60px;width:892px;height:648px">{img("tx-closed",892,648)}</div>'
    body += composer((1440-824)//2)
    page(name, body, edge)


def u1_row_open(pane=False):
    return (f'<div class="row" style="left:256px;right:{0 if not pane else 416}px;padding-left:18px"><span class="title">{TITLE}</span>{actions(pane)}</div>')

def u1_sessions_open(edge="b60", name="U1-SessionsOpen", pane=False, picker=False):
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-sessions",254,848)}</div>'
    body += (f'<div style="position:absolute;left:0;top:848px;width:254px;height:52px">'
             f'<div style="height:1px;background:{BORDER};margin:0 8px"></div>'
             f'<div style="position:absolute;left:0;top:4px">{img("sb-footer",254,48)}</div></div>')
    body += u1_row_open(pane)
    right = 8 if not pane else 8+400+8
    cw = 1440-264-right
    body += f'<div class="card" style="left:264px;top:44px;right:{right}px;bottom:8px">'
    body += f'<div class="fade" style="top:0;background:linear-gradient({S[2]},rgba(30,30,30,0))"></div>'
    sc = 1 if not pane else .84
    body += f'<div style="position:absolute;left:{(cw-892)//2 if not pane else 16}px;top:12px;width:892px;height:648px;transform:scale({sc});transform-origin:0 0">{img("tx-open-s2",892,648)}</div>'
    body += '</div>'
    w_ = min(824, cw-48)
    body += composer(264 + (cw-w_)//2, w=w_, inside=True, picker=picker)
    if pane:
        body += f'<div class="card" style="right:8px;top:44px;bottom:8px;width:400px"><div style="position:absolute;left:-1px;top:-1px">{img("pane",398,900)}</div></div>'
    page(name, body, edge)

def u1_sessions_closed(edge="b60", name="U1-SessionsClosed", peek=False):
    body = LIGHTS
    body += f'<div class="row" style="padding-left:85px;gap:0"><span style="display:flex;align-items:center;gap:6px">{ib("side","Show sidebar")}{switch("s")}</span><span class="title" style="margin-left:14px">{TITLE}</span>{actions()}</div>'
    body += f'<div class="card" style="left:8px;top:44px;right:8px;bottom:8px">'
    body += f'<div class="fade" style="top:0;background:linear-gradient({S[2]},rgba(30,30,30,0))"></div>'
    body += f'<div style="position:absolute;left:266px;top:12px;width:892px;height:648px">{img("tx-closed-s2",892,648)}</div>'
    body += '</div>'
    body += composer((1440-824)//2, inside=True)
    if peek:
        body += (f'<div class="card" style="left:8px;top:44px;bottom:8px;width:256px;box-shadow:{SH[5]}">'
                 f'<div style="position:absolute;left:0;top:4px">{img("sb-rows",254,804)}</div>'
                 f'<div style="position:absolute;left:0;right:0;bottom:0">{img("sb-footer",254,48)}</div></div>')
    page(name, body, edge)

def u1_browse_open(edge="b60", name="U1-BrowseOpen"):
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-browse",254,900)}</div>'
    body += (f'<div class="row" style="left:256px;right:0;padding-left:18px"><span class="addr" style="padding:0">{ico(I["shield"],size=14)}<b>Fluid Functionalism</b>www.fluidfunctionalism.com</span>'
             f'<span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{ib("spark","Ask")}{ib("dots","Page menu")}{ib("plus","New tab")}</span></div>')
    body += f'<div class="card" style="left:264px;top:44px;right:8px;bottom:8px;background:#fff"><div style="position:absolute;left:0;top:0">{img("page",bx1-bx0-4,by1-by0-4)}</div></div>'
    page(name, body, edge)

def u2_browse_open(edge="b60", name="U2-BrowseOpen"):
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-browse",254,900)}</div>'
    body += f'<div class="card" style="left:264px;top:8px;right:8px;bottom:8px;background:#fff"><div style="position:absolute;left:0;top:0">{img("page",bx1-bx0-4,by1-by0-4)}</div></div>'
    page(name, body, edge)

def u2_browse_closed(edge="b60", name="U2-BrowseClosed"):
    body = LIGHTS
    body += (f'<div class="row" style="padding-left:85px"><span style="display:flex;align-items:center;gap:6px">{ib("side","Show sidebar")}{switch("b")}</span>'
             f'<span style="display:flex;gap:2px;margin-left:14px">{ib("back","Back")}{ib("fwd","Forward")}{ib("reload","Reload")}</span>'
             f'<span class="addr" style="margin-left:6px">{ico(I["shield"],size=14)}<b>Fluid Functionalism</b>www.fluidfunctionalism.com</span>'
             f'<span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{ib("spark","Ask")}{ib("dots","Page menu")}{ib("plus","New tab")}</span></div>')
    body += f'<div class="card" style="left:8px;top:44px;right:8px;bottom:8px;background:#FAFAFA"><div style="position:absolute;left:-1px;top:-1px">{img("page-closed",1424,892-PT)}</div></div>'
    page(name, body, edge)

for e in EDGE:
    u2_sessions_open(e, f"U2-SessionsOpen-{e}", pane=True, picker=True)
u2_sessions_open(name="U2-SessionsOpen")
u2_sessions_open(name="U2-SessionsOpen-nofooter", footer_rule=False)
u2_sessions_open(name="U2-SessionsOpen-hairline", hairline=True)
u2_sessions_open(name="U2-Pane", pane=True, picker=True)
u2_sessions_closed(name="U2-SessionsClosed")
u2_sessions_closed(name="U2-Peek", peek=True)
u2_sessions_closed(name="U2-Depth", depth=True)
p_sessions_closed()
u1_sessions_open()
u1_sessions_open(name="U1-Pane", pane=True, picker=True)
u1_sessions_closed()
u1_sessions_closed(name="U1-Peek", peek=True)
u1_browse_open()
u2_browse_closed(name="U1-BrowseClosed")
u2_browse_open()
u2_browse_closed()
print("ok")

# ================= FINAL: four screens =================
# Sidebar on the base. The work area is an s2 card, 8 px in from the window on every side, edge = border/60 + shadow-2.
# Open: the sidebar carries the window row; the session title row sits inside the card (content scrolls under a fade).
# Closed: the card stays where it is; two pills float over it: lights · toggle · switch · title | actions.
PILL = f"background:{S[3]};border:1px solid {BORDER60};box-shadow:{SH[3]};height:36px;border-radius:18px;display:flex;align-items:center;padding:0 4px;position:absolute;top:20px"
def lights_inline():
    return ('<span style="display:flex;gap:8px;margin:0 10px 0 10px" aria-hidden="true">'
            '<i style="width:12px;height:12px;border-radius:999px;background:#FF5F57;display:block"></i>'
            '<i style="width:12px;height:12px;border-radius:999px;background:#FEBC2E;display:block"></i>'
            '<i style="width:12px;height:12px;border-radius:999px;background:#28C840;display:block"></i></span>')
def card_fade():
    return f'<div style="position:absolute;left:0;right:0;top:0;height:64px;background:linear-gradient({S[2]} 20%,rgba(30,30,30,0));pointer-events:none"></div>'

def final_sessions_open(name="F-SessionsOpen"):
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-sessions",254,848)}</div>'
    body += (f'<div style="position:absolute;left:0;top:848px;width:254px;height:52px">'
             f'<div style="height:1px;background:{BORDER};margin:0 8px"></div>'
             f'<div style="position:absolute;left:0;top:4px">{img("sb-footer",254,48)}</div></div>')
    cw = 1440-264-8
    body += f'<div class="card" style="left:264px;top:8px;right:8px;bottom:8px">'
    body += f'<div style="position:absolute;left:{(cw-892)//2}px;top:56px;width:892px;height:648px">{img("tx-open-s2",892,648)}</div>'
    body += card_fade()
    body += f'<div class="row" style="height:44px;padding-left:18px"><span class="title">{TITLE}</span>{actions()}</div>'
    body += '</div>'
    body += composer(264 + (cw-824)//2, inside=True)
    page(name, body, "b60")

def final_sessions_closed(name="F-SessionsClosed"):
    body = f'<div class="card" style="left:8px;top:8px;right:8px;bottom:8px">'
    body += f'<div style="position:absolute;left:266px;top:72px;width:892px;height:648px">{img("tx-closed-s2",892,648)}</div>'
    body += card_fade().replace("height:64px","height:88px")
    body += '</div>'
    body += composer((1440-824)//2, inside=True)
    body += (f'<div style="{PILL};left:20px">{lights_inline()}{ib("side","Show sidebar")}{switch("s")}'
             f'<span class="title" style="margin:0 14px 0 10px">{TITLE}</span></div>')
    body += f'<div style="{PILL};right:20px">{ib("term","Terminal")}{ib("dots","Session menu")}{ib("right","Show right pane")}</div>'
    page(name, body, "b60")

def final_browse_open(name="F-BrowseOpen"):
    body = f'<div style="position:absolute;left:0;top:0">{img("sb-browse",254,900)}</div>'
    body += f'<div class="card" style="left:264px;top:8px;right:8px;bottom:8px;background:#fff"><div style="position:absolute;left:0;top:0">{img("page",bx1-bx0-4,by1-by0-4)}</div></div>'
    page(name, body, "b60")

def final_browse_closed(name="F-BrowseClosed"):
    body = f'<div class="card" style="left:8px;top:8px;right:8px;bottom:8px;background:#FAFAFA"><div style="position:absolute;left:-1px;top:-1px">{img("page-closed",1424,892-PT)}</div></div>'
    body += (f'<div style="{PILL};left:20px">{lights_inline()}{ib("side","Show sidebar")}{switch("b")}'
             f'<span style="width:1px;height:16px;background:{BORDER60};margin:0 6px"></span>{ib("back","Back")}{ib("fwd","Forward")}{ib("reload","Reload")}'
             f'<span class="addr" style="padding:0 14px 0 8px">{ico(I["shield"],size=14)}<b>Fluid Functionalism</b>www.fluidfunctionalism.com</span></div>')
    body += f'<div style="{PILL};right:20px">{ib("spark","Ask")}{ib("dots","Page menu")}{ib("plus","New tab")}</div>'
    page(name, body, "b60")

final_sessions_open(); final_sessions_closed(); final_browse_open(); final_browse_closed()
