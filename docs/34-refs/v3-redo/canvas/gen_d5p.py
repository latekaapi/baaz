#!/usr/bin/env python3
"""D5 · Providers — onboarding, setup and the provider card (PROPOSAL, 2026-10-24, not built).

Owner, 2026-10-23: "review, compare and improve our provider integrations … what happens on startup, lets say the
app is newly installed by a user — how do we check/login/start using claude, codex and muse on baaz. What screens
are visible to the user." Plan and evidence: harness docs/65. Owner answers (docs/65 §6): a visible setup sheet with
the command typed and not run; Codex installs with npm; API-key billing is said on the status line only; Muse links
to dev.meta.ai/lp/muse-code.

Five concepts, no more: a provider, its status line, its one next action, the setup sheet where that action happens,
and the default provider. Built from gen_d6's pieces (mini, grid_board, page6, the v3 tokens and buttons) so the
boards read like the rest of the canvas. The "Now" frame is a capture of the built screen (blob cap-connect-now).
Run: python3 gen_d5p.py   (writes project/D5Providers.dc.html + preview/D5Providers.html)."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import gen_d6 as g
from gen_d6 import ic, mini, grid_board, LIGHT

CLAUDE, CODEX, MUSE = ('C', '#D97757'), ('O', '#10A37F'), ('M', '#1F6FEB')
OWL = ['']  # the mascot's blob URL, set from the blobs at render time

CSS = """
.p5cx{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;background:var(--l0)}
.p5cx .p5col{width:440px;display:flex;flex-direction:column;align-items:center;gap:8px}
.p5cx .p5owl{width:72px;height:81px;object-fit:contain;margin-bottom:4px}
.p5cx h1{font-size:19.8px;line-height:26px;font-weight:600;margin:0}
.p5cx .p5in{font-size:14.3px;line-height:20px;color:var(--muted);text-align:center;margin:0 0 8px}
.p5rowsx{width:100%;border-radius:10px;background:var(--l1);box-shadow:var(--edge);overflow:hidden}
.p5rx{display:flex;align-items:center;gap:12px;padding:12px 14px;min-height:60px}
.p5rx+.p5rx{box-shadow:inset 0 1px 0 var(--line)}
.p5mk{width:24px;height:24px;border-radius:6px;color:#fff;font-size:12px;font-weight:700;display:flex;align-items:center;justify-content:center;flex:none}
.p5rx .p5t{flex:1;min-width:0}
.p5rx .p5n{font-size:14.3px;line-height:20px;font-weight:600}
.p5rx .p5s{font-size:13.2px;line-height:18px;color:var(--muted);display:flex;align-items:center;gap:6px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis}
.p5dt{width:6px;height:6px;border-radius:3px;flex:none}
.p5ok{color:var(--succ);display:inline-flex;align-items:center;gap:4px;font-size:13.2px}
.p5ft{width:100%;display:flex;align-items:center;gap:8px;margin-top:8px}
.p5ft .p5sp{flex:1}
.p5dflt{font-size:13.2px;color:var(--muted);display:flex;align-items:center;gap:6px;white-space:nowrap}
.p5pick{white-space:nowrap;height:24px;border-radius:6px;background:var(--l2h);box-shadow:var(--edge);padding:0 6px 0 4px;display:inline-flex;align-items:center;gap:6px;color:var(--fg);font-size:13.2px}
.p5pick .p5mk{width:16px;height:16px;border-radius:4px;font-size:9px}
.p5scrim{position:absolute;inset:0;background:rgba(0,0,0,.45)}
.p5sht{position:absolute;left:50%;top:50%;transform:translate(-50%,-50%);width:560px;border-radius:12px;background:var(--l1);box-shadow:var(--float);overflow:hidden}
.p5sht .p5hd{height:44px;display:flex;align-items:center;gap:10px;padding:0 8px 0 16px;box-shadow:inset 0 -1px 0 var(--line)}
.p5sht .p5hd b{font-size:14.3px;font-weight:600;flex:1}
.p5sht .p5st{display:flex;align-items:center;gap:8px;padding:12px 16px 0;font-size:13.2px;color:var(--muted)}
.p5sht .p5term{margin:10px 16px 0;border-radius:8px;background:var(--well);box-shadow:var(--edge);padding:10px 12px;height:150px;font-family:"Geist Mono",ui-monospace,Menlo,monospace;font-size:12.65px;line-height:19px;color:var(--fg);white-space:pre;overflow:hidden}
.p5term .p5pr{color:var(--faint)}
.p5term .p5cur{display:inline-block;width:7px;height:15px;background:var(--fg);vertical-align:-3px;opacity:.8}
.p5term .p5lk{color:var(--muted);text-decoration:underline}
.p5sht .p5fn{padding:10px 16px 0;font-size:13.2px;line-height:18px;color:var(--faint)}
.p5sht .p5ac{display:flex;align-items:center;gap:8px;padding:14px 16px 16px}
.p5sht .p5ac .p5sp{flex:1}
.p5spin3{width:12px;height:12px;border-radius:6px;border:1.5px solid rgba(127,127,127,.35);border-top-color:var(--muted);flex:none}
.p5xb{width:28px;height:28px;border:0;border-radius:6px;background:transparent;color:var(--muted);display:inline-flex;align-items:center;justify-content:center}
.p5card{width:100%;border-radius:10px;background:var(--l1);box-shadow:var(--edge)}
.p5card .p5top{display:flex;align-items:center;gap:12px;padding:14px}
.p5card .p5dl{box-shadow:inset 0 1px 0 var(--line);padding:6px 14px 12px}
.p5kv{display:grid;grid-template-columns:96px 1fr;gap:6px 12px;font-size:13.2px;line-height:18px;padding-top:8px}
.p5kv .p5k{color:var(--faint)}
.p5kv .p5v{color:var(--fg);min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.p5adv svg{flex:none;margin-top:2px}
.p5adv{margin:10px 0 0;border-radius:8px;background:var(--l2);box-shadow:var(--edge);padding:8px 10px;font-size:13.2px;line-height:18px;color:var(--muted);display:flex;gap:8px;align-items:flex-start}
.p5bn{margin:0 20px;border-radius:8px;background:var(--l2);box-shadow:var(--edge);display:flex;align-items:center;gap:10px;padding:8px 8px 8px 12px;font-size:13.2px}
.p5cmp{margin:10px 20px 0;border-radius:12px;background:var(--l2);box-shadow:var(--edge);height:96px;padding:12px 14px;color:var(--faint);font-size:14.85px}
.p5lnk{color:var(--muted);text-decoration:underline;text-underline-offset:2px}
"""


def mark(p, size=24):
    letter, colour = p
    r = 6 if size >= 20 else 4
    fs = 12 if size >= 20 else 9
    return f'<span class="p5mk" style="width:{size}px;height:{size}px;border-radius:{r}px;font-size:{fs}px;background:{colour}">{letter}</span>'


def dot(kind):
    colour = {'ok': 'var(--succ)', 'warn': 'var(--warn)', 'off': 'var(--faint)', 'bad': 'var(--dang)'}[kind]
    return f'<span class="p5dt" style="background:{colour}"></span>'


def btn(label, kind='sec', extra=''):
    return f'<button class="btn {kind}" style="height:28px;font-size:13.2px{extra}">{label}</button>'


def row(p, name, status, kind, action='', spin=False):
    d = '<span class="p5spin3"></span>' if spin else dot(kind)
    return (f'<div class="p5rx">{mark(p)}<div class="p5t"><div class="p5n">{name}</div><div class="p5s">{d}<span>{status}</span></div></div>'
            f'{action}</div>')


def connect(rows, foot, intro='Baaz runs the coding agents you already use. Connect at least one to start.', tally=''):
    t = f'<div class="p5dflt" style="margin-bottom:2px">{tally}</div>' if tally else ''
    return (f'<div class="p5cx"><div class="p5col"><img class="p5owl" src="{OWL[0]}" alt=""><h1>Connect your providers</h1>'
            f'<p class="p5in">{intro}</p>{t}<div class="p5rowsx">{"".join(rows)}</div>{foot}</div></div>')


def foot(default=None, cont=True):
    left = (f'<span class="p5dflt">New sessions start in <span class="p5pick">{mark(default[0], 16)}{default[1]}{ic("down", 12)}</span></span>'
            if default else '<span class="p5dflt" style="white-space:normal;max-width:230px;line-height:17px">Nothing is ready yet. Set up later in Settings › Providers.</span>')
    return (f'<div class="p5ft">{left}<span class="p5sp"></span>{btn("Skip for now", "txt")}'
            f'{btn("Continue", "pri" + ("" if cont else " dis"))}</div>')


CONNECTED_CLAUDE = row(CLAUDE, 'Claude Code', 'ada@example.com · Max', 'ok', '<span class="p5ok">' + ic('check', 13) + 'Connected</span>')


def b_connect_now():
    return ('<div style="position:absolute;inset:0;background:var(--l0);display:flex;align-items:center;justify-content:center">'
            '<img src="{cap}" alt="The connect screen as built" style="position:absolute;left:50%;top:50%;width:1440px;height:900px;max-width:none;transform:translate(-50%,-50%)"></div>')


def sheet(p, title, status, term, note, actions, spin=True):
    st_icon = '<span class="p5spin3"></span>' if spin else f'<span style="color:var(--succ);display:flex">{ic("check", 13)}</span>'
    return (f'<div class="p5sht" role="dialog" aria-label="{title}"><div class="p5hd">{mark(p, 20)}<b>{title}</b>'
            f'<button class="p5xb" aria-label="Close">{ic("x", 12)}</button></div>'
            f'<div class="p5st">{st_icon}<span>{status}</span></div>'
            f'<div class="p5term" role="log" aria-label="Terminal">{term}</div><div class="p5fn">{note}</div>'
            f'<div class="p5ac"><span class="p5sp"></span>{actions}</div></div>')


def under(inner):
    """A sheet over the connect screen, dimmed."""
    base = connect([row(CLAUDE, 'Claude Code', 'Not installed', 'off', btn('Install', 'pri')),
                    row(CODEX, 'Codex', 'Signed out', 'warn', btn('Sign in', 'pri')),
                    row(MUSE, 'Muse', 'Not installed', 'off', btn('Get Muse ' + ic('ext', 12), 'sec'))], foot(None, False))
    return base + '<div class="p5scrim"></div>' + inner


def b_providers(src):
    cap = src.get('cap-connect-now', '')
    OWL[0] = src.get('owl', '')
    now = b_connect_now().replace('{cap}', cap)
    W, H = 664, 520
    proposed = connect([
        CONNECTED_CLAUDE,
        row(CODEX, 'Codex', 'Signed out', 'warn', btn('Sign in', 'pri')),
        row(MUSE, 'Muse', 'Not installed', 'off', btn('Get Muse ' + ic('ext', 12), 'sec')),
    ], foot((CLAUDE, 'Claude Code')))
    nothing = connect([
        row(CLAUDE, 'Claude Code', 'Not installed', 'off', btn('Install', 'pri')),
        row(CODEX, 'Codex', 'Installed · signed out', 'warn', btn('Sign in', 'pri')),
        row(MUSE, 'Muse', 'Not installed', 'off', btn('Get Muse ' + ic('ext', 12), 'sec')),
    ], foot(None, False))
    checking = connect([
        row(CLAUDE, 'Claude Code', 'Checking…', 'off', spin=True),
        row(CODEX, 'Codex', 'Checking…', 'off', spin=True),
        row(MUSE, 'Muse', 'Checking…', 'off', spin=True),
    ], foot(None, False))
    too_old = connect([
        row(CLAUDE, 'Claude Code', '2.0.9 is older than Baaz supports (2.1.276)', 'warn', btn('Update', 'pri')),
        row(CODEX, 'Codex', 'ada@example.com · ChatGPT Pro', 'ok', '<span class="p5ok">' + ic('check', 13) + 'Connected</span>'),
        row(MUSE, 'Muse', 'Couldn’t check — the CLI timed out', 'bad', btn('Re-check', 'sec')),
    ], foot((CODEX, 'Codex')))

    install = under(sheet(CLAUDE, 'Install Claude Code', 'Waiting for the install to finish…',
                          '<span class="p5pr">~ %</span> curl -fsSL https://claude.ai/install.sh | bash<span class="p5cur"></span>',
                          'The command is typed, not run: read it, then press Enter. Baaz checks every few seconds and '
                          'shows Connected here when Claude Code answers.',
                          btn('Close', 'sec')))
    signin = under(sheet(CLAUDE, 'Sign in to Claude Code', 'Waiting for sign-in…',
                         '<span class="p5pr">~ %</span> claude auth login\nOpening your browser to sign in…\n\n'
                         'If it doesn’t open, visit:\n<span class="p5lk">https://claude.ai/oauth/authorize?code=true&amp;…</span>\n\n'
                         'Paste code here if prompted &gt; <span class="p5cur"></span>',
                         'Claude Code signs in through your browser. This terminal is Claude Code’s own; Baaz never sees your '
                         'credentials.', btn('Close', 'sec')))
    done = under(sheet(CLAUDE, 'Sign in to Claude Code', 'Connected · ada@example.com · Max',
                       '<span class="p5pr">~ %</span> claude auth login\nOpening your browser to sign in…\n'
                       'Login successful.\n<span class="p5pr">~ %</span> <span class="p5cur"></span>',
                       'Claude Code is ready. New sessions can start in it.', btn('Done', 'pri'), spin=False))

    codex_wait = connect([
        CONNECTED_CLAUDE,
        row(CODEX, 'Codex', 'Finish signing in with your browser…', 'warn', btn('Cancel', 'sec'), spin=True),
        row(MUSE, 'Muse', 'Not installed', 'off', btn('Get Muse ' + ic('ext', 12), 'sec')),
    ], '<div class="p5dflt" style="width:100%;margin-top:6px">Browser didn’t open? <span class="p5lnk">Copy the link</span> · <span class="p5lnk">Use a code instead</span></div>'
       + foot((CLAUDE, 'Claude Code')))
    muse_sheet = connect([CONNECTED_CLAUDE,
                          row(CODEX, 'Codex', 'Signed out', 'warn', btn('Sign in', 'pri')),
                          row(MUSE, 'Muse', 'Signed out', 'warn', btn('Sign in', 'pri'))], foot((CLAUDE, 'Claude Code'))) + \
        '<div class="p5scrim"></div>' + \
        (f'<div class="p5sht" role="dialog" aria-label="Sign in to Muse" style="width:460px"><div class="p5hd">{mark(MUSE, 20)}<b>Sign in to Muse</b>'
         f'<button class="p5xb" aria-label="Close">{ic("x", 12)}</button></div>'
         '<div style="padding:16px 16px 0;font-size:14.3px;line-height:21px">A Meta account draws on your Muse subscription. '
         '<span style="color:var(--muted)">An API key bills every turn to that key.</span></div>'
         f'<div class="p5ac" style="padding-top:18px"><span class="p5sp"></span>{btn("Use an API key", "sec")}{btn("Continue with Meta account", "pri")}</div></div>')

    def card(status, kind, details, adv=''):
        return (f'<div style="position:absolute;inset:0;background:var(--l0);padding:28px 32px">'
                f'<div style="font-size:19.8px;font-weight:600;margin-bottom:4px">Claude Code</div>'
                f'<div style="font-size:14.3px;color:var(--muted);margin-bottom:16px">Account, tools and defaults for this provider.</div>'
                f'<div class="p5card"><div class="p5top">{mark(CLAUDE)}<div style="flex:1;min-width:0"><div style="font-weight:600;font-size:14.3px">Claude Code</div>'
                f'<div class="p5s" style="font-size:13.2px;color:var(--muted);display:flex;align-items:center;gap:6px">{dot(kind)}{status}</div></div>'
                f'{btn("Re-check", "txt")}{btn("Sign out", "sec")}</div>'
                f'<div class="p5dl"><div style="display:flex;align-items:center;gap:6px;font-size:13.2px;color:var(--muted);padding-top:6px">{ic("chevd", 12)}Details</div>'
                f'<div class="p5kv">{details}</div>{adv}'
                f'<div style="display:flex;gap:8px;margin-top:12px">{btn("Change path…", "sec")}{btn("Copy diagnostics", "sec")}</div></div></div></div>')
    kv = ('<span class="p5k">Version</span><span class="p5v">2.1.276</span>'
          '<span class="p5k">Path</span><span class="p5v mono" style="font-size:12.65px">~/.local/bin/claude</span>'
          '<span class="p5k">Found</span><span class="p5v">on your login shell’s PATH (zsh)</span>'
          '<span class="p5k">Signed in</span><span class="p5v">claude.ai · ada@example.com · Max</span>')
    kv_key = kv.replace('claude.ai · ada@example.com · Max', 'API key from ANTHROPIC_API_KEY · billed per token')
    card_ok = card('Connected · ada@example.com · Max', 'ok', kv)
    card_key = card('Connected · API key · billed per token', 'warn', kv_key,
                    f'<div class="p5adv">{ic("warn", 13)}<span>An <span class="mono" style="font-size:12.65px">ANTHROPIC_API_KEY</span> in your shell signs Claude Code in: every turn is billed to that key, not a subscription. Remove it from your shell profile to use your Claude plan.</span></div>')

    banner = ('<div style="position:absolute;inset:0;background:var(--l1)">'
              '<div style="height:44px;display:flex;align-items:center;padding:0 16px;font-weight:600;box-shadow:inset 0 -1px 0 var(--line)">Refactor the probe cache</div>'
              '<div style="position:absolute;left:0;right:0;bottom:16px">'
              f'<div class="p5bn">{dot("warn")}<span style="flex:1">Claude Code is signed out. Sign in to send.</span>{btn("Sign in", "pri")}</div>'
              '<div class="p5cmp">Ask Claude Code…</div></div></div>')

    rows = [
        [mini(now, W, H, label='First run · now (built)', note='Capture of the built screen. The rows and actions are right; what is wrong is around them (docs/65 §1): Install and Sign in type into a terminal that is not on screen, Muse shows two Docs buttons, and nothing says which provider new sessions will use.'), mini(proposed, W, H, label='First run · proposed', note='Same three rows, one action each. Copy says what Baaz is. A Connected row is a check, not a button. Muse without an installer offers “Get Muse ↗” (dev.meta.ai/lp/muse-code). New in the footer: which provider new sessions start in — a menu of the Connected providers only, defaulting to the first Connected.'), ],
        [mini(nothing, W, H, label='Nothing ready yet', note='Continue stays off until one provider is Connected; Skip always works and says where to come back.'), mini(checking, W, H, label='Checking (first second)', note='Spinners, never “Not installed”, until each probe lands. Rows fill in one by one as they answer.'), ],
        [mini(too_old, W, H, label='Too old · couldn’t check', note='Below the version floor: the row says so and offers Update (the CLI’s own update command, in the setup sheet). A timeout reads “Couldn’t check”, never “Not installed”, with Re-check.'), mini(install, W, H, label='Setup sheet · Install', note='NEW. One sheet for every Install, Claude sign-in and Update: a real terminal (Baaz’s own) with the vendor command typed, not run. The status line at the top follows the re-probe every 3 s and turns into Connected by itself. Close never cancels what runs in it.'), ],
        [mini(signin, W, H, label='Setup sheet · Sign in to Claude Code', note='`claude auth login` in the same sheet. The browser link is the CLI’s own output; Baaz reads no credentials.'), mini(done, W, H, label='Setup sheet · done', note='The status line turns Connected the moment the probe sees it; Close becomes Done. The row behind it is Connected too.'), ],
        [mini(codex_wait, W, H, label='Codex · signing in (in the row)', note='Codex signs in over its own app-server: the row waits, its button is Cancel. Under the rows: “Copy the link” (the browser did not open) and “Use a code instead” (device code — for when the browser can’t reach localhost). Proposal: keep or cut the code option?'), mini(muse_sheet, W, H, label='Muse · sign-in sheet', note='Muse’s sign-in moves into the same sheet frame: a real ✕ and Escape, no second mascot, no “Codex, Claude and more to come”. Choices unchanged.'), ],
        [mini(card_ok, W, H, label='Settings › Providers › Claude Code', note='The card gains Details (closed by default): version, path, where it was found, how it is signed in. “Change path…” sets an override when Baaz finds the wrong binary; “Copy diagnostics” copies PATH source, every match, version and the raw status output.'), mini(card_key, W, H, label='API-key login (owner: say it, don’t block)', note='When Claude Code or Codex is signed in with an API key the status line says “API key · billed per token”, and the card explains where the key came from. No send guard (owner, 2026-10-23).'), ],
        [mini(banner, W, 300, label='Signed out mid-session (every provider)', note='One quiet banner above the composer for all three providers, from typed signals (Claude authentication_failed, Codex 401, Muse’s account change) or a re-probe. Sign in opens the setup sheet / Muse sheet / Codex flow.'), ],
    ]
    return grid_board('D5 · Providers — first run, setup and the provider card — PROPOSAL (2026-10-24, not built)',
                      'Owner 2026-10-23: review how a newly installed Baaz finds, installs and signs in to Claude Code, Codex and Muse; compare with the competitors; improve. Plan and evidence: docs/65. Detection fixes (S1–S6) are built already; these boards are the screens, for approval before any of them is built. Five concepts: provider · status line · one action · setup sheet · default provider.',
                      rows, 4200)


if __name__ == "__main__":
    g.D6CSS += CSS
    g.page6("D5Providers", "D5 · Providers (proposal)", b_providers, 1440, 4200)
    print("wrote D5Providers")
