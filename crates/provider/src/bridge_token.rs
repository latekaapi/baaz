//! The bridge's per-session token (docs/85 10.0, rule 20).
//!
//! Every agent reaches Baaz's socket through `mcp-bridge`, whose argv names
//! the session it speaks for. A session id on an argv is not a secret — any
//! process that can reach the socket and read `ps` could claim it — so each
//! session also gets a random token, minted in this process, handed to the
//! bridge **by environment** ([`ENV`], through each provider's MCP server
//! `env`), sent with every call, and checked by the socket service before the
//! call runs. A call with a wrong or missing token is refused.
//!
//! Tokens live only in this process's memory: a relaunch mints new ones, and
//! every agent child is relaunched with them anyway.

use std::collections::HashMap;
use std::io::Read;
use std::sync::{Mutex, OnceLock};

/// The environment variable the bridge reads its token from.
pub const ENV: &str = "BAAZ_BRIDGE_TOKEN";

fn tokens() -> &'static Mutex<HashMap<String, String>> {
    static TOKENS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    TOKENS.get_or_init(Default::default)
}

/// 32 random hex characters (16 bytes from the OS).
fn mint() -> String {
    let mut bytes = [0u8; 16];
    let filled = std::fs::File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes)).is_ok();
    if !filled {
        // No /dev/urandom: fall back to the std hasher's per-process random
        // keys, mixed with the time. Never a constant.
        use std::hash::{BuildHasher, Hasher};
        let state = std::collections::hash_map::RandomState::new();
        for chunk in bytes.chunks_mut(8) {
            let mut hasher = state.build_hasher();
            hasher.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
            chunk.copy_from_slice(&hasher.finish().to_le_bytes()[..chunk.len()]);
        }
    }
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The token for `session`: minted on first ask, the same for the life of
/// this process.
pub fn token_for(session: &str) -> String {
    let mut map = tokens().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    map.entry(session.to_owned()).or_insert_with(mint).clone()
}

/// Whether `token` is `session`'s. A session never minted here has no token,
/// so nothing verifies for it. The comparison takes the same time wherever
/// the strings differ.
pub fn verify(session: &str, token: Option<&str>) -> bool {
    let Some(token) = token else { return false };
    let map = tokens().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(expected) = map.get(session) else { return false };
    let (a, b) = (expected.as_bytes(), token.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_token_is_stable_per_session_and_distinct_between_sessions() {
        let a = token_for("tok-a");
        assert_eq!(a, token_for("tok-a"));
        assert_eq!(a.len(), 32);
        assert_ne!(a, token_for("tok-b"));
    }

    #[test]
    fn only_the_sessions_own_token_verifies() {
        let a = token_for("tok-c");
        let b = token_for("tok-d");
        assert!(verify("tok-c", Some(&a)));
        assert!(!verify("tok-c", Some(&b)), "another session's token");
        assert!(!verify("tok-c", None), "missing");
        assert!(!verify("tok-c", Some("")), "empty");
        assert!(!verify("never-minted", Some(&a)), "unknown session");
    }
}
