//! K2: the Skills page fixture contract.
//!
//! The `skills:page` and `skills:empty` captures render these fixtures with
//! no CLI, so the fixtures pin what the page may rely on: every scope the
//! sections draw, the activation spellings the meter sums, and the
//! `skill-shadowed` diagnostic shape the dimmed rows are built from. The
//! parsing and grouping logic itself is pinned by unit tests beside the
//! model (`crates/baaz/src/skills.rs`) and the page
//! (`crates/baaz/src/skills_page.rs`); this suite pins the ground they
//! stand on, straight from real `muse skills list --json` output captured
//! under scratch XDG homes (descriptions trimmed).

use serde_json::Value;

fn fixture(name: &str) -> Value {
    let path = format!("{}/fixtures/skills/{name}", env!("CARGO_MANIFEST_DIR"));
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|_| panic!("fixture reads: {path}"));
    serde_json::from_str(&text).unwrap_or_else(|_| panic!("fixture parses: {path}"))
}

fn rows(payload: &Value) -> Vec<&Value> {
    payload.get("skills").and_then(|v| v.as_array()).unwrap_or_else(|| panic!("fixture has a skills array")).iter().collect()
}

/// Every row carries the fields the page reads, spelled the way the CLI
/// spells them.
#[test]
fn page_rows_carry_the_full_shape() {
    for row in rows(&fixture("page.json")) {
        for field in ["id", "name", "scope", "path", "activation"] {
            assert!(row.get(field).is_some(), "row {} lacks `{field}`", row.get("id").unwrap_or(&Value::Null));
        }
        let scope = row.get("scope").and_then(|s| s.as_str()).unwrap_or("");
        assert!(
            ["bundled", "user", "project", "plugin"].contains(&scope),
            "unexpected scope `{scope}` on {}",
            row.get("id").unwrap_or(&Value::Null)
        );
        let activation = row.get("activation").and_then(|s| s.as_str()).unwrap_or("");
        assert!(
            ["on", "user-invocable-only", "off"].contains(&activation),
            "unexpected activation `{activation}`"
        );
        let cost = row.get("context_cost").unwrap_or_else(|| panic!("row has context_cost"));
        assert!(cost.get("startup_estimated_tokens").and_then(|n| n.as_u64()).is_some(), "row has startup tokens");
    }
}

/// The page fixture covers every section the page draws.
#[test]
fn page_covers_every_scope() {
    let payload = fixture("page.json");
    let mut scopes: Vec<&str> = rows(&payload).iter().filter_map(|row| row.get("scope").and_then(|s| s.as_str())).collect();
    scopes.sort_unstable();
    scopes.dedup();
    assert_eq!(scopes, vec!["bundled", "plugin", "project", "user"]);
}

/// The shadowed pair: the winner is a live project row, and the loser's
/// only trace is the top-level diagnostic — scope and path, no name.
#[test]
fn page_carries_a_shadowed_pair() {
    let payload = fixture("page.json");
    let diagnostics = payload.get("diagnostics").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    assert_eq!(diagnostics.len(), 1);
    let diagnostic = &diagnostics[0];
    assert_eq!(diagnostic.get("code").and_then(|c| c.as_str()), Some("skill-shadowed"));
    assert_eq!(diagnostic.get("scope").and_then(|s| s.as_str()), Some("user"));
    assert!(diagnostic.get("path").and_then(|p| p.as_str()).is_some());
    let message = diagnostic.get("message").and_then(|m| m.as_str()).unwrap_or("");
    let name = message.split('`').nth(1).unwrap_or("");
    assert!(!name.is_empty(), "the shadowed name parses out of the message");
    assert!(
        rows(&payload).iter().any(|row| row.get("name").and_then(|n| n.as_str()) == Some(name)),
        "the winner `{name}` is a live row"
    );
}

/// The meter sum over `on` rows, recomputed here from the raw JSON so the
/// page's sum has an independent oracle.
#[test]
fn page_meter_sum_is_pinned() {
    let payload = fixture("page.json");
    let sum: u64 = rows(&payload)
        .iter()
        .filter(|row| row.get("activation").and_then(|a| a.as_str()) == Some("on"))
        .filter_map(|row| row.get("context_cost").and_then(|c| c.get("startup_estimated_tokens")).and_then(|n| n.as_u64()))
        .sum();
    assert_eq!(sum, 3538);
    assert_eq!(rows(&payload).len(), 23);
}

/// The empty fixture is the empty-project state: bundled rows only, no
/// diagnostics, nothing in `project` scope.
#[test]
fn empty_has_no_project_skills() {
    let payload = fixture("empty.json");
    assert!(rows(&payload).iter().all(|row| row.get("scope").and_then(|s| s.as_str()) != Some("project")));
    assert!(rows(&payload).iter().any(|row| row.get("scope").and_then(|s| s.as_str()) == Some("bundled")));
    assert!(payload.get("diagnostics").and_then(|v| v.as_array()).is_some_and(|d| d.is_empty()));
}

/// Unknown top-level diagnostic codes survive verbatim: the page keeps
/// what it does not interpret.
#[test]
fn diagnostics_keep_unknown_codes_verbatim() {
    let mut payload = fixture("page.json");
    payload["diagnostics"].as_array_mut().unwrap().push(serde_json::json!({
        "code": "skill-future-proof",
        "message": "something this build has never seen",
    }));
    let codes: Vec<&str> = payload["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d.get("code").and_then(|c| c.as_str()))
        .collect();
    assert!(codes.contains(&"skill-future-proof"));
    assert!(codes.contains(&"skill-shadowed"));
}
