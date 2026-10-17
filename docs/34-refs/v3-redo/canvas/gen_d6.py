#!/usr/bin/env python3
"""D6 — the right pane, on the v3 ladder (2026-10-17).

Owner decisions this round (2026-10-17, chat): the reference product is Claude Desktop; git actions (commit, push,
PR) live in Changes, no Git tab; the pane's browser is the session's page (no tab strip; Browse mode stays the
person's own browsing).

Content is the harness repo as captured (target/debug/baaz --screenshot with the right-* probe entries): the real tree,
the fixture git status the app draws in deterministic mode (crates/baaz/src/right.rs +148 −12 …), real file text.
Themes are CSS variables so every view draws in dark and in light from one source.
Run: python3 gen_d6.py   (after gen_d4r.py; writes project/D6*.dc.html + preview/D6*.html)."""
import os, sys, json, html
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d4r  # noqa: F401  (patches gen_d4 with the shipped D4 values)
import gen_d4 as d4
import gen_canvas as gc
from gen_canvas import ico, I, BLOBS, LOCAL

ROOT = os.path.dirname(os.path.abspath(__file__))
LOCAL["page"] = "/private/tmp/claude-501/-Users-latekaapi-Projects-harness--claude-worktrees-determined-gates-2cbefc/4af776f1-0938-42f7-a939-cec55d31cb6f/scratchpad/page-local.png"; LOCAL["thumb"] = "/private/tmp/claude-501/-Users-latekaapi-Projects-harness--claude-worktrees-determined-gates-2cbefc/4af776f1-0938-42f7-a939-cec55d31cb6f/scratchpad/thumb-local.png"
P = ROOT + "/project"; V = ROOT + "/preview"

I.update(
 changes='<circle cx="5" cy="4" r="1.6"/><circle cx="5" cy="12" r="1.6"/><circle cx="11" cy="6" r="1.6"/><path d="M5 5.6v4.8M11 7.6c0 2-2 2.4-4.6 3.2"/>',
 files='<path d="M2 4a1 1 0 0 1 1-1h3l2 2h5a1 1 0 0 1 1 1v6a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1z"/>',
 fileg='<path d="M4 2h5l3 3v9H4z"/><path d="M9 2v3h3"/>',
 page='<circle cx="8" cy="8" r="6"/><path d="M2 8h12M8 2c2 2 2 10 0 12M8 2c-2 2-2 10 0 12"/>',
 back2='<path d="M10 3L5 8l5 5"/>', ext='<path d="M9 3h4v4M13 3L7.5 8.5M12 9.5V13H3V4h3.5"/>',
 pin='<path d="M8 14s4.5-4.2 4.5-7.5a4.5 4.5 0 0 0-9 0C3.5 9.8 8 14 8 14z"/><circle cx="8" cy="6.5" r="1.5"/>',
 cam='<rect x="2" y="4.5" width="12" height="8.5" rx="1.5"/><path d="M5.5 4.5L6.5 3h3l1 1.5"/><circle cx="8" cy="8.7" r="2.2"/>',
 lock='<rect x="4" y="7" width="8" height="6" rx="1"/><path d="M5.5 7V5.5a2.5 2.5 0 0 1 5 0V7"/>',
 branch='<circle cx="5" cy="4" r="1.6"/><circle cx="5" cy="12" r="1.6"/><circle cx="11" cy="5" r="1.6"/><path d="M5 5.6v4.8M11 6.6c0 2.4-3 2.6-5 4"/>',
 pr='<circle cx="4.5" cy="4" r="1.6"/><circle cx="4.5" cy="12" r="1.6"/><circle cx="11.5" cy="12" r="1.6"/><path d="M4.5 5.6v4.8M11.5 10.4V7a2 2 0 0 0-2-2H7M8.5 3.5L7 5l1.5 1.5"/>',
 img='<rect x="2.5" y="3" width="11" height="10" rx="1.5"/><circle cx="6" cy="6.5" r="1.2"/><path d="M2.5 11l3.5-3 3 2.5 2-1.5 2.5 2"/>',
 term3='<path d="M3 4l4 4-4 4M8 12h5"/>',
 dot='<circle cx="8" cy="8" r="3" fill="currentColor" stroke="none"/>',
 lockf='<path d="M8 2.5v.01"/>',
 rs='<path d="M5 4h6M5 8h6M5 12h4"/>',
 md='<path d="M2.5 4.5h11v7h-11z"/><path d="M4.5 9.5v-3l1.5 1.5 1.5-1.5v3M10.5 6.5v3M9.3 8.4l1.2 1.2 1.2-1.2"/>',
 json='<path d="M6 3c-1.5 0-2 .6-2 2v1.2c0 .8-.4 1.3-1.2 1.8.8.5 1.2 1 1.2 1.8V11c0 1.4.5 2 2 2M10 3c1.5 0 2 .6 2 2v1.2c0 .8.4 1.3 1.2 1.8-.8.5-1.2 1-1.2 1.8V11c0 1.4-.5 2-2 2"/>',
 chk='<path d="M3.5 8.5l3 3 6-7"/>',
 eye='<path d="M1.5 8S4 3.5 8 3.5 14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>',
 arrup='<path d="M8 13V3M4 7l4-4 4 4"/>',
)

DARK = dict(l0="#121215", l1="#1B1B1F", l2="#232328", l2h="#2A2A30", l3="#2C2C32", well="#151518",
            fg="#F2F2F4", muted="#9D9DA6", faint="#6E6E78", hov="rgba(255,255,255,.05)", sel="rgba(255,255,255,.09)",
            line="rgba(255,255,255,.07)", succ="#86EFAC", warn="#FCD34D", dang="#FCA5A5",
            edge="inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06)",
            float="inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 8px 24px -10px rgba(0,0,0,.4)",
            focus="inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.22)",
            addbg="rgba(134,239,172,.10)", delbg="rgba(252,165,165,.10)", band="rgba(255,255,255,.07)",
            pri="#F2F2F4", prifg="#121215", kw="#B88DEA", ty="#7AA4EF", st="#A8D8A0", cm="#6E6E78", shade="rgba(0,0,0,.16)")
LIGHT = dict(l0="#ECECEF", l1="#FAFAFB", l2="#FFFFFF", l2h="#F3F3F5", l3="#FFFFFF", well="#F2F2F4",
             fg="#18181B", muted="#63636D", faint="#9A9AA4", hov="rgba(0,0,0,.04)", sel="rgba(0,0,0,.07)",
             line="rgba(0,0,0,.07)", succ="#15803D", warn="#A16207", dang="#B91C1C",
             edge="inset 0 0 0 1px rgba(0,0,0,.08),0 1px 2px rgba(0,0,0,.04)",
             float="inset 0 0 0 1px rgba(0,0,0,.08),0 8px 24px -10px rgba(0,0,0,.18)",
             focus="inset 0 0 0 1px rgba(0,0,0,.28)",
             addbg="rgba(21,128,61,.09)", delbg="rgba(185,28,28,.08)", band="rgba(0,0,0,.05)",
             pri="#18181B", prifg="#FAFAFB", kw="#7C3AED", ty="#1D4ED8", st="#15803D", cm="#9A9AA4", shade="rgba(0,0,0,.06)")

def vars_of(t):
    return ';'.join(f'--{k}:{v}' for k, v in t.items()) + ';'

MONO = '"Geist Mono",ui-monospace,Menlo,monospace'
D6CSS = f"""
body{{margin:0}}
.z{{font-family:"Inter",-apple-system,"SF Pro Text","Helvetica Neue",sans-serif;color:var(--fg);-webkit-font-smoothing:antialiased;font-size:14.3px;line-height:22px;position:relative;overflow:hidden;background:var(--l0)}}
.z *{{box-sizing:border-box}}
.z .mono{{font-family:{MONO}}}
.z button{{font-family:inherit;color:inherit}}
.z .sheet2{{position:absolute;background:var(--l1);border-radius:8px;box-shadow:var(--edge);overflow:hidden}}
/* pane chrome */
.pn{{position:absolute;inset:0;display:flex;flex-direction:column}}
.ph2{{height:32px;flex:none;display:flex;align-items:center;gap:2px;padding:0 6px;position:relative;z-index:2;background:var(--l1)}}
.pfade{{position:absolute;left:0;right:0;top:32px;height:24px;background:linear-gradient(var(--l1),rgb(from var(--l1) r g b / 0));z-index:2;pointer-events:none}}
.tab{{height:28px;border:0;border-radius:6px;background:transparent;color:var(--muted);display:inline-flex;align-items:center;gap:6px;padding:0 10px 0 8px;font-size:14.3px;white-space:nowrap}}
.tab:hover,.tab.hov{{background:var(--hov);color:var(--fg)}}
.tab.on{{background:var(--sel);color:var(--fg)}}
.tab.ic{{width:28px;padding:0;justify-content:center}}
.ib2{{width:28px;height:28px;border:0;border-radius:6px;background:transparent;color:var(--muted);display:inline-flex;align-items:center;justify-content:center;flex:none;padding:0}}
.ib2:hover,.ib2.hov{{background:var(--hov);color:var(--fg)}}
.ib2.on{{background:var(--sel);color:var(--fg)}}
.ib2.off{{color:var(--faint);opacity:.6}}
.pt{{display:flex;align-items:baseline;gap:8px;min-width:0;flex:1;padding-left:2px}}
.pt b{{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0;flex:none;max-width:100%}}
.pt span{{color:var(--muted);font-size:13.2px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis;min-width:0}}
.pb{{flex:1;position:relative;overflow:hidden}}
.sp{{flex:1}}
/* rows */
.rows{{padding:4px 6px 12px}}
.fr{{height:28px;display:flex;align-items:center;gap:6px;border-radius:6px;padding-right:8px;white-space:nowrap;color:var(--fg)}}
.fr:hover,.fr.hov{{background:var(--hov)}}
.fr.sel{{background:var(--sel)}}
.fr.kbd{{box-shadow:inset 0 0 0 1px rgba(127,127,140,.45)}}
.fr .cv{{width:16px;flex:none;display:flex;justify-content:center;color:var(--muted)}}
.fr .gl{{width:14px;flex:none;display:flex;color:var(--muted)}}
.fr .nm{{overflow:hidden;text-overflow:ellipsis}}
.fr .ign{{color:var(--faint)}}
.fr .bd{{margin-left:auto;font-family:{MONO};font-size:12.1px;width:14px;text-align:center;flex:none}}
.cap2{{height:32px;display:flex;align-items:flex-end;padding:0 8px 6px;font-size:13.2px;line-height:18px;color:var(--muted);gap:6px}}
.cap2 .n{{color:var(--faint);font-family:{MONO};font-size:12.1px}}
.note2{{font-size:13.2px;line-height:18px;color:var(--faint);padding:8px 8px}}
/* changes */
.cr2{{height:32px;display:flex;align-items:center;gap:8px;border-radius:6px;padding:0 8px 0 0;white-space:nowrap}}
.cr2:hover,.cr2.hov{{background:var(--hov)}}
.cr2 .cv{{width:16px;flex:none;display:flex;justify-content:center;color:var(--muted);margin-left:4px}}
.cr2 .st2{{font-family:{MONO};font-size:12.1px;width:12px;flex:none;text-align:center}}
.cr2 .nm{{flex:none}}
.cr2 .dir{{color:var(--muted);font-size:13.2px;overflow:hidden;text-overflow:ellipsis;min-width:0}}
.cr2 .dl{{margin-left:auto;font-family:{MONO};font-size:12.1px;display:inline-flex;gap:6px;flex:none}}
.cr2 .op{{margin-left:4px}}
.diff2{{font-family:{MONO};font-size:12.1px;line-height:20px;margin:2px -6px 6px;white-space:pre;overflow:hidden}}
.diff2 .hh{{height:24px;line-height:24px;background:var(--l2h);color:var(--muted);padding:0 12px}}
.diff2 .ln{{display:grid;grid-template-columns:36px 14px 1fr;padding-right:12px}}
.diff2 .ln .n{{color:var(--faint);text-align:right;padding-right:8px}}
.diff2 .ln .s{{color:var(--faint)}}
.diff2 .a{{background:var(--addbg)}}.diff2 .a .s{{color:var(--succ)}}
.diff2 .d{{background:var(--delbg)}}.diff2 .d .s{{color:var(--dang)}}
.diff2 .gap{{height:24px;line-height:24px;color:var(--muted);font-family:"Inter",sans-serif;font-size:13.2px;padding-left:22px;display:flex;align-items:center;gap:6px}}
/* commit foot */
.foot2{{position:absolute;left:0;right:0;bottom:0;padding:8px;background:var(--l1);z-index:3}}
.foot2::before{{content:"";position:absolute;left:0;right:0;top:-24px;height:24px;background:linear-gradient(rgb(from var(--l1) r g b / 0),var(--l1));pointer-events:none}}
.cbx{{background:var(--l2);border-radius:10px;box-shadow:var(--edge);padding:8px;display:flex;flex-direction:column;gap:6px}}
.cbx.focus{{box-shadow:var(--focus)}}
.cbx .msg{{padding:2px 4px;font-size:14.3px;line-height:22px;min-height:44px}}
.cbx .msg .t1{{font-weight:600}}
.cbx .msg .ph3{{color:var(--muted)}}
.cbx .row3{{display:flex;align-items:center;gap:6px;height:28px}}
.br{{display:inline-flex;align-items:center;gap:5px;font-family:{MONO};font-size:12.1px;color:var(--muted);white-space:nowrap;overflow:hidden;min-width:0}}
.br .f{{color:var(--faint)}}
.btn{{height:28px;border:0;border-radius:6px;padding:0 12px;display:inline-flex;align-items:center;gap:6px;font-size:14.3px;white-space:nowrap;flex:none}}
.btn.pri{{background:var(--pri);color:var(--prifg);font-weight:600}}
.btn.sec{{background:var(--l2h);box-shadow:var(--edge);color:var(--fg)}}
.btn.txt{{background:transparent;color:var(--muted)}}.btn.txt:hover,.btn.txt.hov{{background:var(--hov);color:var(--fg)}}
.btn.dis{{opacity:.45}}
.spin2{{width:12px;height:12px;border-radius:50%;border:1.5px solid rgba(157,157,166,.35);border-top-color:var(--muted);flex:none;display:inline-block}}
.btn.pri .spin2{{border-color:rgba(127,127,127,.35);border-top-color:var(--prifg)}}
.errb{{border-radius:8px;background:var(--l2);box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgb(from var(--dang) r g b / .7);padding:8px 10px;display:flex;flex-direction:column;gap:4px;margin-bottom:8px}}
.errb .t{{font-weight:600;display:flex;align-items:center;gap:8px}}
.errb pre{{margin:0;font-family:{MONO};font-size:12.1px;line-height:18px;color:var(--muted);white-space:pre-wrap}}
.prc{{background:var(--l2);border-radius:10px;box-shadow:var(--edge);padding:10px 12px;display:flex;flex-direction:column;gap:6px}}
.prc .t{{font-weight:600;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}}
.prc .m{{font-size:13.2px;color:var(--muted);display:flex;align-items:center;gap:8px;white-space:nowrap}}
.dotc{{width:8px;height:8px;border-radius:50%;display:inline-block;flex:none}}
/* code view */
.cv2{{font-family:{MONO};font-size:12.65px;line-height:20px;white-space:pre;padding:8px 0 16px}}
.cv2 .l{{display:grid;grid-template-columns:48px 1fr;column-gap:12px;padding-right:12px}}
.cv2 .l .n{{color:var(--faint);text-align:right}}
.cv2 .l.b{{background:var(--band)}}.cv2 .l.b .n{{color:var(--fg)}}
.cv2 .k{{color:var(--kw)}}.cv2 .y{{color:var(--ty)}}.cv2 .c{{color:var(--cm)}}.cv2 .s{{color:var(--st)}}
/* reading in the pane */
.rd2{{padding:16px 20px 24px;font-size:16px;line-height:26px}}
.rd2 h1{{font-size:24px;line-height:32px;font-weight:600;margin:0 0 12px}}
.rd2 h2{{font-size:20px;line-height:28px;font-weight:600;margin:28px 0 10px}}
.rd2 p{{margin:0 0 16px}}
.rd2 .ic{{font-family:{MONO};font-size:14px;background:var(--sel);border-radius:4px;padding:1px 5px}}
.rd2 pre{{margin:0 0 16px;background:var(--l2);box-shadow:var(--edge);border-radius:8px;padding:10px 12px;font-family:{MONO};font-size:12.65px;line-height:20px;white-space:pre;overflow:hidden}}
/* well output */
.out{{position:absolute;inset:32px 0 0 0;background:var(--well);font-family:{MONO};font-size:12.65px;line-height:20px;white-space:pre;padding:12px 16px 40px;overflow:hidden}}
.out .g{{color:var(--succ)}}.out .r{{color:var(--dang)}}.out .f{{color:var(--faint)}}.out .w{{color:var(--warn)}}
.outfoot{{position:absolute;left:0;right:0;bottom:0;height:28px;display:flex;align-items:center;padding:0 16px;font-size:12.1px;color:var(--faint);background:var(--well);border-top:1px solid var(--line)}}
/* empty states */
.emp{{position:absolute;inset:32px 0 0 0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:6px;padding:0 32px;text-align:center}}
.emp .tl{{width:40px;height:40px;border-radius:10px;background:var(--l2);box-shadow:var(--edge);display:flex;align-items:center;justify-content:center;color:var(--muted);margin-bottom:6px}}
.emp .h{{font-weight:600}}
.emp .s{{font-size:13.2px;line-height:18px;color:var(--muted);max-width:260px}}
.sk{{height:10px;border-radius:5px;background:var(--sel)}}
/* page */
.addr2{{height:28px;flex:1;min-width:0;border-radius:6px;background:var(--well);box-shadow:var(--edge);display:flex;align-items:center;gap:6px;padding:0 8px;font-size:13.2px;white-space:nowrap;overflow:hidden}}
.addr2 .h{{color:var(--fg)}}.addr2 .p{{color:var(--muted);overflow:hidden;text-overflow:ellipsis}}
.pgw{{position:absolute;left:0;right:0;top:32px;bottom:0;background:#FFFFFF;overflow:hidden}}
.prog{{position:absolute;left:0;top:0;height:2px;background:var(--muted)}}
.apill{{position:absolute;left:50%;bottom:16px;transform:translateX(-50%);height:32px;border-radius:999px;background:#2C2C32;color:#F2F2F4;box-shadow:inset 0 1px 0 rgba(255,255,255,.07),inset 0 0 0 1px rgba(255,255,255,.06),0 8px 24px -10px rgba(0,0,0,.4);display:inline-flex;align-items:center;gap:8px;padding:0 14px 0 10px;font-size:13.2px;white-space:nowrap}}
.mk2{{width:16px;height:16px;border-radius:4px;color:#fff;font-size:10px;font-weight:700;display:inline-flex;align-items:center;justify-content:center;flex:none;background:#3B6DF2}}
.outl{{position:absolute;border:1.5px solid #3B6DF2;border-radius:4px;background:rgba(59,109,242,.06)}}
.pin2{{position:absolute;width:20px;height:20px;border-radius:50%;background:#18181B;color:#fff;font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;box-shadow:0 0 0 2px #fff}}
.notes2{{position:absolute;left:8px;right:8px;bottom:8px;background:var(--l3);border-radius:10px;box-shadow:var(--float);padding:6px}}
.notes2 .r{{display:flex;gap:8px;padding:6px 6px;font-size:13.2px;line-height:18px;border-radius:6px}}
.notes2 .r .n{{width:18px;height:18px;border-radius:50%;background:var(--sel);font-size:11px;font-weight:600;display:flex;align-items:center;justify-content:center;flex:none}}
.notes2 .r .sel3{{font-family:{MONO};font-size:12.1px;color:var(--muted)}}
.notes2 .ft3{{display:flex;align-items:center;gap:6px;padding:6px 4px 2px;border-top:1px solid var(--line);margin-top:4px}}
/* image view */
.iv{{position:absolute;inset:32px 0 0 0;display:flex;flex-direction:column;align-items:center;justify-content:center;gap:10px;padding:16px}}
.iv img{{max-width:100%;max-height:calc(100% - 40px);border-radius:6px;box-shadow:var(--edge);display:block;object-fit:contain}}
.iv .m{{font-family:{MONO};font-size:12.1px;color:var(--faint)}}
/* form */
.fm{{padding:12px 12px 0;display:flex;flex-direction:column;gap:12px}}
.fl{{display:flex;flex-direction:column;gap:6px}}
.fl label{{font-size:13.2px;color:var(--muted)}}
.inp{{min-height:32px;border-radius:6px;background:var(--well);box-shadow:var(--edge);padding:5px 10px;font-size:14.3px;line-height:22px}}
.inp.focus{{box-shadow:var(--focus)}}
.inp.ta{{min-height:150px}}
.pick{{height:32px;border-radius:6px;background:var(--l2h);box-shadow:var(--edge);display:inline-flex;align-items:center;gap:8px;padding:0 10px;font-family:{MONO};font-size:12.65px}}
.chk3{{width:16px;height:16px;border-radius:4px;box-shadow:inset 0 0 0 1.5px var(--muted);flex:none}}
/* transcript, themed (light screens) */
.tz{{font-size:16px;line-height:26px}}
.tz .ub{{margin-left:auto;max-width:78%;background:var(--l2);border-radius:12px;box-shadow:var(--edge);padding:8px 13px;width:fit-content}}
.tz .qr{{height:32px;display:flex;align-items:center;gap:8px;color:var(--muted);font-size:14.3px;margin:0 -8px;padding:0 8px;border-radius:6px}}
.tz .chip2{{font-family:{MONO};font-size:12.65px;display:inline-flex;gap:5px}}
.tz p{{margin:0}}
/* composer, themed */
.cfz{{background:var(--l2);border-radius:12px;box-shadow:var(--edge),0 8px 20px -8px var(--shade);min-height:50px;padding:12px 48px 12px 13px;position:relative;color:var(--muted);font-size:16px;line-height:26px}}
.cfz .snd{{position:absolute;right:6px;bottom:11px;width:28px;height:28px;border-radius:999px;background:var(--l3);box-shadow:var(--edge);display:flex;align-items:center;justify-content:center;color:var(--muted)}}
.crz{{height:28px;display:flex;align-items:center;gap:4px;padding:0 4px;margin-top:6px;color:var(--muted);font-size:14.3px}}
.crz .c{{display:inline-flex;align-items:center;gap:6px;padding:0 8px;height:28px}}
.lbl2{{font-size:12px;line-height:17px;color:var(--muted)}}
.lbl2 b{{display:block;color:var(--fg);font-weight:600;font-size:13px;margin-bottom:2px}}
"""

# ------------------------------------------------------------------ pieces
def ic(name, size=14, sw="1.5"):
    return ico(I[name], size=size, sw=sw)

def ib2(name, label, cls='', size=14):
    return f'<button class="ib2 {cls}" aria-label="{label}">{ic(name, size)}</button>'

TABS = [('changes', 'Changes'), ('files', 'Files'), ('page', 'Page')]

_ACTIVE = ['changes']
TITLES = dict(TABS)

def header_tabs(active, w, actions=''):
    """LOCKED (owner, 2026-10-17): no tab bar. The pane's row is its glyph, its name, its own actions and ✕;
    the choice of screen lives in the transcript's title row (toggles) and the panes sit on one strip."""
    _ACTIVE[0] = active
    return (f'<div class="ph2" style="padding-left:12px;gap:8px"><span style="color:var(--muted);display:flex">{ic(active, 14)}</span>'
            f'<div class="pt" style="padding-left:0"><b>{TITLES[active]}</b></div>{actions}{ib2("x", "Close the pane", size=12)}</div><div class="pfade"></div>')

def header_tabs_old(active, w, actions=''):
    narrow = w < 300
    t = ''.join(f'<button class="tab{" on" if k == active else ""}{" ic" if narrow else ""}" role="tab" aria-selected="{"true" if k == active else "false"}" aria-label="{lbl}">{ic(k)}{"" if narrow else lbl}</button>' for k, lbl in TABS)
    return f'<div class="ph2"><div role="tablist" aria-label="Right pane" style="display:flex;gap:2px">{t}</div><span class="sp"></span>{actions}</div><div class="pfade"></div>'

def header_push(title, sub='', actions='', back='Back'):
    s = f'<span>{sub}</span>' if sub else ''
    return (f'<div class="ph2">{ib2("back2", back)}<div class="pt"><b>{title}</b>{s}</div>{actions}{ib2("x", "Close the pane", size=12)}</div><div class="pfade"></div>')

def pane(w, h, inner, theme=DARK):
    return f'<div class="pn">{inner}</div>'

# ------------------------------------------------------------------ Files
TREE = [
 (0, 'd', '.cargo', 'ign'), (0, 'd', '.claude', 'ign'), (0, 'f', '.dev.env', 'ign'), (0, 'd', '.github', ''),
 (0, 'f', '.gitignore', ''), (0, 'd', '.relay', 'ign'), (0, 'md', 'CHANGELOG.md', ''), (0, 'md', 'CLAUDE.md', ''),
 (0, 'md', 'CONTRIBUTING.md', ''), (0, 'lock', 'Cargo.lock', 'M'), (0, 'f', 'Cargo.toml', ''), (0, 'f', 'LICENSE', ''),
 (0, 'md', 'README.md', ''), (0, 'd', 'assets', ''), (0, 'do', 'crates', ''), (1, 'do', 'baaz', ''),
 (2, 'f', 'Cargo.toml', ''), (2, 'd', 'assets', ''), (2, 'do', 'src', ''), (3, 'd', 'app', ''),
 (3, 'rs', 'app.rs', ''), (3, 'rs', 'attachments.rs', ''), (3, 'd', 'browser', ''), (3, 'rs', 'layout.rs', 'M'),
 (3, 'rs', 'main.rs', ''), (3, 'd', 'panes', 'A'), (3, 'rs', 'right.rs', 'M'), (3, 'rs', 'session.rs', ''),
 (3, 'rs', 'steps.rs', ''), (3, 'rs', 'transcript.rs', ''), (0, 'd', 'design', ''), (0, 'd', 'docs', 'M'),
 (0, 'd', 'fixtures', ''), (0, 'd', 'schemas', ''), (0, 'd', 'scripts', 'D'), (0, 'json', 'uiprobe.json', ''),
]
GLYPH = {'d': 'files', 'do': 'files', 'f': 'fileg', 'md': 'fileg', 'rs': 'fileg', 'json': 'json', 'lock': 'lock'}

def badge(b):
    col = {'M': 'var(--muted)', 'A': 'var(--succ)', 'D': 'var(--dang)', 'U': 'var(--faint)'}.get(b)
    return f'<span class="bd" style="color:{col}" aria-label="{ {"M":"modified","A":"added","D":"deleted","U":"untracked"}[b] }">{b}</span>' if col else ''

def file_rows(rows, sel=None, hov=None, kbd=None, limit=None):
    out = ''
    for i, (depth, kind, name, flag) in enumerate(rows[:limit]):
        cv = ic('chevd' if kind == 'do' else 'chevr', 12) if kind in ('d', 'do') else ''
        b = badge(flag) if flag in ('M', 'A', 'D', 'U') else ''
        cls = ' sel' if name == sel else (' hov' if name == hov else '')
        if name == kbd: cls += ' kbd'
        nm = f'<span class="nm{" ign" if flag == "ign" else ""}">{name}</span>'
        out += (f'<div class="fr{cls}" role="treeitem" aria-label="{name}" style="padding-left:{2 + 16 * depth}px">'
                f'<span class="cv">{cv}</span><span class="gl">{ic(GLYPH[kind], 14)}</span>{nm}{b}</div>')
    return out

def view_files(w, sel='right.rs', hov=None, filt=None, kbd=None, limit=None):
    acts = ib2('search', 'Filter files', 'on' if filt is not None else '')
    body = ''
    if filt is not None:
        body += (f'<div style="padding:6px 8px 2px"><div class="inp focus" style="display:flex;align-items:center;gap:8px;height:32px;padding:0 10px">'
                 f'<span style="color:var(--muted);display:flex">{ic("search", 14)}</span><span>{filt}</span><span style="margin-left:auto;font-size:12.1px;color:var(--faint)" class="mono">3 of 1,204</span></div></div>')
        rows = [(0, 'do', 'crates', ''), (1, 'do', 'baaz', ''), (2, 'do', 'src', ''), (3, 'rs', 'right.rs', 'M'),
                (2, 'do', 'tests', ''), (3, 'rs', 'right_pane.rs', ''), (0, 'do', 'docs', ''), (1, 'md', '42-handoff-d6-right-pane.md', '')]
        body += f'<div class="rows" role="tree" aria-label="Files">{file_rows(rows, sel=sel, kbd=kbd)}</div>'
    else:
        body += f'<div class="rows" role="tree" aria-label="Files" style="padding-top:4px">{file_rows(TREE, sel=sel, hov=hov, kbd=kbd, limit=limit)}</div>'
    return header_tabs('files', w, acts) + f'<div class="pb">{body}</div>'

# ------------------------------------------------------------------ file preview
RS_SRC = [
 ('c', '/// The merged Changes view (B8b): this session\'s edits on top, the rest of'),
 ('c', '/// the working tree below, in one scrollable view.'),
 ('', '<span class="k">fn</span> <span class="y">changes_pane_from</span>(pane_in: &amp;<span class="y">ChangesPane</span>) -&gt; <span class="y">AnyElement</span> {'),
 ('', '    <span class="k">let</span> status = pane_in.status;'),
 ('', '    <span class="k">let</span> (session_files, other_files) ='),
 ('', '        <span class="y">partition_session_files</span>(&amp;status.files, pane_in.session_edits, pane_in.root);'),
 ('', '    <span class="k">let</span> diffs: <span class="y">HashMap</span>&lt;&amp;<span class="y">str</span>, &amp;<span class="y">Diff</span>&gt; = pane_in'),
 ('', '        .parsed'),
 ('', '        .<span class="y">as_ref</span>()'),
 ('', '        .<span class="y">map</span>(|parsed| {'),
 ('', '            parsed'),
 ('', '                .diffs'),
 ('', '                .<span class="y">iter</span>()'),
 ('', '                .<span class="y">map</span>(|diff| (diff.path.<span class="y">as_str</span>(), diff))'),
 ('', '                .<span class="y">collect</span>()'),
 ('', '        })'),
 ('', '        .<span class="y">unwrap_or_default</span>();'),
 ('', '    <span class="k">let mut</span> pane = <span class="y">v_flex</span>()'),
 ('', '        .<span class="y">id</span>(<span class="s">"right-changes-pane"</span>)'),
 ('', '        .<span class="y">role</span>(gpui::<span class="y">Role</span>::Group)'),
 ('', '        .<span class="y">aria_label</span>(<span class="s">"Changes"</span>)'),
 ('', '        .<span class="y">size_full</span>()'),
 ('', '        .<span class="y">overflow_y_scroll</span>()'),
 ('', '        .<span class="y">child</span>(<span class="y">changes_heading</span>('),
 ('', '            <span class="s">"right-changes-session-heading"</span>,'),
 ('', '            <span class="s">"This session"</span>.<span class="y">into</span>(),'),
 ('', '        ));'),
 ('', '    <span class="k">if</span> pane_in'),
 ('', '        .parsed'),
 ('', '        .<span class="y">as_ref</span>()'),
 ('', '        .<span class="y">is_some_and</span>(|parsed| parsed.truncated)'),
 ('', '    {'),
 ('', '        pane = pane.<span class="y">child</span>('),
 ('', '            <span class="y">div</span>()'),
 ('', '                .<span class="y">id</span>(<span class="s">"right-changes-truncated"</span>)'),
 ('', '                .<span class="y">role</span>(gpui::<span class="y">Role</span>::Label)'),
 ('', '                .<span class="y">aria_label</span>(<span class="s">"Diff limits"</span>)'),
 ('', '                .<span class="y">px</span>(<span class="y">px</span>(16.0))'),
 ('', '                .<span class="y">py</span>(<span class="y">px</span>(4.0))'),
]

def code_lines(src, start, band=None, n=None):
    out = ''
    for i, (kind, t) in enumerate(src[:n]):
        ln = start + i
        b = ' b' if band and band[0] <= ln <= band[1] else ''
        body = f'<span class="c">{t}</span>' if kind == 'c' else t
        out += f'<div class="l{b}"><span class="n">{ln}</span><span>{body}</span></div>'
    return out

def view_preview(w, band=None, name='right.rs', path='crates/baaz/src'):
    _ACTIVE[0] = 'files'
    acts = ib2('ext', f'Open {name} in its default app')
    sub = path if w >= 360 else ''
    return header_push(name, sub, acts, back='Back to files') + f'<div class="pb"><div class="cv2" style="padding-top:12px">{code_lines(RS_SRC, 2953 - 4, band)}</div></div>'

def view_preview_md(w):
    _ACTIVE[0] = 'files'
    acts = ib2('ext', 'Open CLAUDE.md in its default app')
    body = ('<div class="rd2"><h1>baaz — notes for an agent working here</h1>'
            '<h2 style="margin-top:8px">Visual verification is not optional here</h2>'
            '<p>A green <span class="ic">cargo test</span> has never seen the window. Three apps’ worth of cosmetic, flow-broken and jank defects reached the owner through a passing gate, which is why this repo carries a <span class="ic">uiprobe/1</span> adapter and relay carries a rung that uses it.</p>'
            '<p><b>Before reporting any change to a screen as done:</b></p>'
            '<pre>cargo build -p baaz\npython3 ~/.claude/skills/relay/scripts/relay_visual.py \\\n  --repo . --entry login-choose …</pre>'
            '<p>Exit 0 clean · 1 findings · 3 no adapter (never a pass).</p></div>')
    return header_push('CLAUDE.md', '' if w < 360 else 'harness', acts, back='Back to files') + f'<div class="pb">{body}</div>'

def view_binary(w):
    acts = ib2('ext', 'Open app.db in its default app')
    body = (f'<div class="emp"><div class="tl">{ic("fileg", 18)}</div><div class="h">app.db</div><div class="s">SQLite database · 4.2 MB. Baaz shows text, Markdown and images here.</div>'
            f'<button class="btn sec" style="margin-top:8px">{ic("ext", 14)}Open in default app</button></div>')
    return header_push('app.db', '' if w < 360 else 'fixtures', acts, back='Back to files') + body

def view_gone(w):
    body = (f'<div class="emp"><div class="tl">{ic("fileg", 18)}</div><div class="h">This file is gone</div><div class="s"><span class="mono" style="font-size:12.1px">crates/baaz/src/panes/old.rs</span> was moved or deleted since you opened it.</div></div>')
    return header_push('old.rs', '', '', back='Back to files') + body

def view_preview_loading(w):
    sk = ''.join(f'<div style="display:grid;grid-template-columns:48px 1fr;column-gap:12px;height:20px;align-items:center"><span class="sk" style="width:24px;justify-self:end;opacity:.6"></span><span class="sk" style="width:{x}%"></span></div>' for x in (62, 48, 80, 70, 35, 55, 74, 40, 66, 52))
    return header_push('main.rs', '' if w < 360 else 'crates/baaz/src', ib2('ext', 'Open main.rs in its default app'), back='Back to files') + f'<div class="pb" style="padding:12px 12px 0 0">{sk}</div>'

# ------------------------------------------------------------------ Changes
def drow(kind, n, text):
    s = {'a': '+', 'd': '−', '': ' '}[kind]
    return f'<div class="ln {kind}"><span class="n">{n}</span><span class="s">{s}</span><span>{text}</span></div>'

DIFF_RIGHT = (
    '<div class="hh">@@ −2820,10 +2820,12 @@ fn changes_heading</div>' +
    drow('', 2820, '<span>fn changes_heading(id: &amp;\'static str, label: SharedString)</span>') +
    drow('', 2821, '    div()') +
    drow('', 2822, '        .id(id)') +
    drow('d', 2823, '        .px(px(16.0))') +
    drow('d', 2824, '        .pt(px(12.0))') +
    drow('a', 2823, '        .h(px(32.0))') +
    drow('a', 2824, '        .px(px(8.0))') +
    drow('a', 2825, '        .text_color(cx.theme().muted)') +
    drow('a', 2826, '        .text_size(rems(TEXT_CAPTION))') +
    drow('', 2827, '        .child(label)') +
    '<div class="gap">' + ic('chevd', 11) + '24 unchanged lines</div>' +
    '<div class="hh">@@ −2853,7 +2879,9 @@ fn session_file_row</div>' +
    drow('', 2879, 'fn session_file_row(') +
    drow('', 2880, '    change: &amp;FileChange,') +
    drow('d', 2881, '    open: bool,') +
    drow('a', 2881, '    open: bool, hovered: bool,') +
    drow('a', 2882, '    theme: &amp;Theme,')
)

CHG_SESSION = [('M', 'right.rs', 'crates/baaz/src', 148, 12), ('M', 'layout.rs', 'crates/baaz/src', 31, 4), ('A', 'mod.rs', 'crates/baaz/src/panes', 64, 0)]
CHG_OTHER = [('M', '02-app.md', 'docs', 22, 9), ('D', 'old_probe.py', 'scripts', 0, 37)]

def crow(st, name, d, a, r, open_=False, hov=False, w=400):
    col = {'M': 'var(--muted)', 'A': 'var(--succ)', 'D': 'var(--dang)'}[st]
    cv = ic('chevd' if open_ else 'chevr', 12)
    dl = (f'<span style="color:var(--succ)">+{a}</span>' if a else '') + (f'<span style="color:var(--dang)">−{r}</span>' if r else '')
    dirs = f'<span class="dir">{d}</span>' if w >= 340 else ''
    op = ib2('fileg', f'Open {name}', 'hov' if False else '') if hov else ''
    return (f'<div class="cr2{" hov" if hov else ""}" role="button" aria-expanded="{"true" if open_ else "false"}" aria-label="{"Hide" if open_ else "Show"} the diff for {d}/{name}">'
            f'<span class="cv">{cv}</span><span class="st2" style="color:{col}">{st}</span><span class="nm">{name}</span>{dirs}<span class="dl">{dl}</span>{op}</div>')

def commit_box(state='dirty', w=400, focus=False):
    n = 5
    if state == 'dirty':
        msg = ('<div class="t1">Merge Diff and Git into one Changes view</div>'
               '<div style="color:var(--muted)">Session files on top, the rest of the tree below; commit at the foot.</div>')
        return (f'<div class="cbx{" focus" if focus else ""}"><div class="msg" role="textbox" aria-label="Commit message">{msg}</div>'
                f'<div class="row3"><span class="br">{ic("branch", 12)}right-pane<span class="f">↑2</span></span><span class="sp"></span>'
                f'<button class="btn pri">Commit {n} files</button></div></div>')
    if state == 'committing':
        return (f'<div class="cbx"><div class="msg"><div class="t1">Merge Diff and Git into one Changes view</div><div style="color:var(--muted)">Session files on top, the rest of the tree below; commit at the foot.</div></div>'
                f'<div class="row3"><span class="br">{ic("branch", 12)}right-pane<span class="f">↑2</span></span><span class="sp"></span>'
                f'<button class="btn pri dis"><span class="spin2"></span>Committing…</button></div></div>')
    if state == 'failed':
        return (f'<div class="errb"><div class="t"><span style="color:var(--dang);display:flex">{ic("x", 14, "1.8")}</span>pre-commit hook failed</div>'
                f'<pre>cargo clippy --all-targets -- -D warnings\nerror: unused variable: `hovered` (right.rs:2881)</pre>'
                f'<div style="display:flex;gap:6px;margin-top:2px"><button class="btn txt" style="height:24px;padding:0 8px;font-size:13.2px">Show full output</button><button class="btn txt" style="height:24px;padding:0 8px;font-size:13.2px">Ask Muse to fix it</button></div></div>'
                + commit_box('dirty', w))
    if state == 'empty-msg':
        return (f'<div class="cbx focus"><div class="msg"><span class="ph3">Describe the change</span></div>'
                f'<div class="row3"><span class="br">{ic("branch", 12)}right-pane</span><span class="sp"></span>'
                f'<button class="btn pri dis">Commit {n} files</button></div></div>')
    if state == 'ahead':
        return (f'<div class="cbx"><div class="row3"><span class="br">{ic("branch", 12)}right-pane<span class="f">· 2 commits not pushed</span></span><span class="sp"></span>'
                f'<button class="btn pri">Push</button></div></div>')
    if state == 'pushed':
        return (f'<div class="cbx"><div class="row3"><span class="br">{ic("branch", 12)}right-pane<span class="f">· up to date</span></span><span class="sp"></span>'
                f'<button class="btn pri">{ic("pr", 14)}Create PR</button></div></div>')
    if state == 'pr':
        return (f'<div class="prc"><div style="display:flex;align-items:center;gap:8px"><span style="color:var(--succ);display:flex">{ic("pr", 14)}</span>'
                f'<span class="t">Merge Diff and Git into one Changes view</span><span class="mono" style="margin-left:auto;font-size:12.1px;color:var(--faint)">#128</span></div>'
                f'<div class="m"><span class="dotc" style="background:var(--succ)"></span>Open<span>·</span><span class="spin2"></span>3 of 4 checks<span class="sp"></span>'
                f'<button class="btn txt" style="height:24px;padding:0 8px;font-size:13.2px">{ic("ext", 12)}View</button></div></div>')
    return ''

def view_changes(w, open_file='right.rs', hov=None, foot='dirty', focus=False, scrolled=False, other_open=False):
    body = '<div class="rows" style="padding-bottom:160px">'
    if not scrolled:
        body += f'<div class="cap2" style="height:28px">This session<span class="n">3</span></div>'
    for st, name, d, a, r in CHG_SESSION:
        if scrolled and name != 'right.rs':
            continue
        o = name == open_file
        body += crow(st, name, d, a, r, open_=o, hov=(name == hov), w=w)
        if o:
            body += f'<div class="diff2">{DIFF_RIGHT}</div>'
    if not scrolled:
        body += f'<div class="cap2">Other changes<span class="n">2</span></div>'
        for st, name, d, a, r in CHG_OTHER:
            body += crow(st, name, d, a, r, open_=(other_open and name == '02-app.md'), hov=(name == hov), w=w)
    body += '</div>'
    return header_tabs('changes', w) + f'<div class="pb">{body}</div><div class="foot2">{commit_box(foot, w, focus)}</div>'

def view_changes_clean(w, state='pushed'):
    body = (f'<div class="emp" style="bottom:72px"><div class="tl">{ic("chk", 18)}</div><div class="h">No changes</div><div class="s">The working tree is clean.</div></div>')
    return header_tabs('changes', w) + body + f'<div class="foot2">{commit_box(state, w)}</div>'

def view_changes_state(w, title, sub, glyph='changes'):
    return header_tabs('changes', w) + f'<div class="emp"><div class="tl">{ic(glyph, 18)}</div><div class="h">{title}</div><div class="s">{sub}</div></div>'

def view_changes_loading(w):
    rows = ''.join(f'<div style="height:32px;display:flex;align-items:center;gap:10px;padding:0 8px 0 24px"><span class="sk" style="width:10px"></span><span class="sk" style="width:{x}px"></span><span class="sk" style="width:44px;margin-left:auto"></span></div>' for x in (90, 120, 70, 110, 84))
    return header_tabs('changes', w) + f'<div class="pb"><div class="rows"><div class="cap2" style="height:28px">This session</div>{rows}</div></div>'

# ------------------------------------------------------------------ Create PR (pushed view)
def view_pr_form(w):
    _ACTIVE[0] = 'changes'
    body = (f'<div class="fm">'
            f'<div class="fl"><label>Into</label><div style="display:flex;align-items:center;gap:8px;flex-wrap:wrap"><span class="pick">main{ic("down", 12)}</span><span style="color:var(--muted)">←</span><span class="br" style="font-size:12.65px;color:var(--fg)">{ic("branch", 12)}right-pane</span></div></div>'
            f'<div class="fl"><label>Title</label><div class="inp focus">Merge Diff and Git into one Changes view</div></div>'
            f'<div class="fl"><label>Description</label><div class="inp ta" style="color:var(--fg)">Changes is one view now: this session’s files on top, the rest of the working tree below, and commit, push and PR at its foot.<br><br>• Files rows on the v3 ladder (28, r 6, 9 % selected)<br>• Header buttons get accessible names<br><br><span style="color:var(--muted)">Drafted from this session · edit freely</span></div></div>'
            f'<label style="display:flex;align-items:center;gap:8px;font-size:14.3px"><span class="chk3"></span>Open as a draft</label></div>'
            f'<div class="foot2" style="display:flex;gap:6px;justify-content:flex-end;padding:8px 12px"><button class="btn txt">Cancel</button><button class="btn pri">{ic("pr", 14)}Create PR</button></div>')
    return header_push('Create pull request', '', '', back='Back to changes') + f'<div class="pb">{body}</div>'

# ------------------------------------------------------------------ Page
PAGE_IMG = "page"

def nav_row(w, url_host='localhost:3000', url_path='/pricing', annot=False, loading=False):
    narrow = w < 300
    nav = '' if narrow else ib2('back', 'Back') + ib2('fwd', 'Forward', 'off')
    nav += ib2('reload', 'Reload')
    addr = (f'<div class="addr2" role="textbox" aria-label="Address"><span style="color:var(--muted);display:flex">{ic("lock", 12)}</span><span class="h">{url_host}</span><span class="p">{url_path}</span></div>')
    right = ib2('pin', 'Annotate the page', 'on' if annot else '') + ib2('cam', 'Attach a screenshot to the composer') + ib2('ext', 'Open in Browse')
    prog = '<div class="prog" style="width:38%;top:34px"></div>' if loading else ''
    return f'<div class="nv" style="position:absolute;left:0;right:0;top:32px;height:36px;display:flex;align-items:center;gap:2px;padding:0 6px;background:var(--l1);z-index:2">{nav}{addr}{right}{prog}</div>'

def page_img(src, w):
    return f'<img src="{src[PAGE_IMG]}" alt="" style="display:block;width:1168px;height:881px;object-fit:cover;object-position:left top">'

def view_page(src, w, annot=False, pill=True, loading=False):
    pg = page_img(src, w)
    over = ''
    if pill and not annot:
        over += f'<div class="apill"><span class="mk2">M</span>Muse is clicking “Choose” <span class="spin2" style="border-color:rgba(255,255,255,.25);border-top-color:#F2F2F4"></span></div>'
    if annot:
        over += ('<div class="outl" style="left:200px;top:128px;width:150px;height:132px"></div><div class="pin2" style="left:340px;top:118px">2</div>'
                 '<div class="pin2" style="left:24px;top:36px">1</div>')
        over += (f'<div class="notes2" style="background:#2C2C32;color:#F2F2F4"><div class="r"><span class="n" style="background:rgba(255,255,255,.09)">1</span><div><div class="sel3" style="color:#9D9DA6">h1 · “Simple pricing”</div>Say how long the free trial is.</div></div>'
                 f'<div class="r"><span class="n" style="background:rgba(255,255,255,.09)">2</span><div><div class="sel3" style="color:#9D9DA6">div.card.team</div>Make Team the highlighted plan.</div></div>'
                 f'<div class="ft3" style="border-color:rgba(255,255,255,.07)"><span style="font-size:12.1px;color:#6E6E78">Click an element to pin a note</span><span class="sp"></span><button class="btn txt" style="height:24px;font-size:13.2px;color:#9D9DA6">Clear</button><button class="btn pri" style="height:24px;font-size:13.2px;background:#F2F2F4;color:#121215">Send 2 to Muse</button></div></div>')
    return header_tabs('page', w) + nav_row(w, annot=annot, loading=loading) + f'<div class="pgw" style="top:68px">{pg}{over}</div>'

def view_page_empty(w):
    body = (f'<div class="emp"><div class="tl">{ic("page", 18)}</div><div class="h">No page yet</div>'
            f'<div class="s">When Muse opens a page for this session, it shows here. Or type an address.</div>'
            f'<div class="addr2" style="flex:none;width:240px;height:32px;margin-top:8px;color:var(--muted)">{ic("search", 12)}Search or enter an address</div></div>')
    return header_tabs('page', w) + body

def view_page_error(w):
    body = (f'<div class="pgw" style="background:var(--l1)"><div class="emp" style="inset:0"><div class="tl">{ic("page", 18)}</div><div class="h">This page couldn’t load</div>'
            f'<div class="s"><span class="mono" style="font-size:12.1px">localhost:3000</span> refused the connection. Is the dev server running?</div>'
            f'<button class="btn sec" style="margin-top:8px">{ic("reload", 14)}Try again</button></div></div>')
    return header_tabs('page', w) + nav_row(w) + body.replace('class="pgw" style="', 'class="pgw" style="top:68px;', 1)

# ------------------------------------------------------------------ Output / Image (pushed from the transcript)
OUT_TEST = ('<span class="f">$ cargo test --workspace</span>\n'
            '   Compiling baaz v0.1.0 (crates/baaz)\n'
            '    Finished `test` profile [unoptimized + debuginfo] in 41.2s\n'
            '     Running unittests src/main.rs\n\n'
            'running 1 204 tests\n'
            'test right::tests::a_binary_file_diff_parses_without_rows ... <span class="g">ok</span>\n'
            'test right::tests::a_rename_parses_under_its_new_path ... <span class="g">ok</span>\n'
            'test right::tests::session_files_come_first_in_edit_order ... <span class="g">ok</span>\n'
            'test right::tests::the_changes_headings_name_their_counts_once ... <span class="r">FAILED</span>\n'
            'test right::tests::long_paths_middle_truncate ... <span class="g">ok</span>\n'
            'test right::tests::unified_text_renders_every_hunk_with_marks ... <span class="g">ok</span>\n'
            'test right::tests::porcelain_letters_map_to_kinds ... <span class="g">ok</span>\n'
            'test right::tests::numstat_counts_binary_files_as_zero ... <span class="g">ok</span>\n'
            'test layout::tests::legacy_kinds_normalise_to_changes ... <span class="g">ok</span>\n'
            'test layout::tests::right_width_clamps_into_range ... <span class="g">ok</span>\n'
            'test terminal::service::tests::unknown_sessions_are_refused ... <span class="w">ignored</span>\n\n'
            'failures:\n\n'
            '---- right::tests::the_changes_headings_name_their_counts_once stdout ----\n'
            'thread \'main\' panicked at crates/baaz/src/right.rs:3326:9:\n'
            'assertion `left == right` failed\n'
            '  left: "Other changes · 5"\n'
            ' right: "Other changes"\n\n'
            'test result: <span class="r">FAILED</span>. 1 202 passed; 1 failed; 1 ignored; 0 measured\n')

def view_output(w, state='done'):
    right = {'done': f'<span class="mono" style="font-size:12.1px;color:var(--dang)">exit 101</span><span class="mono" style="font-size:12.1px;color:var(--faint);margin:0 6px">43.8 s</span>',
             'running': f'<span class="spin2"></span><span class="mono" style="font-size:12.1px;color:var(--faint);margin:0 6px">running · 12 s</span>'}[state]
    acts = right + ib2('copy', 'Copy the output')
    t = OUT_TEST if state == 'done' else OUT_TEST.split('test right::tests::porcelain')[0]
    foot = '<div class="outfoot">1 204 lines · capped at 10 000</div>' if state == 'done' else ''
    return header_push('<span class="mono" style="font-weight:400;font-size:12.65px">$ cargo test --workspace</span>', '', acts, back='Back') + f'<div class="out">{t}</div>{foot}'

def view_output_loading(w):
    body = f'<div class="emp"><span class="spin2"></span><div class="s">Fetching the full output…</div></div>'
    return header_push('<span class="mono" style="font-weight:400;font-size:12.65px">$ cargo build --workspace</span>', '', ib2('copy', 'Copy the output', 'off'), back='Back') + body

def view_image(src, w, key='thumb', name='transcript-real.png', meta='1440 × 900 · PNG · 312 KB'):
    acts = ib2('ext', f'Open {name} in its default app')
    return header_push(name, '', acts, back='Back') + f'<div class="iv"><img src="{src[key]}" alt="{name}"><div class="m">{meta}</div></div>'

def view_image_missing(w):
    body = (f'<div class="emp"><div class="tl">{ic("img", 18)}</div><div class="h">Image not available</div><div class="s"><span class="mono" style="font-size:12.1px">capture-peek.png</span> could not be read.</div></div>')
    return header_push('capture-peek.png', '', '', back='Back') + body

# ------------------------------------------------------------------ the screens
def themed_transcript(theme_dark=True):
    return ''

def screen6(src, pane_inner, w=400, theme=DARK, title='Merge the Diff and Git kinds into one Changes view.',
            tx=None, sidebar=True, pane_title=None, icons=None, cols=None):
    left = 262 if sidebar else 6
    if cols: w = sum(cw for cw, _ in cols) + 6 * (len(cols) - 1)
    pl = 1434 - w
    cs_w = pl - 6 - left
    meas = min(880, cs_w - 80)
    mx = (cs_w - meas) // 2
    b = f'<div class="z" style="width:1440px;height:900px;{vars_of(theme)}">'
    if sidebar:
        b += sidebar6(theme)
    b += f'<div class="sheet2" style="left:{left}px;top:6px;width:{cs_w}px;bottom:6px">'
    b += f'<div style="position:absolute;left:{mx}px;top:84px;width:{meas}px" class="tz">{tx or transcript6()}</div>'
    b += (f'<div style="position:absolute;left:0;right:0;bottom:0;height:150px;background:linear-gradient(rgb(from var(--l1) r g b / 0),var(--l1) 56px)"></div>'
          f'<div style="position:absolute;left:0;right:0;top:0;height:32px;background:var(--l1);display:flex;align-items:center;padding:0 10px 0 16px;z-index:2">'
          f'<span style="font-weight:600;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{title}</span><span class="sp"></span>'
          f'{centre_actions(icons)}</div>'
          f'<div style="position:absolute;left:0;right:0;top:32px;height:24px;background:linear-gradient(var(--l1),rgb(from var(--l1) r g b / 0));z-index:2"></div>')
    b += (f'<div style="position:absolute;left:{mx}px;width:{meas}px;bottom:20px"><div class="cfz">Ask Muse, or type / for commands<span class="snd">{ic("arrup", 14, "1.8")}</span></div>'
          f'<div class="crz">{ic("plus", 14)}<span class="c" style="color:var(--fg)"><span class="mk2" style="width:14px;height:14px;font-size:9px">M</span>Muse'
          f'{ic("down", 12)}</span><span style="display:inline-flex;align-items:center;gap:6px;margin-left:6px"><span style="width:12px;height:12px;border-radius:50%;box-shadow:inset 0 0 0 1.5px var(--line)"></span><span class="mono" style="font-size:11px;color:var(--faint)">0 tokens</span></span><span class="sp"></span><span class="c">Muse Spark 1.3{ic("down", 12)}</span><span class="c">Auto{ic("down", 12)}</span><span class="c">Default{ic("down", 12)}</span></div></div>')
    b += '</div>'
    if cols:
        x = pl
        for cw, inner in cols:
            b += f'<div class="sheet2" style="left:{x}px;top:6px;width:{cw}px;bottom:6px" role="complementary"><div class="pn">{inner}</div></div>'
            x += cw + 6
    else:
        b += f'<div class="sheet2" style="left:{pl}px;top:6px;width:{w}px;bottom:6px" role="complementary" aria-label="Right pane"><div class="pn">{pane_inner}</div></div>'
    return b + '</div>'

def centre_actions(icons):
    if icons is None:
        icons = {_ACTIVE[0]}
    if icons == 'old':
        return f'{ib2("term3", "Terminal")}{ib2("dots", "Session menu")}{ib2("right", "Hide the right pane", "on")}'
    t = ''.join(ib2(k, lbl, 'on' if k in icons else '') for k, lbl in (('changes', 'Changes'), ('files', 'Files'), ('page', 'Page'), ('term3', 'Terminal')))
    return t + '<span style="width:1px;height:16px;background:var(--line);margin:0 4px"></span>' + ib2('dots', 'Session menu')

def header_plain(glyph, title, actions='', sub=''):
    s = f'<span>{sub}</span>' if sub else ''
    return (f'<div class="ph2" style="padding-left:12px;gap:8px"><span style="color:var(--muted);display:flex">{ic(glyph, 14)}</span>'
            f'<div class="pt" style="padding-left:0"><b>{title}</b>{s}</div>{actions}</div><div class="pfade"></div>')

def untab(inner, glyph, title, actions=''):
    # swap the tab header for a plain title header
    if '<div class="ph2">' not in inner or 'role="tablist"' not in inner:
        return inner
    i = inner.find('<div class="ph2">'); j = inner.find('<div class="pfade"></div>') + len('<div class="pfade"></div>')
    return inner[:i] + header_plain(glyph, title, actions) + inner[j:]

def split(top, bottom, top_h=0.55):
    return (f'<div style="position:absolute;left:0;right:0;top:0;height:{top_h*100:.0f}%;overflow:hidden"><div class="pn">{top}</div></div>'
            f'<div style="position:absolute;left:0;right:0;top:{top_h*100:.0f}%;height:1px;background:var(--line);z-index:4"></div>'
            f'<div style="position:absolute;left:0;right:0;top:calc({top_h*100:.0f}% + 1px);bottom:0;overflow:hidden"><div class="pn">{bottom}</div></div>')

def sidebar6(theme):
    rows = [('search', 'Search'), ('plus', 'New session'), ('files', 'Add project')]
    r = ''.join(f'<div style="height:32px;display:flex;align-items:center;gap:12px;padding:0 20px"><span style="color:var(--muted);display:flex">{ico(I[i], size=16)}</span>{t}</div>' for i, t in rows)
    sess = (f'<div style="margin:0 8px;height:44px;border-radius:6px;background:var(--sel);display:flex;align-items:center;gap:10px;padding:0 10px 0 8px">'
            f'<span style="width:7px;height:7px;border-radius:50%;box-shadow:inset 0 0 0 1px var(--muted);flex:none"></span><div style="min-width:0;flex:1"><div style="font-weight:600;line-height:18px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">Merge the Diff and Git…</div>'
            f'<div style="font-size:13.2px;line-height:17px;color:var(--muted);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">Changes is one view now…</div></div><span style="font-size:13.2px;color:var(--muted)">2 m</span></div>')
    lights = gc.lights()
    return (f'{lights}<button class="ib2" aria-label="Hide sidebar" style="position:absolute;left:84px;top:8px">{ic("side", 16)}</button>'
            f'<div style="position:absolute;left:184px;top:10px;display:flex;height:24px;padding:2px;border-radius:6px;background:var(--hov)"><span style="width:28px;height:20px;border-radius:4px;background:var(--l3);box-shadow:var(--edge);display:flex;align-items:center;justify-content:center">{ic("list", 14)}</span><span style="width:28px;height:20px;display:flex;align-items:center;justify-content:center;color:var(--muted)">{ic("globe", 14)}</span></div>'
            f'<div style="position:absolute;left:0;top:51px;width:256px">{r}<div style="height:30px;display:flex;align-items:flex-end;padding:0 16px 4px;font-size:13.2px;color:var(--muted)">Today</div>{sess}</div>'
            f'<div style="position:absolute;left:0;bottom:0;width:256px;height:48px;display:flex;align-items:center;gap:10px;padding:0 16px;font-weight:600"><span style="width:24px;height:24px;border-radius:6px;background:#C9444C;color:#fff;font-size:12px;display:flex;align-items:center;justify-content:center">H</span>harness</div>')

def transcript6():
    return ('<div style="display:flex;flex-direction:column;gap:14px">'
            '<div class="ub">Merge the Diff and Git kinds into one Changes view.</div><div style="height:14px"></div>'
            f'<div class="qr">{ic("chevr", 12)}<span>Worked for 2 m 14 s · edited 3</span><span class="chip2"><span style="color:var(--succ)">+243</span><span style="color:var(--dang)">−16</span></span></div>'
            '<p>Changes is one view now: this session’s files on top, the rest of the working tree below, and the commit at its foot. The old Diff and Git kinds read as Changes, so a saved layout still opens.</p>'
            '<p>Run <span class="mono" style="font-size:14px;background:var(--sel);border-radius:4px;padding:1px 5px">cargo test -p baaz right::</span> before you commit; the headings test changed.</p></div>')

# ------------------------------------------------------------------ boards
def shot(src, inner, w=400, theme=DARK, **kw):
    return screen6(src, inner, w=w, theme=theme, **kw)

def b_changes(src): return shot(src, view_changes(400))
def b_files(src): return shot(src, view_files(400, sel='right.rs', hov='layout.rs'))
def b_preview(src): return shot(src, view_preview(400, band=(2953, 2955)))
def b_page(src): return shot(src, view_page(src, 400))
def b_output(src): return shot(src, view_output(400), icons={'changes'})
def b_image(src): return shot(src, view_image(src, 400), icons={'files'})
def b_prform(src): return shot(src, view_pr_form(400))
def b_changes_light(src): return shot(src, view_changes(400), theme=LIGHT)
def b_wide(src): return shot(src, view_changes(720, open_file='right.rs'), w=720, sidebar=False)
def b_narrow(src): return shot(src, view_files(280, sel='right.rs'), w=280)

def mini(inner, w, h=640, theme=DARK, label=None, note=None):
    lab = f'<div class="lbl2" style="width:{w}px;margin-bottom:8px"><b>{label}</b>{note or ""}</div>' if label else ''
    return (f'<div style="flex:none">{lab}<div style="position:relative;width:{w}px;height:{h}px;border-radius:8px;background:var(--l1);box-shadow:var(--edge);overflow:hidden;{vars_of(theme)}color:var(--fg)"><div class="pn">{inner}</div></div></div>')

def grid_board(title, sub, rows, h, theme=DARK, bg=None):
    b = f'<div class="z" style="width:1440px;height:{h}px;{vars_of(theme)};background:{bg or "var(--l0)"}">'
    b += f'<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">{title}</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;line-height:17px;color:var(--muted);width:1360px">{sub}</div>'
    b += '<div style="position:absolute;left:40px;top:100px;display:flex;flex-direction:column;gap:40px">'
    for row in rows:
        b += '<div style="display:flex;gap:32px;align-items:flex-start">' + ''.join(row) + '</div>'
    return b + '</div></div>'

def b_states_files(src):
    rows = [[
        mini(view_files(320, sel=None, hov='crates', limit=18), 320, label='Files · at rest + hover', note='Rows 28, r 6, 6 in from the pane; chevron 12 in a 16 slot, glyph 14, name 14.3; hover 5 %. Ignored paths faint. Git letter at the right: M muted, A success, D danger.'),
        mini(view_files(320, sel='right.rs', kbd='right.rs', limit=30)[:], 320, label='Selected + keyboard focus', note='Selected 9 %. Keyboard focus adds a 1 px ring at 45 % grey on the row; ↑↓ move, → opens a folder, ← closes, ⏎ previews.'),
        mini(view_files(320, sel='right.rs', filt='right'), 320, label='Filter', note='The header’s search button (lit while on) opens a 32 px Well field under the header: matches stay in their folders, others leave. Esc clears and closes it. No refresh button: the tree follows the disk.'),
        mini(header_tabs('files', 320) + f'<div class="pb"><div class="rows">{file_rows(TREE, limit=8)}</div><div class="note2" style="padding-left:14px">Showing the first 300 of 1 204 in this folder. Filter to find the rest.</div></div>', 320, 360, label='Capped folder', note='A folder past 300 entries ends with one faint line instead of a footer count.'),
    ], [
        mini(view_preview(320, band=(2953, 2955)), 320, label='Preview · code, revealed', note='A pushed view: ← back, name 600, folder muted, open-in-app on the right. Code on the sheet, gutter 48 faint, mono 12.65 / 20, no wrap. A reveal (from a link in the transcript) bands its lines at 7 % with their numbers in the foreground.'),
        mini(view_preview_md(320), 320, label='Preview · Markdown', note='Markdown reads like an answer: 16 / 26 on the sheet, 20 px in, code blocks Raised. Not a white page any more.'),
        mini(view_binary(320), 320, label='Preview · not shown', note='Binary or past 1 MB: a centred note, one secondary “Open in default app”.'),
        mini(view_preview_loading(320), 320, label='Preview · loading', note='Gutter and line bars at 9 %, no spinner; the header is already real.'),
    ], [
        mini(view_gone(320), 320, 360, label='Preview · gone', note='The file moved or was deleted.'),
        mini(header_tabs('files', 320) + f'<div class="emp"><div class="tl">{ic("files", 18)}</div><div class="h">No project</div><div class="s">Open or add a project to see its files.</div></div>', 320, 360, label='Files · no project'),
        mini(header_tabs('files', 320) + '<div class="pb"><div class="rows">' + ''.join(f'<div style="height:28px;display:flex;align-items:center;gap:6px;padding-left:{2+16*d}px"><span style="width:16px"></span><span class="sk" style="width:14px;height:14px;border-radius:4px"></span><span class="sk" style="width:{x}px"></span></div>' for d, x in ((0,70),(0,90),(0,60),(0,110),(1,80),(1,96),(0,74))) + '</div></div>', 320, 360, label='Files · loading'),
    ]]
    return grid_board('D6 · Files and preview — every state', 'The tree and the file preview, dark. Content is the harness repo as the app reads it (probe entries right-files, right-files-reveal, right-file-preview-rs). One pushed-view header serves the preview, the PR form, the full output and the image: ← back, title, at most two actions, ✕.', rows, 2230)

def b_states_changes(src):
    rows = [[
        mini(view_changes(400, open_file=None, hov='layout.rs'), 400, 700, label='Changes · closed rows', note='Two sections in caption: This session (what the agent edited, in edit order) and Other changes. Rows 32: chevron, status letter (M muted, A success, D danger), name, folder muted, +N −M. Hover shows one open-file button.'),
        mini(view_changes(400), 400, 700, label='A file open', note='The row turns its chevron and the diff opens under it, full-bleed: hunk header 24 on Raised-lit, rows 20 in mono 12.1, one gutter (new line numbers), sign column, add / delete tinted 10 %. Unchanged runs fold into a “24 unchanged lines” row.'),
        mini(view_changes(400, foot='dirty', focus=True, scrolled=True), 400, 700, label='Scrolled into a diff', note='The open file’s row sticks under the header while its diff scrolls; the commit box stays at the foot over a 24 px fade.'),
    ], [
        mini(view_changes(400, foot='committing'), 400, 700, label='Committing', note='The commit box: Raised r 10, the message (drafted from the session, first line 600), branch + ahead count in faint mono, one primary “Commit N files”. It commits everything listed — no staging.'),
        mini(view_changes(400, foot='failed'), 400, 700, label='A hook failed', note='A danger-ringed note above the box: the hook’s first lines in mono, “Show full output” (opens the output view) and “Ask Muse to fix it” (puts the error in the composer). The message is kept.'),
        mini(view_changes_clean(400, 'ahead'), 400, 700, label='Clean, not pushed', note='Nothing changed but commits are ahead: the box is one row, branch + “2 commits not pushed”, Push.'),
    ], [
        mini(view_changes_clean(400, 'pushed'), 400, 700, label='Pushed, no PR', note='Up to date with its remote: Create PR (opens the form).'),
        mini(view_changes_clean(400, 'pr'), 400, 700, label='PR open', note='The PR as a card: icon in success, title, number, Open · checks (spinner while running), View → opens it in Browse.'),
        mini(view_pr_form(400), 400, 700, label='Create PR', note='A pushed view. Into: base picker ← head branch. Title and description drafted from the session, editable on the Well. Open as a draft. Cancel, Create PR.'),
    ], [
        mini(view_changes_loading(400), 400, 420, label='Loading'),
        mini(view_changes_state(400, 'Not a git repository', 'Changes needs git. Files still shows the project.'), 400, 420, label='Not a repository'),
        mini(view_changes_state(400, 'No project', 'Open or add a project to see its changes.', 'files'), 400, 420, label='No project'),
    ]]
    return grid_board('D6 · Changes, commit and PR — every state', 'One Changes view carries git (owner, 2026-10-17): this session’s files, the others, the diff inline, and at the foot one box whose single primary action follows the branch — Commit, then Push, then Create PR, then the PR itself. Content: the fixture git status the app draws (right-changes).', rows, 2580)

def b_states_page(src):
    rows = [[
        mini(view_page(src, 400), 400, 700, label='The session’s page', note='Under the pane row, a 36 px nav row: back · forward · reload, the address on the Well (host in the foreground, path muted), then annotate · screenshot to the composer · open in Browse. No tab strip: one page per session. While the agent acts, one pill names what it does.'),
        mini(view_page(src, 400, annot=True), 400, 700, label='Annotating', note='Annotate lit: hover outlines an element (Muse blue 1.5 px), a click pins a numbered note; the notes collect in a Floating panel at the bottom with “Send N to Muse” — the batch lands in the transcript as the annotation turn (D4).'),
        mini(view_page(src, 400, pill=False, loading=True), 400, 700, label='Loading', note='A 2 px muted bar under the address grows while the page loads; the old page stays until the new one paints.'),
    ], [
        mini(view_page_empty(400), 400, 520, label='No page yet', note='Until the agent opens one; the field takes an address or a search.'),
        mini(view_page_error(400), 400, 520, label='Couldn’t load', note='Baaz’s own error page on the sheet, not Chromium’s: the reason, one “Try again”.'),
        mini(view_page(src, 280, pill=False), 280, 520, label='Narrow (280)', note='Back and forward leave; reload, the address and the three actions stay.'),
    ]]
    return grid_board('D6 · Page — the session’s browser', 'The pane shows the one page this session’s agent works on (owner, 2026-10-17); Browse mode stays your own browsing, and “open in Browse” carries the page there. The page is a native window over the pane: everything drawn over it (pill, pins, notes panel) is an overlay window, as in Browse.', rows, 1520)

def b_states_views(src):
    rows = [[
        mini(view_output(400), 400, 700, label='Full output', note='From a tool card’s “N more lines” or its header: a pushed view. Header: the command in mono, exit (danger when non-zero) and time in faint mono, copy. Body on the Well, mono 12.65 / 20, ANSI colours mapped to the status tokens, no wrap. A footer line says when the output was capped.'),
        mini(view_output(400, 'running'), 400, 700, label='Still running', note='The tail follows new lines until you scroll up; the header shows a spinner and the running time.'),
        mini(view_output_loading(400), 400, 700, label='Fetching', note='The full output comes from the provider; a spinner and one line while it does.'),
    ], [
        mini(view_image(src, 400), 400, 560, label='Image', note='From an attachment tile, a Markdown image or an image file: a pushed view, the picture fitted on the sheet (r 6, the edge), its size and type in faint mono under it. Click toggles fit ↔ 100 % (then drag to pan); Esc or ← goes back.'),
        mini(view_image_missing(400), 400, 560, label='Image not available'),
        mini(view_image(src, 280, key='page', name='pricing.png', meta='1168 × 881 · PNG'), 280, 560, label='Narrow'),
    ]]
    return grid_board('D6 · Views the transcript opens — full output and image', 'Neither existed: today the output expands in place and an image file shows a binary card. Both open as a pushed view on the screen the pane is showing (with the pane closed: it opens on its last screen), and Back returns to what was there.', rows, 1500)

def centre_row(lit, hov=None, w=760):
    acts = centre_actions(set(lit) if lit else set())
    if hov:
        acts = acts.replace(f'class="ib2 " aria-label="{hov}"', f'class="ib2 hov" aria-label="{hov}"', 1)
    return (f'<div class="ph2" style="padding:0 10px 0 16px"><span style="font-weight:600;font-size:13px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">Merge the Diff and Git kinds into one Changes view.</span><span class="sp"></span>{acts}</div>')

def b_header(src):
    def strip(inner, w, theme=DARK, label=None, note=None):
        return mini(inner, w, 44, theme=theme, label=label, note=note)
    rows = [[
        strip(centre_row([]), 760, label='Transcript row · pane closed', note='Changes · Files · Page toggles, then Terminal (as today), a hairline, the session menu. Icon buttons 28, glyph 14, muted; hover 5 %.'),
        strip(centre_row(['changes'], hov='Files'), 560, label='Pane on Changes, pointer on Files', note='The lit toggle (9 % + foreground) is the screen in the pane: exactly one while the pane is open. Clicking Files slides the strip; clicking Changes again closes the pane.'),
    ], [
        strip(header_tabs('changes', 400), 400, label='Pane row · Changes', note='12 in: glyph 14 muted, the name 600, then the screen’s own actions, then ✕ (28, glyph 12). Opaque, the 24 px fade under it.'),
        strip(header_tabs('files', 400, ib2('search', 'Filter files')), 400, label='Pane row · Files', note='Its one action: filter.'),
        strip(header_tabs('page', 400), 400, label='Pane row · Page', note='The nav row sits under it (36).'),
    ], [
        strip(header_push('right.rs', 'crates/baaz/src', ib2('ext', 'Open right.rs in its default app')), 400, label='Pushed view', note='← back (28), title 600, context muted, ≤ 2 actions, ✕.'),
        strip(header_tabs('files', 280, ib2('search', 'Filter files')), 280, label='Narrow (280)', note='Nothing to drop: the row is already one name.'),
        strip(header_tabs('changes', 400), 400, LIGHT, label='Light'),
    ], [
        strip(centre_row(['page']), 560, LIGHT, label='Transcript row · light'),
        strip(header_push('right.rs', 'crates/baaz/src', ib2('ext', 'Open right.rs in its default app')), 400, LIGHT, label='Pushed view · light'),
    ]]
    return grid_board('D6 · Choosing the pane — toggles in the transcript row, no tab bar (LOCKED 2026-10-17)', 'Owner, 2026-10-17: no tab bar; icon toggles in the transcript’s title row choose the pane (Option A), and the panes sit as screens on one strip that slides to the toggled one. Close it with the pane row’s ✕, the lit toggle again, or ⌘⌥B. The pane is a second sheet 6 px right of the centre one, its own 32 px row on the lights’ line.', rows, 760)

def b_light(src):
    rows = [[
        mini(view_files(320, sel='right.rs', hov='layout.rs', limit=22), 320, 640, LIGHT, 'Files'),
        mini(view_preview(320, band=(2953, 2955)), 320, 640, LIGHT, 'Preview'),
        mini(view_changes(320, foot='dirty'), 320, 640, LIGHT, 'Changes'),
        mini(view_output(320), 320, 640, LIGHT, 'Full output'),
    ], [
        mini(view_changes(320, foot='failed'), 320, 640, LIGHT, 'Hook failed'),
        mini(view_changes_clean(320, 'pr'), 320, 640, LIGHT, 'PR open'),
        mini(view_pr_form(320), 320, 640, LIGHT, 'Create PR'),
        mini(view_page(src, 320, pill=True), 320, 640, LIGHT, 'Page'),
    ]]
    return grid_board('D6 · Light', 'Light is new (docs/35 left it undesigned): frame #ECECEF, sheet #FAFAFB, Raised #FFFFFF, Raised-lit #F3F3F5, Well #F2F2F4; text #18181B / #63636D / #9A9AA4; hover 4 % / selected 7 % black; edge = a 8 % black ring + a 1 px 4 % drop (in light the drop carries depth, FF’s light rule); status success #15803D, warning #A16207, danger #B91C1C. Same sizes as dark.', rows, 1540, theme=LIGHT)

def b_today(src):
    shots = [("cap-right-changes-dark", "Changes · grey full-width buttons, centred text"), ("cap-right-files-dark", "Files · tree rows, header, footer count"),
             ("cap-right-preview-rs-dark", "Preview · Back / Open as grey buttons"), ("cap-right-browser-dark", "Browser · tab strip + nav row")]
    b = f'<div class="z" style="width:1440px;height:1000px;{vars_of(DARK)}">'
    b += '<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">The right pane today — offscreen captures of baaz 65f3698</div>'
    b += f'<div style="position:absolute;left:40px;top:46px;font-size:12px;color:var(--muted);width:1360px">Probe entries right-changes, right-files, right-file-preview-rs, right-browser (BAAZ_DETERMINISTIC, 400 wide). Also seen: Markdown previews as a white serif page, an image file as a binary card, the header’s close ✕, Git verbs that do nothing yet.</div>'
    for i, (k, lab) in enumerate(shots):
        x = 40 + (i % 2) * 690; y = 92 + (i // 2) * 452
        b += f'<div style="position:absolute;left:{x}px;top:{y}px;width:670px"><img src="{src[k]}" alt="{lab}" style="width:670px;height:419px;display:block;border-radius:6px;box-shadow:0 0 0 1px rgba(255,255,255,.08)"><div style="font-size:12px;color:var(--muted);margin-top:6px">{lab}</div></div>'
    return b + '</div>'

CLOSE = None
def closeb(): return ib2('x', 'Close the pane', size=12)
def b_opt_a(src):
    return screen6(src, untab(view_changes(400), 'changes', 'Changes', closeb()), icons={'changes'})

def b_carousel(src):
    panes = [('changes', 'Changes', untab(view_changes(400), 'changes', 'Changes', closeb())),
             ('files', 'Files', untab(view_files(400, sel='right.rs', limit=26), 'files', 'Files', ib2('search', 'Filter files') + closeb())),
             ('page', 'Page', untab(view_page(src, 400, pill=False), 'page', 'Page', closeb()))]
    def card(inner, x, y, dim=False):
        return (f'<div class="sheet2" style="left:{x}px;top:{y}px;width:400px;height:640px;{"opacity:.35" if dim else ""}"><div class="pn">{inner}</div></div>')
    b = f'<div class="z" style="width:1440px;height:1560px;{vars_of(DARK)}">'
    b += '<div style="position:absolute;left:40px;top:24px;font-size:16px;font-weight:600">The panes as screens on a strip — a toggle slides the pane to its screen</div>'
    b += ('<div style="position:absolute;left:40px;top:46px;font-size:12px;line-height:17px;color:var(--muted);width:1360px">Changes · Files · Page sit side by side on one horizontal strip, in the order of their toggles. The pane is a window onto one of them. '
          'Clicking another toggle slides the strip to that screen (240 ms ease-out, the screens move together, nothing fades); each screen keeps its own scroll and state, so going back finds it as you left it. '
          'Two-finger swipe sideways in the pane does the same, one screen per swipe, snapping. Closing: the ✕ in the pane’s row, the lit toggle again, or ⌘⌥B — the pane collapses to the right as today and reopens on the screen it was showing.</div>')
    # strip
    b += '<div style="position:absolute;left:40px;top:120px;font-size:13px;font-weight:600">1 · At rest on Files</div>'
    for i, (k, lbl, inner) in enumerate(panes):
        b += card(inner, 40 + i * 440, 150, dim=(k != 'files'))
    b += '<div style="position:absolute;left:472px;top:142px;width:416px;height:656px;border:2px solid #3B6DF2;border-radius:12px;pointer-events:none"></div>'
    b += '<div style="position:absolute;left:480px;top:806px;font-size:12px;color:#7AA4EF">the pane: what you see</div>'
    b += f'<div style="position:absolute;left:1380px;top:150px;width:40px;font-size:12px;color:var(--muted)"></div>'
    # mid slide
    b += '<div style="position:absolute;left:40px;top:860px;font-size:13px;font-weight:600">2 · Mid-slide, Files → Page (the click on the Page toggle)</div>'
    b += ('<div style="position:absolute;left:472px;top:890px;width:400px;height:640px;border-radius:8px;overflow:hidden;box-shadow:0 0 0 2px #3B6DF2">'
          f'<div style="position:absolute;left:-120px;top:0;width:400px;height:640px" class="sheet2"><div class="pn">{panes[1][2]}</div></div>'
          f'<div style="position:absolute;left:286px;top:0;width:400px;height:640px" class="sheet2"><div class="pn">{panes[2][2]}</div></div></div>')
    b += ('<div style="position:absolute;left:920px;top:900px;width:460px;font-size:12px;line-height:17px;color:var(--muted)"><b style="color:var(--fg);font-size:13px;display:block;margin-bottom:4px">Rules</b>'
          'The 6 px gap between screens shows while sliding (frame colour), so each screen reads as its own sheet. The pane’s width never changes during a slide. Toggle icons in the transcript row are the position indicator: exactly one lit while the pane is open, none when closed. '
          'A view opened from the transcript (a diff chip, a file link, an output, an image) slides to its screen first, then pushes its view there with ← Back. With reduced motion: no slide, an instant swap. The Page screen is a native page window: during the slide it is shown as a still picture and goes live when the slide lands.</div>')
    return b + '</div>'

def b_opt_b(src):
    top = untab(view_changes(400, foot='dirty'), 'changes', 'Changes')
    bot = untab(view_page(src, 400, pill=False), 'page', 'Page')
    return screen6(src, split(top, bot, 0.56), icons={'changes', 'page'})
def b_opt_c(src):
    return screen6(src, None, icons={'changes', 'page'}, sidebar=False,
                   cols=[(400, untab(view_changes(400), 'changes', 'Changes')), (400, untab(view_page(src, 400, pill=False), 'page', 'Page'))])
def b_opt_d(src):
    inner = header_push('right.rs', 'crates/baaz/src', ib2('ext', 'Open right.rs in its default app'), back='Back to Changes') + f'<div class="pb"><div class="cv2" style="padding-top:12px">{code_lines(RS_SRC, 2953 - 4, (2953, 2955))}</div></div>'
    return screen6(src, inner)

BOARDS = {
 "D6OptA.dc.html": ("Option A · toggles, one pane", b_opt_a, 1440, 900),
 "D6Carousel.dc.html": ("Option A + strip: panes as screens", b_carousel, 1440, 1560),
 "D6OptB.dc.html": ("Option B · toggles, stack two", b_opt_b, 1440, 900),
 "D6OptC.dc.html": ("Option C · toggles, side by side", b_opt_c, 1440, 900),
 "D6OptD.dc.html": ("Option D · no chooser, the pane follows what you open", b_opt_d, 1440, 900),
 "D6Today.dc.html": ("D6 · Today (captures)", b_today, 1440, 1000),
 "D6Changes.dc.html": ("D6 · Changes", b_changes, 1440, 900),
 "D6Files.dc.html": ("D6 · Files", b_files, 1440, 900),
 "D6Preview.dc.html": ("D6 · File preview", b_preview, 1440, 900),
 "D6Page.dc.html": ("D6 · Page", b_page, 1440, 900),
 "D6Output.dc.html": ("D6 · Full output", b_output, 1440, 900),
 "D6Image.dc.html": ("D6 · Image", b_image, 1440, 900),
 "D6PRForm.dc.html": ("D6 · Create PR", b_prform, 1440, 900),
 "D6ChangesLight.dc.html": ("D6 · Changes, light", b_changes_light, 1440, 900),
 "D6Wide.dc.html": ("D6 · Wide (720), sidebar closed", b_wide, 1440, 900),
 "D6Narrow.dc.html": ("D6 · Narrow (280)", b_narrow, 1440, 900),
 "D6Header.dc.html": ("D6 · Choosing the pane (locked)", b_header, 1440, 760),
 "D6StatesFiles.dc.html": ("D6 · Files + preview states", b_states_files, 1440, 2230),
 "D6StatesChanges.dc.html": ("D6 · Changes + git states", b_states_changes, 1440, 2580),
 "D6StatesPage.dc.html": ("D6 · Page states", b_states_page, 1440, 1520),
 "D6StatesViews.dc.html": ("D6 · Output + image", b_states_views, 1440, 1500),
 "D6Light.dc.html": ("D6 · Light", b_light, 1440, 1540),
}

def page6(name, title, body, w, h):
    def render(src):
        return body(src)
    html_ = f'''<!doctype html>
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
<style>{D6CSS}</style>
</helmet>
{render(BLOBS)}
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
    assert '{{' not in html_.split('<script type="text/x-dc"')[0].split('</helmet>')[1], name
    open(f"{P}/{name}.dc.html", "w").write(html_)
    prev = f'<!doctype html><html><head><meta charset="utf-8"><link rel="stylesheet" href="https://fonts.googleapis.com/css2?family=Inter:wght@400;500;600;700&family=Geist+Mono:wght@400&display=swap"><style>{D6CSS}body{{background:#000}}</style></head><body>{render(LOCAL)}</body></html>'
    open(f"{V}/{name}.html", "w").write(prev)

if __name__ == "__main__":
    for f, (t, fn, w, h) in BOARDS.items():
        page6(f[:-8], t, fn, w, h)
    print("wrote", list(BOARDS))
