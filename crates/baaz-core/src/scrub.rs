//! Obvious secrets taken out of text before it leaves the session it came
//! from (docs/85 12.3 `session_read`, 12.4 digests): API keys and tokens by
//! their well-known prefixes, private key blocks, `Authorization: Bearer`
//! values, and the value after a `password`/`secret`/`token`/`api_key`
//! style key. Each becomes `[secret removed]`.
//!
//! A heuristic, on purpose: it removes what is obviously a secret and
//! keeps everything else readable. It is not a guarantee, and nothing
//! relies on it as one.

/// What a removed secret reads as.
pub const REMOVED: &str = "[secret removed]";

/// Token prefixes that are secrets whatever follows (at least 16 more
/// characters of the token alphabet).
const PREFIXES: [&str; 14] = [
    "sk-ant-", "sk-proj-", "sk-", "ghp_", "gho_", "ghu_", "ghs_", "github_pat_", "glpat-", "xoxb-", "xoxp-", "AKIA", "AIza", "npm_",
];

/// Keys whose value is a secret (`key = value`, `key: value`, `"key": "value"`).
const SECRET_KEYS: [&str; 10] =
    ["password", "passwd", "secret", "token", "api_key", "apikey", "api-key", "access_key", "private_key", "client_secret"];

/// `text` with its obvious secrets replaced by [`REMOVED`].
pub fn scrub(text: &str) -> String {
    let text = scrub_key_blocks(text);
    let text = scrub_prefixed(&text);
    text.lines().map(scrub_line).collect::<Vec<_>>().join("\n") + if text.ends_with('\n') { "\n" } else { "" }
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '+' | '=')
}

/// `-----BEGIN … PRIVATE KEY-----` … `-----END … PRIVATE KEY-----`.
fn scrub_key_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("-----BEGIN") {
        let header_end = rest[start..].find("-----\n").or_else(|| rest[start + 10..].find("-----").map(|at| at + 10));
        let is_private = rest[start..].lines().next().is_some_and(|line| line.contains("PRIVATE KEY"));
        if !is_private || header_end.is_none() {
            out.push_str(&rest[..start + 10]);
            rest = &rest[start + 10..];
            continue;
        }
        out.push_str(&rest[..start]);
        match rest[start..].find("-----END") {
            Some(end_at) => {
                let after = &rest[start + end_at..];
                let close = after[8..].find("-----").map(|at| at + 8 + 5).unwrap_or(after.len());
                out.push_str(REMOVED);
                rest = &after[close..];
            }
            None => {
                out.push_str(REMOVED);
                rest = "";
            }
        }
    }
    out.push_str(rest);
    out
}

/// Well-known token prefixes anywhere in the text.
fn scrub_prefixed(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    'outer: while index < chars.len() {
        // A token starts at a word boundary.
        if index == 0 || !token_char(chars[index - 1]) {
            for prefix in PREFIXES {
                let prefix_chars: Vec<char> = prefix.chars().collect();
                if chars[index..].starts_with(&prefix_chars) {
                    let mut end = index + prefix_chars.len();
                    while end < chars.len() && token_char(chars[end]) {
                        end += 1;
                    }
                    if end - index - prefix_chars.len() >= 16 {
                        out.push_str(REMOVED);
                        index = end;
                        continue 'outer;
                    }
                }
            }
        }
        out.push(chars[index]);
        index += 1;
    }
    out
}

/// `Bearer <token>` and `<secret key> = <value>` on one line.
fn scrub_line(line: &str) -> String {
    let mut line = line.to_owned();
    if let Some(at) = line.to_ascii_lowercase().find("bearer ") {
        let value_start = at + 7;
        let value_end = line[value_start..].find(|c: char| !token_char(c)).map_or(line.len(), |end| value_start + end);
        if value_end - value_start >= 12 {
            line.replace_range(value_start..value_end, REMOVED);
        }
    }
    let lower = line.to_ascii_lowercase();
    for key in SECRET_KEYS {
        let Some(at) = lower.find(key) else { continue };
        // The key must be a word of its own (`token_count` is not a key).
        let after_key = at + key.len();
        if lower[after_key..].starts_with(|c: char| c.is_ascii_alphanumeric()) {
            continue;
        }
        let rest = &line[after_key..];
        let Some(sep) = rest.find([':', '=']) else { continue };
        if !rest[..sep].chars().all(|c| c == '"' || c == '\'' || c.is_whitespace()) {
            continue;
        }
        let value_start = after_key + sep + 1;
        let trimmed = line[value_start..].trim_start_matches([' ', '"', '\'']);
        let start = line.len() - trimmed.len();
        let end = trimmed.find(['"', '\'', ',', ' ', ';']).map_or(line.len(), |end| start + end);
        if end > start && end - start >= 4 {
            line.replace_range(start..end, REMOVED);
            return line;
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obvious_secrets_go_and_the_rest_stays() {
        let text = "export ANTHROPIC_API_KEY=sk-ant-api03-AbCdEfGhIjKlMnOpQrStUvWx\n\
                    curl -H 'Authorization: Bearer eyJhbGciOiJIUzI1NiJ9.payload' https://x\n\
                    password: hunter2hunter2\n\
                    {\"api_key\": \"abcd1234efgh5678\"}\n\
                    push with ghp_0123456789abcdefABCDEF0123456789\n\
                    the token_count was 12 and the secret sauce is love\n\
                    -----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAABG5vbmU=\n-----END OPENSSH PRIVATE KEY-----\n\
                    done\n";
        let out = scrub(text);
        for gone in ["sk-ant-api03", "eyJhbGci", "hunter2", "abcd1234efgh5678", "ghp_0123", "b3BlbnNzaC1"] {
            assert!(!out.contains(gone), "{gone} stayed:\n{out}");
        }
        assert!(out.contains("the token_count was 12"), "words that only look like keys stay:\n{out}");
        assert!(out.contains("curl -H 'Authorization: Bearer [secret removed]"), "{out}");
        assert!(out.ends_with("done\n"));
        assert_eq!(scrub("plain text, sk-short"), "plain text, sk-short", "short tails are not tokens");
    }
}
