//! The approval-mode matrix against a real `codex app-server`: four chip
//! modes × {set at open, switched mid-session}, one real turn per cell
//! (two for a mid-session cell), on the model `BAAZ_LIVE_MODES_MODEL`
//! names (pick the cheapest; `BAAZ_LIVE_MODES_LIST=1` prints the catalog
//! without spending).
//!
//! **Spends real turns.** `#[ignore]`d and gated on `BAAZ_LIVE_MODES=1`:
//!
//! ```text
//! BAAZ_LIVE_MODES=1 BAAZ_LIVE_MODES_MODEL=<id> \
//!   cargo test -p provider-codex --test live_modes -- --ignored --nocapture
//! ```
//!
//! Runs under a throwaway `BAAZ_STATE_DIR` (its own `codex-home`, linking
//! the owner's `auth.json` and `config.toml` the way Baaz does), never the
//! app's state dir.

#[path = "../../provider/tests/support/live_modes.rs"]
mod live_modes;
#[path = "../../provider/tests/support/provider_runner.rs"]
mod provider_runner;

use provider::{ConnectInfo, Provider};
use provider_codex::CodexAdapter;

#[test]
#[ignore = "spends real Codex turns; BAAZ_LIVE_MODES=1 to run"]
fn live_modes_codex() {
    if !live_modes::enabled() {
        eprintln!("BAAZ_LIVE_MODES is not 1: nothing spent");
        return;
    }
    let root = live_modes::run_root("codex");
    let state = root.join("state");
    std::fs::create_dir_all(&state).expect("state dir");
    std::env::set_var("BAAZ_STATE_DIR", &state);
    let owner = std::path::PathBuf::from(std::env::var_os("HOME").expect("HOME"));
    provider_codex::home::ensure_home(&owner, &state).expect("codex home");
    let program = std::env::var("BAAZ_CODEX").unwrap_or_else(|_| "codex".into());
    let make = || {
        let adapter = CodexAdapter::new(&program);
        adapter.set_state_dir(state.clone());
        let mut provider = Provider::new(adapter);
        provider.connect(&ConnectInfo::new("baaz", env!("CARGO_PKG_VERSION"))).expect("connect");
        provider
    };
    if std::env::var("BAAZ_LIVE_MODES_LIST").as_deref() == Ok("1") {
        let cell = live_modes::Cell {
            name: "catalog".into(),
            mode: aui_protocol::PermissionMode::DenyUnmatched,
            from: None,
            deny: false,
        };
        let paths = live_modes::prepare(&root, "codex", &cell);
        provider_runner::print_catalog(&mut make(), &paths.workspace);
        return;
    }
    let model = std::env::var("BAAZ_LIVE_MODES_MODEL").ok();
    let choices = provider_runner::Choices { allow: "accept", deny: "decline" };

    let mut results = Vec::new();
    for cell in live_modes::matrix() {
        let paths = live_modes::prepare(&root, "codex", &cell);
        let mut provider = make();
        eprintln!("[codex] running {}", cell.name);
        let outcome = provider_runner::run_cell(&mut provider, &cell, &paths, model.as_deref(), &choices);
        eprintln!("[codex] {} → {outcome:?}", cell.name);
        results.push((cell, outcome));
    }
    assert!(live_modes::report("codex", &results), "a Codex cell failed; see the table");
}
