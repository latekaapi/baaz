#!/usr/bin/env python3
"""P7 · Import — bring existing Claude Code and Codex sessions into Baaz (PROPOSAL, 2026-10-30, not built).

Owner decision 2026-10-16 (docs/50 §6.2): existing ~/.claude/projects and ~/.codex/sessions sessions become resumable
Baaz sessions. docs/69 S3: the offer is new UI, so it comes as a board first.

Five concepts: the offer (a count, where it was found), folders (what to bring), the import (progress), the result,
and the rows it leaves in the sidebar. Built from gen_d5p's pieces (connect screen, sheet, card) so it reads like D5.
Numbers are this Mac's on 30 Oct: 1,896 Claude Code sessions in 37 folders, 95 Codex sessions in 2.
Run: python3 gen_p7.py   (writes project/P7Import.dc.html + preview/P7Import.html)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d6 as g
import gen_d5p as d5
from gen_d6 import ic, mini, grid_board
from gen_d5p import CLAUDE, CODEX, MUSE, mark, dot, btn, row, connect, foot, CONNECTED_CLAUDE, OWL

CSS = """
.p7sht{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);width:540px;border-radius:12px;background:var(--l1);box-shadow:var(--float);overflow:hidden}
.p7sht .p7hd{height:44px;display:flex;align-items:center;gap:10px;padding:0 8px 0 16px;box-shadow:inset 0 -1px 0 var(--line)}
.p7sht .p7hd b{font-size:14.3px;font-weight:600;flex:1}
.p7in{padding:12px 16px 0;font-size:13.2px;line-height:19px;color:var(--muted)}
.p7list{margin:10px 16px 0;border-radius:8px;background:var(--l2);box-shadow:var(--edge);overflow:hidden}
.p7r{display:flex;align-items:center;gap:10px;height:36px;padding:0 12px;font-size:13.2px}
.p7r+.p7r{box-shadow:inset 0 1px 0 var(--line)}
.p7r input{margin:0;width:14px;height:14px;accent-color:var(--fg);flex:none}
.p7r .p7n{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.p7r .p7p{color:var(--faint);font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12px}
.p7r .p7c{display:inline-flex;align-items:center;gap:5px;color:var(--muted);font-variant-numeric:tabular-nums;white-space:nowrap}
.p7r .p7c .p5mk{width:14px;height:14px;border-radius:4px;font-size:8px}
.p7r .p7tag{font-size:11.5px;color:var(--faint);white-space:nowrap}
.p7hdr{display:flex;align-items:center;gap:10px;height:30px;padding:0 12px;font-size:12px;color:var(--faint);box-shadow:inset 0 -1px 0 var(--line)}
.p7opt{display:flex;align-items:center;gap:8px;padding:10px 16px 0;font-size:13.2px;color:var(--fg)}
.p7opt input{margin:0;width:14px;height:14px;accent-color:var(--fg)}
.p7opt span.m{color:var(--muted)}
.p7fn{padding:10px 16px 0;font-size:13.2px;line-height:18px;color:var(--faint)}
.p7ac{display:flex;align-items:center;gap:8px;padding:14px 16px 16px}
.p7ac .p5sp{flex:1}
.p7bar{margin:16px 16px 0;height:6px;border-radius:3px;background:var(--l2);box-shadow:var(--edge);overflow:hidden}
.p7bar i{display:block;height:100%;background:var(--fg);opacity:.75;border-radius:3px}
.p7big{padding:16px 16px 0;font-size:14.3px;line-height:21px}
.p7big .m{color:var(--muted)}
.p7line{display:flex;align-items:center;gap:10px;padding:12px 14px;box-shadow:inset 0 1px 0 var(--line);font-size:13.2px;color:var(--muted)}
.p7line .p5sp{flex:1}
.p7find{width:100%;margin-top:8px;border-radius:10px;background:var(--l1);box-shadow:var(--edge);display:flex;align-items:center;gap:10px;padding:10px 10px 10px 14px;font-size:13.2px;color:var(--muted)}
.p7find .p5sp{flex:1}
.p7sb{position:absolute;inset:0;background:var(--l0);padding:14px 10px;font-size:13.2px}
.p7g{display:flex;align-items:center;gap:6px;height:28px;padding:0 8px;color:var(--muted);font-size:12px;font-weight:600}
.p7row{display:flex;align-items:center;gap:8px;height:32px;padding:0 8px;border-radius:6px;color:var(--fg)}
.p7row .p7t{flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.p7row .p7a{color:var(--faint);font-size:12px;white-space:nowrap}
.p7row .p5mk{width:14px;height:14px;border-radius:4px;font-size:8px}
.p7toast{position:absolute;left:50%;bottom:20px;transform:translateX(-50%);border-radius:8px;background:var(--l2);box-shadow:var(--float);padding:8px 10px 8px 12px;display:flex;align-items:center;gap:10px;font-size:13.2px;white-space:nowrap}
"""


def cnt(p, n):
    return f'<span class="p7c">{mark(p, 14)}{n}</span>'


def folder(name, path, c=None, o=None, checked=True, tag=''):
    counts = ''.join(x for x in [cnt(CLAUDE, c) if c else '', cnt(CODEX, o) if o else ''])
    t = f'<span class="p7tag">{tag}</span>' if tag else ''
    ch = ' checked' if checked else ''
    return (f'<label class="p7r"><input type="checkbox"{ch} aria-label="{name}"><span class="p7n">{name} '
            f'<span class="p7p">{path}</span></span>{t}{counts}</label>')


def import_sheet(body, actions, title='Import sessions'):
    return ('<div class="p5scrim"></div>'
            f'<div class="p7sht" role="dialog" aria-label="{title}"><div class="p7hd"><b>{title}</b>'
            f'<button class="p5xb" aria-label="Close">{ic("x", 12)}</button></div>{body}'
            f'<div class="p7ac"><span class="p5sp"></span>{actions}</div></div>')


def settings_card(line, sub='Account, tools and defaults for this provider.'):
    return (f'<div style="position:absolute;inset:0;background:var(--l0);padding:28px 32px">'
            f'<div style="font-size:19.8px;font-weight:600;margin-bottom:4px">Claude Code</div>'
            f'<div style="font-size:14.3px;color:var(--muted);margin-bottom:16px">{sub}</div>'
            f'<div class="p5card"><div class="p5top">{mark(CLAUDE)}<div style="flex:1;min-width:0"><div style="font-weight:600;font-size:14.3px">Claude Code</div>'
            f'<div class="p5s" style="font-size:13.2px;color:var(--muted);display:flex;align-items:center;gap:6px">{dot("ok")}Connected · ada@example.com · Max</div></div>'
            f'{btn("Re-check", "txt")}{btn("Sign out", "sec")}</div>'
            f'<div style="box-shadow:inset 0 1px 0 var(--line);padding:10px 14px;display:flex;align-items:center;gap:6px;font-size:13.2px;color:var(--muted)">{ic("chevr", 12)}Details</div>'
            f'{line}</div></div>')


FOLDERS = [
    folder('harness', '~/Projects', c=83, tag='project'),
    folder('agentic-ui', '~/Projects', c=11, tag='project'),
    folder('cockpit', '~/Projects', c=19, tag='project'),
    folder('dharisanam', '~/Projects', c=9, tag='project'),
    folder('lakshya-v2', '~/Projects', c=862, checked=False),
    folder('exam', '~/Projects/lakshya-v2/platform/tools', c=744, checked=False),
    folder('art-src', '~/Projects/dharisanam-revival/storage', o=92, checked=False),
    folder('31 more folders', '', c=168, o=3, checked=False),
]


def b_import(src):
    OWL[0] = src.get('owl', '')
    W, H = 664, 640

    # 1 · Where the offer lives: the provider's own card in Settings.
    card_offer = settings_card(
        f'<div class="p7line">{ic("clock", 14)}<span>1,896 Claude Code sessions on this Mac, in 37 folders. '
        f'None are in Baaz yet.</span><span class="p5sp"></span>{btn("Import…", "sec")}</div>')
    card_done = settings_card(
        f'<div class="p7line">{ic("clock", 14)}<span>122 imported 30 Oct · 1,774 left on this Mac</span>'
        f'<span class="p5sp"></span>{btn("Import more…", "txt")}</div>')

    # 2 · First run: once a provider is Connected, one quiet line under the rows.
    first_run = connect([
        CONNECTED_CLAUDE,
        row(CODEX, 'Codex', 'ada@example.com · ChatGPT Pro', 'ok', '<span class="p5ok">' + ic('check', 13) + 'Connected</span>'),
        row(MUSE, 'Muse', 'Not installed', 'off', btn('Get Muse ' + ic('ext', 12), 'sec')),
    ], f'<div class="p7find">{ic("clock", 14)}<span>Found 1,991 earlier sessions on this Mac.</span>'
       f'<span class="p5sp"></span>{btn("Import…", "sec")}</div>' + foot((CLAUDE, 'Claude Code')))

    # 3 · Choose: folders, Baaz projects ticked.
    choose_body = (
        '<div class="p7in">Earlier Claude Code and Codex sessions, by the folder they ran in. Folders that are Baaz '
        'projects are ticked.</div>'
        '<div class="p7list"><div class="p7hdr"><span style="flex:1">Folder</span><span>Sessions</span></div>'
        + ''.join(FOLDERS) + '</div>'
        '<label class="p7opt"><input type="checkbox" checked aria-label="Skip scripted runs"><span>Skip scripted runs '
        '<span class="m">(claude -p, codex exec · 34)</span></span></label>'
        '<div class="p7fn">They stay where they are: nothing is copied or moved, and Claude Code and Codex keep using '
        'them. Continuing one in Baaz adds to the same session.</div>')
    choose = settings_card('') + import_sheet(choose_body, btn('Cancel', 'sec') + btn('Import 122 sessions', 'pri'))

    # 4 · Progress: the same sheet; Close never stops it.
    prog_body = ('<div class="p7big">Importing 48 of 122… <span class="m">harness</span></div>'
                 '<div class="p7bar"><i style="width:39%"></i></div>'
                 '<div class="p7fn">Reading each session’s first prompt and words for search. You can close this; '
                 'it keeps going, and the sessions appear in the sidebar as they land.</div>')
    progress = settings_card('') + import_sheet(prog_body, btn('Close', 'sec'))

    # 5 · Result.
    done_body = ('<div class="p7big">122 sessions imported into 4 projects. '
                 '<span class="m">2 couldn’t be read and were left out.</span></div>'
                 '<div class="p7fn"><span class="p5lnk">Show the 2</span> · Import more any time from Settings › Providers.</div>')
    result = settings_card('') + import_sheet(done_body, btn('Show in sidebar', 'sec') + btn('Done', 'pri'),
                                              title='Import finished')

    # 6 · The sidebar after: ordinary rows, in their projects, by when they last moved.
    def srow(p, t, a, sel=False):
        bg = 'background:rgba(255,255,255,.07);' if sel else ''
        return f'<div class="p7row" style="{bg}"><span class="p7t">{t}</span><span class="p7a">{a}</span>{mark(p, 14)}</div>'
    sidebar = ('<div class="p7sb">'
               f'<div class="p7g">{ic("chevd", 11)}harness</div>'
               + srow(MUSE, 'Close both tracks: P4-B design note', 'now')
               + srow(CLAUDE, 'Sidebar fluid round, FF values', '18 d')
               + srow(CLAUDE, 'Tier V probe for right pane', '38 d', sel=True)
               + srow(CLAUDE, 'Relay: L3 gate failing on pty', '40 d')
               + f'<div class="p7g" style="margin-top:6px">{ic("chevd", 11)}cockpit</div>'
               + srow(CLAUDE, 'Ledger double-counts a turn', '35 d')
               + srow(CLAUDE, 'Helicon accounting', '41 d')
               + '<div class="p7toast">' + ic('check', 13) + '<span>122 sessions imported</span>'
               + btn('Undo', 'txt') + '</div></div>')

    rows = [
        [mini(card_offer, W, 300, label='Where · Settings › Providers › the provider’s card',
              note='The offer lives on each provider’s own card, under Details: how many sessions this Mac has and how many are in Baaz. No modal, no banner, no badge in the sidebar. Codex’s card says the same for ~/.codex.'),
         mini(first_run, W, H, label='Where · first run, once a provider is Connected',
              note='One quiet line under the rows the moment Claude Code or Codex is Connected and has earlier sessions. Skip and Continue are unchanged; nothing asks again after first run.')],
        [mini(choose, W, H, label='Choose · folders',
              note='One sheet for both providers, one row per folder the sessions ran in, newest activity first. Folders that are Baaz projects are ticked; everything else is one click. Scripted runs (claude -p, codex exec) are skipped by default. The button counts what will happen.'),
         mini(progress, W, H, label='Import · progress',
              note='Same sheet. The bar counts sessions; the label names the folder in hand. Close never stops it. Sessions land in the sidebar as they go, so there is nothing to wait for.')],
        [mini(result, W, H, label='Result',
              note='What landed and where, and what didn’t (unreadable files are named, never a silent drop). Show in sidebar scrolls to the first imported row.'),
         mini(card_done, W, 300, label='Afterwards · the card',
              note='The line keeps the tally: when, how many, how many are left. “Import more…” reopens the sheet with the rest; already-imported sessions are never offered twice.')],
        [mini(sidebar, 320, 400, label='Afterwards · the sidebar',
              note='Imported sessions are ordinary rows: in their project, sorted by when they last moved, with the provider’s mark. No “imported” tag. A click resumes it like any session; search finds it by its words. The toast after an import offers Undo (removes the records; never touches the files).')],
    ]
    return grid_board('P7 · Import — earlier Claude Code and Codex sessions — PROPOSAL (2026-10-30, not built)',
                      'Owner decision 16 Oct: existing ~/.claude/projects and ~/.codex/sessions sessions become resumable Baaz sessions. '
                      'This Mac today: 1,896 Claude Code sessions in 37 folders (1,606 of them in lakshya-v2) and 95 Codex sessions in 2 folders (92 in dharisanam-revival/art-src). '
                      'Five concepts: the offer · folders · progress · result · ordinary rows. Questions for you are on the note to the right.',
                      rows, 2900)


if __name__ == "__main__":
    g.D6CSS += d5.CSS + CSS
    g.page6("P7Import", "P7 · Import (proposal)", b_import, 1440, 2900)
    print("wrote P7Import")
