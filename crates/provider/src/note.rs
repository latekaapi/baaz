//! The appended Baaz note and `notice_suffix` (docs/85 10.5, rule 17).
//!
//! One short static note tells the agent how to work well inside Baaz. It is
//! appended through each provider's own channel — `--append-system-prompt` on
//! Claude Code, `developerInstructions` on Codex's `thread/start` and
//! `thread/resume` — never replacing the vendor's prompt. Muse and ACP have no
//! such channel (Muse's `session/start` config carries only `mcpServers`; ACP's
//! `session/new` only `cwd` and `mcpServers`), so they get none.
//!
//! **Byte-identical for the session.** The start of a provider's context must
//! not change mid-session, or every cache behind it is lost. So the note holds
//! no date and no model name, the text sent at open is stored with the session
//! (`<state dir>/notes/<session>.txt`), and every resume and relaunch sends the
//! stored bytes — even after a Baaz update changed the wording. A Codex
//! compaction keeps developer instructions (probed live 25 Nov 2026,
//! docs/85-evidence/10.5), so nothing is re-sent after one.
//!
//! Anything Baaz has to tell the agent mid-session (the folder, branch or mode
//! changed; text the person put back) goes at the **end** of the next user
//! message, through [`notice_suffix`] — never into the prompt prefix.

use std::path::{Path, PathBuf};

/// Which of Baaz's tool families this build serves the agent: the note names
/// only tools that exist (12.3 added the memory line; 13.1 adds its own).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoteTools {
    /// `terminal_*`, `browser_*` and the session and memory tools over the
    /// bridge.
    pub bridge: bool,
}

/// The note's ceiling with the bridge's lines (docs/88 C.5): every request
/// carries it, so it names only what the agent would not find on its own —
/// each tool's own description says the rest. Without the bridge it is
/// shorter still.
pub const MAX_CHARS: usize = 500;

/// The note: at most [`MAX_CHARS`] characters (about 120 tokens), no dates,
/// no model names.
pub fn compose(tools: NoteTools) -> String {
    let mut lines = vec![
        "You are running inside Baaz, the person's desktop app. Give file paths absolute, with :line when it matters.",
    ];
    if tools.bridge {
        lines.push("terminal_* runs commands in a terminal the person watches: for when they say terminal, or long or interactive runs.");
        // Owner 2026-11-28: a "google search" ran on the agent's own web
        // tools and the Page stayed empty; asked to look at a site, the
        // agent uses the pane so the person sees it.
        lines.push("browser_* drives their browser pane: when they say search, open, browse or show a site, use it, not web tools. pane_show shows a file, diff, terminal or PR.");
        lines.push("Check session_search before redoing work; memory_save what later sessions need.");
    }
    lines.push("Ask choices with your question tool.");
    lines.join("\n")
}

/// Where a session's note is stored: `<state_dir>/notes/<session>.txt`.
/// `None` for an id that could escape the folder.
pub fn stored_path(state_dir: &Path, session_id: &str) -> Option<PathBuf> {
    let safe = !session_id.is_empty()
        && session_id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        && session_id != "."
        && session_id != "..";
    safe.then(|| state_dir.join("notes").join(format!("{session_id}.txt")))
}

/// The note `session_id` was opened with, when stored.
pub fn stored(state_dir: &Path, session_id: &str) -> Option<String> {
    std::fs::read_to_string(stored_path(state_dir, session_id)?).ok()
}

/// Store `note` as `session_id`'s, unless one is stored already (the first
/// note a session got is the one it keeps). Best-effort: a failed write means
/// the next resume composes again.
pub fn store(state_dir: &Path, session_id: &str, note: &str) {
    let Some(path) = stored_path(state_dir, session_id) else { return };
    if path.exists() {
        return;
    }
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, note).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// The note for `session_id`: the stored one, else `compose()` — stored now,
/// so the session keeps exactly these bytes.
pub fn for_session(state_dir: &Path, session_id: &str, compose: impl FnOnce() -> String) -> String {
    if let Some(note) = stored(state_dir, session_id) {
        return note;
    }
    let note = compose();
    store(state_dir, session_id, &note);
    note
}

/// `message` with Baaz's `notices` after it (rule 17): the one way Baaz tells
/// the agent something mid-session. Nothing to say leaves the message as is.
pub fn notice_suffix(message: &str, notices: &[String]) -> String {
    let notices: Vec<&str> = notices.iter().map(|n| n.trim()).filter(|n| !n.is_empty()).collect();
    if notices.is_empty() {
        return message.to_owned();
    }
    format!("{message}\n\n<baaz-notice>\n{}\n</baaz-notice>", notices.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_note_is_short_and_carries_no_date_or_model() {
        let note = compose(NoteTools { bridge: true });
        assert!(note.len() <= MAX_CHARS, "{} chars", note.len());
        assert!(compose(NoteTools { bridge: false }).len() < note.len());
        // Owner 2026-11-28: asked to search or open a site, the agent
        // uses the browser pane the person sees, not its own web tools.
        assert!(note.contains("when they say search, open, browse or show a site, use it, not web tools"), "{note}");
        for word in ["2026", "claude-", "gpt-", "haiku", "sonnet", "opus"] {
            assert!(!note.to_lowercase().contains(word), "{word} in the note");
        }
        assert!(note.contains("terminal_"));
        assert!(note.contains("session_search") && note.contains("memory_save"), "12.3's tools are named");
        assert!(note.contains("pane_show"), "13.1's line (W4)");
        assert!(!compose(NoteTools { bridge: false }).contains("terminal_"), "no tools named that do not exist");
    }

    /// docs/88 C.5: every tool the note names is one the bridge serves (a
    /// `name_*` family needs at least one tool with that prefix). The
    /// bridge is a binary no crate links, so its tool-definition sources
    /// are read as text: each tool is a quoted name there.
    #[test]
    fn every_tool_the_note_names_is_served_by_the_bridge() {
        let sources = [
            include_str!("../../mcp-bridge/src/terminal.rs"),
            include_str!("../../mcp-bridge/src/browser.rs"),
            include_str!("../../mcp-bridge/src/memory.rs"),
            include_str!("../../mcp-bridge/src/pane.rs"),
            include_str!("../../mcp-bridge/src/orchestra.rs"),
        ]
        .concat();
        let note = compose(NoteTools { bridge: true });
        let named: Vec<&str> = note
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '*'))
            .filter(|word| word.contains('_') && word.chars().next().is_some_and(|c| c.is_ascii_lowercase()))
            .collect();
        assert!(named.len() >= 5, "{named:?}");
        for name in named {
            let served = match name.strip_suffix('*') {
                Some(prefix) => sources.contains(&format!("\"{prefix}")),
                None => sources.contains(&format!("\"{name}\"")),
            };
            assert!(served, "the note names {name}, which the bridge does not serve");
        }
    }

    #[test]
    fn the_first_note_a_session_gets_is_the_one_it_keeps() {
        let dir = std::env::temp_dir().join(format!("baaz-note-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(for_session(&dir, "s-1", || "first".into()), "first");
        assert_eq!(for_session(&dir, "s-1", || "second".into()), "first", "a later wording never replaces it");
        assert_eq!(for_session(&dir, "s-2", || "second".into()), "second");
        assert!(stored_path(&dir, "../x").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn notices_go_after_the_message() {
        assert_eq!(notice_suffix("hi", &[]), "hi");
        assert_eq!(notice_suffix("hi", &["  ".into()]), "hi");
        let out = notice_suffix("fix the test", &["The folder is now /w/s-1.".into()]);
        assert!(out.starts_with("fix the test\n\n"), "{out}");
        assert!(out.ends_with("</baaz-notice>"), "{out}");
    }
}
