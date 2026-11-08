#!/usr/bin/env python3
"""R3 · The agent window — the four pieces the gallery does not draw yet (PROPOSAL, 2026-11-08, not built).

harness docs/75 §1b and docs/27 §4.5/§6/§8. The gallery (agentic-ui v0.6.6) already draws, and the owner approved on
5 Nov: the line's five states, the narrow line, the card, the column, the task faces, the sidebar and the Field. Those
are wired, not redrawn. What is new, and so comes as a board first:

  1. the line's inline answer (a small ask answers in a card that grows out of the line);
  2. the draft-in-page state (the draft lands in the page's own box, nothing sent);
  3. the handoff picker from the card ("continue this in Fix login" / "a new session in harness");
  4. Settings › Browser › Assistant (provider and model; the Agents level of docs/27 §6.2 is drawn for a decision).

Owner answers 2026-11-08: no "From sessions" heading (hairline only); round faces + word; the line stays under the
card; the Field moves to the prototype's centred position.

The line and card here copy the gallery captures' values (line row 10 r, 1 px 10 % edge, 44 px row, sec/pri buttons)
so the new states read as the same component. Run: python3 gen_r3.py (writes project/R3Agent.dc.html + preview)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d6 as g
import gen_d5p as d5
from gen_d6 import ic, mini, grid_board, I
from gen_d5p import CLAUDE, CODEX, MUSE, mark, btn

I.update(
    dock='<rect x="2" y="3" width="12" height="10" rx="2"/><path d="M10 3v10"/>',
    out='<path d="M9 3h4v4M13 3L7.5 8.5"/><path d="M12 9.5V12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h2.5"/>',
    folder='<path d="M2 4.5a1 1 0 0 1 1-1h3l1.5 1.5H13a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/>',
    undo='<path d="M5 6H10a3 3 0 0 1 0 6H7"/><path d="M7 3.5L4.5 6 7 8.5"/>',
)

CSS = """
.r3pg{position:absolute;inset:0;background:#F7F7F8;color:#1F1F23;font-size:13px;line-height:20px;padding:22px 28px;overflow:hidden}
.r3pg h3{margin:0 0 6px;font-size:18px;line-height:24px;font-weight:600;color:#18181B}
.r3pg .r3m{color:#6B6B75;font-size:12px;margin-bottom:12px}
.r3pg p{margin:0 0 10px;color:#3F3F46}
.r3bar{height:9px;border-radius:4px;background:#E2E2E6;margin:0 0 9px}
.r3box{border-radius:8px;background:#fff;box-shadow:inset 0 0 0 1px #D9D9DE;padding:10px 12px;min-height:88px;color:#27272A;font-size:13px;line-height:20px}
.r3box.on{box-shadow:inset 0 0 0 2px #7C8CF8}
.r3box .r3ph{color:#A1A1AA}
.r3cur{display:inline-block;width:1.5px;height:15px;background:#27272A;vertical-align:-3px}
.r3sendrow{display:flex;justify-content:flex-end;gap:8px;margin-top:8px}
.r3pg .r3pbtn{height:28px;border-radius:6px;padding:0 12px;font-size:12.5px;display:inline-flex;align-items:center;border:0;background:#18181B;color:#fff}
.r3pg .r3pbtn.g{background:#E8E8EC;color:#27272A}
/* the line, as the gallery draws it */
.r3dock{position:absolute;left:0;right:0;bottom:0;padding:0 12px 12px;display:flex;flex-direction:column;gap:8px}
.r3ln{border-radius:10px;background:#2A2A2F;box-shadow:inset 0 0 0 1px rgba(255,255,255,.10),0 10px 30px -12px rgba(0,0,0,.55);color:#F2F2F4}
.r3row{height:44px;display:flex;align-items:center;gap:10px;padding:0 8px 0 14px;font-size:14px}
.r3row .r3in{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r3row .r3ph{color:#8E8E98}
.r3ib{width:28px;height:28px;border:0;border-radius:6px;background:transparent;color:#9D9DA6;display:inline-flex;align-items:center;justify-content:center;flex:none}
.r3send{width:30px;height:30px;border:0;border-radius:7px;background:#F2F2F4;color:#121215;display:inline-flex;align-items:center;justify-content:center;flex:none}
.r3send.off{background:#3A3A40;color:#7A7A84}
.r3chips{display:flex;align-items:center;gap:8px;padding:0 14px 10px}
.r3chip{height:24px;border-radius:6px;background:#1B1B1F;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08);padding:0 8px;display:inline-flex;align-items:center;gap:6px;font-size:12.5px;color:#E4E4E7}
.r3chip .x{color:#8E8E98}
.r3plus{width:24px;height:24px;border-radius:6px;background:#4A4A50;color:#F2F2F4;display:inline-flex;align-items:center;justify-content:center}
.r3b{height:28px;border:0;border-radius:6px;padding:0 11px;font-size:13px;display:inline-flex;align-items:center;gap:6px;white-space:nowrap;flex:none}
.r3b.s{background:#4A4A50;color:#F2F2F4}
.r3b.p{background:#F2F2F4;color:#121215;font-weight:600}
.r3b.t{background:transparent;color:#B4B4BC}
.r3kbd{font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:10.5px;padding:1px 5px;border-radius:4px;background:#1B1B1F;color:#C8C8CF}
/* the answer: the line grown upward; the line itself becomes its foot */
.r3ans{padding:12px 14px 4px;font-size:14px;line-height:21px;color:#F2F2F4}
.r3ans .r3src{display:flex;align-items:center;gap:6px;margin-top:8px;font-size:12.5px;color:#9D9DA6}
.r3ans .r3src u{text-decoration:none;color:#C8C8CF;box-shadow:inset 0 -1px 0 rgba(255,255,255,.25)}
.r3act{display:flex;align-items:center;gap:6px;padding:8px 10px 10px 14px}
.r3act .sp{flex:1}
.r3sep{height:1px;background:rgba(255,255,255,.08);margin:0}
.r3busy{display:flex;align-items:center;gap:10px;padding:12px 14px;color:#B4B4BC;font-size:13.5px}
.r3spin{width:13px;height:13px;border-radius:7px;border:1.5px solid rgba(255,255,255,.22);border-top-color:#E4E4E7;flex:none}
.r3face{width:18px;height:18px;border-radius:9px;box-shadow:inset 0 0 0 1.5px #C8C8CF;position:relative;flex:none}
.r3face:before,.r3face:after{content:"";position:absolute;width:2.5px;height:2.5px;border-radius:2px;background:#C8C8CF;top:9px}
.r3face:before{left:5px}.r3face:after{right:5px}
.r3ok{width:18px;height:18px;border-radius:9px;background:rgba(134,239,172,.16);color:#86EFAC;display:inline-flex;align-items:center;justify-content:center;flex:none}
/* the card (gallery values) */
.r3card{position:absolute;width:420px;height:560px;right:16px;bottom:72px;border-radius:12px;background:#26262B;box-shadow:inset 0 0 0 1px rgba(255,255,255,.10),0 18px 48px -16px rgba(0,0,0,.6);color:#F2F2F4;display:flex;flex-direction:column;overflow:hidden}
.r3ch{padding:12px 10px 0 14px;display:flex;align-items:flex-start;gap:4px}
.r3ch .t{flex:1;min-width:0}
.r3ch .t b{display:block;font-size:15px;line-height:21px;font-weight:600}
.r3ch .t span{font-size:13px;color:#9D9DA6;display:inline-flex;align-items:center;gap:4px}
.r3ib.on{background:rgba(255,255,255,.09);color:#F2F2F4}
.r3body{flex:1;padding:12px 14px;display:flex;flex-direction:column;gap:10px;font-size:14px;line-height:21px;min-height:0}
.r3user{align-self:flex-end;max-width:85%;background:#34343A;border-radius:10px;padding:8px 12px}
.r3steps{border-radius:8px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.09);height:36px;display:flex;align-items:center;gap:8px;padding:0 10px;font-size:13px;color:#C8C8CF}
.r3steps .sp{flex:1}
.r3comp{box-shadow:inset 0 1px 0 rgba(255,255,255,.08);padding:10px 12px 12px}
.r3comp .r3row{padding:0 0 0 4px;height:36px}
.r3moved{border-radius:8px;background:#1F1F23;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08);padding:10px 12px;display:flex;align-items:center;gap:10px;font-size:13.5px}
.r3moved .t{flex:1;min-width:0}
.r3moved .t small{display:block;color:#9D9DA6;font-size:12.5px;line-height:18px}
/* the picker: a menu over the card */
.r3menu{position:absolute;border-radius:10px;background:#2E2E34;box-shadow:inset 0 0 0 1px rgba(255,255,255,.12),0 16px 40px -12px rgba(0,0,0,.7);color:#F2F2F4;overflow:hidden}
.r3menu .q{height:38px;display:flex;align-items:center;gap:8px;padding:0 12px;box-shadow:inset 0 -1px 0 rgba(255,255,255,.08);font-size:13.5px;color:#8E8E98}
.r3menu .h{font-size:11.5px;letter-spacing:.02em;color:#8E8E98;padding:8px 12px 4px;text-transform:uppercase}
.r3menu .r{height:34px;margin:0 4px;border-radius:6px;display:flex;align-items:center;gap:9px;padding:0 8px;font-size:13.5px}
.r3menu .r.hl{background:rgba(255,255,255,.08)}
.r3menu .r .n{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.r3menu .r .m{color:#8E8E98;font-size:12.5px;white-space:nowrap}
.r3menu .r .ck{color:#F2F2F4}
.r3menu .r.dim{color:#7A7A84}
.r3menu .f{box-shadow:inset 0 1px 0 rgba(255,255,255,.08);padding:8px 12px 10px;font-size:12px;line-height:17px;color:#8E8E98;margin-top:4px}
/* settings, as settings-browser draws it */
.r3set{position:absolute;inset:0;background:#1B1B1F;padding:28px 0 0 40px;color:#F2F2F4}
.r3set h2{margin:0 0 6px;font-size:19.8px;font-weight:600}
.r3set .in{font-size:14.3px;color:#9D9DA6;margin:0 0 14px}
.r3set .sh{font-size:13.2px;color:#9D9DA6;margin:14px 0 6px;display:flex;align-items:center;gap:8px}
.r3set .tag{font-size:11px;padding:1px 6px;border-radius:4px;background:rgba(252,211,77,.14);color:#FCD34D}
.r3grp{width:584px;border-radius:6px;box-shadow:inset 0 0 0 1px rgba(255,255,255,.09)}
.r3sr{display:flex;align-items:center;gap:12px;padding:9px 12px;min-height:46px}
.r3sr+.r3sr{box-shadow:inset 0 1px 0 rgba(255,255,255,.07)}
.r3sr .t{flex:1;min-width:0}
.r3sr .t b{display:block;font-weight:500;font-size:14.3px;line-height:20px}
.r3sr .t span{display:block;font-size:13.2px;line-height:18px;color:#9D9DA6}
.r3pick{height:28px;border-radius:6px;background:#2A2A30;box-shadow:inset 0 0 0 1px rgba(255,255,255,.08);padding:0 8px 0 6px;display:inline-flex;align-items:center;gap:7px;font-size:13.2px;color:#F2F2F4;white-space:nowrap}
.r3pick .p5mk{width:16px;height:16px;border-radius:4px;font-size:9px}
.r3seg{display:inline-flex;height:28px;border-radius:7px;background:#151518;box-shadow:inset 0 0 0 1px rgba(255,255,255,.07);padding:2px;gap:2px}
.r3seg span{padding:0 10px;display:inline-flex;align-items:center;border-radius:5px;font-size:13px;color:#9D9DA6}
.r3seg span.on{background:#34343A;color:#F2F2F4;box-shadow:inset 0 1px 0 rgba(255,255,255,.06)}
.r3radio{width:13px;height:13px;border-radius:7px;box-shadow:inset 0 0 0 1.5px #6E6E78;flex:none}
.r3radio.on{box-shadow:inset 0 0 0 1.5px #F2F2F4,inset 0 0 0 4px #1B1B1F,inset 0 0 0 7px #F2F2F4}
"""


# ------------------------------------------------------------------ pieces
def page_article():
    return ('<div class="r3pg"><h3>Returns and refunds</h3><div class="r3m">shop.example · Help centre · updated 2 Nov</div>'
            '<p>You can return most items within 30 days of delivery. Items marked final sale, gift cards and opened '
            'software can’t be returned.</p><p>Refunds go to the original payment method within 5–7 business days of '
            'the return arriving at our warehouse. Shipping costs are refunded only if the item was faulty.</p>'
            + ''.join(f'<div class="r3bar" style="width:{w}%"></div>' for w in (92, 86, 95, 70, 88, 91, 60, 84, 77)) + '</div>')


def page_thread(state):
    """A support-forum thread with a reply box. state: empty | typing | landed."""
    text = {
        'empty': '<span class="r3ph">Write a reply…</span>',
        'typing': 'Thanks for the detailed report. The export fails because the date column has two formats; '
                  'we’re fixing the parser<span class="r3cur"></span>',
        'landed': 'Thanks for the detailed report. The export fails because the date column has two formats; '
                  'we’re fixing the parser and will post here when 4.2.1 ships. Until then, saving the sheet as '
                  'CSV first works around it.',
    }[state]
    on = ' on' if state == 'landed' else ''
    return ('<div class="r3pg"><h3>CSV export drops rows after a blank date</h3><div class="r3m">forum.example · '
            'Bugs · 4 replies</div><p><b>mira_k</b> · Exporting a sheet with a blank date cell silently drops every '
            'row after it. Steps and the file are attached.</p>'
            + ''.join(f'<div class="r3bar" style="width:{w}%"></div>' for w in (90, 72, 84))
            + f'<div style="margin-top:14px;font-size:12px;color:#6B6B75;margin-bottom:6px">Your reply</div>'
            f'<div class="r3box{on}">{text}</div><div class="r3sendrow"><button class="r3pbtn g">Preview</button>'
            '<button class="r3pbtn">Post reply</button></div></div>')


def chips():
    return (f'<div class="r3chips"><span class="r3chip">{ic("fileg", 12)}This page <span class="x">×</span></span>'
            f'<span class="r3plus">{ic("plus", 12)}</span></div>')


def ask_row(text='', ph='Ask about this page', send_on=False):
    inner = text if text else f'<span class="r3ph">{ph}</span>'
    s = '' if send_on else ' off'
    return (f'<div class="r3row"><span style="color:#C8C8CF;display:inline-flex">{ic("spark", 15)}</span>'
            f'<span class="r3in">{inner}</span><button class="r3ib" aria-label="Open the card">{ic("dock", 14)}</button>'
            f'<button class="r3send{s}" aria-label="Send">{ic("arrup", 14)}</button></div>')


def dock(inner, narrow=False):
    return f'<div class="r3dock">{inner}</div>'


def frame(page, line, w, h, label, note):
    return mini(page + dock(line), w, h, label=label, note=note)


ANSWER = ('Within <b>30 days of delivery</b>. Final-sale items, gift cards and opened software can’t be returned; '
          'the refund reaches your card 5–7 business days after the return arrives.')


def answer_card(busy=False):
    if busy:
        top = f'<div class="r3busy"><span class="r3spin"></span>Reading this page…<span style="flex:1"></span><button class="r3b t">Stop</button></div>'
    else:
        top = (f'<div class="r3ans">{ANSWER}<div class="r3src">{ic("fileg", 12)}<span>From <u>Returns and refunds</u> · '
               f'paragraphs 1–2</span></div></div>'
               f'<div class="r3act"><button class="r3b t">{ic("copy", 13)}Copy</button><span class="sp"></span>'
               f'<button class="r3b s">Continue in a card <span class="r3kbd">⌘E</span></button>'
               f'<button class="r3ib" aria-label="Close the answer">{ic("x", 13)}</button></div>')
    q = 'What’s the refund window?'
    return (f'<div class="r3ln">{top}<div class="r3sep"></div>'
            + (ask_row(q, send_on=False) if busy else ask_row()) + (chips() if not busy else '') + '</div>')


def draft_line(state):
    if state == 'typing':
        row = (f'<div class="r3row"><span class="r3face"></span><span class="r3in">Claude is writing in “Your reply”</span>'
               f'<button class="r3b s">Take over <span class="r3kbd">⌘.</span></button></div>')
    else:
        row = (f'<div class="r3row"><span class="r3ok">{ic("check", 11)}</span><span class="r3in">Draft ready · '
               f'<span style="color:#9D9DA6">not sent</span></span>'
               f'<button class="r3b t">{ic("undo", 13)}Undo</button><button class="r3b p">Done</button></div>')
    return f'<div class="r3ln">{row}</div>'


def card_shell(body, comp, header_on=False, menu=''):
    on = ' on' if header_on else ''
    return ('<div class="r3card"><div class="r3ch"><div class="t"><b>Refund policies, three shops</b>'
            f'<span>Claude Haiku 4.5 {ic("chevd", 11)}</span></div>'
            f'<button class="r3ib{on}" aria-label="Continue in a session">{ic("out", 14)}</button>'
            f'<button class="r3ib" aria-label="Dock as a column">{ic("dock", 14)}</button>'
            f'<button class="r3ib" aria-label="Close">{ic("x", 14)}</button></div>'
            f'<div class="r3body">{body}</div><div class="r3comp">{comp}</div></div>{menu}')


CARD_BODY = ('<div class="r3user">Compare the refund windows on these three shops and save the policy pages as PDFs '
             'to ~/Documents/refunds</div>'
             f'<div class="r3steps"><span class="r3ok">{ic("check", 11)}</span><span>Worked for 1m 12s · 9 steps</span>'
             f'<span class="sp"></span>{ic("chevd", 12)}</div>'
             '<div>shop.example gives 30 days, northgear.example 14, and lumen.example 60 for unopened items. Three PDFs '
             'are in ~/Documents/refunds.</div>')


def card_comp(ph='Steer, or ⌘↩ to queue'):
    return (f'<div class="r3row"><span class="r3in"><span class="r3ph">{ph}</span></span>'
            f'<button class="r3send off" aria-label="Send">{ic("arrup", 14)}</button></div>'
            f'<div style="display:flex;gap:8px;padding-top:6px"><span class="r3chip">{ic("fileg", 12)}This page '
            f'<span class="x">×</span></span><span class="r3plus">{ic("plus", 12)}</span></div>')


def picker():
    rows = (f'<div class="h">Continue in a session</div>'
            f'<div class="r hl">{mark(CLAUDE, 16)}<span class="n">Fix login</span><span class="m">harness · 2 h</span></div>'
            f'<div class="r">{mark(CODEX, 16)}<span class="n">Checkout flow tests</span><span class="m">harness · yesterday</span></div>'
            f'<div class="r">{mark(MUSE, 16)}<span class="n">Sidebar polish</span><span class="m">agentic-ui · Tue</span></div>'
            f'<div class="h">New session in</div>'
            f'<div class="r">{ic("folder", 14)}<span class="n">harness</span><span class="m">Claude Code</span></div>'
            f'<div class="r">{ic("folder", 14)}<span class="n">agentic-ui</span><span class="m">Claude Code</span></div>'
            f'<div class="r">{ic("folder", 14)}<span class="n">cockpit</span><span class="m">Claude Code</span></div>'
            '<div class="f">Takes a summary of this chat and the pages it used. Its tabs move to that session’s task '
            'folder. This chat stays here.</div>')
    return (f'<div class="r3menu" style="width:320px;right:122px;bottom:258px"><div class="q">{ic("search", 13)}'
            f'Find a session or project</div>{rows}</div>')


def settings(menu=False):
    agents = (
        '<div class="sh">Agents <span class="tag">decide: R3 or R4</span></div><div class="r3grp">'
        '<div class="r3sr"><div class="t"><b>Before acting on a page</b><span>Auto still asks before Send, Pay, '
        'Delete, Upload and signing in.</span></div><span class="r3seg"><span class="on">Ask</span><span>Auto</span></span></div>'
        '<div class="r3sr"><div class="t"><b>Your own tabs</b><span>Whether a chat may use tabs you opened, not just its own.</span>'
        '</div><span class="r3seg"><span>Yes</span><span class="on">Ask first</span><span>Read only</span></span></div>'
        f'<div class="r3sr"><div class="t"><b>Sites</b><span>Always here: github.com, docs.rs · Never here: bank.example</span>'
        f'</div>{btn("Edit…", "sec")}</div></div>')
    m = ''
    if menu:
        m = ('<div class="r3menu" style="width:300px;right:20px;top:136px">'
             f'<div class="h">Claude Code</div>'
             f'<div class="r hl"><span class="n">Claude Haiku 4.5</span><span class="m">cheapest</span><span class="ck">{ic("check", 13)}</span></div>'
             '<div class="r"><span class="n">Claude Sonnet 4.5</span></div>'
             '<div class="r"><span class="n">Claude Opus 4.5</span></div>'
             '<div class="h">Codex</div>'
             '<div class="r"><span class="n">gpt-5.1-codex-mini</span><span class="m">cheapest</span></div>'
             '<div class="r"><span class="n">gpt-5.1-codex</span></div>'
             '<div class="h">Muse</div>'
             '<div class="r dim"><span class="n">Not connected</span><span class="m">Set up…</span></div>'
             '<div class="f">Names come from each provider’s live model list. Changing it applies to the next chat; '
             'an open chat keeps its model.</div></div>')
    return ('<div class="r3set"><h2>Browser</h2><div class="in">How Baaz browses: the assistant, tabs, search, downloads.</div>'
            '<div class="sh" style="margin-top:4px">Assistant</div><div class="r3grp">'
            f'<div class="r3sr"><div class="t"><b>Provider and model</b><span>Used by ⌘J, the card and Ask in the Field. '
            f'New chats start with it.</span></div><span class="r3pick">{mark(CLAUDE, 16)}Claude Haiku 4.5 {ic("chevd", 11)}</span></div>'
            '</div>'
            + agents +
            '<div class="sh">Search</div><div class="r3grp">'
            '<div class="r3sr" style="min-height:34px;gap:10px"><span class="r3radio on"></span><div class="t"><b>Google</b></div></div>'
            '<div class="r3sr" style="min-height:34px;gap:10px"><span class="r3radio"></span><div class="t"><b>DuckDuckGo</b></div></div>'
            '</div><div style="font-size:12px;color:#6E6E78;margin-top:8px">… Downloads and Default browser unchanged.</div>'
            f'</div>{m}')


# ------------------------------------------------------------------ the board
def b_r3(src):
    WW, NW, H = 712, 400, 460
    rows = [
        [frame(page_article(), answer_card(busy=True), WW, H, '1a · Inline answer · working',
               'You typed a small ask on the ⌘J line and pressed ↩. The line grows upward into a card with one quiet '
               'working row; Stop cancels. Nothing opens in the sidebar: a one-shot answer is not a task.'),
         frame(page_article(), answer_card(), NW, H, '1b · Answered · your pane width (400)',
               'The answer sits above the line, with where it came from. Copy, or “Continue in a card ⌘E” makes it a '
               'chat (the same browser session, so nothing is asked twice). × or Esc puts the line back. The line '
               'below is ready for the next ask.')],
        [frame(page_thread('typing'), draft_line('typing'), WW, H, '2a · Draft in page · writing',
               'A site action (“Draft a reply”) or an ask like “reply to this”. The line is the driving bar from the '
               'gallery: the typing face, Take over. The words land in the page’s own box as they are typed.'),
         frame(page_thread('landed'), draft_line('landed'), NW, H, '2b · Draft landed · your pane width (400)',
               'The box keeps its focus ring so you can see where it went. The line says “nothing sent”. Undo clears '
               'the box; Done closes the line. Posting is always yours: the agent never presses Post here, and if '
               'asked to, that is the Send approval.')],
        [mini(page_article() + card_shell(CARD_BODY, card_comp(), header_on=True, menu=picker())
              + dock(f'<div class="r3ln">{ask_row()}</div>'), WW, 680, label='3a · Handoff picker, from the card',
              note='A new header button, Continue in a session (↗), left of Dock and Close. Recent sessions first '
                   '(provider mark, project, when), then “New session in” per project with its default provider. '
                   'Typing filters both. ↩ hands off through docs/22: summary + the pages used. The line stays under '
                   'the card (your answer to Q3).'),
         mini(page_article() + card_shell(
             CARD_BODY + f'<div class="r3moved">{mark(CLAUDE, 18)}<div class="t">Continued in <b>Fix login</b>'
             f'<small>harness · its 3 tabs are now in that session’s task folder</small></div>{btn("Open", "sec")}</div>',
             card_comp('Ask something else here')), 600, 680, label='3b · Handed off',
             note='One settled row in this chat; this chat stays usable. Open switches to Sessions on that session, '
                  'whose transcript starts with the handoff block it already draws. The tabs leave this chat’s task '
                  'folder for the target’s.')],
        [mini(settings(), 664, 640, label='4a · Settings › Browser › Assistant',
              note='One new section at the top of the existing Browser page, not a new page. Default: the first '
                   'connected provider’s cheapest model (the auto-titles rule). The Agents block is docs/27 §6.2 and is '
                   'drawn so you can say whether it is R3 or waits for R4: today R2 always asks before the five, '
                   'with “Always on this site” on the approval.'),
         mini(settings(menu=True), 664, 640, label='4b · The model menu',
              note='Grouped by provider, from the live model list; the cheapest is marked. Unconnected providers show '
                   'one dim row with Set up…, which opens the provider sheet.')],
    ]
    return grid_board('R3 · The agent window — the four new pieces — PROPOSAL (2026-11-08, not built)',
                      'Everything else in R3 (the line’s five states, the card, the column, task faces, the sidebar, '
                      'the Field) is the gallery as approved on 5 Nov and is wired, not redrawn. Your 8 Nov answers: no '
                      '“From sessions” heading; faces + word; the line stays under the card; the Field moves to the '
                      'centred prototype. Four concepts here: an inline answer, a draft in the page, a handoff target, '
                      'the assistant’s model.', rows, 2720)


if __name__ == "__main__":
    g.D6CSS += d5.CSS + CSS
    g.page6("R3Agent", "R3 · Agent window (proposal)", b_r3, 1440, 2720)
    print("wrote R3Agent")
