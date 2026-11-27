# Plan 88: Codex through Baaz costs no more than bare Codex

Written 26 Nov 2026 with a Fable advisor. The advisor read the Stage 17 and fix-run rollouts, Codex's own logs, `strings` of the Codex binary, and the code. No model was run for this plan. The build session checked the two most important facts (marked ✔).

**Owner's requirement:** for the same task on the same model, a Codex session through Baaz must use no more input tokens than the bare `codex` CLI, and must not finish fewer tasks. "Worse than default is not acceptable."

---

## 1. Where it stands

The numbers below are summed from each rollout's per-request `token_count`, over 10 tasks each.

| | requests | input | cached | uncached | first request |
|---|---|---|---|---|---|
| Bare | 59 | 744,667 | 90.1% | 73,691 | 11,726 |
| Baaz, Stage 17 | 66 | 1,214,398 (+63%) | 84.7% | 185,918 | 17,364 |
| Baaz + `codex/model-at-start` (5cc8dd0b) | 60 | 802,517 (+7.8%) | 92.9% | **57,173 (−22%)** | 12,448 |

- **The model-switch fix already removes most of the gap.** That's commit 5cc8dd0b on branch `codex/model-at-start`, not merged. After it, uncached input is 22% *below* bare, and the cache rate is above bare. The cache-rate drop in Stage 17 came from the `<model_switch>` block sitting at the front of the developer message.
- **The remaining +7.8% is mostly fixed prompt prefix.** About 722 extra tokens × 60 requests ≈ 43k of the 58k gap. The rest is one extra request and trajectory noise.
- **Ten tasks cannot resolve a gap this small.** The paired per-task difference has an sd of about 17.8k, so the standard error is 7.6% of the bare mean at n=10 and 4.4% at n=30. Requests per task swing between 4 and 9 on both sides. First-request tokens are deterministic: use them as the low-noise signal.
- **The bench counts honestly.** Baaz's `turn` table equals the rollout sum (52,064 on task 01).

## 2. Every source of extra tokens

1. **`<model_switch>`, about 4.4k per request.** Fixed on `codex/model-at-start`: 0 blocks in 10 rollouts.
2. ✔ **The bench compared two Codex versions.** Bare runs used `/opt/homebrew/bin/codex` 0.144.6, first on the bench shell's PATH. Baaz runs used `~/.nvm/versions/node/v22.22.2/bin/codex` 0.153.2, first on the login PATH that `baaz_core::providers::resolve_program` uses. The base prompt, the skill-list format and the blocks differ between them, so every Stage 17 Codex comparison is across versions. (The build session confirmed this from `cli_version` in a bare and a Baaz smoke rollout.)
3. **The owner's plugin skills are still listed.** In the fix run, 6 plugin skills (about 580 tokens) were listed although Baaz sends `plugins."x".enabled=false`: that override does not remove skills on 0.153.2. In Stage 17, 22 plugin skills were listed. Those come from remotely installed plugins that are absent from `config.toml`, so `inherited_plugins()` never names them. The owner's plugin set also changed within the day, which makes a linked home unrepeatable for a bench.
4. **`<plugins_instructions>`, about 230 tokens.** Bare carries `<apps_instructions>` (about 150 tokens) instead.
5. **Baaz's note, 1,081 chars (about 245 tokens),** from `crates/provider/src/note.rs`.
6. **`<recommended_plugins>`, 786 chars.** Baaz has no handler for `request_plugin_install`, so these are dead weight.
7. **Five bundled system skills, about 2.5k chars.** Both sides carry them; not a Baaz cost.

**Checked and not a cost:**
- The bridge's MCP tools. The first request carries none, and the visible text explains the fix run's +701 to within about 125 tokens.
- The bridge's MCP instructions; they are not in the rollout.
- Title and byline side turns.
- `environment_context`, effort, summary and personality: the same on both sides.
- Compaction: none happened.

**Latency only:**
- The first request logs "websocket reuse properties didn't match". The prewarm used thread settings that `turn/start` then overrode.
- Every bench run took the restart path (0.3–1.7 s), because the bench's `setmodel:` saves no remembered pick.

## 3. A fair bench

- **Same binary on both sides.** Resolve it once the way Baaz does (`resolve_program`), pass it to Baaz as `BAAZ_CODEX` and use it as the bare argv. Read `cli_version` from both rollouts and fail the row on a mismatch.
- **Gating arm: "clean".** Neither side gets the owner's skill folders, plugins or `AGENTS.md`. The Baaz side gets a probe-only override of the owner's Codex home, pointing at a throwaway dir that holds only the `auth.json` link. This isolates what Baaz itself adds, and it is repeatable.
- **Reported arm: "installed" (not gating).** Bare gets copies of the owner's `config.toml` (minus `notify`), `skills/`, `plugins/` and `AGENTS.md`. Baaz links them as in production. This is what the owner actually pays day to day. Both sides carry the same personal skills, so the gap shows only Baaz's own additions plus any plugin leakage.
- **Report per arm:** input and uncached tokens per finished task, finished rate, requests per task, first-request tokens, median wall time, and a paired mean with its standard error.

## 4. What Baaz turns off or makes lazy

- **The owner's plugins are really off unless "use my own MCP servers" is on.** This is already the intent; make it true for plugin skills and remotely installed plugins.
- **Install suggestions off.** Baaz cannot act on them.
- **Bundled system skills stay on.** Hiding them only to win the bench would be a product change. If the gate still fails after tasks 3–5, ask the owner whether they should be off by default with switches on the Skills page (§0.5 stop).
- **MCP tools are already lazy.** Nothing to do.

Expected result after tasks 3–5: about 450 tokens per request *below* bare on the clean arm, roughly −3.5%.

---

## 5. Tasks (branch `plan/codex-parity` from `main`)

Run each task with docs/85-plan.md §0's loop:
- read the cited facts;
- do the task;
- prove its "done when";
- add a row to docs/85-status.md (a new "Codex parity" table, ids `C.<n>`);
- commit, then take the next task.

Rules: CLAUDE.md and docs/85-plan.md §1 apply. In particular:
- never `cargo fmt`, never `pre_exec`;
- Codex config only on argv (rule 26);
- tests never reach a real CLI: fixtures and scripted adapters only;
- live turns only in throwaway repos with throwaway homes that never touch `~/.codex`; never the API-key lane;
- never commit `Cargo.lock` while the patch is on; never `git commit -a`.

| ID | Task | Proof | Done when |
|---|---|---|---|
| C.0 | **Merge `codex/model-at-start`** (5cc8dd0b; merge-base 9b9e745b is on main): `git merge --no-ff`. Check rule 9 on its changes to `session.rs` and `session/runtime.rs`. Its session only reran some test modules after its rebase. Then remove its worktree (`git worktree remove ../harness-codexfix`; the uncommitted `.cargo/config.toml` and `Cargo.lock` copies there are disposable) and prune leftover units in `target/debug/deps`. | `cargo test --workspace` | Green on `plan/codex-parity`. |
| C.1 | **Bench fairness** (`scripts/bench/bench.py`): one resolved binary for both sides; the `cli_version` check; `--arm clean\|installed`; `--repeat N` (the run key gains the repeat); new columns (uncached, requests per task, first request); paired mean ± standard error in the report. | `scripts/bench/test_bench.py` over two checked-in trimmed rollouts and a results fixture | Reproduces 744,667 and 802,517 from fixtures; refuses a version mismatch. |
| C.2 | **Offline prompt probe** (`scripts/bench/prompt_probe.py`): render the model-visible prompt for a throwaway home plus a `-c` list, and print each block's size. Try `codex debug prompt-input` (seen in the 0.153.2 binary; not yet run). If it needs the network or doesn't exist, use one live turn per variant (count them) and read the rollout. | The probe's output table, saved | A table of block sizes for the baseline and each candidate key below, in `docs/85-evidence/codex-parity/`. |
| C.3 | **Owner plugins really off** (`provider-codex/src/terminal.rs`; `lib.rs` `spawn_args_for`). Use the key C.2 shows works. Candidates from the binary: `features.plugins`, `features.remote_plugin`, or one disable per directory under `plugins/cache`. Nothing changes when "use my own MCP servers" is on. | argv tests for both settings; a fixture home with a remotely installed plugin dir that is not in `config.toml` | The probe shows 0 plugin skills and no `<plugins_instructions>`. |
| C.4 | **Install suggestions off** (same files): `features.recommended_plugins` or `features.tool_suggest`, whichever C.2 confirms. | argv test | The probe shows no `<recommended_plugins>`. |
| C.5 | **Shorter note** (`provider/src/note.rs`): at most 500 chars with the bridge. Fix the "under 300 tokens" comment (rule 12). | A length-ceiling test; every tool the note names exists in the bridge's tool list; the stored-note test stays green, so old sessions keep their bytes | The probe shows the developer note ≤ 500 chars. |
| C.6 | **Settings at thread start** (`provider-codex/src/child.rs`, `lib.rs`): `thread/start` carries mode and effort, and `turn/start` sends an override only when it differs from what the thread already holds. This also fixes the prewarm socket mismatch. | Request-shape tests against fixtures; the existing sticky-effort tests stay green | An unchanged second turn's `turn/start` carries only `threadId`, `model`, `input`. |
| C.7 | **The bench measures the normal open path**: the Baaz side opens on a remembered model (or `setmodel` runs in the offline pass), not the restart path. | Fixture log-parse test | A bench run's `baaz.log` has 0 `restart on model` lines. |

C.2 comes before C.3 and C.4: the key names come from `strings`, not a live check. If no key removes the plugin skills, stop and ask (§0.5) before inventing a mechanism.

## 6. Gate

1. Standard gate on the branch:
   - Tier V before tests (no screen changes are expected, so no new findings);
   - `cargo test --workspace`;
   - clippy, workspace and `-p baaz --no-default-features`;
   - `scripts/flat-gate.py`.
2. Merge to `main` (`--no-ff`). There are no baselines unless Tier V shows an intended change; any baseline goes on main in its own commit.
3. **Bench, clean arm, gating:** 3 × 10 tasks on each side, with the same binary, model `gpt-5.6-luna` and effort medium; throwaway repos and homes; the subscription lane only. Build the check bundle from `main` after the merge. Check `HIDIdleTime` first; if the owner is at the machine, ask them to step away.

   **Pass when all of these hold:**
   - Baaz mean input tokens per finished task ≤ bare;
   - Baaz uncached tokens ≤ bare;
   - Baaz finished count ≥ bare;
   - Baaz first-request tokens < bare on every run;
   - 0 `<model_switch>` blocks and 0 version mismatches.

   If the mean passes by less than one standard error, say so plainly and point to first-request and per-request tokens.
4. **Bench, installed arm, reported:** 1 × 10 on each side.
5. **If the gate fails:**
   - Do not loosen it.
   - Write up what is left (block by block, from the probe table) and stop for the owner (§0.5).
6. Report in `docs/85-evidence/codex-parity/README.md` in the style of the s17 README. Update the s17 README with a note that its Codex comparison crossed versions. Update status, the tracker (a "Codex parity" row), the handoff and memory.

**Live-turn budget:** about 60 (clean arm) + 20 (installed arm) + up to 6 (probe fallback) = **about 86 Codex turns** on the owner's ChatGPT subscription. This is above the standing 20, so the owner sets it in the session prompt. No Claude or Muse turns are needed.

## 7. Risks
- The feature-key names are unverified until C.2.
- `features.plugins=false` may also drop something Baaz wants; the probe table will show it.
- The owner's default `codex` depends on the shell: 0.144.6 from Homebrew, 0.153.2 from nvm. Baaz uses the login-PATH one (0.153.2). Record the binary the gate used. Ask the owner whether the stale Homebrew copy should go; it is their machine, so never remove it unasked.
