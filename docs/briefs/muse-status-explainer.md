# Brief — a one-page HTML explainer of the project's state (no code changes)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, clean tree. Your only write is
the new file `docs/status/explainer.html`. Do not commit; do not touch source files,
`/Users/latekaapi/Projects/agentic-ui` or `~/Projects/cockpit`. Prefix every shell command with
`export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`. Never run anything
that starts a model turn (no `--send`, `send:`/`steer:`, `turn/start`, live tests).

Audience: the project owner, reading on a phone, who has not followed the last two days in
detail and wants to know in plain words **what is done, what is pending, and what only they
can do**. Not a changelog: short sentences, no jargon without a one-line gloss, no commit
hashes in the prose (a small "receipts" table at the end may list them).

Sources to read, in this order: `docs/CHANGELOG.md` (the two newest entries: 2026-09-12
"Code review and performance pass" and 2026-09-11 "Sign-in over the wire"),
`docs/audit/01-plan.md` (status table at the end), `docs/diagnosis/login.md` §7 (test log),
`docs/09-handoff-improvements.md` (only its "what only the owner can verify" part, if
present), `docs/05-handoff.md`.

Content, in this order, each a short section:

1. **Where things stand** — three sentences.
2. **Done** — the sign-in rework (two ways to sign in: Meta account with a browser code,
   or an API key; the API-key way was tested end to end with a real reply), and the
   review/performance pass (what a reader gains: fewer bugs, smaller files, faster
   rendering — quote the before/after numbers from the changelog table as a tiny table,
   explained in one line each: "element construction" = the work the app does per frame
   before drawing; "frame" = the whole draw; "idle frames" = frames drawn while nothing
   changes, which should be zero).
3. **Pending, and who owns it** — a checklist with an owner tag on every item:
   - *Owner*: approve the Meta-account sign-in once in the browser (a code appears in the app;
     it lives 10 minutes) so the subscription lane is verified; then send one short message so
     the app's first turn after that login is confirmed and the last legacy fallback can be
     deleted.
   - *Owner*: merge the library branch stack `improvements-2026-09-10` → `login-methods` →
     `audit-2026-09-12` into the library's `main` (the app builds against the checkout, so
     nothing is broken meanwhile, but the work is not on `main`).
   - *Owner*: decide the API-key budget going forward (the implementation of the audit ran on
     their key at their instruction; say only that, no numbers).
   - *Later work*: the two library polish items left open (terminal/TUI cursor blinks without a
     focus gate; the caret samples at frame rate), and the automations slice.
4. **How to check it yourself** — three commands, each with one line on what to expect:
   `cargo run -p harness -- --replay fixtures/msp/transcript-approve.jsonl --theme dark`;
   `cargo run -p harness -- --no-connect --login choose --screenshot /tmp/login.png`;
   `./target/debug/harness --bench fixtures/msp/synthetic-stress-300.jsonl`.
5. **Receipts** — a compact table: package → what → commit (from `docs/audit/01-plan.md`'s
   status table and the changelog).

Form: one self-contained HTML file, no external resources, no JavaScript needed; system
font stack; readable at 380 px wide and at desktop width (max content width ~720 px,
centred); light and dark via `prefers-color-scheme` with colours defined once as CSS
variables; generous line height; the checklist rendered as real `<ul>` with an owner tag
styled as a small pill. Keep it under 20 KB. Validate by opening it with
`python3 -c "import html.parser"`-level sanity (well-formed tags) and by reading it back.
Report the file size and the section list.
