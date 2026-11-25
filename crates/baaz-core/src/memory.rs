//! Memory notes every provider shares (docs/85 12.3, D-Q7).
//!
//! **Where.** A project's notes live in the folder Claude Code's auto
//! memory uses for that repo (docs/85 10.1): `~/.claude/projects/<main
//! repo slug>/memory` for the owner's own run, and
//! `<state dir>/claude-memory/<slug>` whenever the state dir is redirected,
//! the run is a capture or the build is a test build (rule 25: then nothing
//! is written into the owner's `~/.claude`). That folder is the canonical memory for every
//! provider. Personal notes — about the person, across projects — live in
//! Baaz's own folder, `<state dir>/memory`.
//!
//! **Format.** Claude's own: one Markdown file per note with a frontmatter
//! (`name`, `description`, `metadata.type`) and a body, and a `MEMORY.md`
//! index of one line per note (`- [Title](file.md) — description`).
//! [`save`] and [`edit`] change one note at a time and keep the index in
//! step; Baaz never edits `AGENTS.md` or `CLAUDE.md` (rule 21).
//!
//! **The index in the prompt.** Claude loads `MEMORY.md` itself. Codex,
//! Muse and ACP get it through Baaz's appended note at session open
//! ([`index_for_prompt`]), capped at [`INDEX_LINES`] lines.

use std::path::{Path, PathBuf};

/// The index file's name.
pub const INDEX: &str = "MEMORY.md";

/// How many index lines go into a prompt before "N more".
pub const INDEX_LINES: usize = 60;

/// Which folder a note goes to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// The project's shared folder.
    Project,
    /// Baaz's own folder: about the person, across projects.
    Personal,
}

impl Scope {
    /// From the tool's `scope` argument (`project` when absent).
    pub fn parse(raw: Option<&str>) -> Result<Scope, String> {
        match raw.unwrap_or("project") {
            "project" => Ok(Scope::Project),
            "personal" => Ok(Scope::Personal),
            other => Err(format!("scope must be project or personal, not {other:?}")),
        }
    }

    /// How a row names it: "project memory" or "personal memory".
    pub fn words(self) -> &'static str {
        match self {
            Scope::Project => "project memory",
            Scope::Personal => "personal memory",
        }
    }
}

/// The project memory folder for a session in `workspace`.
pub fn project_dir(workspace: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    // Rule 25: a redirected state dir, a capture or a test build never
    // writes the owner's `~/.claude`.
    let redirected = crate::store::state_dir_override().or_else(|| {
        (crate::runtime::captures() || cfg!(any(test, feature = "test-support"))).then(crate::store::support_dir)
    });
    crate::providers::claude_memory_dir(workspace, &home, redirected.as_deref())
}

/// The personal memory folder.
pub fn personal_dir() -> PathBuf {
    crate::store::support_dir().join("memory")
}

/// The folder for `scope` and a session in `workspace`.
pub fn dir_for(scope: Scope, workspace: &Path) -> PathBuf {
    match scope {
        Scope::Project => project_dir(workspace),
        Scope::Personal => personal_dir(),
    }
}

/// One note as the tools write it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    /// The title the index and the row show.
    pub title: String,
    /// One line: what the note is about (the index's hook).
    pub description: String,
    /// `user`, `feedback`, `project` or `reference` (Claude's types).
    pub kind: String,
    /// The note itself.
    pub body: String,
}

/// A note's file name: its title as a kebab-case slug.
pub fn slug(title: &str) -> String {
    let mut slug = String::new();
    for c in title.trim().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug = slug.trim_end_matches('-');
    let slug: String = slug.chars().take(60).collect();
    if slug.is_empty() { "note".to_owned() } else { slug }
}

/// The note's file text, in Claude's format.
fn render(note: &Note) -> String {
    let one_line = |text: &str| text.replace(['\n', '\r'], " ").replace('"', "'");
    format!(
        "---\nname: {}\ndescription: \"{}\"\nmetadata:\n  type: {}\n---\n\n{}\n",
        slug(&note.title),
        one_line(&note.description),
        one_line(&note.kind),
        note.body.trim_end()
    )
}

/// The index line for a note.
fn index_line(note: &Note, file: &str) -> String {
    format!("- [{}]({file}) — {}", note.title.replace(['\n', '[', ']'], " "), note.description.replace('\n', " "))
}

/// Save `note` into `dir`, replacing a note of the same title; the index
/// gains (or updates) its line. Returns the note's path.
pub fn save(dir: &Path, note: &Note) -> Result<PathBuf, String> {
    if note.title.trim().is_empty() || note.body.trim().is_empty() {
        return Err("a note needs a title and a body".into());
    }
    let kind = note.kind.trim();
    if !["user", "feedback", "project", "reference"].contains(&kind) {
        return Err(format!("type must be user, feedback, project or reference, not {kind:?}"));
    }
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    let file = format!("{}.md", slug(&note.title));
    let path = dir.join(&file);
    std::fs::write(&path, render(note)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    set_index_line(dir, &file, Some(&index_line(note, &file)))?;
    Ok(path)
}

/// What [`edit`] changes.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Change {
    /// A new body.
    pub body: Option<String>,
    /// A new description.
    pub description: Option<String>,
    /// Remove the note and its index line.
    pub delete: bool,
}

/// Change or remove the note named `name` (its title or file name) in
/// `dir`. Returns its title.
pub fn edit(dir: &Path, name: &str, change: &Change) -> Result<String, String> {
    let file = find(dir, name).ok_or_else(|| format!("no note named {name:?} in {}", dir.display()))?;
    let path = dir.join(&file);
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut note = parse(&text, &file);
    // The title the person saw is the index's.
    if let Some((title, _, _)) = list(dir).into_iter().find(|(_, listed, _)| *listed == file) {
        note.title = title;
    }
    if change.delete {
        std::fs::remove_file(&path).map_err(|e| format!("cannot remove {}: {e}", path.display()))?;
        set_index_line(dir, &file, None)?;
        return Ok(note.title);
    }
    if let Some(body) = &change.body {
        note.body = body.clone();
    }
    if let Some(description) = &change.description {
        note.description = description.clone();
    }
    std::fs::write(&path, render(&note)).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    set_index_line(dir, &file, Some(&index_line(&note, &file)))?;
    Ok(note.title)
}

/// The note file `name` names: the file itself, or a title's slug.
fn find(dir: &Path, name: &str) -> Option<String> {
    let name = name.trim();
    let direct = name.strip_suffix(".md").unwrap_or(name);
    [format!("{direct}.md"), format!("{}.md", slug(name))]
        .into_iter()
        .find(|file| file != INDEX && dir.join(file).is_file())
}

/// A note read back from its file (a file without frontmatter is all body).
pub fn parse(text: &str, file: &str) -> Note {
    let stem = file.strip_suffix(".md").unwrap_or(file).to_owned();
    let mut note = Note { title: stem.clone(), description: String::new(), kind: "project".into(), body: text.to_owned() };
    let Some(rest) = text.strip_prefix("---\n") else { return note };
    let Some(end) = rest.find("\n---\n") else { return note };
    for line in rest[..end].lines() {
        if let Some(value) = line.strip_prefix("description:") {
            note.description = value.trim().trim_matches('"').to_owned();
        } else if let Some(value) = line.trim_start().strip_prefix("type:") {
            note.kind = value.trim().to_owned();
        }
    }
    note.body = rest[end + 5..].trim_start_matches('\n').to_owned();
    // The title is the index's, when the index names this file.
    note
}

/// Put (or with `None`, take out) the index line for `file`.
fn set_index_line(dir: &Path, file: &str, line: Option<&str>) -> Result<(), String> {
    let path = dir.join(INDEX);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let target = format!("]({file})");
    let mut lines: Vec<String> = text.lines().filter(|l| !l.contains(&target)).map(str::to_owned).collect();
    if let Some(line) = line {
        lines.push(line.to_owned());
    }
    let mut out = lines.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// The notes in `dir`, from its index: `(title, file, description)`.
pub fn list(dir: &Path) -> Vec<(String, String, String)> {
    let text = std::fs::read_to_string(dir.join(INDEX)).unwrap_or_default();
    text.lines()
        .filter_map(|line| {
            let line = line.trim().strip_prefix("- [")?;
            let (title, rest) = line.split_once("](")?;
            let (file, rest) = rest.split_once(')')?;
            let description = rest.trim().trim_start_matches(['—', '-']).trim().to_owned();
            Some((title.to_owned(), file.to_owned(), description))
        })
        .collect()
}

/// The index as a prompt section for a provider that does not load it
/// itself: at most [`INDEX_LINES`] lines, then "N more". `None` when the
/// folder has no notes.
pub fn index_for_prompt(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join(INDEX)).ok()?;
    let lines: Vec<&str> = text.lines().filter(|line| !line.trim().is_empty()).collect();
    if lines.is_empty() {
        return None;
    }
    let mut out = format!("Project memory notes (in {}; read one with your file tools):\n", dir.display());
    for line in lines.iter().take(INDEX_LINES) {
        out.push_str(line);
        out.push('\n');
    }
    if lines.len() > INDEX_LINES {
        out.push_str(&format!("{} more: use the memory tools.\n", lines.len() - INDEX_LINES));
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("baaz-memory-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn note(title: &str) -> Note {
        Note { title: title.into(), description: "Postcodes are validated per country".into(), kind: "project".into(), body: "CA and GB branch.".into() }
    }

    #[test]
    fn a_note_saves_in_claudes_format_with_its_index_line_and_edits_one_at_a_time() {
        let dir = temp("save");
        let path = save(&dir, &note("Postcode rules")).expect("saved");
        assert_eq!(path.file_name().unwrap(), "postcode-rules.md");
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("---\nname: postcode-rules\ndescription: \"Postcodes are validated per country\"\nmetadata:\n  type: project\n---\n\nCA and GB branch.\n"), "{text}");
        assert_eq!(list(&dir), [("Postcode rules".to_owned(), "postcode-rules.md".to_owned(), "Postcodes are validated per country".to_owned())]);
        save(&dir, &Note { body: "CA, GB and NL branch.".into(), ..note("Postcode rules") }).expect("replaced");
        assert_eq!(list(&dir).len(), 1, "the same title replaces, never duplicates");
        let title = edit(&dir, "postcode-rules", &Change { body: Some("Only CA now.".into()), ..Change::default() }).unwrap();
        assert_eq!(title, "Postcode rules", "the index's title, not the file's");
        assert!(std::fs::read_to_string(&path).unwrap().contains("Only CA now."));
        edit(&dir, "Postcode rules", &Change { delete: true, ..Change::default() }).expect("forgotten");
        assert!(!path.exists() && list(&dir).is_empty(), "forget removes the note and its line");
        assert!(edit(&dir, "nothing", &Change::default()).is_err());
        assert!(save(&dir, &Note { kind: "gossip".into(), ..note("x") }).is_err(), "Claude's four types only");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_prompt_index_is_capped_with_n_more() {
        let dir = temp("cap");
        std::fs::create_dir_all(&dir).unwrap();
        let index: String = (0..75).map(|i| format!("- [Note {i}](note-{i}.md) — about {i}\n")).collect();
        std::fs::write(dir.join(INDEX), index).unwrap();
        let prompt = index_for_prompt(&dir).expect("an index");
        assert_eq!(prompt.lines().filter(|l| l.starts_with("- [")).count(), INDEX_LINES);
        assert!(prompt.ends_with("15 more: use the memory tools.\n"), "{prompt}");
        assert_eq!(index_for_prompt(&temp("none")), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn slugs_are_claudes_kebab_names() {
        assert_eq!(slug("  Postcode rules (GB/CA)! "), "postcode-rules-gb-ca");
        assert_eq!(slug("!!!"), "note");
    }
}
