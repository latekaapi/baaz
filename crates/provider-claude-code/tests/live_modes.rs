//! The approval-mode matrix against a real `claude` child: four chip modes
//! × {set at open, switched mid-session}, one real turn per cell (two for a
//! mid-session cell), on the cheapest model.
//!
//! **Spends real turns.** `#[ignore]`d and gated on `BAAZ_LIVE_MODES=1`:
//!
//! ```text
//! BAAZ_LIVE_MODES=1 cargo test -p provider-claude-code --test live_modes -- --ignored --nocapture
//! BAAZ_LIVE_MODES_CELLS=ask-mid,full-mid …   # rerun only these cells
//! ```
//!
//! Runs under a throwaway `BAAZ_STATE_DIR` (its own `claude-home`, sharing
//! the owner's Keychain login), never the app's state dir.

#[path = "../../provider/tests/support/live_modes.rs"]
mod live_modes;
#[path = "../../provider/tests/support/provider_runner.rs"]
mod provider_runner;

use provider::{ConnectInfo, Provider};
use provider_claude_code::ClaudeCodeAdapter;

#[test]
#[ignore = "spends real Claude Code turns; BAAZ_LIVE_MODES=1 to run"]
fn live_modes_claude_code() {
    if !live_modes::enabled() {
        eprintln!("BAAZ_LIVE_MODES is not 1: nothing spent");
        return;
    }
    let root = live_modes::run_root("claude-code");
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::env::set_var("BAAZ_STATE_DIR", &state);
    provider::child_env::capture_startup_entrypoint();
    let owner = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    let home = provider_claude_code::home::ensure_home(&owner, &state).expect("claude home");
    let program = std::env::var("BAAZ_CLAUDE").unwrap_or_else(|_| "claude".into());
    let model = std::env::var("BAAZ_LIVE_MODES_MODEL").unwrap_or_else(|_| "haiku".into());
    let choices = provider_runner::Choices { allow: "allow", deny: "deny" };

    let mut results = Vec::new();
    for cell in live_modes::matrix() {
        let paths = live_modes::prepare(&root, "claude-code", &cell);
        let adapter = ClaudeCodeAdapter::new(&program);
        adapter.set_config_dir(home.clone());
        let mut provider = Provider::new(adapter);
        provider.connect(&ConnectInfo::new("baaz", env!("CARGO_PKG_VERSION"))).expect("connect");
        eprintln!("[claude-code] running {}", cell.name);
        let outcome = provider_runner::run_cell(&mut provider, &cell, &paths, Some(&model), &choices);
        eprintln!("[claude-code] {} → {outcome:?}", cell.name);
        results.push((cell, outcome));
    }
    assert!(live_modes::report("claude-code", &results), "a Claude Code cell failed; see the table");
}
