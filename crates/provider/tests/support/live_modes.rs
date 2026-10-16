//! The live approval-mode matrix: the cells, the probe turn, and the
//! verdict — shared by each provider's `live_modes` test through
//! `#[path]`, so all three backends are judged by one rule.
//!
//! Std plus `aui_protocol` only: `muse-client` includes this file too, and
//! it does not depend on the `provider` crate.
//!
//! **Every cell spends a real model turn** (two for a mid-session cell).
//! The tests that include this are `#[ignore]`d and also refuse to run
//! unless `BAAZ_LIVE_MODES=1`, so neither CI nor a stray `--ignored` run
//! ever spends. `BAAZ_LIVE_MODES_CELLS=ask-open,full-mid` reruns only the
//! named cells.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use aui_protocol::PermissionMode;

/// The four chip modes, in chip order.
pub const MODES: [PermissionMode; 4] = [
    PermissionMode::AllowAll,
    PermissionMode::OnRequest,
    PermissionMode::PromptUnmatched,
    PermissionMode::DenyUnmatched,
];

/// One cell of the matrix.
#[derive(Clone, Debug)]
pub struct Cell {
    /// `full-open`, `ask-mid`, …
    pub name: String,
    /// The mode the probe turn runs under.
    pub mode: PermissionMode,
    /// For a mid-session cell: the mode the session opened in and ran one
    /// trivial turn under before the switch.
    pub from: Option<PermissionMode>,
    /// Answer the first approval with a denial (the one cell per provider
    /// that proves denial works); every other approval is allowed so the
    /// write can be seen to happen.
    pub deny: bool,
}

/// A short slug for a mode, for cell names.
pub fn slug(mode: PermissionMode) -> &'static str {
    match mode {
        PermissionMode::AllowAll => "full",
        PermissionMode::OnRequest => "auto",
        PermissionMode::PromptUnmatched => "ask",
        PermissionMode::DenyUnmatched => "readonly",
    }
}

/// The mode a mid-session cell for `to` starts in. Chosen to cross the
/// interesting seams: into Full access out of a launched Read-only (two
/// relaunch paths at once), down out of Full access live, Auto → Ask, and
/// Ask → Read-only live.
pub fn mid_from(to: PermissionMode) -> PermissionMode {
    match to {
        PermissionMode::AllowAll => PermissionMode::DenyUnmatched,
        PermissionMode::OnRequest => PermissionMode::AllowAll,
        PermissionMode::PromptUnmatched => PermissionMode::OnRequest,
        PermissionMode::DenyUnmatched => PermissionMode::PromptUnmatched,
    }
}

/// The full matrix: four modes × {set at open, switched mid-session}, with
/// the denial proof on `ask-mid`. Filtered by `BAAZ_LIVE_MODES_CELLS`.
pub fn matrix() -> Vec<Cell> {
    let mut cells = Vec::new();
    for mode in MODES {
        cells.push(Cell { name: format!("{}-open", slug(mode)), mode, from: None, deny: false });
        cells.push(Cell {
            name: format!("{}-mid", slug(mode)),
            mode,
            from: Some(mid_from(mode)),
            deny: mode == PermissionMode::PromptUnmatched,
        });
    }
    match std::env::var("BAAZ_LIVE_MODES_CELLS") {
        Ok(list) if !list.trim().is_empty() => {
            let wanted: Vec<&str> = list.split(',').map(str::trim).collect();
            cells.into_iter().filter(|cell| wanted.contains(&cell.name.as_str())).collect()
        }
        _ => cells,
    }
}

/// Whether the live matrix may run (it spends real turns).
pub fn enabled() -> bool {
    std::env::var("BAAZ_LIVE_MODES").as_deref() == Ok("1")
}

/// Where a provider's cells write: a fresh git workspace per cell under
/// `root`, plus one shared "outside" directory that is in no workspace and
/// not under `/tmp` (Codex's workspace-write sandbox allows `/tmp` and
/// `$TMPDIR` by default, so a `/tmp` target cannot tell Auto from Full).
pub struct Paths {
    pub workspace: PathBuf,
    pub inside: PathBuf,
    pub outside: PathBuf,
}

/// The run's scratch root: `BAAZ_LIVE_MODES_DIR`, else a fresh dir under
/// the system temp dir.
pub fn run_root(provider: &str) -> PathBuf {
    let base = std::env::var_os("BAAZ_LIVE_MODES_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = base.join(format!("baaz-live-modes-{provider}-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("run root");
    root.canonicalize().expect("run root canonicalizes")
}

/// The outside-write directory: under `$HOME`, so no sandbox's default
/// writable roots cover it.
pub fn outside_dir() -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).expect("HOME is set");
    let dir = home.join(".baaz-mode-probe-out");
    std::fs::create_dir_all(&dir).expect("outside dir");
    dir
}

/// A fresh `git init`ed workspace for `cell`, with both targets cleared.
pub fn prepare(root: &Path, provider: &str, cell: &Cell) -> Paths {
    let workspace = root.join(&cell.name);
    let _ = std::fs::remove_dir_all(&workspace);
    std::fs::create_dir_all(&workspace).expect("cell workspace");
    let status = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&workspace)
        .status()
        .expect("git runs");
    assert!(status.success(), "git init in {}", workspace.display());
    std::fs::write(workspace.join("README.md"), "probe workspace\n").expect("seed file");
    let inside = workspace.join("inside.txt");
    let outside = outside_dir().join(format!("{provider}-{}.txt", cell.name));
    let _ = std::fs::remove_file(&outside);
    Paths { workspace, inside, outside }
}

/// The trivial first turn of a mid-session cell: no tool, one word.
pub const WARMUP: &str = "Reply with the single word OK. Do not use any tools.";

/// The probe turn: one shell command writing once inside the workspace and
/// once outside it. Absolute paths so nothing depends on the shell's cwd
/// beyond the workspace the session opened in.
pub fn probe_prompt(paths: &Paths) -> String {
    format!(
        "Run exactly this one shell command with your shell tool, once, and nothing else: \
         `echo hi > {} && echo hi > {}` — do not run any other command, do not retry or \
         work around a refusal, do not ask me anything. Then reply with the single word DONE \
         (or BLOCKED if the command was not allowed to run).",
        paths.inside.display(),
        paths.outside.display(),
    )
}

/// What one cell observed.
#[derive(Clone, Debug, Default)]
pub struct Outcome {
    /// Approval headlines raised during the probe turn, in order.
    pub asked: Vec<String>,
    /// How each was answered (`allow` / `deny` / the error).
    pub answered: Vec<String>,
    /// Approvals raised during the warm-up turn (should be none).
    pub warmup_asked: usize,
    pub inside: bool,
    pub outside: bool,
    /// The last mode the provider reported for the session.
    pub reported: Option<PermissionMode>,
    /// Anything that went wrong driving the cell.
    pub error: Option<String>,
    /// The tail of the reply text, for the record.
    pub reply: String,
    /// Model turns this cell spent.
    pub turns: u32,
}

/// The verdict for one cell: pass, and why not.
pub fn verdict(cell: &Cell, outcome: &Outcome) -> (bool, String) {
    let mut faults = Vec::new();
    if let Some(error) = &outcome.error {
        faults.push(format!("error: {error}"));
    }
    if outcome.reported != Some(cell.mode) {
        faults.push(format!("reported {:?}, chip {:?}", outcome.reported, cell.mode));
    }
    if outcome.warmup_asked > 0 {
        faults.push(format!("{} ask(s) on the tool-free warm-up", outcome.warmup_asked));
    }
    let asked = !outcome.asked.is_empty();
    let mut note = String::new();
    match cell.mode {
        PermissionMode::AllowAll => {
            if asked {
                faults.push("Full access asked".into());
            }
            if !outcome.inside || !outcome.outside {
                faults.push("Full access did not write both".into());
            }
        }
        PermissionMode::OnRequest => {
            if !outcome.inside && !(asked && cell.deny) {
                faults.push("Auto did not write inside the workspace".into());
            }
            if outcome.outside && !asked {
                note = "outside write ran unasked".into();
            } else if !outcome.outside && !asked {
                note = "outside write blocked, never asked".into();
            } else if asked {
                note = "asked for the outside write".into();
            }
        }
        PermissionMode::PromptUnmatched => {
            if !asked {
                faults.push("Ask never asked".into());
            }
            if cell.deny {
                if outcome.inside || outcome.outside {
                    faults.push("a denied command still wrote".into());
                }
            } else if asked && !outcome.inside {
                faults.push("allowed, but the workspace write did not land".into());
            } else if asked && !outcome.outside {
                note = "allowed; the outside write was still sandbox-blocked".into();
            }
        }
        PermissionMode::DenyUnmatched => {
            if asked {
                faults.push("Read-only asked".into());
            }
            if outcome.inside || outcome.outside {
                faults.push("Read-only wrote".into());
            }
        }
    }
    let pass = faults.is_empty();
    let mut why = faults.join("; ");
    if !note.is_empty() {
        if !why.is_empty() {
            why.push_str("; ");
        }
        why.push_str(&note);
    }
    (pass, why)
}

/// Print the provider's table and return whether every cell passed.
pub fn report(provider: &str, results: &[(Cell, Outcome)]) -> bool {
    let mut all = true;
    let mut turns = 0;
    println!("\n== {provider} ==");
    println!("| cell | asked | inside | outside | reported | verdict |");
    println!("|---|---|---|---|---|---|");
    for (cell, outcome) in results {
        let (pass, why) = verdict(cell, outcome);
        all &= pass;
        turns += outcome.turns;
        let asked = if outcome.asked.is_empty() {
            "no".to_owned()
        } else {
            format!("yes×{} ({})", outcome.asked.len(), outcome.answered.join(","))
        };
        println!(
            "| {} | {} | {} | {} | {} | {}{} |",
            cell.name,
            asked,
            outcome.inside,
            outcome.outside,
            outcome.reported.map(|m| m.label()).unwrap_or("none"),
            if pass { "PASS" } else { "FAIL" },
            if why.is_empty() { String::new() } else { format!(" — {why}") },
        );
        for (index, headline) in outcome.asked.iter().enumerate() {
            println!("    ask {index}: {headline}");
        }
        if !outcome.reply.is_empty() {
            println!("    reply: {}", outcome.reply.replace('\n', " "));
        }
    }
    println!("{provider}: {turns} model turns");
    all
}

/// The last `n` characters of `text`.
pub fn tail(text: &str, n: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    chars[chars.len().saturating_sub(n)..].iter().collect()
}
