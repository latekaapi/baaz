//! The test that keeps the seam honest: `provider` must not depend on
//! `muse-client`, `muse-adapter`, or any wire crate — directly or
//! transitively, in any dependency kind.
//!
//! The previous version parsed the crate's own `Cargo.toml` as text and only
//! recognised `key = value` lines under a header spelled exactly
//! `[dependencies]`, so `[dependencies.sneaky]` (dotted-table form) walked
//! straight past it, and `[dev-dependencies]` / `[build-dependencies]` were
//! never examined at all. This version asks `cargo metadata` for the
//! resolved graph instead: whatever TOML syntax declared a dependency, the
//! resolver normalises it, so there is no spelling the check cannot see.
//!
//! It asserts two things over that graph:
//!
//! - every dependency `provider` declares — normal, dev, and build — is in
//!   the neutral allowlist;
//! - no wire crate appears anywhere in `provider`'s transitive closure.
//!
//! Shelling out to `cargo metadata` from a test is a subprocess, not a
//! dependency: this crate gains no new dependency from the check.

use std::collections::{HashMap, HashSet};
use std::process::Command as ProcCommand;

/// The whole neutral vocabulary: the render model, identity, and the
/// channel primitive the transports already use. Nothing else.
const ALLOWED: &[&str] = &["aui-protocol", "crossbeam-channel"];

/// Every wire spelling that must never appear in this crate's graph.
const FORBIDDEN: &[&str] = &["muse-client", "muse-adapter", "msp"];

/// The resolved dependency graph of the workspace, via `cargo metadata`.
fn metadata_json() -> String {
    let manifest = format!("{}/Cargo.toml", env!("CARGO_MANIFEST_DIR"));
    let cargo = option_env!("CARGO").unwrap_or("cargo");
    let output = ProcCommand::new(cargo)
        .args(["metadata", "--format-version", "1", "--manifest-path", &manifest])
        .output()
        .expect("cargo metadata runs");
    assert!(
        output.status.success(),
        "cargo metadata failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).expect("cargo metadata emits UTF-8 JSON")
}

/// One `key: value` pair at the top level of a JSON object, with the raw
/// value slice. Enough of a parser for what this test needs; the values it
/// reads back out are compared as opaque strings, never interpreted.
fn top_level_pairs(object: &str) -> Vec<(&str, &str)> {
    let bytes = object.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0;
    // Skip the opening brace.
    assert!(bytes.first() == Some(&b'{'), "expected a JSON object");
    i += 1;
    while i < bytes.len() {
        // Skip whitespace, commas, and the closing brace.
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b'}' {
            break;
        }
        assert!(bytes[i] == b'"', "expected a string key");
        let (key, next) = scan_string(object, i);
        i = next;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        assert!(bytes[i] == b':', "expected a colon");
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let start = i;
        if bytes[i] == b'"' {
            i = scan_string(object, i).1;
        } else if bytes[i] == b'[' || bytes[i] == b'{' {
            i = match_bracket(object, i);
        } else {
            while i < bytes.len() && !matches!(bytes[i], b',' | b'}' | b']') {
                i += 1;
            }
        }
        pairs.push((key, object[start..i].trim()));
    }
    pairs
}

/// Scan a `"..."` string starting at the opening quote; returns the
/// unescaped contents and the index just past the closing quote.
fn scan_string(json: &str, start: usize) -> (&str, usize) {
    let bytes = json.as_bytes();
    assert!(bytes[start] == b'"', "expected an opening quote");
    let mut i = start + 1;
    while bytes[i] != b'"' {
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    // Raw contents, escapes and all: the values this test compares (names,
    // ids) never contain escapes in practice, and comparing the raw form
    // keeps the check fail-closed rather than mis-decoding.
    (&json[start + 1..i], i + 1)
}

/// Index just past the bracket matching the opener at `start`.
fn match_bracket(json: &str, start: usize) -> usize {
    let bytes = json.as_bytes();
    let (open, close) = (bytes[start], if bytes[start] == b'[' { b']' } else { b'}' });
    assert!(bytes[start] == open, "expected a bracket");
    let mut depth = 0;
    let mut i = start;
    let mut in_string = false;
    while i < bytes.len() {
        let byte = bytes[i];
        if in_string {
            if byte == b'\\' {
                i += 1;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == open {
            depth += 1;
        } else if byte == close {
            depth -= 1;
            if depth == 0 {
                return i + 1;
            }
        }
        i += 1;
    }
    panic!("unbalanced brackets");
}

/// Split a `[...]` array slice into its top-level element slices.
fn array_elements(array: &str) -> Vec<&str> {
    let array = array.trim();
    assert!(array.starts_with('['), "expected a JSON array");
    let mut out = Vec::new();
    let mut i = 1;
    let bytes = array.as_bytes();
    while i < bytes.len() {
        while i < bytes.len()
            && (bytes[i].is_ascii_whitespace() || bytes[i] == b',')
        {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] == b']' {
            break;
        }
        let start = i;
        if bytes[i] == b'"' {
            i = scan_string(array, i).1;
        } else if bytes[i] == b'[' || bytes[i] == b'{' {
            i = match_bracket(array, i);
        } else {
            while i < bytes.len() && !matches!(bytes[i], b',' | b']') {
                i += 1;
            }
        }
        out.push(array[start..i].trim());
    }
    out
}

/// Find `"key":` at the top level of `object` and return its raw value.
fn field<'a>(object: &'a str, key: &str) -> &'a str {
    top_level_pairs(object)
        .into_iter()
        .find(|(name, _)| *name == key)
        .unwrap_or_else(|| panic!("resolved graph has no `{key}` where expected"))
        .1
}

/// The `{"id": ..., "name": ...}` of every package, plus the raw
/// `dependencies` array of the one named `provider`.
fn packages(metadata: &str) -> (HashMap<&str, &str>, &str) {
    let start = metadata.find("\"packages\":").expect("metadata has packages") + 11;
    let end = match_bracket(metadata, metadata[start..].find('[').unwrap() + start);
    let mut ids = HashMap::new();
    let mut provider_deps = None;
    for entry in array_elements(&metadata[start..end]) {
        let name = field(entry, "name");
        let name = name.trim_matches('"');
        let id = field(entry, "id").trim_matches('"');
        ids.insert(id, name);
        if name == "provider" {
            provider_deps = Some(field(entry, "dependencies"));
        }
    }
    (ids, provider_deps.expect("the workspace resolves a package named `provider`"))
}

/// Adjacency of the resolved graph: package id to the package ids of its
/// dependencies, from the `resolve.nodes` table.
fn resolve_edges(metadata: &str) -> HashMap<&str, Vec<&str>> {
    let resolve_at = metadata.find("\"resolve\":").expect("metadata has a resolve table");
    let nodes_at = metadata[resolve_at..].find("\"nodes\":").expect("resolve has nodes") + resolve_at;
    let abs = nodes_at + metadata[nodes_at..].find('[').unwrap();
    let end = match_bracket(metadata, abs);
    let mut edges = HashMap::new();
    for node in array_elements(&metadata[abs..end]) {
        let id = field(node, "id").trim_matches('"');
        let mut pkgs = Vec::new();
        for dep in array_elements(field(node, "deps")) {
            pkgs.push(field(dep, "pkg").trim_matches('"'));
        }
        edges.insert(id, pkgs);
    }
    edges
}

#[test]
fn provider_depends_on_nothing_wire_shaped() {
    let metadata = metadata_json();
    let (names, provider_deps) = packages(&metadata);

    // Direct declarations — normal, dev, and build alike, whatever TOML
    // syntax declared them: the resolver normalised them all into this one
    // array, so dotted `[dependencies.x]` tables and kind-specific tables
    // cannot hide.
    let mut direct = Vec::new();
    for dep in array_elements(provider_deps) {
        let name = field(dep, "name").trim_matches('"');
        direct.push(name);
        assert!(
            ALLOWED.contains(&name),
            "provider depends on `{name}`, which is not in the neutral allowlist {ALLOWED:?}"
        );
        let rename = field(dep, "rename");
        if rename != "null" {
            let rename = rename.trim_matches('"');
            assert!(
                ALLOWED.contains(&rename),
                "provider renames a dependency to `{rename}`, which is not in the neutral allowlist {ALLOWED:?}"
            );
        }
    }
    assert!(!direct.is_empty(), "provider must depend on something");

    // Transitive closure from `provider`'s own node: no wire crate anywhere
    // downstream, however many hops away.
    let edges = resolve_edges(&metadata);
    let provider_id = names
        .iter()
        .find(|(_, name)| **name == "provider")
        .map(|(id, _)| *id)
        .expect("provider has a resolved node");
    assert!(edges.contains_key(provider_id), "provider has a resolved node");
    let mut seen = HashSet::new();
    let mut stack = vec![provider_id];
    while let Some(id) = stack.pop() {
        if !seen.insert(id) {
            continue;
        }
        if let Some(deps) = edges.get(id) {
            stack.extend(deps.iter());
        }
    }
    assert!(!seen.is_empty(), "provider's transitive closure is empty");
    let mut wire = Vec::new();
    for id in &seen {
        let name = names.get(id).unwrap_or_else(|| panic!("resolved node `{id}` has no package"));
        if FORBIDDEN.contains(name) {
            wire.push((*name, *id));
        }
    }
    assert!(
        wire.is_empty(),
        "wire crates in provider's transitive graph: {wire:?}"
    );
}
