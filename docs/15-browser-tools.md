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

## The six tools

| tool | what it does |
|---|---|
| `browser_open {url}` | Open a URL in the visible browser: opens the Browser pane when closed, navigates, waits for load up to 15 s, answers `{url, title}`. |
| `browser_read {max_chars?=8000}` | The visible page as text: `{title, url, text}`. Read-only. |
| `browser_links {max?=100}` | The visible page's links as `{links: [{href, text}]}`. Read-only. |
| `browser_click {selector}` | Click the first match of a CSS selector; answers the element's text. |
| `browser_type {selector, text}` | Type text into the first match of a CSS selector; answers the text set. |
| `browser_screenshot {}` | The visible page as a PNG, returned as an MCP image content block (base64). Read-only. |

Each tool acts on the calling session's own webview — never another
session's. URLs are limited to `http`, `https`, `file` and `about:blank`;
anything else (`javascript:`, `data:`, …) is refused before any webview
sees it. A result that does not arrive within 15 s is the error
`the page did not answer`. Opening the pane from a tool updates the
session's saved right-pane state (Browser, open) like a person's click
would, without moving the keyboard.
