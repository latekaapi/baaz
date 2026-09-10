# Full-text search (`search.db` and the search palette)

The sidebar's filter is a subsequence match over session labels. The search
palette is the full-text complement: it matches transcript words Muse indexed
plus the files this workspace's turns actually created.

## Storage

`~/Library/Application Support/harness/search.db` (sqlite via the existing
`rusqlite` bundled build, which ships FTS5 — no new dependency), owned by
`crates/harness/src/search.rs`:

| table | contents | fed by |
|---|---|---|
| `sessions_fts(session_id UNINDEXED, label, title, first_prompt, body)` | every session Muse's index knows, with `body` = its `search_text` column (transcript text) | rebuilt off the UI thread at boot and after each index refresh, from `index::read()` plus the `sessions.json` name/derived-title overrides |
| `files_fts(path, session_id UNINDEXED, kind)` | "files we created": workspace-relative targets of write/edit tool calls | appended off the UI thread when a turn completes (the recorder, below) |

Muse's own `session-index.db` stays read-only and stays the freshness
source; `search.db` is a cache. A rebuild replaces the session half in one
transaction and never touches the files half, so recorded files survive it.
Opening the db runs a `USING fts5` smoke query first: a build without FTS5
treats search as unavailable rather than as "no matches".

Queries are FTS5 `MATCH` over alphanumeric tokens joined with `OR`, ordered
by `bm25`, capped at 12 rows per section. Punctuation is dropped, never
interpreted, so typing `foo(` cannot error the query. Every query runs on
`background_spawn` with a query epoch — only the latest keystroke's result
is applied. An empty query skips FTS: sessions come from the sidebar's
recent order, files from the most recently recorded rows.

## The palette (`PaletteKind::Search`)

Two sections: **Sessions** (the sidebar label plus a one-line `snippet()`
around the first match) and **Files** (path plus the owning session's label).
The snippet is cut from `clean_search_text`, not from Muse's raw
`search_text`: the `\x1f`-separated index envelope (session id, short id,
`valid`, the workspace path, `meta`, the model id) is stripped, whitespace
collapses, and the window holds ~90 chars around the first match. Matching
still runs on the raw body in FTS; only the shown row is cleaned. The query's
first hit in each label is emphasised through the row's own `matched` ranges;
the snippet itself stays the library's muted mono context (the row offers no
proportional-font or snippet-highlight shape). Enter or
click on a session resumes it through the same path as `/resume`; on a file
reveals it in Finder (`cx.reveal_path`), or toasts when it no longer exists.

Opened three ways: the sidebar header's search icon (`.on_search`), ⌘⇧F
(retargeted from the sidebar filter — the palette's empty state *is* the
quick-filter now: recent sessions plus recent files), and `/search` in the
composer menu and the ⌘K palette. Scripted as `--steps search:<query>`.

## What "files we created" means

Paths written by tool calls: `ToolKind::Write` and `ToolKind::Edit` targets
only. Reads, searches, shells, web fetches, sub-agents and unknown (MCP)
tools are never recorded — a path the agent only read is not a file we
created, and a tool this build has never seen is not guessed at. Absolute
targets are relativized against the session workspace; anything escaping
above it (`..`, or an absolute path outside the root) is dropped.

## Off-thread work this task moved (findings P3, P4)

- `history::write_all` goes through `store::write_atomic` (it was the one
  store write that did not). `history::read` runs once per opened session on
  the background executor (`SessionView::load_history`); `append` writes on
  the background executor and the cursor updates when the write lands.
- `files::walk` lowercases once at walk time (`FileEntry { path, lower }`).
  The `@` menu ranks on the background executor with the same
  epoch/latest-wins rule as the palette; the render path reads the last
  completed rank and never scans. An empty `@` filter is the head of walk
  order (a slice, not a rank) and stays synchronous.
