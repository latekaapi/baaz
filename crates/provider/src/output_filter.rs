//! The shared tool-output filter (docs/85 12.2): one Rust function that
//! trims a long command's output before the model reads it.
//!
//! What it drops is what a person scrolls past in a test or build log —
//! passing test lines, compile and download lines, progress bars (a line
//! redrawn with `\r` keeps its last state) and runs of the same line —
//! and what it keeps is what they read: every line that names a failure,
//! an error or a warning, the head of the output, and its last
//! [`TAIL_LINES`] lines verbatim (where a runner prints its failures and
//! summary). Dropped runs leave one marker line saying how many lines went,
//! so the model knows the output was trimmed and can re-run narrower.
//!
//! Short output ([`MIN_LINES`]) and output the rules would barely shrink
//! ([`MIN_SAVING`]) come back untouched (`None`). The filter never rewrites
//! a command, so the command's exit status is the command's own.

/// Output shorter than this many lines is never filtered.
pub const MIN_LINES: usize = 40;

/// The last lines kept verbatim: where test runners print the failures.
pub const TAIL_LINES: usize = 40;

/// The first lines kept verbatim: what ran.
pub const HEAD_LINES: usize = 4;

/// The filter only answers when it saves at least this share of the bytes.
pub const MIN_SAVING: f64 = 0.2;

/// What the filter made of one output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Filtered {
    /// The trimmed text, with its marker lines.
    pub text: String,
    /// Lines in the input (after progress redraws were folded).
    pub lines_in: usize,
    /// Lines dropped.
    pub dropped: usize,
}

/// Trim `output`, or `None` when it is short or would barely shrink.
pub fn filter(output: &str) -> Option<Filtered> {
    let cleaned = strip_ansi(output);
    let lines: Vec<&str> = cleaned.lines().map(last_redraw).collect();
    if lines.len() < MIN_LINES {
        return None;
    }
    let tail_from = lines.len().saturating_sub(TAIL_LINES);
    let mut out: Vec<String> = Vec::new();
    let mut dropped = 0usize;
    let mut run_dropped = 0usize;
    let flush = |out: &mut Vec<String>, run: &mut usize| {
        if *run > 0 {
            out.push(format!("[… {} line{} of passing or progress output trimmed by Baaz]", run, if *run == 1 { "" } else { "s" }));
            *run = 0;
        }
    };
    let mut index = 0;
    while index < lines.len() {
        let line = lines[index];
        // A run of the same line folds into one, wherever it is.
        let mut repeat = 1;
        while index + repeat < lines.len() && lines[index + repeat] == line && !line.trim().is_empty() {
            repeat += 1;
        }
        let keep = index < HEAD_LINES || index >= tail_from || important(line) || !noise(line);
        if keep {
            flush(&mut out, &mut run_dropped);
            out.push(line.to_owned());
            if repeat > 1 {
                out.push(format!("[… the line above {} more time{}]", repeat - 1, if repeat == 2 { "" } else { "s" }));
                dropped += repeat - 1;
            }
        } else {
            run_dropped += repeat;
            dropped += repeat;
        }
        index += repeat;
    }
    flush(&mut out, &mut run_dropped);
    let text = out.join("\n") + if output.ends_with('\n') { "\n" } else { "" };
    let saving = 1.0 - text.len() as f64 / output.len().max(1) as f64;
    (dropped > 0 && saving >= MIN_SAVING).then_some(Filtered { text, lines_in: lines.len(), dropped })
}

/// A line that names a failure, an error or a warning: always kept.
fn important(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error", "fail", "panicked", "warning", "assert", "expected", "exception", "traceback", "not ok", "✖", "✗", "×",
        "fatal", "abort", "timed out", "segmentation", "left:", "right:", "---- ",
    ]
    .iter()
    .any(|word| lower.contains(word))
        || line.trim_start().starts_with("at ")
        || line.trim_start().starts_with("--> ")
}

/// A line a person scrolls past: a passing test, a compile, download or
/// install step, a progress bar, an empty line between them.
fn noise(line: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return true;
    }
    // Passing tests: cargo, node's runner, TAP, jest, pytest, go.
    if (trimmed.starts_with("test ") && trimmed.ends_with(" ... ok"))
        || trimmed.starts_with("✔ ")
        || trimmed.starts_with("✓ ")
        || trimmed.starts_with("ok ")
        || trimmed.starts_with("PASS ")
        || trimmed.starts_with("--- PASS")
        || trimmed.ends_with(" PASSED")
        || (trimmed.starts_with("ℹ ") && !trimmed.starts_with("ℹ fail"))
    {
        return true;
    }
    // Build and install steps.
    const STEPS: [&str; 12] = [
        "Compiling ", "Checking ", "Downloaded ", "Downloading ", "Fresh ", "Updating ", "Locking ", "Adding ",
        "Building ", "Blocking ", "Installing ", "Unpacking ",
    ];
    if STEPS.iter().any(|step| trimmed.starts_with(step)) {
        return true;
    }
    progress(trimmed)
}

/// A progress line: a bar (`[=====>    ]`, `####`, `▕██▏`) or a bare
/// percentage with counts.
fn progress(line: &str) -> bool {
    let bar = line.chars().filter(|c| matches!(c, '=' | '#' | '█' | '▉' | '▊' | '▋' | '▌' | '▍' | '▎' | '▏' | '░' | '▒' | '▓')).count();
    if bar >= 8 && bar * 2 >= line.chars().count() / 2 {
        return true;
    }
    let percent = line.find('%').is_some_and(|at| at > 0 && line[..at].chars().last().is_some_and(|c| c.is_ascii_digit()));
    percent && line.len() < 120 && (line.contains('/') || line.contains("ETA") || line.contains('|') || line.contains('['))
}

/// A redrawn line keeps only what was drawn last (`\r` overwrites).
fn last_redraw(line: &str) -> &str {
    let line = line.strip_suffix('\r').unwrap_or(line);
    line.rsplit('\r').next().unwrap_or(line)
}

/// `text` without ANSI escape sequences (colours, cursor moves).
pub fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // CSI: parameters, then one final byte in @..~.
                for next in chars.by_ref() {
                    if ('@'..='~').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                chars.next();
                // OSC: up to BEL or ESC \.
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/output-filter").join(name);
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
    }

    fn saving(input: &str, out: &Filtered) -> f64 {
        1.0 - out.text.len() as f64 / input.len() as f64
    }

    /// A real `cargo test` with 204 tests and one failure: at least half
    /// smaller, every failing line kept, the summary kept.
    #[test]
    fn a_cargo_test_log_halves_and_keeps_its_failure() {
        let input = fixture("cargo-test.log");
        let out = filter(&input).expect("filtered");
        eprintln!("cargo-test.log: {} → {} bytes", input.len(), out.text.len());
        assert!(saving(&input, &out) >= 0.5, "saved {:.0}%", saving(&input, &out) * 100.0);
        for kept in [
            "test tests::subtracts_wrongly ... FAILED",
            "assertion `left == right` failed: two minus three",
            "  left: -1",
            " right: 1",
            "test result: FAILED. 203 passed; 1 failed",
            "error: test failed, to rerun pass `--lib`",
        ] {
            assert!(out.text.contains(kept), "kept {kept:?}:\n{}", out.text);
        }
        assert!(!out.text.contains("test tests::case_050 ... ok"), "passing lines go");
        assert!(out.text.contains("trimmed by Baaz"), "and say they went");
    }

    /// A real `npm test` (node's runner) with 80 passing and one failing.
    #[test]
    fn an_npm_test_log_halves_and_keeps_its_failure() {
        let input = fixture("npm-test.log");
        let out = filter(&input).expect("filtered");
        eprintln!("npm-test.log: {} → {} bytes", input.len(), out.text.len());
        assert!(saving(&input, &out) >= 0.5, "saved {:.0}%", saving(&input, &out) * 100.0);
        for kept in ["✖ sum of prices", "AssertionError [ERR_ASSERTION]", "+ 3.3000000000000003", "ℹ fail 1", "> node --test"] {
            assert!(out.text.contains(kept), "kept {kept:?}:\n{}", out.text);
        }
        assert!(!out.text.contains("✔ chunk 40 "), "passing lines go");
    }

    /// A short build log with a compile error is left exactly as it is.
    #[test]
    fn a_short_log_is_untouched() {
        assert_eq!(filter(&fixture("cargo-build.log")), None);
    }

    #[test]
    fn progress_redraws_colours_and_repeats_fold() {
        let mut input = String::from("$ fetch\n");
        for i in 0..60 {
            input.push_str(&format!("\u{1b}[32mReceiving objects: {i}% ({i}/100)\r\n"));
        }
        for _ in 0..30 {
            input.push_str("warning: deprecated flag\n");
        }
        input.push_str("[==========>          ] 50%\rdone\n");
        let out = filter(&input).expect("filtered");
        assert!(!out.text.contains('\u{1b}'), "colours go");
        assert!(out.text.contains("warning: deprecated flag\n[… the line above 29 more times]"), "{}", out.text);
        assert!(out.text.contains("done"), "a redrawn line keeps its last state");
    }

    #[test]
    fn ansi_sequences_strip() {
        assert_eq!(strip_ansi("\u{1b}[1;31merror\u{1b}[0m: x \u{1b}]8;;http://a\u{7}link\u{1b}]8;;\u{7}"), "error: x link");
    }
}
