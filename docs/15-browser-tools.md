# The agent's browser tools

Baaz's browser tools drive the person's visible browser: every page the
agent opens there the person can watch. Declared in
`crates/mcp-bridge/src/browser.rs` beside the seven terminal tools — same
socket, same `{id, session, tool, params}` routing, same unavailable rule —
and served by `crates/baaz/src/terminal/service.rs` against the session's
own webview (`crates/baaz/src/browser.rs` registry), on the UI thread, one
pass per drain, so a slow page never stalls a frame.

Steering (advertised with the bridge's `instructions`): the person sees
this browser; use it for pages they should watch or for local dev servers.
Use the shell tool for quick captured checks. If a browser tool reports
that Baaz isn't running, the browser is unavailable — say so and carry on
without it.

## The tools (docs/27 §6.1, R2, as built)

Written in `crates/baaz/src/browser/automation/` as linear async jobs over
`aui_webview::PageAutomation` (CEF's in-process DevTools channel, no debugging port), polled by
the service's drain. Each takes `tab` and defaults to the session's current page; a tab belonging to
another session is refused.

| tool | what it does |
|---|---|
| `browser_tabs {op, tab?, url?, name?}` | list, open (hidden, background), close, activate, name this session's tabs |
| `browser_navigate {url}` | open a URL, wait for load; answers final url and title |
| `browser_snapshot {interactive?, scope?, delta?, max_chars?, cursor?}` | AX tree as text with `[ref=eN]`, same-process iframes stitched, fenced |
| `browser_act {action, ref or x,y, text?, key?, value?, to?}` | trusted click, type, fill, press, hover, select, scroll, drag; answers the settled diff and `effect` |
| `browser_read {format?, max_chars?}` | text, markdown or links |
| `browser_screenshot {full?, ref?, if_changed?}` | viewport, full page or element |
| `browser_wait {ms?, text?, selector?, idle?}` | wait |
| `browser_run {code}` | JS function body, 30 s; asks first on a page that isn't this machine's |
| `browser_fetch {url}` | GET with the page's cookies |
| `browser_console`, `browser_errors`, `browser_network {since?, ...}` | what the page said; `since: last_action`; `body_of` for a response |
| `browser_emulate {device?, width?, height?, dark?, offline?, reset?}` | viewport and media |
| `browser_upload {ref, files}` | always asks |
| `browser_ask {message, kind?}` | waits for the person's Done |

**Approvals (§6.2):** `policy.rs` classifies a click, an Enter, or a value going into a password or file
field, structurally, with no model: Send, Pay, Delete, Upload, sign-in. The tool waits on the line's card for
up to 600 s (the bridge's socket gives up at 660 s), and a "no" answers `stopped` with nothing done.

**Not built:** `browser_download`, `browser_history` (needs R4's db), cross-process iframes (out-of-process
frames need a flat DevTools session), per-site "Always here" levels, before/after screenshots in the transcript.

**Repeatable live check:** `scripts/agent-live-check.py`, on a real Chromium page, no model.
