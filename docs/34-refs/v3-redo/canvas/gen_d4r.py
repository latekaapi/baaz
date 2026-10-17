#!/usr/bin/env python3
"""D4 refresh (2026-10-17): bring the D4 boards up to the build the owner reviewed and shipped (aui v0.5.1, baaz 65f3698).

The owner's two live review rounds (docs/41 "Owner review of the live build") changed values after canvas v42 was
approved. This re-renders every D4 board through gen_d4.py with those values patched in, measured from offscreen
captures of the shipped app (target/debug/baaz --replay fixtures/msp/<f> --screenshot, BAAZ_DETERMINISTIC=1):
  - reading scale 16 / 26: h1 24, h2 20 (28 above / 10 below), h3 17 (22 / 8), paragraphs 16, list items 4, rules 22;
    blocks of a turn 14 apart, turns 28; the user bubble 16 / 26 (8 / 13 padding, 42 tall for one line).
  - composer in Claude Desktop's shape: the field is its own Raised box (r 12, edge ink 8 %, 22 % while focused,
    a soft drop under it), 50 tall for one line, text 16 / 26 at 12 / 13, send 28 inside at the right (6 in, centred);
    the controls row (28) stands outside, 6 below; 20 from the controls to the sheet's bottom (window y 790 / 840 / 874).
    It floats over the transcript, which fades out behind it.
  - progress inline: no status row pinned over the composer; the status row ends the transcript and scrolls with it,
    no "esc to interrupt" hint printed.
  - the title row is opaque (32) with a 24 px fade under it; the sidebar list fades at its scroll edges.
  - pickers hang 6 px above their own button; the right-side ones are right-aligned and their submenus open left.
Run: python3 gen_d4r.py  (writes project/D4*.dc.html + preview/D4*.html; canvas.json is not touched).
"""
import os, sys, re
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d4 as d4
from gen_d4 import ico, I, ib, muse_mark, actions, FG, MUTED, FAINT, L1

D4R_CSS = """
.tx{font-size:16px;line-height:26px}
.bk{gap:14px}
.gapturn{height:0}
.u .bub{padding:8px 13px;font-size:16px;line-height:26px}
.a p{margin:0 0 16px}.a p:last-child{margin-bottom:0}
.a h2{font-size:20px;line-height:28px;margin:28px 0 10px}.a h2:first-child{margin-top:0}
.a ul{margin:0 0 16px;padding-left:24px}.a li{margin:0 0 4px}.a ul:last-child{margin-bottom:0}
.rd{font-size:16px;line-height:26px}
.rd p{margin:0 0 16px}.rd h2{font-size:20px;line-height:28px;margin:28px 0 10px}.rd h2:first-child{margin-top:0}
.rd h3{font-size:17px;line-height:24px;margin:22px 0 8px}
.rd ul,.rd ol{margin:0 0 16px}.rd li{margin:0 0 4px}.rd hr{margin:22px 0}.rd blockquote{margin:0 0 16px}
.rd .img,.rd .tbl{margin:0 0 16px}
.tbl{font-size:16px;line-height:24px}
.ic{font-size:14px}
.fade{height:56px;background:linear-gradient(var(--l1) 32px,rgb(from var(--l1) r g b / 0))}
.bfade{height:150px;background:linear-gradient(rgb(from var(--l1) r g b / 0),var(--l1) 56px)}
.sbfade{position:absolute;left:0;bottom:48px;width:256px;height:24px;background:linear-gradient(rgb(from var(--l0) r g b / 0),var(--l0));z-index:2}
.cc{display:flex;flex-direction:column;gap:6px}
.cc.abs{position:absolute;bottom:26px}
.cf{position:relative;background:var(--l2);border-radius:12px;box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.08),0 8px 20px -8px rgba(0,0,0,.45);padding:12px 48px 12px 13px;min-height:50px;display:flex;flex-direction:column;gap:8px}
.cf.focus{box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.22),0 8px 20px -8px rgba(0,0,0,.45)}
.cf .ft{font-size:16px;line-height:26px;color:#F2F2F4}.cf .ft.pl{color:#9D9DA6}
.cf .send{position:absolute;right:6px;bottom:11px}
.cf .att{padding:0;margin:0 -35px 0 -1px}
.cr{height:28px;display:flex;align-items:center;gap:4px;padding:0 4px}
.cr .ctl{font-size:14.3px}
.cr .meter{width:12px;height:12px;border-radius:50%;box-shadow:inset 0 0 0 1.5px rgba(255,255,255,.18);flex:none}
.sub{left:auto;right:100%;margin:0 4px 0 0}
"""
d4.D4CSS += D4R_CSS


def composer_c(field, chips='', ph=False, stop=False, focus=False, above='', abs_left=None, w=880):
    """The shipped composer: field box (chips above the text, send inside) + the controls row outside it."""
    go = stop or (not ph) or bool(chips)
    send = ico(I["sqr"], size=12) if stop else ico(I["up"], sw="2" if go else "1.8", size=14)
    cls = 'cc abs' if abs_left is not None else 'cc'
    pos = f' style="left:{abs_left}px;width:{w}px"' if abs_left is not None else f' style="width:{w}px"'
    return (f'{above}<div class="{cls}"{pos}>'
            f'<div class="cf{" focus" if focus else ""}">{chips}<div class="ft{" pl" if ph else ""}">{field}</div>'
            f'<button class="send{" go" if go else ""}" aria-label="{"Stop" if stop else "Send"}">{send}</button></div>'
            f'<div class="cr">{ib("plus", "Attach or insert")}<button class="ctl on">{muse_mark()}Muse{ico(I["down"], size=12)}</button>'
            f'<span style="display:inline-flex;align-items:center;gap:6px;margin-left:6px"><span class="meter"></span><span class="mono" style="font-size:11px;color:{FAINT}">0 tokens</span></span>'
            f'<span style="margin-left:auto;display:flex;align-items:center;gap:4px"><button class="ctl">Muse Spark 1.3{ico(I["down"], size=12)}</button>'
            f'<button class="ctl">Auto{ico(I["down"], size=12)}</button><button class="ctl">Default{ico(I["down"], size=12)}</button></span></div></div>')


def status_inline(text, tail=None):
    t = f'<span style="color:{FAINT}">{tail}</span>' if tail else ''
    return f'<div class="st"><span class="spin"></span><span>{text}</span>{t}</div>'


def screen(sidebar_html, title, transcript_html, below_composer='', overlay='', stop=False, status='', draft=None,
           focus=False, sheet_left=262, chips=''):
    """The shipped Sessions screen: transcript on the 880 measure, the inline status row at its end, the composer
    floating over the bottom with the fade behind it, the opaque title row."""
    sw = 1440 - sheet_left - 6
    mx = (sw - 880) // 2
    if status:
        # progress is inline (owner review): drop the printed esc hint, end the transcript with the row
        status = re.sub(r'<span>·</span><span class="key">esc</span><span>to interrupt</span>', '', status)
        transcript_html += status
    b = f'<div class="w" style="width:1440px;height:900px">{sidebar_html}'
    b += f'<div class="sheet" style="left:{sheet_left}px;top:6px;right:6px;bottom:6px">'
    b += f'<div class="tx bk" style="position:absolute;left:{mx}px;top:84px">{transcript_html}</div>'
    b += '<div class="bfade"></div>'
    b += (f'<div class="fade"></div><div class="row" style="padding-left:16px;background:var(--l1)"><span class="title">{title}</span>'
          f'<span style="margin-left:auto;display:flex;gap:2px;padding-right:10px">{actions()}</span></div>')
    b += overlay + '</div>'
    b += composer_c(draft or 'Ask Muse, or type / for commands', ph=draft is None, stop=stop, focus=focus, chips=chips,
                    abs_left=sheet_left + mx)
    b += below_composer + '</div>'
    return b


_sidebar_prev = d4.sidebar_drawn
def sidebar_drawn(title, byline, when='now', need=False):
    out = _sidebar_prev(title, byline, when, need)
    return out.replace('<div class="sb">', '<div class="sb" style="top:51px">', 1) + '<div class="sbfade"></div>'


def composer(field, chips='', ph=False, stop=False, above=''):
    if chips:
        chips = chips  # the .att row sits inside the field, above the text
    return composer_c(field, chips=chips, ph=ph, stop=stop, above=above)


d4.screen = screen
d4.sidebar_drawn = sidebar_drawn
d4.composer = composer

# the corner stack in Sessions sits 8 above the composer's top edge: field top is at window y 790 → 110 from the bottom
_homes_prev = d4.b_homes_sessions
def b_homes_sessions(src):
    return _homes_prev(src).replace('right:12px;bottom:104px', 'right:12px;bottom:112px')
d4.BOARDS["D4HomeSessions.dc.html"] = ("Homes · Sessions", b_homes_sessions, 1440, 900, 0, 8660)

# right-side pickers: the nested submenu opens to the LEFT, inside the reach of its row
_pickers_prev = d4.b_pickers
def b_pickers(src):
    out = _pickers_prev(src)
    wrap = '<div style="display:flex;flex-wrap:wrap;gap:8px;align-items:flex-start">'
    head, tail = out.split('class="pk sub"', 1)
    i = head.rfind(wrap)
    head = head[:i] + wrap.replace('align-items:flex-start', 'align-items:flex-start;padding-left:304px') + head[i + len(wrap):]
    return head + 'class="pk sub"' + tail
d4.BOARDS["D4Pickers.dc.html"] = ("D4 · Pickers + send", b_pickers, 1440, 2190, 0, 13300)


# ======================= the quote flow (docs/42 §1.2, new) =======================
I.update(quote='<path d="M3 9.5c0-2.6 1.2-4.3 3.3-5M9 9.5c0-2.6 1.2-4.3 3.3-5"/><path d="M3 9.5h2.8v3H3zM9 9.5h2.8v3H9z"/>')
d4.D4CSS += """
.selx{background:rgba(61,114,242,.27);border-radius:2px;box-decoration-break:clone;-webkit-box-decoration-break:clone}
.code .b .selx{border-radius:0}
.qbar{position:absolute;height:32px;padding:3px;border-radius:8px;background:var(--l3);box-shadow:var(--float-sh);display:inline-flex;align-items:center;gap:2px;z-index:5}
.qbar button{height:26px;padding:0 10px 0 8px;border-radius:6px;border:0;background:transparent;color:#F2F2F4;display:inline-flex;align-items:center;gap:6px;font-size:14.3px}
.qbar button svg{color:#9D9DA6}
.qbar button.hov{background:rgba(255,255,255,.05)}
.cq{position:relative;padding:1px 28px 1px 12px;border-left:2px solid rgba(255,255,255,.18);font-size:14.3px;line-height:22px;color:#9D9DA6;margin:0 -35px 0 -1px}
.cq .tx2{display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical;overflow:hidden}
.cq .x{position:absolute;right:0;top:1px;width:20px;height:20px;border-radius:4px;border:0;background:transparent;padding:0;display:flex;align-items:center;justify-content:center;color:#9D9DA6}
.cq.cd .tx2{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px;white-space:pre;-webkit-line-clamp:3;color:#C9C9D0}
.cq .src{font-size:12.1px;line-height:16px;color:#6E6E78;margin-top:2px}
.u .bub .qt.qcd{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:20px;white-space:pre;-webkit-line-clamp:4;color:#C9C9D0}
.u .bub .qt{font-size:14.3px;line-height:22px}
.anch{position:absolute;border:1px dashed rgba(255,255,255,.35);border-radius:2px;pointer-events:none}
.ann2{position:absolute;font-size:12px;line-height:16px;color:#9D9DA6;font-family:"Geist Mono",ui-monospace,Menlo,monospace}
"""

def qbar(left, top, hov=False):
    return (f'<div class="qbar" role="toolbar" aria-label="Selection" style="left:{left}px;top:{top}px">'
            f'<button class="{"hov" if hov else ""}" aria-label="Quote the selection in the composer">{ico(I["quote"], size=14)}Quote</button></div>')

def cquote(text, code=False, src=None):
    s = f'<div class="src">{src}</div>' if src else ''
    return (f'<div class="cq{" cd" if code else ""}"><div class="tx2">{text}</div>{s}'
            f'<button class="x" aria-label="Remove quote">{ico(I["x"], size=11)}</button></div>')

SEL_ANSWER = ('<h2>Validation summary</h2><p>CA and GB now branch per country, <span class="selx">with focused tests over both.</span></p>'
              '<ul><li><span class="selx">CA outward codes accept the space form</span></li><li><span class="selx">GB outward codes still fail on the space</span> (next)</li>'
              '<li>Full suite stays green: <span class="ic">cargo test --workspace</span></li></ul>')
Q_TEXT = 'with focused tests over both.<br>• CA outward codes accept the space form<br>• GB outward codes still fail on the space'

def b_quote_select(src):
    tx = (d4.user_turn('Show me the validation summary with code and links.') +
          d4.assistant_turn(SEL_ANSWER, rail=False) + d4.code_block('rust', d4.RUST) +
          d4.assistant_turn(d4.MD_TAIL, rail=False))
    # the bar: 6 above the selection's first line, its left edge at the selection's start (sheet coords)
    ov = qbar(422, 169, hov=True)
    return screen(sidebar_drawn('Show me the validation su…', 'Notes live in the plan and…'),
                  'Show me the validation summary with code and links.', tx, overlay=ov)

def b_quote_compose(src):
    tx = (d4.user_turn('Show me the validation summary with code and links.') +
          d4.assistant_turn(d4.MD_ANSWER, rail=False) + d4.code_block('rust', d4.RUST) +
          d4.assistant_turn(d4.MD_TAIL, rail=False))
    chips = cquote(Q_TEXT)
    return screen(sidebar_drawn('Show me the validation su…', 'Notes live in the plan and…'),
                  'Show me the validation summary with code and links.', tx,
                  draft='Why does GB still fail on the space? Fix it the same way as CA.', focus=True, chips=chips)

def b_quote_sheet(src):
    rust_sel = [l for l in d4.RUST]
    rust_sel[2] = '<span class="selx">' + rust_sel[2] + '</span>'
    rust_sel[3] = '<span class="selx">' + rust_sel[3] + '</span>'
    code_quote = ('Country::Ca =&gt; CA_RE.is_match(code),\nCountry::Gb =&gt; GB_RE.is_match(code),')
    sel_prose = (f'<div style="position:relative;padding-top:44px">{qbar(371, 4)}'
                 f'<div class="a"><p>CA and GB now branch per country, <span class="selx">with focused tests over both.</span></p>'
                 f'<ul><li><span class="selx">CA outward codes accept the space form</span></li><li><span class="selx">GB outward codes still fail on the space</span> (next)</li></ul></div></div>')
    sel_code = (f'<div style="position:relative;padding-top:44px">{qbar(103, 4 + 0)}'
                f'{d4.code_block("rust", rust_sel)}</div>')
    flip = (f'<div style="position:relative;height:150px;border-radius:8px;background:var(--l1);overflow:hidden;box-shadow:inset 0 0 0 1px rgba(255,255,255,.05)">'
            f'<div style="position:absolute;left:0;right:0;top:0;height:32px;display:flex;align-items:center;padding-left:16px;background:var(--l1);font-weight:600;font-size:13px;z-index:2">Show me the validation summary with code and links.</div>'
            f'<div class="a" style="position:absolute;left:16px;top:38px;width:848px"><p><span class="selx">CA and GB now branch per country, with focused tests over both.</span></p></div>'
            f'{qbar(16, 38 + 26 + 6)}</div>')
    composer_states = (composer_c('Why does GB still fail on the space? Fix it the same way as CA.', focus=True, chips=cquote(Q_TEXT)))
    composer_code = composer_c('Is the order of these two arms right?', chips=cquote(code_quote, code=True, src='rust · 2 lines'))
    composer_two = composer_c('Compare these two.', chips=cquote('CA outward codes accept the space form') + cquote('GB outward codes still fail on the space'))
    sent = d4.user_turn_rich('<div class="qt">with focused tests over both. • CA outward codes accept the space form • GB outward codes still fail on the space</div><div class="qsrc">from the reply above</div>Why does GB still fail on the space? Fix it the same way as CA.', w=560)
    sent_code = d4.user_turn_rich(f'<div class="qt qcd">{code_quote}</div><div class="qsrc">from the reply above · rust</div>Is the order of these two arms right?', w=560)
    items = [
     ('Selecting · the bar', 'Select any text in a turn (an answer, your own message, a card’s output) and on mouse-up one small bar appears: Floating #2C2C32, r 8, the edge + the float drop, 32 tall, 3 px padding, one text control “Quote” (26, r 6, quote glyph 14 muted, label 14.3). It hangs 6 px above the selection’s first line with its left edge at the selection’s start, clamped inside the measure. Selection tint as today: Muse blue at 27 % (#3D72F2 · .27). No Copy button: ⌘C already copies.',
      sel_prose, 150),
     ('Selecting inside code', 'A selection that stays inside one code block (or one shell output on the Well) is a code quote: the same bar over its first selected line. The highlight runs edge to edge per line, square.',
      sel_code, 250),
     ('Near the top', 'When the bar would sit under the title row (the first line is within 38 px of the sheet’s top), it hangs 6 px under the selection’s last line instead.',
      flip, 160),
     ('In the composer', 'Quote puts the selection in the field, above the text: a 2 px rule at white 18 %, the text in body-small muted (14.3 / 22) clamped to two lines, ✕ (20) at the right. The selection clears; the field takes the keyboard (edge at 22 %) with the caret after any draft. A selection across blocks keeps its structure as Markdown (list markers, a fence for code, | cells | for table rows); here it is shown flattened.',
      composer_states, 150),
     ('A code quote', 'A code-only selection shows in mono 12.65 / 20, clamped to three lines, with “rust · 2 lines” faint under it; it is sent as a fenced block in the selection’s language.',
      composer_code, 170),
     ('Two quotes', 'Quote again and a second block stacks under the first, 8 apart, in the order they were added. Quoting text that is already quoted adds nothing.',
      composer_two, 150),
     ('Sent', 'The quote rides at the top of the bubble as the D4 quote block, at the shipped scale: 2 px rule at 18 %, 14.3 / 22 muted, three lines; “from the reply above” faint under it; then your words. A code quote stays mono, four lines. Clicking the quote scrolls to its source (ease-out 260 ms) and flashes the source span’s tint once.',
      sent + sent_code, 280),
    ]
    return d4.sheet_board('D4 · Quote', 'Baaz had no selection bar. Selecting text in a turn now offers one action, Quote, which carries the words into the composer as a quote block above your draft and sends them as a quote at the top of your message. One bar, one action, one block.', items, d4.H["D4Quote.dc.html"])

d4.BOARDS.update({
 "D4QuoteSelect.dc.html": ("Quote · selecting", b_quote_select, 1440, 900, 0, 0),
 "D4QuoteCompose.dc.html": ("Quote · in the composer", b_quote_compose, 1440, 900, 0, 0),
 "D4Quote.dc.html": ("D4 · Quote", b_quote_sheet, 1440, 1760, 0, 0),
})

# ======================= as shipped: the live captures the refresh was measured from =======================
import gen_canvas as gc
_CAP = "/private/tmp/claude-501/-Users-latekaapi-Projects-harness--claude-worktrees-determined-gates-2cbefc/4af776f1-0938-42f7-a939-cec55d31cb6f/scratchpad/cap/"
for _k in [k for k in gc.BLOBS if k.startswith("cap-")]:
    gc.LOCAL[_k] = _CAP + _k[4:] + ".png"

def shots_board(title, sub, shots, h):
    b = f'<div class="w" style="width:1440px;height:{h}px;background:var(--l1)">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">{title}</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;color:{MUTED};width:1360px">{sub}</div>'
    for i, (key, label) in enumerate(shots):
        x = 40 + (i % 2) * 690; y = 92 + (i // 2) * 452
        b += (f'<div style="position:absolute;left:{x}px;top:{y}px;width:670px"><img src="{{src}}" alt="{label}" style="width:670px;height:419px;display:block;border-radius:6px;box-shadow:0 0 0 1px rgba(255,255,255,.08)">'
              f'<div style="font-size:12px;color:{MUTED};margin-top:6px">{label}</div></div>').replace('{src}', '@@' + key + '@@')
    return b + '</div>'

def _shots(title, sub, shots, h):
    def fn(src):
        out = shots_board(title, sub, shots, h)
        for key, _ in shots:
            out = out.replace('@@' + key + '@@', src[key])
        return out
    return fn

d4.BOARDS["D4Shipped.dc.html"] = ("D4 · As shipped (captures, 2026-10-17)", _shots(
    "D4 as shipped — offscreen captures of baaz 65f3698 (aui v0.5.1)",
    "The refreshed boards were measured from these: reading scale 16 / 26, the composer in Claude Desktop’s shape (field 790–840, controls 846–874, sheet edge 894), the opaque title row, the terminal dock as a surface, and today’s selection with no bar (bottom right).",
    [("cap-d4-markdown", "synthetic-markdown · the answer, the floating composer"), ("cap-d4-toolgroup", "synthetic-toolgroup · fold open, groups as rows"),
     ("cap-d4-terminal", "terminal dock as a surface under the composer"), ("cap-d4-span", "select-span · today: a selection, nothing to do with it")], 1000), 1440, 1000, 0, 0)

# the four screens (Sessions open / closed): the sheet is now the real app's capture (D4 as shipped)
gc.LOCAL["cap-sheet-open"] = "/private/tmp/claude-501/-Users-latekaapi-Projects-harness--claude-worktrees-determined-gates-2cbefc/4af776f1-0938-42f7-a939-cec55d31cb6f/scratchpad/canvas/up/sheet-open.png"
gc.LOCAL["cap-shot-closed"] = "/private/tmp/claude-501/-Users-latekaapi-Projects-harness--claude-worktrees-determined-gates-2cbefc/4af776f1-0938-42f7-a939-cec55d31cb6f/scratchpad/canvas/up/shot-closed.png"
def b_sessions_open2(src):
    b = f'<div class="w" style="width:1440px;height:900px">{gc.sidebar_sessions(src)}<div class="sbfade"></div>'
    b += f'<div class="sheet" style="left:262px;top:6px;right:6px;bottom:6px"><img src="{src["cap-sheet-open"]}" alt="" style="width:1172px;height:888px;display:block"></div>'
    return b + '</div>'
def b_sessions_closed2(src):
    return f'<div class="w" style="width:1440px;height:900px"><img src="{src["cap-shot-closed"]}" alt="" style="width:1440px;height:900px;display:block"></div>'
FOUR = {"SessionsOpen.dc.html": ("Sessions · open", b_sessions_open2, 1440, 900, 0, 2000),
        "SessionsClosed.dc.html": ("Sessions · closed", b_sessions_closed2, 1440, 900, 1520, 2000)}
d4.BOARDS.update(FOUR)

# label text: the old numbers in the board notes → the shipped ones
LABEL_FIXES = [
    ('body 13 → 14.3 / 22', 'body 16 / 26'), ('body 14.3 / 22', 'body 16 / 26'), ('Body 14.3 / 23 (1.6), paragraphs 12 apart', 'Body 16 / 26, paragraphs 16 apart'),
    ('h2 17.6 / 24 semibold with 20 above and 8 below; h3 15.4 / 22 with 16 / 6', 'h2 20 / 28 semibold with 28 above and 10 below; h3 17 / 24 with 22 / 8'),
    ('items 2 apart', 'items 4 apart'), ('Rule: 1 px 7 %, 16 above and below', 'Rule: 1 px 7 %, 22 above and below'),
    ('paragraphs 10 apart, h2 as title 17.6 / 24 semibold', 'paragraphs 16 apart, h2 20 / 28 semibold'),
    ('Raised · r 12 · the edge · 9 / 13', 'Raised · r 12 · the edge · 8 / 13'),
    ('As approved on Composers: Raised, r 12, the edge, 8 around; the field text at 16 in subtitle 15.4 / 22; text controls below; the round send at Floating.',
     'Claude Desktop’s shape (owner review, 2026-10-14): the field is its own Raised box, r 12, edge ink 8 % (22 % while it holds the keyboard), 50 tall for one line, text 16 / 26; send 28 inside at the right. The controls row stands outside it, 6 below; 20 under it to the sheet’s edge.'),
    ('A chip row above the field, 6 apart, inset 8:', 'A chip row inside the field, above the text, 6 apart:'),
    ('While a turn runs the send button becomes stop (a 12 px square), and the status row sits 8 above the card.',
     'While a turn runs the send button becomes stop (a 12 px square). Progress is inline: the status row ends the transcript and scrolls with it; nothing is pinned over the composer, and the esc hint is not printed (esc still interrupts).'),
    ('the status row (28 px, caption muted) 8 above the composer, with the key cap.', 'one status row (28 px, caption muted) at the end of the transcript, scrolling with it — no pinned row, no printed key hint.'),
    ('8 above the composer; the count', '8 above the composer’s field; the count'),
]
_page_prev = d4.page
def page(name, title, body, w, h):
    def body2(src):
        out = body(src)
        for a, b in LABEL_FIXES:
            out = out.replace(a, b)
        # the Running item showed a pinned status row over the composer; keep the row, drop the esc hint
        out = re.sub(r'<span>·</span><span class="key">esc</span><span>to interrupt</span>', '', out)
        out = re.sub(r'<div class="att"><span class="c f">(?:(?!</span></span></div>).)*?quote</span><span class="x">.*?</span></span></div>',
                     lambda m: cquote('the provider closed the stream before the reply finished'), out, flags=re.S)
        out = out.replace('A Text attachment: the quote glyph and the first words of the selected text; sent, it becomes the quote block in the user turn.',
                          'A quote block above the draft (rule, muted text, ✕); the whole flow is on D4 · Quote.')
        return out
    return _page_prev(name, title, body2, w, h)
d4.page = page

HEIGHTS = {"D4Blocks.dc.html": 3760, "D4Cards.dc.html": 1980, "D4Composer.dc.html": 2240, "D4More.dc.html": 2240,
           "D4Reading.dc.html": 1680, "D4Pickers.dc.html": 2270}
for _f, _h in HEIGHTS.items():
    _t = d4.BOARDS[_f]; d4.BOARDS[_f] = (_t[0], _t[1], _t[2], _h, _t[4], _t[5])

if __name__ == "__main__":
    for f, (t, fn, w, h, x, y) in d4.BOARDS.items():
        d4.H[f] = h
        d4.page(f[:-8], t, fn, w, h)
    print("refreshed", list(d4.BOARDS))
