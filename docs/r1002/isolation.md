# R1002: keeping Baaz's provider sessions out of Claude for Mac and Codex for Mac (2026-10-02)

This is a diagnosis with recommendations. No source was edited, no model turn was sent, and
nothing under `~/.claude`, `~/.codex`, `~/.config` or the Keychain was changed. I read files,
opened the SQLite stores read-only (`?mode=ro`), unpacked both apps' `app.asar` into the
scratchpad and grepped them, grepped the `claude` and `codex` binaries for strings, ran
`codex app-server generate-json-schema` (offline), and called the desktop app's read-only
`list_sessions`. Tags used below: **[measured]** means counted on this machine today.
**[source]** means read in a shipped bundle or binary. **[baaz]** means read in this repo.
**[guess]** means inferred and not yet checked.

Versions: `claude` CLI 2.1.276 (`~/.local/bin/claude`, the one Baaz spawns). Claude.app
2.16120.0, which bundles its own CLI 2.1.284. Codex for Mac is `/Applications/ChatGPT.app`
26.928.31416, which bundles codex 0.159.2. Baaz spawns codex 0.153.2 (nvm) for Codex.
Muse Code is 1.4.2.

---

## 0. Summary

| | Why Baaz sessions show up | Recommended fix | Resume, fork and handoff | Auth, skills and MCP |
|---|---|---|---|---|
| **Claude Code** | Baaz writes into the owner's `~/.claude` (`projects/`, `sessions/`). Worse, a Baaz started from inside a Claude desktop session **inherits that session's env**, including `CLAUDE_CODE_ENTRYPOINT=claude-desktop`, `CLAUDE_CODE_SESSION_ID` and the messaging socket. Its children then stamp transcripts as the desktop's own: 37 of the 38 Baaz Claude transcripts are stamped this way. | (1) **Scrub the inherited `CLAUDE*` env** from every child. (2) Give children their own `CLAUDE_CONFIG_DIR` under Baaz's Application Support folder, plus `CLAUDE_SECURESTORAGE_CONFIG_DIR=""` so they keep using the owner's existing Keychain login. Symlink `settings.json`, `skills/`, `plugins/`, `CLAUDE.md`, `agents/` and `commands/`. | Unchanged inside Baaz's dir. Existing sessions must be **moved** into it, or they can no longer be resumed. | The Keychain item stays shared. Settings, hooks, skills and plugins are reached through symlinks. User-scope MCP servers in `~/.claude.json` are lost, but Baaz runs `--strict-mcp-config` by default anyway. |
| **Codex** | Codex for Mac lists threads from `~/.codex/state_5.sqlite` using `thread/list` with the default "interactive" source kinds and **no originator filter** for the local host. Baaz's app-server threads are `source=vscode`, so every one of them is listed. | Give children their own `CODEX_HOME` under Baaz's Application Support folder. Symlink `config.toml`, `skills/`, `plugins/` and `AGENTS.md`. `auth.json` needs care (§3.2). | Unchanged inside Baaz's home. Existing threads must be moved (rollout files) to stay resumable. | Shared through symlinks. `auth.json` refresh-token rotation is the one real risk. |
| **Muse** | No GUI app reads Muse's store. The only place it shows is the terminal `muse resume` picker. | Nothing for now. Optionally use `muse serve` with a Baaz session dir, if Muse has a knob for it (not checked). | n/a | n/a |

Already-leaked sessions: **leave them where they are, and offer a one-time "Move Baaz
sessions into Baaz" step**. Moving is not optional for sessions the owner still wants to
resume (§4), so the offer doubles as the migration. Delete nothing.

---

## 1. Where each CLI persists, and how to tell Baaz's sessions apart

### 1.1 Claude Code

- **Transcripts** are stored as `$CLAUDE_CONFIG_DIR/projects/<cwd-slug>/<session-id>.jsonl`,
  where `$CLAUDE_CONFIG_DIR` defaults to `~/.claude` **[source]**. In the CLI,
  `Ee = (CLAUDE_CONFIG_DIR ?? ~/.claude)`. Baaz's `history.rs` hardcodes `~/.claude/projects`
  **[baaz]** (`crates/provider-claude-code/src/history.rs:49,118`).
- **Live registry**: every running CLI writes `$CLAUDE_CONFIG_DIR/sessions/<pid>.json`. The
  file holds `sessionId`, `kind`, `entrypoint`, `messagingSocketPath` and `peerProtocol`
  **[measured]**. Today it holds four Baaz children, for example
  `22838.json → sessionId 01a0ea0b-…, kind interactive, entrypoint claude-desktop`.
- **Global config** lives in `$CLAUDE_CONFIG_DIR/.claude.json`, or `~/.claude.json` when the
  variable is unset **[source]** (`bkn()`). It holds `oauthAccount`, user-scope `mcpServers`,
  `projects` trust and other caches.
- **Identifying fields**: each transcript line carries `entrypoint`, `version`, `cwd` and
  `sessionId`. Baaz mints UUIDv7 ids (`01a0…-7…`) and passes them as `--session-id`, while
  the desktop uses UUIDv4 `cliSessionId`s.

**Counts [measured]**: Baaz's own registry
(`~/Library/Application Support/baaz/provider-sessions.json`) lists **41 `claude-code`
sessions**. 38 of them have a transcript in `~/.claude/projects`, and their `entrypoint`
values are:

| entrypoint | count |
|---|---|
| `claude-desktop` | **37** |
| `sdk-cli` | 1 |

Across the whole store there are 62 UUIDv7 transcripts written by CLI 2.1.276: 52 stamped
`claude-desktop` and 10 stamped `sdk-cli`. The extra ones are Baaz probe and test runs that
never entered the registry. For scale, the store holds 1,323 transcripts in total, of which
1,297 are `claude-desktop`, 24 are `sdk-cli` and 2 are `cli`.

**Why `claude-desktop`?** `--print --output-format stream-json` stamps `sdk-cli` on its own.
The running Baaz (pid 22059) was started from a Claude desktop agent session, and `ps eww`
shows that it, its `claude` children (22838) and even its `codex app-server` (64416)
carry the desktop's env **[measured]**:

```
CLAUDE_CODE_ENTRYPOINT=claude-desktop
CLAUDE_CODE_SESSION_ID=56918b2c-…        (the desktop agent session that launched Baaz)
CLAUDE_CODE_HOST_SESSION_ID=local_d61c…
CLAUDE_CODE_MESSAGING_SOCKET / _TOKEN, CLAUDE_CODE_OAUTH_SCOPES, CLAUDE_CODE_CHILD_SESSION,
CLAUDE_CODE_SDK_HAS_HOST_AUTH_REFRESH, CLAUDECODE=1, CLAUDE_AGENT_SDK_VERSION, CLAUDE_PID,
ANTHROPIC_BASE_URL, … (27 CLAUDE*/ANTHROPIC* names in all)
```

`child.rs` sets only `PATH` and inherits everything else **[baaz]**
(`crates/provider-claude-code/src/child.rs:43-46`, `provider-codex/src/child.rs:871-874`).
This means Baaz's Claude sessions are written **as if they were the desktop's own Code-tab
sessions**, register as interactive desktop peers with messaging sockets, and inherit
another session's id and host-auth flags. This leak exists independently of the history
problem. It is also why most Baaz sessions are stamped `claude-desktop`: the owner's and
the agents' Baaz instances have mostly been launched from desktop agent sessions. A Baaz
launched from the Dock gets a clean env and stamps `sdk-cli`.

### 1.2 Codex

- **Rollouts** are stored as `$CODEX_HOME/sessions/YYYY/MM/DD/rollout-…-<thread-id>.jsonl`.
  The first line is `session_meta`, which carries `originator`, `source`, `cli_version`
  and `cwd`.
- **Index**: `$CODEX_HOME/state_5.sqlite`, table `threads`, with columns `source`,
  `originator`, `thread_source`, `archived` and `cwd` **[measured]**. Its location can also
  be moved on its own through `sqlite_home` / `CODEX_SQLITE_HOME` **[source]**.
- **Identifying fields**: Baaz's `initialize` sends `clientInfo.name = "baaz"`
  (`provider-codex/src/child.rs:212`) **[baaz]**. That name becomes the rollout `originator`.
  App-server threads get `source = "vscode"`.

**Counts [measured]** from rollout `session_meta`: 35 rollouts have `originator=baaz`, 19
have `baaz-probe`, and 1 is a `baaz-probe` subagent. Baaz's registry lists 18 `codex`
sessions, and 13 of them have rollouts. In `state_5.sqlite`, Baaz threads show
`source=vscode` with an **empty `originator` column**: 106 such rows, which also include
cockpit's. That means the Codex app cannot tell them apart from its own by the index alone.

---

## 2. How the two apps discover sessions

### 2.1 Claude for Mac

Claude.app reads the CLI store directly, always rooted at its own `claudeConfigDir`. That is
`~/.claude` unless the desktop itself is given `CLAUDE_CONFIG_DIR`. **[source]**
`index.chunk-BvFMb8a_.js`, `CliSessionDiscovery`.

- `listCliSessions()` drives the **sidebar** and `listResumableCliSessions()` drives the
  **resume picker**. Both enumerate `<claudeConfigDir>/projects/*/*.jsonl`, head-scan each
  file for `entrypoint` and `cwd`, take liveness from `<claudeConfigDir>/sessions/*.json`,
  and drop sessions the desktop already owns.
- The filter (`fpa` in `index.chunk-kXuYPTnM.js`) hides a transcript whose entrypoint is in:

  ```
  ["claude-desktop","claude-desktop-3p","local-agent","sdk-cli","sdk-ts","sdk-py","mcp",
   "bench","claude-code-github-action","remote",…,"ssh-remote"] + prefix "claude-coworker"
  ```

  The one exception is `admitEntrypoint`, which the **recovery/adopt** scan
  (`scanRecoverableCliSessions`) sets to the desktop's own entrypoint, `claude-desktop`.
  Plain `cli` (terminal) sessions are listed.
- The recovery scan admits `claude-desktop`-stamped transcripts whose ids the desktop does
  not know, which are exactly Baaz's leaked ones. It only does so for transcripts started
  before an "index known since" date. Its log shows `desktop-stamped transcripts re-admitted
  only if started before 2026-09-04`, and Baaz's are counted as `skippedNotLost` **[measured]**,
  `~/Library/Logs/Claude/main.log`. The cutoff is a date taken from the desktop's own index,
  so any index damage moves it and would import every Baaz session as a desktop session.
- The desktop's own index (`claude-code-sessions/…/local_*.json`, `ccd-ids.json`) contains
  **no Baaz ids** today, and `list_sessions` returns none **[measured]**.

**What I could not pin down.** According to the current bundle, neither `sdk-cli` nor
`claude-desktop` transcripts are listed in the sidebar's CLI section. So I could not find
the exact view where the owner saw them. Candidates are the resume picker, the running-peer
registry (`sessions/<pid>.json` with messaging sockets, which the desktop's cross-session
messaging can see), or an older or newer desktop build. **This doesn't change the fix:**
every discovery path in Claude.app is rooted at `<claudeConfigDir>`, and every one of them
keys on the `claude-desktop` stamp that Baaz is passing on. See question Q1.

### 2.2 Codex for Mac (ChatGPT.app)

Codex for Mac runs its own bundled app-server against `~/.codex`. It lists threads through
`thread/list` with `useStateDbOnly: true` **[source]**
(`webview/assets/app-shared-*.js`, `listRecentThreads`):

```
thread/list { archived:false, modelProviders:null, originators:<host hint>, sourceKinds:[] , useStateDbOnly:true }
```

- `sourceKinds: []` means "defaults to interactive sources" **[source, schema]**
  (`ThreadListParams.sourceKinds`). `exec` threads (754 of them, all `codex_exec`) stay out
  of the list. `vscode` threads, which include Baaz's, are listed.
- `originators` comes from `readThreadListOriginators()`. That hook returns
  `["codex_cloud"]` only for the cloud ("durable") host behind a flag, and `undefined` for
  local threads. **No originator filter is applied locally.**
- After listing, the client drops threads with `ephemeral === true` or
  `threadSource === "ambient_suggestions"` (`m2t`). The sidebar also drops
  `threadSource === "chatgpt_hidden"`. Both values are internal to OpenAI and reserved for
  its own use; §3.2 explains why not to borrow them.

---

## 3. Options and their costs

### 3.1 Claude Code

| Option | Hides from Claude.app | Resume and fork | Cost |
|---|---|---|---|
| **A. Scrub inherited env** (`CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_AGENT_SDK_*`, `CLAUDE_PID`, `CLAUDE_EFFORT`; also `ANTHROPIC_BASE_URL` when its value is the desktop's) | Partly. Transcripts revert to `sdk-cli`, which every desktop path filters out. The live `sessions/<pid>.json` peer entry stays in `~/.claude`. | Yes | None. **Needed regardless.** Without it, Baaz children also claim another session's `CLAUDE_CODE_SESSION_ID` and host-auth flags. |
| **B. `CLAUDE_CONFIG_DIR=<baaz>/claude-home`** + `CLAUDE_SECURESTORAGE_CONFIG_DIR=""` | **Fully.** The desktop never looks there. | Yes, as long as `history.rs` follows the same dir and old transcripts are moved (§4). | See below. |
| C. `--no-session-persistence` | Yes | **No.** Transcript writes are disabled, so `--resume` and Baaz's `ReadSession` have nothing to read. The flag is also `--print`-only. | Rejected. |
| D. `cleanupPeriodDays` | No | — | Unrelated. Only sets how long transcripts are kept. Note that it defaults to **30 days**, so Baaz's Claude history is already being culled by the owner's settings. There is also a separate retention ceiling for desktop-host-stamped transcripts, which the `claude-desktop` stamp may expose Baaz sessions to. **[source, guess on applicability]** |
| E. `--session-id` | No | — | Baaz already uses it. It sets the id, not where the session is stored. |

**Auth under option B [source]:** the Keychain service name is computed in the CLI as

```js
YI(n="") { e = env.CLAUDE_SECURESTORAGE_CONFIG_DIR;
  t = e !== undefined ? !e : !env.CLAUDE_CONFIG_DIR;          // no suffix when unset, or explicitly ""
  r = e !== undefined ? e : Ee();                              // Ee() = CLAUDE_CONFIG_DIR ?? ~/.claude
  c = t ? "" : "-" + sha256(r).hex.slice(0,8);
  return `Claude Code${OAUTH_FILE_SUFFIX}${n}${c}` }           // e.g. "Claude Code-credentials"
```

So setting only `CLAUDE_CONFIG_DIR` points the CLI at `Claude Code-credentials-<hash8>`, an
item that doesn't exist, and the child would look logged out. Adding
`CLAUDE_SECURESTORAGE_CONFIG_DIR=""` (empty, not unset) brings back the plain
`Claude Code-credentials`. That plain item is the only Claude Code Keychain item on this
machine **[measured]**, from a service-name-only listing: `Claude Code-credentials`,
`Claude Safe Storage`. The CLI's own error text even suggests "Use CLAUDE_CONFIG_DIR=/tmp
for ephemeral local writes". **[guess]** The new dir's `.claude.json` has no `oauthAccount`.
I expect the CLI to rebuild it from the token, but this is unverified. It needs one no-turn
check: `claude auth status` under the new env. Under my rules I could not run it.

**What lives in `~/.claude` today [measured], and what to do with each:**

| Entry | Under option B |
|---|---|
| `settings.json` (hooks, permissions, `autoMode`, theme) | **symlink**. The owner's hooks and permissions keep applying, and `providers.rs:483` already reads it. |
| `skills/`, `plugins/` (`installed_plugins.json` uses absolute paths, so links resolve) | **symlink** |
| `CLAUDE.md`, `agents/`, `commands/`, `keybindings.json` (none present today) | symlink if present |
| `projects/`, `sessions/`, `history.jsonl`, `file-history/`, `session-env/`, `shell-snapshots/`, `tasks/`, `plans/`, `todos/`, `backups/`, `cache/`, `telemetry/` | **keep private.** These are exactly the stores the desktop scans. |
| `~/.claude.json`: `oauthAccount`, user-scope `mcpServers`, `projects` trust | Not shared. Baaz's default `--strict-mcp-config` already excludes user MCP. The opt-in "your MCP servers too" mode would lose the user-scope servers in `~/.claude.json` but keep project `.mcp.json`. Trust prompts don't apply in `--print`. |

Do not symlink the whole `~/.claude` minus `projects`. The desktop also reads `sessions/`.

### 3.2 Codex

| Option | Hides from Codex app | Resume and fork | Cost |
|---|---|---|---|
| **A. `CODEX_HOME=<baaz>/codex-home`** | **Fully.** Sessions, state db and logs all move. | Yes, inside it. Old threads need their rollouts moved (§4). | Auth and config linking, below. |
| B. `-c sqlite_home=<baaz>` only | Today, yes: the app lists from the state db only. | Probably. **[guess]** Not verified whether `thread/resume` looks up the state db or scans `sessions/`. | Fragile. The rollouts stay in `~/.codex/sessions`, and the owner's app-server backfills its state db from that dir on a schema bump (`backfill_state`, `rollout_migration_state` tables). Baaz threads would come back. |
| C. `ephemeral: true` on `thread/start` / `thread/fork` (both accept it in 0.153.2) | Yes | **No.** Nothing is persisted, so a thread can't be resumed after the app-server exits, and Baaz relaunches with `thread/resume`. | Rejected. |
| D. `threadSource: "chatgpt_hidden"` or `"ambient_suggestions"` (`ThreadStartParams.threadSource` is a free string) | Yes, in the current app build | Yes | Borrows values reserved for OpenAI's internal use, which may carry other app behaviour and may change without notice. Not recommended. At most a stopgap. |
| E. Originator: `clientInfo.name` (already `baaz`) or `CODEX_INTERNAL_ORIGINATOR_OVERRIDE` (present in the binary) | **No.** The local list applies no originator filter (§2.2). | — | Useless for this problem. |

**Auth under option A.** `auth.json` (`cli_auth_credentials_store = "file"`, the default)
holds ChatGPT tokens, and the refresh token **rotates**. Copying the file creates two
holders of one rotating token. The first one to refresh can invalidate the other, which
would log the owner out of Codex for Mac. Use a **symlink** instead. I couldn't confirm
from strings whether codex rewrites `auth.json` in place or by temp-file-plus-rename. A
rename would quietly replace the symlink with a private copy and bring the rotation hazard
back. Baaz should therefore check before each spawn that `<home>/auth.json` is still a
symlink and re-link it if not. That check is cheap and catches both cases.
`CODEX_API_KEY` / `CODEX_ACCESS_TOKEN` env auth exists, but it would make Baaz hold
secrets. Not recommended.

**Other entries in `~/.codex` [measured]:**

| Entry | Under option A |
|---|---|
| `config.toml` (model providers incl. `meta` with a token script, `[projects]` trust, plugins, MCP servers, `[desktop]`) | **symlink**. Baaz already layers `-c` overrides on top. `provider-codex/src/terminal.rs:200-207` already honours `CODEX_HOME`. |
| `auth.json` | **symlink**, re-checked before each spawn (above) |
| `skills/`, `plugins/`, `AGENTS.md` (absent), `models_cache.json`, `installation_id` | symlink |
| `*.sqlite` (`state_5`, `memories_1`, `goals_1`, `logs_2`, `thread_history_1`, `queue_1`) | **never symlink.** SQLite places `-wal` and `-shm` next to the path it opened, so two processes reaching one db through a link and a real path can corrupt it. This loses Codex "memories" in Baaz sessions. See question Q3. |
| `sessions/`, `worktrees/`, `shell_snapshots/`, `log*` | private |

### 3.3 Muse

There is no Muse GUI app on this machine. `~/Library/Application Support/Muse` and
`~/.config/muse` exist, but only the terminal `muse resume` picker would show Baaz's Muse
sessions. That isn't the complaint, so leave Muse alone for now. The env scrub in §3.1-A
should still apply to the `muse serve` child.

---

## 4. Recommendation

**Do it in this order, as one change, because 2 and 3 break resume without 4:**

1. **Scrub the env for every child**: claude, codex app-server, muse serve, terminal shells
   and side sessions. Remove `CLAUDECODE`, `CLAUDE_CODE_*`, `CLAUDE_AGENT_SDK_*`,
   `CLAUDE_PID` and `CLAUDE_EFFORT`. Remove `ANTHROPIC_BASE_URL` only when Baaz itself was
   started with `CLAUDE_CODE_ENTRYPOINT` set (that is, under a desktop agent), so an
   owner-set value survives. This alone stops the `claude-desktop` stamping and the
   borrowed session id.
2. **Claude**: give children `CLAUDE_CONFIG_DIR=~/Library/Application Support/baaz/claude-home`
   and `CLAUDE_SECURESTORAGE_CONFIG_DIR=""`. Create the dir with symlinks to `settings.json`,
   `skills/` and `plugins/` (plus `CLAUDE.md`, `agents/` and `commands/` when they exist).
   Make `history.rs` (and anything else that builds `~/.claude/projects`) resolve through
   the same dir. Gate the change on one no-turn `claude auth status` under the new env
   **before** shipping.
3. **Codex**: give children `CODEX_HOME=~/Library/Application Support/baaz/codex-home`, with
   symlinks to `config.toml`, `auth.json` (re-checked before each spawn), `skills/` and
   `plugins/`. Never link the sqlite files. Gate on one no-turn app-server `initialize`
   plus `account/read`, or whatever Baaz's connect probe already does, under the new home.
4. **Migrate (move, never delete) Baaz's existing sessions**, keyed on Baaz's own registry
   (`provider-sessions.json`), not on heuristics:
   - Claude: move `~/.claude/projects/<slug>/<id>.jsonl`, plus the `<id>/` subdir if
     present, to `claude-home/projects/<slug>/`. That covers 38 files today. Without the
     move, `--resume <id>` fails under the new dir. Baaz's `claude-desktop`-stamped files
     stay stamped, which is harmless once they're outside `~/.claude`.
   - Codex: move the 13 registered rollouts into `codex-home/sessions/<same date path>`.
     The owner's `state_5.sqlite` then keeps **dangling rows** for them. Don't write to the
     owner's db. Either let the owner archive them in Codex for Mac, or (Q2) call
     `thread/archive` against an app-server running on the owner's `~/.codex`, which needs
     the owner's say-so because it writes their store. Whether `thread/resume` in the new
     home finds a moved rollout that has no state-db row is **unverified**. Check it with a
     `thread/read`-only probe on one moved thread before migrating the rest.
   - Leave unregistered probe and test leftovers alone: the 24 extra Claude UUIDv7 files and
     the 20 `baaz-probe` rollouts. Mention them in the migration dialog and let the owner
     decide.
   - Do this as an explicit, owner-confirmed step ("Move 38 Claude and 13 Codex sessions
     into Baaz"), with a dry-run list. Lazily moving each session on its first resume is
     the fallback.

What still won't be hidden after this: anything the owner opens in Baaz's *Terminal* pane
that runs `claude` or `codex` interactively. That uses the owner's own homes by design,
because Baaz's PTY env should keep the owner's real config. Scrub the inherited desktop
vars there too, but don't set the Baaz homes.

---

## 5. Questions for the owner

- **Q1.** Where exactly in Claude for Mac do you see them: the sidebar, the "resume" picker,
  or a running-sessions list? A screenshot would let us confirm the leak path. Per §2.1 the
  fix is the same either way, but it would tell us whether step 1 (env scrub) alone would
  have been enough.
- **Q2.** For Codex threads already in Codex for Mac's list, would you rather archive them
  yourself in that app, or have Baaz archive them (which writes to your `~/.codex` index
  once)?
- **Q3.** Is it acceptable that Codex sessions run from Baaz don't see your Codex
  "memories", and that Claude sessions run from Baaz don't see user-scope MCP servers from
  `~/.claude.json`? The alternative is reading them and re-passing them explicitly.
- **Q4.** Should unregistered Baaz probe and test leftovers (24 Claude files, 20 Codex
  rollouts) move too, or stay where they are?

## 6. Evidence index

- Env of the running Baaz and its children: `ps eww -o command= -p 22059 / 22838 / 64416`
  (names listed above, no values beyond entrypoint and session id).
- Claude store counts: python over `~/.claude/projects/*/*.jsonl`. Registry cross-reference:
  `~/Library/Application Support/baaz/provider-sessions.json`.
- Claude.app filter and discovery: unpacked `app.asar` → `.vite/build/index.chunk-BvFMb8a_.js`
  (`CliSessionDiscovery`, liveness registry `sessions/`), `index.chunk-kXuYPTnM.js`
  (`Ofa` exclusion set, `fpa`, `exr=["claude-desktop","claude-desktop-3p"]`),
  `index.chunk-Nnw-A1di.js` (`scanRecoverableCliSessions`, `admitEntrypoint`).
  Logs: `~/Library/Logs/Claude/main.log` `[CCD-SessionRecovery]`.
- Keychain naming: string `var $te="-credentials";function vb(){…}function YI(n=""){…}` in
  `~/.local/share/claude/versions/2.1.276`. Service names come from
  `security dump-keychain | grep svce` (names only).
- `--no-session-persistence` and `cleanupPeriodDays` semantics: the CLI's own help and error
  strings in the same binary.
- Codex index: `sqlite3 'file:~/.codex/state_5.sqlite?mode=ro'` (`threads` schema, source
  and originator tallies). Rollout `session_meta` originators: python over `~/.codex/sessions`.
- Codex app listing: unpacked ChatGPT.app `app.asar` →
  `webview/assets/app-shared-*.js` (`listRecentThreads`, `readThreadListOriginators`, `m2t`,
  `mP=[]`).
- Codex protocol: `codex app-server generate-json-schema` (0.153.2) → `v2/ThreadStartParams.json`
  (`ephemeral`, `threadSource: string`), `v2/ThreadListParams.json` (`sourceKinds` default
  interactive). `sqlite_home` / `CODEX_SQLITE_HOME` / `CODEX_INTERNAL_ORIGINATOR_OVERRIDE`:
  strings in the codex binary.
