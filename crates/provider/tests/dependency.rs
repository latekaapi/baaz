//! The test that keeps the seam honest: `provider` must not depend on
//! `muse-client`, `muse-adapter`, or any wire crate.
//!
//! Without a mechanical check this rots in a week, and a comment is not a
//! check. It reads the crate's own `Cargo.toml` dependency table — embedded
//! at compile time, so no runner, no `cargo metadata`, no new dependency —
//! and enforces an allowlist, which is stronger than forbidding two names:
//! anything new must justify itself here.

/// The whole neutral vocabulary: the render model, identity, and the
/// channel primitive the transports already use. Nothing else.
const ALLOWED: &[&str] = &["aui-protocol", "crossbeam-channel"];

/// Every wire spelling that must never appear in this crate's manifest.
const FORBIDDEN: &[&str] = &["muse-client", "muse-adapter", "msp"];

/// The `[dependencies]` table of this crate's own manifest.
fn dependency_names(manifest: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut in_deps = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_deps = line == "[dependencies]";
            continue;
        }
        if !in_deps || line.is_empty() || line.starts_with('#') {
            continue;
        }
        let name = line.split(['=', ' ']).next().unwrap_or("").trim();
        if !name.is_empty() {
            names.push(name.to_owned());
        }
    }
    names
}

#[test]
fn provider_depends_on_nothing_wire_shaped() {
    let manifest = include_str!("../Cargo.toml");

    for spelling in FORBIDDEN {
        assert!(
            !manifest.contains(spelling),
            "provider's Cargo.toml mentions forbidden wire crate `{spelling}`"
        );
    }

    let names = dependency_names(manifest);
    assert!(!names.is_empty(), "provider must depend on something");
    for name in &names {
        assert!(
            ALLOWED.contains(&name.as_str()),
            "provider depends on `{name}`, which is not in the neutral allowlist {ALLOWED:?}"
        );
    }
}
