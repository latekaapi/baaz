# Brief — two small fixes after the API-key end-to-end test (Harness)

Repository `/Users/latekaapi/Projects/harness`, branch `main`, tree carries the uncommitted
sign-in-over-the-wire work: keep it, do NOT commit, do not touch agentic-ui or cockpit.
Prefix every shell command with `export PATH="/opt/homebrew/bin:$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
Spend rule as in `docs/briefs/muse-harness-login-wire.md`: never `turn/start`, never
`--send`, never `account/logout`, never `muse logout`, never a real key. The machine is
signed in on the API-key lane and your own run depends on it.

Verified so far on a signed-out machine: `--login-steps 'apikey;key-from-env:…;submit'`
walks choose → apikey → validating → success → `account → apiKey` and lands in the shell
with the footer "API key / Pay-as-you-go · API key". Two defects showed in that capture and
the next one:

1. **`--tier` must win over the lane.** `apply_account` in `crates/harness/src/app.rs`
   sets `self.tier = Some(Tier::PayAsYouGo)` for the key lanes unconditionally, so
   `--tier subscription` (documented in `main.rs` as applying to every mode; it is how a
   scripted capture gets past the pay-as-you-go guard) is overridden and the guard still
   blocks `--send`. Make the faked tier take precedence exactly as `probe_tier` already does
   (`if let Some(faked) = self.args.tier.clone() { … return }`), for both lanes.
2. **Avatar initial.** `Identity::initial` in `crates/harness/src/auth.rs` derives from
   `name`, which for the key lanes is the wire label ("stored key") → the footer shows "S"
   next to "API key". The initial must come from what the footer shows: "A" for both key
   lanes. Keep `name` as the wire label for the account lane. Add/adjust the unit test.

Gates: `cargo build --workspace`; `cargo test --workspace`;
`cargo clippy --workspace --all-targets -- -D warnings`;
`RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps`. Verify item 1 with
`cargo run -q -p harness -- --theme dark --tier subscription --screenshot-delay 12000 --screenshot /tmp/t.png`
(no `--send`): the footer must read the subscription label, not "Pay-as-you-go · API key",
and the avatar must show "A". Report the diff and gates verbatim.
