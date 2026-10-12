import os, re
OUT = os.path.dirname(os.path.abspath(__file__)) + "/project"
os.makedirs(OUT, exist_ok=True)
OWL = "/_blob/32f8077c1893c70aa92dc89c186d75f3"

D1 = "0 1px 1px -0.5px rgba(0,0,0,.18)"
D3 = "0 3px 3px -1.5px rgba(0,0,0,.18)"
D6 = "0 6px 6px -3px rgba(0,0,0,.18)"
D12 = "0 12px 12px -6px rgba(0,0,0,.18)"
SH = {
 1: "inset 0 0 0 1px rgba(255,255,255,.02)",
 2: f"inset 0 1px 0 rgba(255,255,255,.01),inset 0 0 0 1px rgba(255,255,255,.02),{D1}",
 3: f"inset 0 1px 0 rgba(255,255,255,.02),inset 0 0 0 1px rgba(255,255,255,.02),0 0 0 1px rgba(0,0,0,.12),{D1},{D3}",
 4: f"inset 0 1px 0 rgba(255,255,255,.02),inset 0 0 0 1px rgba(255,255,255,.04),0 0 0 1px rgba(0,0,0,.14),{D1},{D3},{D6}",
 5: f"inset 0 1px 0 rgba(255,255,255,.04),inset 0 0 0 1px rgba(255,255,255,.04),0 0 0 1px rgba(0,0,0,.16),{D1},{D3},{D6},{D12}",
}
S = {1:"#171717",2:"#1E1E1E",3:"#252525",4:"#2C2C2C",5:"#333333",6:"#3A3A3A",7:"#414141",8:"#484848"}

CSS = """
body{margin:0}
.w{font-family:"Inter",-apple-system,"SF Pro Text","Helvetica Neue",Arial,sans-serif;color:#F5F5F5;-webkit-font-smoothing:antialiased;font-size:13px;line-height:20px}
.w *{box-sizing:border-box}
.mono{font-family:"Geist Mono",ui-monospace,Menlo,monospace}
.ib{width:22px;height:22px;border-radius:6px;display:flex;align-items:center;justify-content:center;color:#A3A3A3;background:transparent;border:0;padding:0;flex:none}
.hb{width:28px;height:28px;border-radius:6px;display:flex;align-items:center;justify-content:center;color:#A3A3A3;background:transparent;border:0;padding:0;flex:none}
.modes{display:flex;height:24px;padding:2px;border-radius:6px;background:rgba(255,255,255,.04)}
.modes button{width:28px;height:20px;border:0;border-radius:4px;background:transparent;color:#A3A3A3;padding:0;display:flex;align-items:center;justify-content:center}
.modes button.on{background:#2C2C2C;color:#F5F5F5;box-shadow:SH4}
.row{position:relative;display:flex;align-items:center;height:32px;padding:0 8px 0 0;border-radius:6px;color:#A3A3A3;white-space:nowrap}
.row.two{height:46px}
.row.nest{padding-left:16px}
.row.sel{background:rgba(255,255,255,.10);color:#F5F5F5}
.row.sel .ttl>span:first-child{font-weight:600}
.lead{flex:none;width:24px;height:24px;display:flex;align-items:center;justify-content:center;color:#A3A3A3}
.ttl{flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;margin-left:8px;display:flex;flex-direction:column;justify-content:center}
.ttl>span{overflow:hidden;text-overflow:ellipsis}
.by{font-size:12px;line-height:16px;color:#A3A3A3}
.tr{flex:none;margin-left:8px;font-size:12px;line-height:16px;color:#A3A3A3;font-variant-numeric:tabular-nums}
.tr.micro{font-size:11px;line-height:14px}
.sec{display:flex;align-items:center;height:28px;margin-top:12px;padding:0 4px 0 8px;font-size:12px;line-height:16px;color:#A3A3A3}
.sec .n{margin-left:6px;font-variant-numeric:tabular-nums}
.ph{display:flex;align-items:center;height:32px;padding:0 4px 0 0;border-radius:6px;white-space:nowrap}
.ph .name{margin-left:8px;color:#A3A3A3}
.ph .n{margin-left:6px;color:#A3A3A3;font-size:12px}
.ph .br{margin-left:auto;padding-left:8px;font-size:11px;color:#A3A3A3}
.dot{width:8px;height:8px;border-radius:999px;border:1px solid #5D5D5D}
.dot.on{border:0;background:#93C5FD;box-shadow:0 0 0 3px rgba(147,197,253,.16)}
.dot.wait{border:0;background:#FCD34D}
.dot.fail{border:0;background:#FCA5A5}
.dot.done{border:1px solid #86EFAC}
.chip{height:28px;padding:0 10px;border-radius:999px;background:#252525;box-shadow:SH3;display:flex;align-items:center;gap:6px;font-size:13px;color:#D4D4D4}
.cc{height:28px;padding:0 9px;border-radius:6px;display:flex;align-items:center;gap:6px;font-size:13px;color:#D4D4D4}
.cc:hover,.cc.hov{background:rgba(255,255,255,.06)}
.card{background:#1E1E1E;border-radius:10px;box-shadow:SH2;position:relative;overflow:hidden}
.pill{position:absolute;display:flex;align-items:center;height:36px;border-radius:999px;background:#252525;box-shadow:SH3;padding:0 6px}
.mark{width:16px;height:16px;border-radius:4px;flex:none;display:flex;align-items:center;justify-content:center;font-size:10px;font-weight:600;background:rgba(255,255,255,.06);color:#A3A3A3}
.pin{width:36px;height:36px;border-radius:8px;background:rgba(255,255,255,.04);display:flex;align-items:center;justify-content:center;font-size:13px;font-weight:500;color:#D4D4D4}
.space{height:24px;padding:0 10px;border-radius:999px;font-size:12px;line-height:24px;color:#A3A3A3}
.space.on{background:rgba(255,255,255,.10);color:#F5F5F5}
.vsep{width:1px;height:16px;background:rgba(245,245,245,.12);flex:none;margin:0 6px}
.menu{position:absolute;width:288px;padding:4px;border-radius:10px;background:#2C2C2C;box-shadow:SH3}
.mi{height:36px;display:flex;align-items:center;gap:8px;padding:0 8px;border-radius:6px;color:#D4D4D4}
.mi.hov{background:rgba(255,255,255,.06);color:#F5F5F5}
.mi .k{margin-left:auto;font-size:11px;color:#A3A3A3}
.mlab{height:28px;display:flex;align-items:center;padding:0 8px;font-size:12px;color:#A3A3A3}
.msep{height:1px;background:rgba(245,245,245,.12);margin:4px -4px}
"""
for k in SH: CSS = CSS.replace(f"SH{k}", SH[k])

def ico(path, sw="1.5", size=16, fill=False):
    if fill: return f'<svg width="{size}" height="{size}" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">{path}</svg>'
    return f'<svg width="{size}" height="{size}" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="{sw}" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">{path}</svg>'
I = dict(
 side='<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M6 3v10"/>',
 right='<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M10 3v10"/>',
 list='<path d="M3 4h10M3 8h10M3 12h7"/>',
 globe='<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
 search='<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
 plus='<path d="M8 3v10M3 8h10"/>',
 folder='<path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/>',
 slash='<path d="M10 3L6 13"/>',
 bolt='<path d="M9 2L3 9h4.5L7 14l6-7H8.5z"/>',
 down='<path d="M4 6l4 4 4-4"/>', rightc='<path d="M6 4l4 4-4 4"/>', up='<path d="M4 10l4-4 4 4"/>',
 newchat='<path d="M3 3h10v7H7l-3 3v-3H3zM8 5v3M6.5 6.5h3"/>',
 term='<path d="M3 4l4 4-4 4M8 12h5"/>',
 dots='<circle cx="3" cy="8" r="1.3"/><circle cx="8" cy="8" r="1.3"/><circle cx="13" cy="8" r="1.3"/>',
 back='<path d="M13 8H3M7 4L3 8l4 4"/>', fwd='<path d="M3 8h10M9 4l4 4-4 4"/>',
 reload='<path d="M13 8a5 5 0 1 1-1.5-3.6M13 3v2.5h-2.5"/>',
 spark='<path d="M8 2l1.4 3.6L13 7l-3.6 1.4L8 12l-1.4-3.6L3 7l3.6-1.4z"/>',
 shield='<path d="M8 2l5 2v4c0 3-2.2 5.2-5 6-2.8-.8-5-3-5-6V4z"/>',
 x='<path d="M4 4l8 8M12 4l-8 8"/>', check='<path d="M3.5 8.5l3 3 6-7"/>',
 send='<path d="M8 13V3M4 7l4-4 4 4"/>',
)
def btn(cls, icon, label, extra="", fill=False, style=""):
    return f'<button class="{cls}" aria-label="{label}" style="{style}">{ico(I[icon], fill=fill)}</button>'

LIGHTS = '<span style="position:absolute;left:13px;top:16px;display:flex;gap:8px;z-index:5"><span style="width:12px;height:12px;border-radius:999px;background:#FF5F57"></span><span style="width:12px;height:12px;border-radius:999px;background:#FEBC2E"></span><span style="width:12px;height:12px;border-radius:999px;background:#28C840"></span></span>'
def modes(on):
    a = 'class="on" aria-selected="true"'; b = 'aria-selected="false"'
    return (f'<div class="modes" role="tablist" aria-label="Sidebar mode">'
            f'<button role="tab" {a if on=="s" else b} aria-label="Sessions">{ico(I["list"])}</button>'
            f'<button role="tab" {a if on=="b" else b} aria-label="Browse">{ico(I["globe"])}</button></div>')

def page(title, body, w=1440, h=900):
    return f'''<!doctype html>
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
{body}
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

# ---------- shared pieces ----------
def sessions_sidebar():
    return f'''
  <!-- sidebar: sits on the base (surface-1), FF variant="inset": no hairline, no footer rule — the card's edge is the divider -->
  <div style="width:256px;flex:none;height:900px;display:flex;flex-direction:column;">
    <div style="height:44px;flex:none;position:relative;">
      {LIGHTS}
      {btn("ib","side","Hide sidebar",style="position:absolute;left:85px;top:11px;")}
      <div style="position:absolute;left:184px;top:10px;">{modes("s")}</div>
    </div>
    <div style="flex:1 1 auto;min-height:0;padding:7px 8px 0;display:flex;flex-direction:column;">
      <div class="row"><span class="lead">{ico(I["search"])}</span><span class="ttl">Search</span><span class="tr micro">⌘K</span></div>
      <div class="row"><span class="lead">{ico(I["plus"])}</span><span class="ttl">New session</span></div>
      <div class="row"><span class="lead">{ico(I["folder"])}</span><span class="ttl">Add project</span></div>
      <div class="row"><span class="lead">{ico(I["slash"])}</span><span class="ttl">Skills</span><span class="tr">23</span></div>
      <div class="row"><span class="lead">{ico(I["bolt"])}</span><span class="ttl">Automations</span><span class="tr">Soon</span></div>
      <div class="sec">Pinned<span class="n">2</span></div>
      <div class="row two sel"><span class="lead"><span class="dot"></span></span><span class="ttl"><span>Design v2 tokens</span><span class="by">Apply D1 review · four fixes landed</span></span><span class="tr">12m</span></div>
      <div class="row two"><span class="lead"><span class="dot on"></span></span><span class="ttl"><span>Browser integration audit</span><span class="by">Running the visual probe</span></span><span class="tr">2m</span></div>
      <div class="sec">Projects<span class="n">3</span></div>
      <div class="ph"><span class="lead" style="color:#A3A3A3">{ico(I["down"])}</span><span class="name">harness</span><span class="n">4</span><span class="br mono">feedback-mode</span></div>
      <div class="row two nest"><span class="lead"><span class="dot wait"></span></span><span class="ttl"><span>Checklist run</span><span class="by">Needs you · approve cargo build</span></span></div>
      <div class="row two nest"><span class="lead"><span class="dot"></span></span><span class="ttl"><span>Token ladder audit</span><span class="by">Audit the ladder · 11 metrics → 7</span></span><span class="tr">2h</span></div>
      <div class="row two nest"><span class="lead"><span class="dot"></span></span><span class="ttl"><span>Relay capture pass</span><span class="by">Capture the right pane · 5 entries</span></span><span class="tr">3h</span></div>
      <div class="ph"><span class="lead" style="color:#A3A3A3">{ico(I["down"])}</span><span class="name">agentic-ui</span><span class="n">3</span><span class="br mono">design-v2</span></div>
      <div class="row two nest"><span class="lead"><span class="dot done"></span></span><span class="ttl"><span>Fluid list glide</span><span class="by">Glide between rows · merged</span></span><span class="tr">1d</span></div>
      <div class="row two nest"><span class="lead"><span class="dot fail"></span></span><span class="ttl"><span>Rename field fit</span><span class="by">Typecheck failed in sidebar view</span></span><span class="tr">1d</span></div>
      <div class="ph"><span class="lead" style="color:#A3A3A3">{ico(I["rightc"])}</span><span class="name">lakshya</span><span class="n">2</span></div>
    </div>
    <div style="flex:none;padding:8px;">
      <div class="row" style="padding-left:6px;">
        <span style="width:20px;height:20px;border-radius:999px;background:#5FB8B5;color:#171717;font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;flex:none;">B</span>
        <span class="ttl" style="margin-left:14px;color:#F5F5F5;"><span>Bharani <span style="color:#A3A3A3;">· Max</span></span></span>
        <span style="color:#A3A3A3;display:flex">{ico(I["up"])}</span>
      </div>
    </div>
  </div>'''

def session_header_actions(right_on=False):
    rp = 'style="background:rgba(255,255,255,.10);color:#F5F5F5"' if right_on else ''
    return f'''<span style="display:flex;gap:2px;">
        {btn("hb","newchat","New session")}
        {btn("hb","term","Terminal")}
        {btn("hb","dots","Session menu",fill=True)}
        <button class="hb" aria-label="{'Hide' if right_on else 'Show'} right pane" {rp}>{ico(I["right"])}</button>
      </span>'''

def empty_state():
    return f'''<div style="flex:1 1 auto;display:flex;flex-direction:column;align-items:center;justify-content:center;padding-bottom:24px;">
      <img src="{OWL}" alt="" style="width:112px;height:112px;object-fit:contain;">
      <div style="margin-top:12px;font-size:16px;line-height:24px;font-weight:600;color:#D4D4D4;">New session</div>
      <div style="margin-top:4px;font-size:12px;line-height:16px;color:#A3A3A3;">Claude Code runs in harness.</div>
      <div style="margin-top:16px;display:flex;gap:8px;">
        <span class="chip">{ico(I["spark"],size=12)}What is in this workspace?</span>
        <span class="chip">Explain how this project is laid out</span>
        <span class="chip">Find the entry point and walk me through it</span>
      </div>
    </div>'''

def transcript():
    tool = lambda t, d: f'<div style="display:flex;align-items:center;gap:8px;height:28px;color:#A3A3A3;font-size:12px;"><span style="color:#86EFAC;display:flex">{ico(I["check"],size=14)}</span><span style="color:#D4D4D4">{t}</span><span class="mono" style="font-size:11px">{d}</span></div>'
    return f'''<div style="flex:1 1 auto;min-height:0;overflow:hidden;">
      <div style="width:760px;margin:0 auto;padding:28px 0 0;display:flex;flex-direction:column;gap:18px;">
        <div style="align-self:flex-end;max-width:560px;background:#252525;box-shadow:{SH[3]};border-radius:10px;padding:10px 14px;color:#F5F5F5;">Apply the D1 review — the four fixes in docs/31a, then rerun the probe.</div>
        <div style="color:#D4D4D4;line-height:22px;">I'll start with the surface ladder, since two of the four fixes depend on it.</div>
        <div style="display:flex;flex-direction:column;gap:0;">
          {tool("Read","crates/aui-tokens/src/surface.rs")}
          {tool("Edit","crates/aui-tokens/src/surface.rs  +18 −6")}
          {tool("Bash","cargo test -p aui-tokens  ·  42 passed")}
        </div>
        <div style="background:#171717;box-shadow:{SH[1]};border-radius:8px;padding:12px 14px;font-size:12px;line-height:19px;color:#D4D4D4;" class="mono">
<span style="color:#A3A3A3">// surface level for a popup = min(substrate + offset, 8)</span><br>pub fn lift(substrate: u8, offset: u8) -&gt; u8 {{<br>&nbsp;&nbsp;&nbsp;&nbsp;(substrate + offset).min(8)<br>}}</div>
        <div style="color:#D4D4D4;line-height:22px;">Fixes 1 and 2 landed. The dropdown now reads its substrate, so the model picker opened inside the card sits at surface-4 instead of matching the card. Moving on to the press spread.</div>
      </div>
    </div>'''

def composer(width=760, lifted=True, picker_open=False):
    hov = " hov" if picker_open else ""
    return f'''<div style="flex:none;padding:0 0 16px;">
      <div style="width:{width}px;margin:0 auto;background:#252525;box-shadow:{SH[3]};border-radius:12px;padding:12px 10px 10px;display:flex;flex-direction:column;gap:12px;">
        <div style="font-size:14px;color:#A3A3A3;padding-left:6px;">Ask Claude Code, or type / for commands</div>
        <div style="display:flex;align-items:center;gap:2px;">
          <span class="cc" style="width:28px;padding:0;justify-content:center;color:#D4D4D4">{ico(I["plus"],size=14)}</span>
          <span class="cc"><span style="width:12px;height:12px;border-radius:3px;background:#D08A4A;font-size:8px;font-weight:700;color:#171717;display:flex;align-items:center;justify-content:center;">C</span>Claude Code<span style="color:#A3A3A3;display:flex">{ico(I["down"],"1.8",10)}</span></span>
          <span class="cc{hov}">Opus 5<span style="color:#A3A3A3;display:flex">{ico(I["down"],"1.8",10)}</span></span>
          <span class="cc">Auto</span>
          <span class="cc">Default</span>
          <span style="margin-left:8px;font-size:11px;color:#A3A3A3;" class="mono">18.2k tokens</span>
          <span style="margin-left:auto;width:28px;height:28px;border-radius:8px;background:#F5F5F5;color:#171717;display:flex;align-items:center;justify-content:center;">{ico(I["send"],"1.8",14)}</span>
        </div>
      </div>
    </div>'''

FADE_TOP = '<div style="position:absolute;left:0;right:0;top:0;height:56px;background:linear-gradient(#1E1E1E 30%,rgba(30,30,30,0));pointer-events:none;"></div>'

# ---------- board 1: Sessions open ----------
b1 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;display:flex;">
{sessions_sidebar()}
  <!-- the session: one card, surface-2 + shadow-2, radius 10, inset 8 top / right / bottom, 0 beside the sidebar -->
  <div class="card" style="flex:1 1 auto;min-width:0;margin:8px 8px 8px 0;display:flex;flex-direction:column;">
    <div style="height:44px;flex:none;display:flex;align-items:center;padding:0 8px 0 16px;position:relative;z-index:2;">
      <span style="font-size:13px;font-weight:600;color:#F5F5F5;">Design v2 tokens</span>
      <span class="mono" style="margin-left:10px;font-size:11px;color:#A3A3A3;">harness · feedback-mode</span>
      <span style="margin-left:auto;">{session_header_actions()}</span>
    </div>
    {transcript()}
    {composer()}
  </div>
</div>'''

# ---------- board 2: Sessions open + right pane + a menu ----------
diff_lines = [
 ("ctx","  pub fn surface(level: u8) -> Hsla {"),
 ("del","-     LADDER[level as usize]"),
 ("add","+     LADDER[(level.clamp(1, 8) - 1) as usize]"),
 ("ctx","  }"),
 ("ctx",""),
 ("add","+ /// FF Elevated: the popup lifts off whatever it sits in."),
 ("add","+ pub fn lift(substrate: u8, offset: u8) -> u8 {"),
 ("add","+     (substrate + offset).min(8)"),
 ("add","+ }"),
 ("ctx",""),
 ("ctx","  pub fn shadow(level: u8) -> Vec<BoxShadow> {"),
 ("del","-     elevation_shadow(level)"),
 ("add","+     SHADOWS[(level.clamp(1, 5) - 1)].to_vec()"),
 ("ctx","  }"),
]
def dl(k,t):
    bg = {"add":"rgba(134,239,172,.07)","del":"rgba(252,165,165,.07)","ctx":"transparent"}[k]
    c = {"add":"#BBF7D0","del":"#FECACA","ctx":"#A3A3A3"}[k]
    return f'<div class="mono" style="height:20px;line-height:20px;font-size:11.5px;padding-left:12px;background:{bg};color:{c};white-space:pre;">{t or " "}</div>'
diff = "".join(dl(k,t) for k,t in diff_lines)
b2 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;display:flex;">
{sessions_sidebar()}
  <div class="card" style="flex:1 1 auto;min-width:0;margin:8px 8px 8px 0;display:flex;flex-direction:column;">
    <div style="height:44px;flex:none;display:flex;align-items:center;padding:0 8px 0 16px;">
      <span style="font-size:13px;font-weight:600;color:#F5F5F5;">Design v2 tokens</span>
      <span style="margin-left:auto;">{session_header_actions(True)}</span>
    </div>
    <div style="flex:1 1 auto;min-height:0;overflow:hidden;opacity:.9">
      <div style="width:560px;margin:0 auto;padding:28px 0 0;display:flex;flex-direction:column;gap:18px;">
        <div style="align-self:flex-end;max-width:440px;background:#252525;box-shadow:{SH[3]};border-radius:10px;padding:10px 14px;">Apply the D1 review — the four fixes in docs/31a.</div>
        <div style="color:#D4D4D4;line-height:22px;">Fixes 1 and 2 landed. The dropdown now reads its substrate, so the model picker opened inside the card sits one step above it.</div>
      </div>
    </div>
    {composer(560, picker_open=True)}
  </div>
  <!-- the right pane: a sibling card at the same level, 8 px gutter, its own header -->
  <div class="card" style="width:448px;flex:none;margin:8px 8px 8px 0;display:flex;flex-direction:column;">
    <div style="height:44px;flex:none;display:flex;align-items:center;padding:0 8px 0 8px;gap:2px;">
      <div class="modes" style="height:28px;padding:2px;"><button class="on" style="width:auto;height:24px;padding:0 10px;font-size:12px" aria-label="Changes">Changes</button><button style="width:auto;height:24px;padding:0 10px;font-size:12px" aria-label="Files">Files</button><button style="width:auto;height:24px;padding:0 10px;font-size:12px" aria-label="Git">Git</button><button style="width:auto;height:24px;padding:0 10px;font-size:12px" aria-label="Browser">Browser</button></div>
      <span style="margin-left:auto"></span>{btn("hb","x","Close pane")}
    </div>
    <div style="padding:4px 8px 8px;display:flex;flex-direction:column;gap:8px;min-height:0;flex:1 1 auto;">
      <div style="display:flex;align-items:center;height:32px;padding:0 8px;border-radius:6px;background:rgba(255,255,255,.06);">
        <span class="mono" style="font-size:12px;color:#F5F5F5">aui-tokens/src/surface.rs</span><span class="mono" style="margin-left:auto;font-size:11px;color:#86EFAC">+18</span><span class="mono" style="margin-left:6px;font-size:11px;color:#FCA5A5">−6</span>
      </div>
      <div style="border-radius:8px;background:#171717;box-shadow:{SH[1]};padding:8px 0;overflow:hidden;">{diff}</div>
      <div style="display:flex;align-items:center;height:32px;padding:0 8px;border-radius:6px;color:#A3A3A3">
        <span class="mono" style="font-size:12px">aui/src/interact/dropdown.rs</span><span class="mono" style="margin-left:auto;font-size:11px;color:#86EFAC">+4</span><span class="mono" style="margin-left:6px;font-size:11px;color:#FCA5A5">−2</span>
      </div>
      <div style="display:flex;align-items:center;height:32px;padding:0 8px;border-radius:6px;color:#A3A3A3">
        <span class="mono" style="font-size:12px">docs/31a-ff-component-values.md</span><span class="mono" style="margin-left:auto;font-size:11px;color:#86EFAC">+2</span>
      </div>
    </div>
  </div>
  <!-- the model picker, opened from the composer (inside the card): surface 2 + 2 = surface-4, shadow pinned at 3 -->
  <div class="menu" style="left:508px;top:594px;">
    <div class="mlab">Claude Code · model</div>
    <div class="mi hov"><span style="flex:1">Opus 5</span><span style="color:#F5F5F5;display:flex">{ico(I["check"],size=14)}</span></div>
    <div class="mi"><span style="flex:1">Sonnet 5</span></div>
    <div class="mi"><span style="flex:1">Haiku 5</span></div>
    <div class="msep"></div>
    <div class="mi"><span style="flex:1">Effort</span><span class="k">Medium</span><span style="color:#A3A3A3;display:flex">{ico(I["rightc"],size=14)}</span></div>
  </div>
</div>'''

# ---------- board 3: Sessions closed (pills) ----------
LIGHTS_IN = LIGHTS.replace("left:13px;top:16px","left:30px;top:28px")
def left_pill(mode, title, extra=""):
    return f'''<div class="pill" style="left:16px;top:16px;padding-left:76px;gap:2px;">
    {btn("ib","side","Show sidebar",style="width:28px;height:28px")}
    <div style="margin:0 2px">{modes(mode)}</div>
    {extra}
    <span class="vsep"></span>
    <span style="font-size:13px;font-weight:600;color:#F5F5F5;padding:0 6px 0 2px;white-space:nowrap;max-width:260px;overflow:hidden;text-overflow:ellipsis">{title}</span>
  </div>'''
b3 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;">
  <!-- closed = FF offcanvas: the card takes the window, inset 8 on every side -->
  <div class="card" style="position:absolute;left:8px;top:8px;right:8px;bottom:8px;display:flex;flex-direction:column;">
    <div style="height:40px;flex:none"></div>
    {transcript()}
    {composer()}
    {FADE_TOP}
  </div>
  <!-- the window pill: lights where they always are, toggle, mode switch, the session title — nothing else -->
  {left_pill("s","Design v2 tokens")}
  {LIGHTS_IN}
  <!-- the session pill: the header's actions, same surface, mirrored on the right -->
  <div class="pill" style="right:16px;top:16px;gap:2px;">
    {btn("hb","newchat","New session")}{btn("hb","term","Terminal")}{btn("hb","dots","Session menu",fill=True)}{btn("hb","right","Show right pane")}
  </div>
</div>'''

# ---------- Browse ----------
def browse_sidebar():
    return f'''
  <div style="width:256px;flex:none;height:900px;display:flex;flex-direction:column;">
    <div style="height:44px;flex:none;position:relative;">
      {LIGHTS}
      {btn("ib","side","Hide sidebar",style="position:absolute;left:85px;top:11px;")}
      <div style="position:absolute;left:184px;top:10px;">{modes("b")}</div>
    </div>
    <div style="flex:1 1 auto;min-height:0;padding:4px 8px 0;display:flex;flex-direction:column;">
      <div style="display:flex;align-items:center;height:36px;border-radius:8px;background:#1E1E1E;box-shadow:{SH[2]};padding-right:8px;" role="textbox" aria-label="Address">
        <span class="lead" style="margin-left:4px;">{ico(I["shield"],size=14)}</span>
        <span class="ttl" style="flex-direction:row;justify-content:flex-start"><span style="color:#F5F5F5;">pgi.udiseplus.gov.in</span><span style="color:#A3A3A3;">/#/home</span></span>
      </div>
      <div style="display:flex;gap:4px;height:36px;align-items:center;padding-left:2px;margin-top:2px;">
        {btn("hb","back","Back")}<button class="hb" style="color:#5D5D5D" aria-label="Forward">{ico(I["fwd"])}</button>{btn("hb","reload","Reload")}{btn("hb","plus","New tab")}
        <span style="flex:1 1 auto;"></span>
        {btn("hb","spark","Ask Claude about this page")}{btn("hb","dots","Page menu",fill=True)}
      </div>
      <div class="row"><span class="lead">{ico(I["search"])}</span><span class="ttl">Search</span></div>
      <div class="sec">Pinned</div>
      <div style="display:flex;gap:8px;padding:0 0 4px 8px;">
        <span class="pin" aria-label="Unified dashboard">U</span><span class="pin" aria-label="Performance grades">P</span><span class="pin" aria-label="Open data portal">D</span><span class="pin" aria-label="Survey reports">N</span>
      </div>
      <div class="sec">Today<span class="n">5</span></div>
      <div class="row sel"><span class="lead"><span class="mark">S</span></span><span class="ttl"><span>State scores 2023-24</span></span><button class="ib" aria-label="Close tab" style="width:20px;height:20px;">{ico(I["x"],"1.6",12)}</button></div>
      <div class="row"><span class="lead"><span class="mark">D</span></span><span class="ttl"><span>Dashboard 2024-25</span></span></div>
      <div class="row"><span class="lead"><span class="mark">S</span></span><span class="ttl"><span>State report cards</span></span></div>
      <div class="ph"><span class="lead" style="color:#A3A3A3">{ico(I["down"])}</span><span class="name">Karnataka review</span><span class="n">2</span></div>
      <div class="row nest"><span class="lead"><span class="mark">B</span></span><span class="ttl"><span>Board results 2025</span></span></div>
      <div class="row nest"><span class="lead"><span class="mark">S</span></span><span class="ttl"><span>Scheme review · Karnataka</span></span></div>
      <div class="row" style="background:#1E1E1E;box-shadow:{SH[2]};margin-top:8px;color:#F5F5F5;" role="button" aria-label="New tab"><span class="lead" style="color:#F5F5F5;">{ico(I["plus"])}</span><span class="ttl">New tab</span><span class="tr micro">⌘T</span></div>
      <div style="margin-top:auto;"></div>
      <div class="sec" style="margin-top:0;">From sessions<span class="n">2</span></div>
      <div class="ph"><span class="lead"><span class="dot wait"></span></span><span class="name">Checklist run</span><span class="br" style="color:#FCD34D;">needs you</span></div>
      <div class="row nest"><span class="lead"><span class="mark">L</span></span><span class="ttl"><span>Ledger</span></span><span class="tr micro mono">:5173</span></div>
      <div class="ph"><span class="lead"><span class="dot on"></span></span><span class="name">Browser integration audit</span><span class="br">reading</span></div>
      <div class="row nest"><span class="lead"><span class="mark">G</span></span><span class="ttl"><span>Pull request #412 · baaz</span></span></div>
      <div style="display:flex;gap:4px;padding:8px 0;align-items:center;" role="tablist" aria-label="Spaces">
        <span class="space on" role="tab">Personal</span><span class="space" role="tab">Work</span><span class="space" role="tab">Lakshya</span>
        <button class="ib" style="margin-left:auto;" aria-label="New space">{ico(I["plus"],size=14)}</button>
      </div>
    </div>
    <div style="flex:none;padding:0 8px 8px;">
      <div class="row" style="padding-left:6px;">
        <span style="width:20px;height:20px;border-radius:999px;background:#5FB8B5;color:#171717;font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;flex:none;">B</span>
        <span class="ttl" style="margin-left:14px;color:#F5F5F5;"><span>Bharani <span style="color:#A3A3A3;">· Max</span></span></span>
        <span style="color:#A3A3A3;display:flex">{ico(I["up"])}</span>
      </div>
    </div>
  </div>'''

def web_page(pad_top=0):
    return f'''<div style="height:56px;flex:none;display:flex;align-items:center;padding:0 32px 0 {32+pad_top}px;gap:24px;border-bottom:1px solid #E2E5EB;font-size:13px;color:#191B21">
      <span style="font-weight:600;font-size:15px;">UDISE+ · PGI</span>
      <span style="color:#4D5262;">Home</span><span style="color:#4D5262;">State scores</span><span style="color:#4D5262;">Report cards</span><span style="color:#4D5262;">Open data</span>
      <span style="margin-left:auto;height:32px;width:220px;border-radius:6px;border:1px solid #C9CED8;display:flex;align-items:center;padding:0 10px;color:#7B818F;">Search a state or district</span>
    </div>
    <div style="padding:40px 48px;display:flex;flex-direction:column;gap:24px;max-width:1040px;color:#191B21">
      <div style="font-size:12px;color:#7B818F;">Performance Grading Index · 2023-24</div>
      <div style="font-size:32px;line-height:40px;font-weight:600;">State scores 2023-24</div>
      <div style="display:grid;grid-template-columns:repeat(3,minmax(0,1fr));gap:16px;">
        <div style="border:1px solid #E2E5EB;border-radius:8px;padding:16px;"><div style="font-size:12px;color:#7B818F;">States graded</div><div style="font-size:24px;font-weight:600;margin-top:4px;">36</div></div>
        <div style="border:1px solid #E2E5EB;border-radius:8px;padding:16px;"><div style="font-size:12px;color:#7B818F;">Highest band</div><div style="font-size:24px;font-weight:600;margin-top:4px;">Prachesta-2</div></div>
        <div style="border:1px solid #E2E5EB;border-radius:8px;padding:16px;"><div style="font-size:12px;color:#7B818F;">Updated</div><div style="font-size:24px;font-weight:600;margin-top:4px;">29 Sept 2026</div></div>
      </div>
      <div style="display:flex;flex-direction:column;gap:10px;margin-top:8px;">
        <span style="height:8px;width:92%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:84%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:88%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:60%;border-radius:4px;background:#EFF1F5;"></span>
      </div>
      <div style="display:grid;grid-template-columns:repeat(2,minmax(0,1fr));gap:16px;margin-top:8px;">
        <div style="border:1px solid #E2E5EB;border-radius:8px;height:220px;display:flex;align-items:flex-end;gap:10px;padding:16px;">
          <span style="flex:1;height:40%;background:#E6E9EF;border-radius:4px 4px 0 0;"></span><span style="flex:1;height:65%;background:#E6E9EF;border-radius:4px 4px 0 0;"></span><span style="flex:1;height:55%;background:#E6E9EF;border-radius:4px 4px 0 0;"></span><span style="flex:1;height:80%;background:#565CC4;border-radius:4px 4px 0 0;"></span><span style="flex:1;height:48%;background:#E6E9EF;border-radius:4px 4px 0 0;"></span><span style="flex:1;height:70%;background:#E6E9EF;border-radius:4px 4px 0 0;"></span>
        </div>
        <div style="border:1px solid #E2E5EB;border-radius:8px;height:220px;padding:16px;display:flex;flex-direction:column;gap:12px;">
          <span style="height:8px;width:70%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:90%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:64%;border-radius:4px;background:#EFF1F5;"></span><span style="height:8px;width:82%;border-radius:4px;background:#EFF1F5;"></span>
        </div>
      </div>
    </div>'''

b4 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;display:flex;">
{browse_sidebar()}
  <!-- the page: the same card as a session — inset 8, radius 10; the native page fills it, white canvas -->
  <div style="flex:1 1 auto;min-width:0;margin:8px 8px 8px 0;border-radius:10px;background:#FFFFFF;overflow:hidden;display:flex;flex-direction:column;box-shadow:{SH[2]};">
    {web_page()}
  </div>
</div>'''

b5 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;">
  <div style="position:absolute;left:8px;top:8px;right:8px;bottom:8px;border-radius:10px;background:#FFFFFF;overflow:hidden;display:flex;flex-direction:column;box-shadow:{SH[2]};">
    {web_page()}
  </div>
  <!-- the window pill over the page (an overlay window, like the menus): lights, toggle, switch, back / forward / reload, the page at rest -->
  {left_pill("b","State scores 2023-24 <span style='font-weight:400;color:#A3A3A3'>· pgi.udiseplus.gov.in</span>",
     extra=f'<span class="vsep"></span>{btn("hb","back","Back")}<button class="hb" style="color:#5D5D5D" aria-label="Forward">{ico(I["fwd"])}</button>{btn("hb","reload","Reload")}')}
  {LIGHTS_IN}
  <div class="pill" style="right:16px;top:16px;gap:2px;">
    {btn("hb","spark","Ask Claude about this page")}{btn("hb","plus","New tab")}{btn("hb","dots","Page menu",fill=True)}
  </div>
</div>'''

# ---------- board 6: the depth map ----------
def swatch(level, name, uses, shadow_level=None):
    sl = shadow_level or min(level,5)
    return f'''<div style="display:flex;align-items:center;gap:16px;height:64px;">
      <div style="width:120px;height:48px;border-radius:10px;background:{S[level]};box-shadow:{SH.get(sl, SH[5]) if level>1 else 'inset 0 0 0 1px rgba(255,255,255,.06)'};flex:none"></div>
      <div style="width:150px;flex:none"><div style="font-weight:600">{name}</div><div class="mono" style="font-size:11px;color:#A3A3A3">{S[level]} · shadow-{sl if level>1 else '—'}</div></div>
      <div style="color:#A3A3A3;font-size:12px;line-height:17px">{uses}</div>
    </div>'''
ladder = "".join([
 swatch(1,"surface-1 · base","The window. Sidebar sits on it with no edge of its own; the gutters around every card are this."),
 swatch(2,"surface-2 · card","The session, the right pane, the page frame. Siblings share the level; an 8 px gutter separates them, not a line."),
 swatch(3,"surface-3 · control","Composer, user turn, chips, the collapsed window pills — anything raised one step off a card."),
 swatch(4,"surface-4 · popup","Menus and pickers opened from a card (card 2 + 2). Shadow pinned at 3 at every depth. The mode-switch thumb is the same step (FF tabs: + 3, shadow-4)."),
 swatch(5,"surface-5 · dialog","Dialogs: page + 4, shadow-5, over a 40 % black scrim. Settings sheet, rename, approvals."),
 swatch(7,"surface-7 · popup in a dialog","A select inside a dialog: 5 + 2. Shadow still 3.",3),
])
b6 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;display:flex;">
  <div style="width:620px;flex:none;padding:40px 40px 0 48px;">
    <div style="font-size:20px;line-height:28px;font-weight:600">Depth, one step at a time</div>
    <div style="margin-top:6px;color:#A3A3A3;max-width:520px;line-height:20px">FF surfaces, dark. Every container knows its level and whatever opens inside lifts from it, so a menu never matches what it sits on. Dark reads depth from fill; the shadow is a faint ring + drops (FF's recipe, unchanged).</div>
    <div style="margin-top:24px;display:flex;flex-direction:column;gap:8px">{ladder}</div>
    <div style="margin-top:24px;display:flex;flex-direction:column;gap:6px;color:#A3A3A3;font-size:12px;line-height:17px;max-width:540px">
      <div><span style="color:#F5F5F5">Hover / selected</span> are overlays, not greys: white 6 % / 10 % on whatever level they sit.</div>
      <div><span style="color:#F5F5F5">Tooltip</span> inverts (#F5F5F5 on dark text), no shadow, 200 ms delay.</div>
      <div><span style="color:#F5F5F5">Lines</span> survive only inside a surface (menu separators, the diff). No line ever separates two surfaces.</div>
      <div><span style="color:#F5F5F5">Radii</span> card 10 · popup 10 · control 8–12 · row 6 · pill full (our one-step-tighter set).</div>
    </div>
  </div>
  <!-- a live stack: base → card → composer → dialog → select -->
  <div style="flex:1 1 auto;position:relative;">
    <div class="card" style="position:absolute;left:0;top:40px;right:40px;bottom:40px;">
      <div style="height:44px;display:flex;align-items:center;padding:0 16px;font-weight:600">Checklist run</div>
      <div style="padding:8px 48px;color:#D4D4D4;line-height:22px;opacity:.5">Ready to rename this session and move it into a project.</div>
      <div style="position:absolute;left:32px;right:32px;bottom:16px;height:96px;background:#252525;box-shadow:{SH[3]};border-radius:12px;opacity:.5"></div>
      <div style="position:absolute;inset:0;background:rgba(0,0,0,.40)"></div>
      <div style="position:absolute;left:50%;top:50%;transform:translate(-50%,-56%);width:440px;background:#333333;box-shadow:{SH[5]};border-radius:12px;padding:20px;">
        <div style="font-size:16px;line-height:22px;font-weight:600">Move session</div>
        <div style="margin-top:4px;color:#A3A3A3">Pick the project this session belongs to.</div>
        <div style="margin-top:16px;font-size:12px;color:#A3A3A3">Name</div>
        <div style="margin-top:6px;height:36px;border-radius:8px;background:#3A3A3A;box-shadow:{SH[2]};display:flex;align-items:center;padding:0 12px">Checklist run</div>
        <div style="margin-top:12px;font-size:12px;color:#A3A3A3">Project</div>
        <div style="margin-top:6px;height:36px;border-radius:8px;background:#3A3A3A;box-shadow:0 0 0 1px rgba(245,245,245,.20);display:flex;align-items:center;padding:0 12px">harness<span style="margin-left:auto;color:#A3A3A3;display:flex">{ico(I["down"])}</span></div>
        <div style="height:120px"></div>
        <div style="display:flex;justify-content:flex-end;gap:8px">
          <span style="height:36px;padding:0 14px;border-radius:8px;display:flex;align-items:center;color:#D4D4D4">Cancel</span>
          <span style="height:36px;padding:0 14px;border-radius:8px;display:flex;align-items:center;background:#F5F5F5;color:#171717;font-weight:600">Move</span>
        </div>
        <div class="menu" style="left:20px;top:222px;width:400px;background:#414141;">
          <div class="mi hov"><span style="flex:1">harness</span><span style="display:flex">{ico(I["check"],size=14)}</span></div>
          <div class="mi"><span style="flex:1">agentic-ui</span></div>
          <div class="mi"><span style="flex:1">lakshya</span></div>
        </div>
      </div>
      <span style="position:absolute;left:24px;bottom:132px;" ></span>
    </div>
    <div style="position:absolute;right:48px;top:48px;width:28px;height:28px;border-radius:6px;background:rgba(255,255,255,.06);display:flex;align-items:center;justify-content:center;color:#F5F5F5">{ico(I['term'])}</div>
    <div style="position:absolute;right:30px;top:82px;height:24px;padding:0 8px;border-radius:6px;background:#F5F5F5;color:#171717;font-size:12px;display:flex;align-items:center;gap:8px">Terminal<span style="color:#737373">⌃`</span></div>
  </div>
</div>'''

b7 = f'''<div class="w" style="width:1440px;height:900px;background:#171717;position:relative;overflow:hidden;">
  <!-- option C: the page card starts under a 44 px strip of base; the pills sit on the base, never over the page -->
  <div style="position:absolute;left:8px;top:52px;right:8px;bottom:8px;border-radius:10px;background:#FFFFFF;overflow:hidden;display:flex;flex-direction:column;box-shadow:{SH[2]};">
    {web_page()}
  </div>
  <div class="pill" style="left:6px;top:8px;height:36px;padding-left:76px;gap:2px;background:#1E1E1E;box-shadow:{SH[2]}">
    {btn("ib","side","Show sidebar",style="width:28px;height:28px")}
    <div style="margin:0 2px">{modes("b")}</div>
    <span class="vsep"></span>{btn("hb","back","Back")}<button class="hb" style="color:#5D5D5D" aria-label="Forward">{ico(I["fwd"])}</button>{btn("hb","reload","Reload")}
    <span class="vsep"></span>
    <span style="font-size:13px;font-weight:600;color:#F5F5F5;padding:0 8px 0 2px;white-space:nowrap">State scores 2023-24 <span style="font-weight:400;color:#A3A3A3">· pgi.udiseplus.gov.in</span></span>
  </div>
  {LIGHTS.replace("left:13px;top:16px","left:20px;top:20px")}
  <div class="pill" style="right:6px;top:8px;gap:2px;background:#1E1E1E;box-shadow:{SH[2]}">
    {btn("hb","spark","Ask Claude about this page")}{btn("hb","plus","New tab")}{btn("hb","dots","Page menu",fill=True)}
  </div>
</div>'''
boards = {
 "V3SessionsOpen.dc.html": ("v3 · Sessions open", b1),
 "V3SessionsPane.dc.html": ("v3 · Sessions + right pane + menu", b2),
 "V3SessionsClosed.dc.html": ("v3 · Sessions closed (pills)", b3),
 "V3BrowseOpen.dc.html": ("v3 · Browse open", b4),
 "V3BrowseClosed.dc.html": ("v3 · Browse closed (pills)", b5),
 "V3Depth.dc.html": ("v3 · Depth", b6),
 "V3BrowseClosedStrip.dc.html": ("v3 · Browse closed, option C (pills on a base strip)", b7),
}
for f,(t,b) in boards.items():
    open(f"{OUT}/{f}","w").write(page(t,b))
print("wrote", list(boards))
