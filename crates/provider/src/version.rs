//! Version floors, shared by every adapter.
//!
//! Wherever a capability depends on the backend's version, the comparison is
//! a numeric **minimum**, never an exact string. One sentence for why: an
//! earlier implementation pinned a provider's CLI version with exact string
//! equality, so a routine upgrade on the owner's own machine made half the
//! features fail closed and no test caught it, because a shell stub stood in
//! for the real CLI.
//!
//! This lives in `provider` rather than in one adapter because a second
//! adapter needs the same rule and must not invent a second mechanism. The
//! shapes it must already survive are both real:
//!
//! * `muse --version`   → `1.3.0-R3401.1`      (a `-` trailer)
//! * `claude --version` → `2.1.276 (Claude Code)` (a whitespace trailer)

use std::cmp::Ordering;

/// Whether `version` is at least `floor`.
///
/// Newer than the floor is supported; older is not; unparseable fails
/// closed (below everything), because assuming support for a version nobody
/// can read is how the exact pin failed — loudly, in the wrong direction.
pub fn version_at_least(version: &str, floor: &str) -> bool {
    compare_versions(version, floor) != Ordering::Less
}

/// Numeric `major.minor.patch` comparison of `version` against `floor`.
///
/// A leading `v` is ignored; anything from the first `-`, `+` or whitespace
/// on is a trailer and ignored; missing components are zero (`"1.3"` is
/// `1.3.0`). Anything unparseable compares below everything: fail closed.
pub fn compare_versions(version: &str, floor: &str) -> Ordering {
    match (parse_version(version), parse_version(floor)) {
        (Some(version), Some(floor)) => version.cmp(&floor),
        (None, _) => Ordering::Less,
        (_, None) => Ordering::Greater,
    }
}

/// Parse `major.minor.patch` numerically. Returns `None` when a component
/// that is present is not a number at all.
pub fn parse_version(version: &str) -> Option<(u64, u64, u64)> {
    let version = version.trim();
    let version = version.strip_prefix('v').unwrap_or(version);
    // A trailer starts at the first `-`, `+`, or whitespace. Splitting on
    // whitespace is what lets `2.1.276 (Claude Code)` parse at all: without
    // it the patch component reads `276 (Claude` and fails closed, which
    // would refuse a version that is in fact far above every floor.
    let version = version
        .split(|c: char| c == '-' || c == '+' || c.is_whitespace())
        .next()
        .unwrap_or(version);
    let mut parts = version.split('.');
    let mut next = || match parts.next() {
        None | Some("") => Some(0),
        Some(part) => part.parse::<u64>().ok(),
    };
    Some((next()?, next()?, next()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_muse_style_dash_trailer_parses() {
        assert_eq!(parse_version("1.3.0-R3401.1"), Some((1, 3, 0)));
        assert!(version_at_least("1.3.0-R3401.1", "1.2.1"));
    }

    #[test]
    fn a_claude_style_whitespace_trailer_parses() {
        // The regression this module exists for: before whitespace was a
        // trailer boundary this returned None and failed closed, refusing a
        // version four majors above the floor.
        assert_eq!(parse_version("2.1.276 (Claude Code)"), Some((2, 1, 276)));
        assert!(version_at_least("2.1.276 (Claude Code)", "2.1.0"));
    }

    #[test]
    fn newer_than_the_floor_is_supported_and_older_is_not() {
        assert!(version_at_least("1.2.1", "1.2.1"));
        assert!(version_at_least("2.0.0", "1.2.1"));
        assert!(!version_at_least("1.2.0", "1.2.1"));
        assert!(!version_at_least("0.9.9", "1.2.1"));
    }

    #[test]
    fn missing_components_are_zero_and_a_leading_v_is_ignored() {
        assert_eq!(parse_version("1.3"), Some((1, 3, 0)));
        assert_eq!(parse_version("v2"), Some((2, 0, 0)));
    }

    #[test]
    fn unparseable_fails_closed_below_everything() {
        assert_eq!(parse_version("not-a-version"), None);
        assert!(!version_at_least("not-a-version", "0.0.0"));
        assert_eq!(compare_versions("", "0.0.0"), Ordering::Equal);
    }
}
