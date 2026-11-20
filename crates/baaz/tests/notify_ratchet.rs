//! Notify through owners (task 4.9b, audit D row 2): the ratchet.
//!
//! The audit counts `cx.notify()` as a proxy for "a state change goes
//! through the owner that knows what changed": the owner's mutation method
//! notifies once, callers stop notifying after they call it, and a view
//! redraws from its own entity rather than the root's. That proxy is easy
//! to game — move the call behind a helper and the count falls while
//! nothing changed — so these pins read source text and refuse every way of
//! hiding the call:
//!
//! - a `fn` whose whole body is a notify (`fn redraw(cx) { cx.notify() }`);
//! - a `macro_rules!` that expands to a notify;
//! - the other spellings of it: `Context::notify(…)`, a free `notify(cx)`,
//!   `.notify()` on anything but `cx`.
//!
//! And the count itself only falls: [`MAX_NOTIFIES`] is the ceiling, and a
//! change that lowers the count lowers it here too.

use std::path::{Path, PathBuf};

/// The ceiling on `cx.notify()` in `crates/baaz/src` (tests included, as
/// the audit counts them). Lower it when a change lowers the count; never
/// raise it to make room — give the new state an owner that notifies once.
const MAX_NOTIFIES: usize = 290;

fn src() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src")
}

/// Every `.rs` file under `src`, as `(relative path, text)`.
fn sources() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in std::fs::read_dir(dir).expect("src reads").flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }
    let mut files = Vec::new();
    walk(&src(), &mut files);
    files.sort();
    files
        .into_iter()
        .map(|path| {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            (path.strip_prefix(src()).expect("under src").display().to_string(), text)
        })
        .collect()
}

/// The text without `//` comments (doc comments included) and without the
/// contents of string literals: what the code says, not what its prose or
/// its messages mention.
fn code_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        let mut in_str = false;
        let mut escaped = false;
        let mut chars = line.chars().peekable();
        while let Some(c) = chars.next() {
            if in_str {
                if escaped {
                    escaped = false;
                } else if c == '\\' {
                    escaped = true;
                } else if c == '"' {
                    in_str = false;
                    out.push('"');
                }
                continue;
            }
            if c == '/' && chars.peek() == Some(&'/') {
                break;
            }
            if c == '"' {
                in_str = true;
            }
            out.push(c);
        }
        out.push('\n');
    }
    out
}

/// The body of the brace block opening at `open` (`text[open] == '{'`),
/// without its braces.
fn block(text: &str, open: usize) -> &str {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    for (at, byte) in bytes.iter().enumerate().skip(open) {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &text[open + 1..at];
                }
            }
            _ => {}
        }
    }
    &text[open + 1..]
}

/// Every `fn` in `text` whose body, whitespace aside, is one notify call —
/// whatever the receiver is called and with or without the `;`.
fn notify_only_fns(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("fn ") {
        let start = from + at;
        from = start + 3;
        // A real `fn` keyword: not the tail of an identifier.
        if start > 0 && text.as_bytes()[start - 1].is_ascii_alphanumeric() || start > 0 && text.as_bytes()[start - 1] == b'_' {
            continue;
        }
        let Some(open) = text[start..].find(['{', ';']).map(|at| start + at) else { break };
        if text.as_bytes()[open] == b';' {
            continue;
        }
        let body: String = block(text, open).chars().filter(|c| !c.is_whitespace()).collect();
        let body = body.trim_end_matches(';');
        if let Some(receiver) = body.strip_suffix(".notify()") {
            if !receiver.is_empty() && receiver.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.') {
                let name: String = text[start + 3..].chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
                found.push(name);
            }
        }
    }
    found
}

/// Every `macro_rules!` in `text` whose body notifies.
fn notifying_macros(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find("macro_rules!") {
        let start = from + at;
        from = start + 12;
        let Some(open) = text[start..].find('{').map(|at| start + at) else { break };
        if block(text, open).contains("notify") {
            let name: String =
                text[start + 12..].trim_start().chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect();
            found.push(name);
        }
    }
    found
}

/// The spellings of a notify that are not `cx.notify()`: `Context::notify`
/// (any path to it), a free `notify(cx)`, and `.notify()` on a receiver
/// other than `cx`.
fn other_spellings(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let squeezed: String = line.chars().filter(|c| !c.is_whitespace()).collect();
        if squeezed.contains("Context::notify") || squeezed.contains("Context::<") && squeezed.contains(">::notify") {
            found.push(format!("line {}: `Context::notify`", index + 1));
        }
        let mut from = 0;
        while let Some(at) = squeezed[from..].find("notify(") {
            let start = from + at;
            from = start + 7;
            let before = squeezed[..start].chars().last();
            match before {
                // `….notify()`: only `cx` may carry it.
                Some('.') => {
                    let receiver: String = squeezed[..start - 1]
                        .chars()
                        .rev()
                        .take_while(|c| c.is_alphanumeric() || *c == '_')
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect();
                    let args = &squeezed[start + 7..];
                    // `app.notify(entity_id)` and `cx.notify(id)` name another
                    // entity: that is App::notify, not a hidden self-notify.
                    if receiver != "cx" && args.starts_with(')') {
                        found.push(format!("line {}: `{receiver}.notify()`", index + 1));
                    }
                }
                // A free `notify(cx)`: a wrapper's call site.
                Some(c) if !(c.is_alphanumeric() || c == '_') && squeezed[start + 7..].starts_with("cx") => {
                    found.push(format!("line {}: `notify(cx)`", index + 1));
                }
                None if squeezed[start + 7..].starts_with("cx") => {
                    found.push(format!("line {}: `notify(cx)`", index + 1));
                }
                _ => {}
            }
        }
    }
    found
}

#[test]
fn no_function_is_a_notify_wrapper() {
    let mut offenders = Vec::new();
    for (path, text) in sources() {
        for name in notify_only_fns(&code_only(&text)) {
            offenders.push(format!("{path}: fn {name}"));
        }
    }
    assert!(
        offenders.is_empty(),
        "a fn whose body is only a notify hides the call instead of giving the change an owner: {offenders:#?}"
    );
}

#[test]
fn no_macro_expands_to_a_notify() {
    let mut offenders = Vec::new();
    for (path, text) in sources() {
        for name in notifying_macros(&code_only(&text)) {
            offenders.push(format!("{path}: macro_rules! {name}"));
        }
    }
    assert!(offenders.is_empty(), "a macro that notifies hides the call: {offenders:#?}");
}

#[test]
fn notify_has_one_spelling() {
    let mut offenders = Vec::new();
    for (path, text) in sources() {
        for found in other_spellings(&code_only(&text)) {
            offenders.push(format!("{path}: {found}"));
        }
    }
    assert!(offenders.is_empty(), "spell a redraw `cx.notify()`, in the owner that changed: {offenders:#?}");
}

#[test]
fn the_notify_count_only_falls() {
    let count: usize = sources().iter().map(|(_, text)| text.matches("cx.notify()").count()).sum();
    assert!(
        count <= MAX_NOTIFIES,
        "cx.notify() rose to {count} (ceiling {MAX_NOTIFIES}): give the new state an owner that notifies once"
    );
}

/// The detectors themselves, on planted code.
#[test]
fn the_detectors_catch_planted_wrappers() {
    let planted = code_only(
        "impl A {\n    fn redraw(cx: &mut Context<Self>) { cx.notify() }\n    fn n(&mut self, c: &mut App) {\n        c.notify();\n    }\n    fn real(&mut self, cx: &mut Context<Self>) { self.x = 1; cx.notify(); }\n}\n\
         macro_rules! bump { ($cx:expr) => { $cx.notify() }; }\n\
         fn f(cx: &mut Context<A>) { Context::notify(cx); notify(cx); this.notify(); app.notify(id); }\n",
    );
    assert_eq!(notify_only_fns(&planted), vec!["redraw".to_owned(), "n".to_owned()]);
    assert_eq!(notifying_macros(&planted), vec!["bump".to_owned()]);
    let spellings = other_spellings(&planted);
    assert!(spellings.iter().any(|s| s.contains("Context::notify")), "{spellings:?}");
    assert!(spellings.iter().any(|s| s.contains("notify(cx)")), "{spellings:?}");
    assert!(spellings.iter().any(|s| s.contains("this.notify()")), "{spellings:?}");
    assert!(!spellings.iter().any(|s| s.contains("app.notify")), "App::notify(id) names another entity: {spellings:?}");
    // A string or a comment that says it is not code.
    assert!(other_spellings(&code_only("let s = \"Context::notify\"; // notify(cx)\n")).is_empty());
}
