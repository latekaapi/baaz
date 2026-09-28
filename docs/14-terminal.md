# The agent's terminal tools

Baaz's terminal tools drive the person's visible terminal: every command
the agent runs there the person can watch. Served on
`<support_dir>/run/terminal-<pid>.sock` (dir `0700`, socket `0600`) as
line-delimited `{id, session, tool, params}` requests; owned by
`crates/baaz/src/terminal/service.rs`, relayed as MCP by
`crates/mcp-bridge/src/terminal.rs`. Every route — Claude Code and Codex
via the per-session bridge, Muse via session MCP — names the same bridge,
so the tools and the tab rules do not change with the route.

Steering (advertised as the bridge's `instructions`): use the terminal
when the user says terminal, for long-running or interactive commands, or
anything the user should watch; use the shell tool for quick captured
checks. If a terminal tool reports that Baaz isn't running, the terminal
is unavailable — say so and carry on without it.

## The seven tools (§4 contract)

| tool | what it does |
|---|---|
| `terminal_list` | List this project's terminal tabs: id, title, owner, busy state. Read-only. |
| `terminal_open` | Open a new agent-owned tab. `cwd` may name any directory the user could open themselves; defaults to the project root. |
| `terminal_run` | Run a command where the person can watch. `tab`: auto (default), new, or an id. `wait: exit` waits (default 30 s, max 600 s; a timeout leaves the command running); `wait: none` returns at once. |
| `terminal_read` | Read a tab's output: a block, or what is new since a cursor. Cap `max_bytes` (default 4096, max 32768). Read-only. |
| `terminal_screen` | The tab's visible grid as text, for prompts and TUIs. Read-only. |
| `terminal_send` | Type into a tab: text and/or named keys. Person-owned tabs take only answers to a prompt of a command the agent ran there. |
| `terminal_close` | Close an agent-opened tab. Refused for tabs the person opened. |

Only session ids the app registered are served; anything else is refused.
When Baaz is not serving its socket every tool answers
`Baaz isn't running; the terminal is unavailable` rather than failing. A
served refusal (unknown session, a busy tab, a user-owned close) is a tool
error. Output is head+tail capped (4096 bytes for runs); new blocks the
agent started are marked agent-owned.
