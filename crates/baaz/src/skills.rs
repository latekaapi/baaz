//! Skills for the `/` menu and the Skills page (docs/15-skills.md).
//!
//! Skills reach MSP only as ordinary `toolCall` items — there is no skills
//! method on the wire — so the list comes from the CLI: `muse skills list
//! --json --workspace <root> --trust-workspace`, run on a background thread.
//! Its shape is
//!
//! ```json
//! {"skills":[{"id":"bundled:browser-app-delivery","name":"browser-app-delivery",
//!             "display_name":"…","description":"…","scope":"bundled",
//!             "source":{"type":"local"},"path":"bundled://…",
//!             "activation":"on","diagnostics":[],
//!             "context_cost":{"startup_bytes":1884,
//!                            "startup_estimated_tokens":471,
//!                            "invoke_bytes":null,
//!                            "invoke_estimated_tokens":null}}],
//!  "diagnostics":[{"code":"skill-shadowed","message":"skill `git` skipped …",
//!                  "scope":"bundled","path":"bundled://…"}]}
//! ```
//!
//! Unknown top-level diagnostic codes are kept verbatim on
//! [`SkillDiagnostic::code`]; `skill-shadowed` entries become
//! [`Overridden`] rows in the loser's own section (D58). The `/` menu reads
//! the same catalog the page shows (D64): `Off` skills are absent, `Only /`
//! skills are marked.

use std::process::Command;

use serde::Deserialize;

/// How Muse loads a skill: the CLI's three activation values, named plainly
/// (D57).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activation {
    /// Loaded whenever a task matches the description.
    #[default]
    On,
    /// Loaded only when the person types `/name`.
    UserInvocableOnly,
    /// Never loaded. The row stays for later.
    Off,
}

impl Activation {
    /// Parse the CLI's `activation` value. Unknown spellings are `Off`: a
    /// skill that guessed would be worse than one that stays dark.
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_lowercase().as_str() {
            "on" => Activation::On,
            "user-invocable-only" | "user-only" | "only" => Activation::UserInvocableOnly,
            _ => Activation::Off,
        }
    }

    /// Whether the switch reads on.
    pub fn is_on(&self) -> bool {
        matches!(self, Activation::On)
    }

    /// The `muse skills …` subcommand that sets this state (S1).
    pub fn cli_verb(&self) -> &'static str {
        match self {
            Activation::On => "enable",
            Activation::UserInvocableOnly => "user-only",
            Activation::Off => "disable",
        }
    }
}

/// What a skill costs in context, as Muse reports it (D59).
#[derive(Clone, Copy, Debug, Default, Deserialize)]
pub struct ContextCost {
    /// Frontmatter bytes loaded at startup. Parsed for completeness; the
    /// meter sums tokens, never bytes.
    #[serde(default)]
    #[allow(dead_code)]
    pub startup_bytes: u64,
    /// Estimated startup tokens. Muse reports the same figure whatever the
    /// state, so the page sums over rows with `activation == on`.
    #[serde(default)]
    pub startup_estimated_tokens: u64,
    /// Bytes loaded on invocation, if Muse reports any. Parsed for
    /// completeness; the detail shows tokens, never bytes.
    #[serde(default)]
    #[allow(dead_code)]
    pub invoke_bytes: Option<u64>,
    /// Estimated invocation tokens, if Muse reports any.
    #[serde(default)]
    pub invoke_estimated_tokens: Option<u64>,
}

/// One top-level diagnostic of `muse skills list --json`.
///
/// Unknown codes are kept verbatim on [`SkillDiagnostic::code`]; only
/// `skill-shadowed` is interpreted, into [`Overridden`].
#[derive(Clone, Debug, Deserialize)]
pub struct SkillDiagnostic {
    /// `skill-shadowed`, or whatever Muse sent.
    #[serde(default)]
    pub code: String,
    /// Human sentence, e.g. "skill `git` skipped because a higher-priority
    /// source defines the same id".
    #[serde(default)]
    pub message: Option<String>,
    /// The loser's scope (`bundled`, `user`, …).
    #[serde(default)]
    pub scope: Option<String>,
    /// The loser's path, virtual (`bundled://…`) or real.
    #[serde(default)]
    pub path: Option<String>,
}

/// One row of `muse skills list --json`.
#[derive(Clone, Debug, Deserialize)]
pub struct Skill {
    /// `bundled:plan`, `user:my-thing`, bare for user and project rows.
    #[serde(default)]
    pub id: String,
    /// The name the slash command uses.
    #[serde(default)]
    pub name: String,
    /// The human label; often the same as the name.
    #[serde(default)]
    pub display_name: Option<String>,
    /// One-line description.
    #[serde(default)]
    pub description: Option<String>,
    /// Short description, when Muse sends one. Parsed for the H2
    /// install/import previews; K2 keeps it without reading it.
    #[serde(default)]
    #[allow(dead_code)]
    pub short_description: Option<String>,
    /// `bundled` / `user` / `project` / `plugin`. Falls back to the `id`
    /// prefix when Muse omits it (see [`Skill::scope`]).
    #[serde(default)]
    pub scope: Option<String>,
    /// Where the row came from (`{"type":"local"}`, …). Kept as JSON for
    /// the H2 install/import previews; K2 never branches on it.
    #[serde(default)]
    #[allow(dead_code)]
    pub source: Option<serde_json::Value>,
    /// `bundled://…`, `plugin://…`, relative for project, absolute for user.
    #[serde(default)]
    pub path: Option<String>,
    /// `on`, `user-invocable-only`, `off`.
    #[serde(default, deserialize_with = "de_activation")]
    pub activation: Activation,
    /// Row-level diagnostics (issue counts ride here).
    #[serde(default)]
    pub diagnostics: Vec<SkillDiagnostic>,
    /// Provenance, when Muse sends any. Kept as JSON for the H2
    /// install/import previews; K2 never reads it.
    #[serde(default)]
    #[allow(dead_code)]
    pub provenance: Option<serde_json::Value>,
    /// Context cost, as Muse reports it.
    #[serde(default)]
    pub context_cost: ContextCost,
}

fn de_activation<'de, D>(deserializer: D) -> Result<Activation, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?.unwrap_or_default();
    Ok(Activation::parse(&raw))
}

impl Skill {
    /// The scope tag: the explicit `scope` field, else the `id` prefix. An
    /// id with no prefix is `skill`, because a tag that guessed would be
    /// worse than one that does not.
    pub fn scope(&self) -> &str {
        if let Some(scope) = self.scope.as_deref().filter(|s| !s.is_empty()) {
            return scope;
        }
        match self.id.split_once(':') {
            Some((scope, _)) => scope,
            None => "skill",
        }
    }

    /// Which page section this row belongs to (D56).
    pub fn section(&self) -> ScopeSection {
        match self.scope() {
            "project" => ScopeSection::Project,
            "user" => ScopeSection::Personal,
            "plugin" => ScopeSection::Plugins,
            _ => ScopeSection::Builtin,
        }
    }

    /// The description, trimmed to one readable line.
    pub fn summary(&self) -> String {
        let text = self.description.clone().unwrap_or_default();
        let line = text.split(['\n', '.']).find(|s| !s.trim().is_empty()).unwrap_or("").trim();
        if line.is_empty() {
            self.display_name.clone().unwrap_or_else(|| self.name.clone())
        } else {
            format!("{line}.")
        }
    }

    /// Startup tokens, for the row's mono tokens cell.
    pub fn startup_tokens(&self) -> u64 {
        self.context_cost.startup_estimated_tokens
    }

    /// Whether the `/` menu lists this row (D64): `Off` skills are absent.
    pub fn in_slash_menu(&self) -> bool {
        !matches!(self.activation, Activation::Off)
    }

    /// Whether the row's path is virtual (`bundled://…`, `plugin://…`):
    /// no reveal, no editor, read-only preview from `inspect` (D60, see
    /// [`inspect_preview`]).
    pub fn is_virtual(&self) -> bool {
        self.path.as_deref().is_some_and(|p| p.contains("://"))
    }

    /// The row's SKILL.md on disk, when it has one: project paths resolve
    /// against `root`, absolute paths stand alone, virtual paths answer
    /// `None` (read those through [`inspect_body`]).
    pub fn disk_path(&self, root: &str) -> Option<std::path::PathBuf> {
        let path = self.path.as_deref()?;
        if self.is_virtual() {
            return None;
        }
        let candidate = std::path::PathBuf::from(path);
        if candidate.is_absolute() {
            return Some(candidate);
        }
        let root = std::path::PathBuf::from(root);
        let joined = root.join(&candidate);
        if joined.is_file() {
            return Some(joined);
        }
        // A project row whose relative path does not resolve under the
        // root (older CLIs, renamed folders): walk the discovery dirs for
        // a `SKILL.md` whose folder matches the skill name.
        if self.section() == ScopeSection::Project {
            for dir in ["agents/skills", ".agents/skills", ".claude/skills", ".codex/skills"] {
                let dir = if dir == "agents/skills" { root.join(".agents/skills") } else { root.join(dir) };
                let file = dir.join(&self.name).join("SKILL.md");
                if file.is_file() {
                    return Some(file);
                }
            }
        }
        Some(joined)
    }
}

/// One page section (D56).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScopeSection {
    /// This project.
    Project,
    /// Personal ("all projects").
    Personal,
    /// Plugins.
    Plugins,
    /// Built-in.
    Builtin,
}

impl ScopeSection {
    /// Every section, in page order.
    pub const ALL: [ScopeSection; 4] = [
        ScopeSection::Project,
        ScopeSection::Personal,
        ScopeSection::Plugins,
        ScopeSection::Builtin,
    ];

    /// The section header label.
    pub fn label(&self) -> &'static str {
        match self {
            ScopeSection::Project => "This project",
            ScopeSection::Personal => "Personal",
            ScopeSection::Plugins => "Plugins",
            ScopeSection::Builtin => "Built-in",
        }
    }

}

/// A scope name for chips and quiet rows: "built-in" for bundled, so the
/// winner wears "Overrides built-in git" (D58) and a skill row reads
/// "Loaded skill `git` · built-in" (D63).
pub(crate) fn scope_word(scope: &str) -> &str {
    match scope {
        "bundled" => "built-in",
        "user" => "personal",
        "project" => "this project",
        "plugin" => "plugin",
        _ => scope,
    }
}

/// One shadowed skill: the loser's dimmed row (D58).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Overridden {
    /// The skill name both sides share.
    pub name: String,
    /// The loser's scope, from the diagnostic.
    pub scope: String,
    /// The winner's scope: the live row's, else the higher-precedence guess.
    pub by_scope: String,
    /// The loser's path, when the diagnostic carries one.
    pub path: Option<String>,
    /// Why it lost, in Muse's words.
    pub message: Option<String>,
}

impl Overridden {
    /// Why the row lost: the diagnostic's message, else the fallback.
    pub fn message(&self) -> String {
        self.message.clone().unwrap_or_else(|| "Shadowed by a higher-priority skill.".to_owned())
    }

    /// The dimmed row's chip: "Overridden by this project's git" (D58).
    pub fn chip(&self) -> String {
        if self.by_scope == "project" {
            format!("Overridden by this project's {}", self.name)
        } else {
            let scope = match self.by_scope.as_str() {
                "bundled" => "built-in",
                "user" => "personal",
                "plugin" => "plugin",
                scope => scope,
            };
            format!("Overridden by {scope} {}", self.name)
        }
    }
}

/// The page's list: one CLI run, fully parsed (S1).
#[derive(Clone, Debug, Default)]
pub struct SkillsCatalog {
    /// The workspace root the list was taken for.
    pub project_root: String,
    /// The live rows, in CLI order.
    pub rows: Vec<Skill>,
    /// One dimmed row per `skill-shadowed` diagnostic, in diagnostic order.
    pub overridden: Vec<Overridden>,
    /// What went wrong, in the page banner's words. Empty is clean.
    pub errors: Vec<String>,
}

impl SkillsCatalog {
    /// An empty catalog for `root`: what the page holds before the first
    /// list lands, and what a failed re-list keeps showing behind.
    pub fn empty(root: &str) -> Self {
        SkillsCatalog { project_root: root.to_owned(), rows: Vec::new(), overridden: Vec::new(), errors: Vec::new() }
    }

    /// How many skills are on: the sidebar nav row's count (D54).
    pub fn on_count(&self) -> usize {
        self.rows.iter().filter(|s| s.activation.is_on()).count()
    }

    /// The meter sum (D59): `startup_estimated_tokens` over rows with
    /// `activation == on` only. `Only /` counts as zero.
    pub fn meter_tokens(&self) -> u64 {
        self.rows.iter().filter(|s| s.activation.is_on()).map(|s| s.startup_tokens()).sum()
    }

    /// The meter sum split by section, in page order: the cost bar's slices.
    pub fn meter_by_section(&self) -> [(ScopeSection, u64); 4] {
        let mut sums = [(ScopeSection::Project, 0u64), (ScopeSection::Personal, 0u64), (ScopeSection::Plugins, 0u64), (ScopeSection::Builtin, 0u64)];
        for row in self.rows.iter().filter(|s| s.activation.is_on()) {
            if let Some(slot) = sums.iter_mut().find(|(section, _)| *section == row.section()) {
                slot.1 += row.startup_tokens();
            }
        }
        sums
    }

    /// Rows for one section, in CLI order.
    pub fn section_rows(&self, section: ScopeSection) -> Vec<&Skill> {
        self.rows.iter().filter(|s| s.section() == section).collect()
    }

    /// Whether `row` matches the search box: name and description (D56).
    pub fn matches_query(row: &Skill, query: &str) -> bool {
        let needle = query.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        row.name.to_lowercase().contains(&needle)
            || row.description.as_deref().unwrap_or_default().to_lowercase().contains(&needle)
    }

    /// Find a live row by skill id or name.
    pub fn find(&self, id_or_name: &str) -> Option<&Skill> {
        self.rows.iter().find(|s| s.id == id_or_name || s.name == id_or_name)
    }
}

#[derive(Deserialize)]
struct Listing {
    #[serde(default)]
    skills: Vec<Skill>,
    #[serde(default)]
    diagnostics: Vec<SkillDiagnostic>,
}

/// Build a catalog from one parsed `list --json` payload. Pure, so the unit
/// tests pin the grouping without a CLI.
pub fn build_catalog(project_root: &str, payload: serde_json::Value) -> SkillsCatalog {
    let listing: Listing = match serde_json::from_value(payload) {
        Ok(listing) => listing,
        Err(error) => {
            return SkillsCatalog {
                project_root: project_root.to_owned(),
                rows: Vec::new(),
                overridden: Vec::new(),
                errors: vec![format!("could not parse `muse skills list`: {error}")],
            };
        }
    };
    let mut catalog = SkillsCatalog {
        project_root: project_root.to_owned(),
        rows: listing.skills,
        overridden: Vec::new(),
        errors: Vec::new(),
    };
    for diagnostic in &listing.diagnostics {
        if diagnostic.code != "skill-shadowed" {
            continue;
        }
        let Some(name) = shadowed_name(diagnostic) else { continue };
        // The winner is the live row sharing the name; without one, the
        // higher-precedence side wins (project > personal > bundled).
        let by_scope = catalog
            .find(&name)
            .map(|winner| winner.scope().to_owned())
            .unwrap_or_else(|| match diagnostic.scope.as_deref() {
                Some("user") => "project".to_owned(),
                _ => "project".to_owned(),
            });
        let scope = diagnostic.scope.clone().unwrap_or_default();
        catalog.overridden.push(Overridden {
            name,
            scope,
            by_scope,
            path: diagnostic.path.clone(),
            message: diagnostic.message.clone(),
        });
    }
    catalog
}

/// The shadowed skill's name: parsed out of the diagnostic's message
/// ("skill `name` skipped …"), because the diagnostic carries scope and
/// path but no name field.
fn shadowed_name(diagnostic: &SkillDiagnostic) -> Option<String> {
    let message = diagnostic.message.as_deref()?;
    let backticked = message.split('`').nth(1)?;
    if backticked.is_empty() {
        return None;
    }
    Some(backticked.to_owned())
}

/// How long `muse skills list --json` is given before it is killed.
///
/// `Command::output()` waits for ever, so a hung CLI held the background task
/// that owns the `/` menu's sources for the life of the process (finding
/// `support-10`). A timeout is a fifth failure mode with the same answer as
/// the other four: no Skills section.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How often the wait checks on the child.
const POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Run `program` with `args` in `dir`, bounded by [`TIMEOUT`]. `None` is
/// every failure mode at once: not there, non-zero exit, hung, unreadable.
fn run_cli(program: &str, args: &[&str], dir: Option<&std::path::Path>) -> Option<Vec<u8>> {
    let mut command = Command::new(program);
    command.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(
        std::process::Stdio::piped(),
    );
    if let Some(dir) = dir {
        command.current_dir(dir);
    }
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            // Exited: `wait_with_output` now only drains the pipe.
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(POLL),
            Ok(None) => {
                // Over the deadline. Kill it and reap it, so no `muse`
                // outlives the window that asked.
                let _ = child.kill();
                let _ = child.wait();
                crate::baaz_log!("`{program} skills` did not answer in {TIMEOUT:?}");
                return None;
            }
            Err(_) => return None,
        }
    }
    let output = child.wait_with_output().ok()?;
    if !output.status.success() {
        crate::baaz_log!("`{program} skills {}` failed: {}", args.first().unwrap_or(&""), String::from_utf8_lossy(&output.stderr).trim());
        return None;
    }
    Some(output.stdout)
}

/// The page's list: one `muse skills list --json --workspace <root>
/// --trust-workspace` (D55, D65), fully parsed. Blocking and bounded by
/// [`TIMEOUT`]; a failed run answers with its error on
/// [`SkillsCatalog::errors`], and the caller keeps the previous catalog
/// until a clean one lands.
pub fn list_catalog(program: &str, root: &str) -> SkillsCatalog {
    let root_path = std::path::Path::new(root);
    let args = ["skills", "list", "--json", "--workspace", root, "--trust-workspace"];
    let Some(stdout) = run_cli(program, &args, Some(root_path)) else {
        return SkillsCatalog {
            project_root: root.to_owned(),
            rows: Vec::new(),
            overridden: Vec::new(),
            errors: vec!["`muse skills list` did not answer. Try again.".to_owned()],
        };
    };
    match serde_json::from_slice::<serde_json::Value>(&stdout) {
        Ok(payload) => build_catalog(root, payload),
        Err(_) => SkillsCatalog {
            project_root: root.to_owned(),
            rows: Vec::new(),
            overridden: Vec::new(),
            errors: vec!["`muse skills list` answered something this build cannot parse. Try again.".to_owned()],
        },
    }
}

/// Set one skill's activation through the CLI, then let the caller re-list:
/// the UI changes only on the re-list, never optimistically (D55).
/// `Err` carries the CLI's message for the page banner.
pub fn set_activation(program: &str, root: &str, id: &str, scope: &str, activation: Activation) -> Result<(), String> {
    let verb = activation.cli_verb().to_owned();
    let args = [ "skills", verb.as_str(), id, "--scope", scope, "--workspace", root, "--trust-workspace", "--json" ];
    let root_path = std::path::Path::new(root);
    match run_cli(program, &args, Some(root_path)) {
        Some(_) => Ok(()),
        None => Err("`muse skills` did not answer. Try again.".to_owned()),
    }
}

/// Validate a skill folder: `muse skills validate <dir> --json`. `Ok` carries
/// the one-line verdict for the toast; `Err` carries the CLI's message.
pub fn validate_skill(program: &str, dir: &std::path::Path) -> Result<String, String> {
    let dir = dir.to_string_lossy().into_owned();
    let args = ["skills", "validate", dir.as_str(), "--json"];
    match run_cli(program, &args, None) {
        Some(stdout) => {
            let verdict: serde_json::Value = serde_json::from_slice(&stdout).unwrap_or(serde_json::Value::Null);
            let detail = verdict
                .get("message")
                .or_else(|| verdict.get("summary"))
                .and_then(|v| v.as_str())
                .unwrap_or("valid");
            Ok(format!("Validate: {detail}"))
        }
        None => Err("`muse skills validate` did not answer.".to_owned()),
    }
}

/// Strip YAML frontmatter (`---` … `---`) from a SKILL.md body, for the
/// detail pane's rendered section (D60).
pub fn strip_frontmatter(body: &str) -> &str {
    let mut start = 0usize;
    let mut chunks = body.split_inclusive('\n');
    let Some(first) = chunks.next() else { return body };
    if first.trim() != "---" {
        return body;
    }
    start += first.len();
    for chunk in chunks {
        start += chunk.len();
        if chunk.trim() == "---" {
            return body.get(start..).unwrap_or(body);
        }
    }
    body
}

/// A skill's SKILL.md body: the file for real paths, the read-only
/// `inspect` preview for virtual ones (D60, see [`inspect_preview`]).
/// Frontmatter stripped.
pub fn skill_body(program: &str, skill: &Skill, root: &str) -> Option<String> {
    if skill.is_virtual() {
        return inspect_preview(program, &skill.id);
    }
    let path = skill.disk_path(root)?;
    std::fs::read_to_string(path).ok().map(|body| strip_frontmatter(&body).to_owned())
}

// Add, import and remove (K3, docs/15-skills.md §5, D61–D62): the page's
// mutations behind the "Add skill ▾" menu and the detail's ⋯ → Remove….
//
// Every mutation runs `muse skills …` and then re-lists; nothing moves
// before the re-list lands (D55). The pure helpers below (validation,
// scaffolding, import classification) are unit-tested; the thin CLI
// runners reuse `run_cli` and carry the CLI's shape, not the page's.

/// Where an import preview's candidates come from (D61).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportSource {
    /// `~/.claude/skills`.
    ClaudeCode,
    /// `~/.codex/skills`.
    Codex,
}

impl ImportSource {
    /// The `--from` value the CLI takes.
    pub fn arg(&self) -> &'static str {
        match self {
            ImportSource::ClaudeCode => "claude",
            ImportSource::Codex => "codex",
        }
    }

    /// The menu and dialog label.
    pub fn label(&self) -> &'static str {
        match self {
            ImportSource::ClaudeCode => "Claude Code",
            ImportSource::Codex => "Codex",
        }
    }
}

/// Whether an import candidate is new, replaces a personal skill, or is
/// already installed (D61). Mirrors the library's preview rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStatus {
    /// Personal has no skill under this name: picked, live checkbox.
    New,
    /// Personal has the name with different content: picked, `--force`
    /// only for rows the person leaves checked.
    Replaces,
    /// Byte-identical with the personal copy: unchecked, disabled.
    Installed,
}

/// One `import --dry-run` candidate, enriched from its source SKILL.md.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportCandidate {
    /// The dry-run's `id`.
    pub id: String,
    /// The frontmatter name, else the folder name.
    pub name: String,
    /// The frontmatter description, else "".
    pub description: String,
    /// Startup tokens, estimated from the SKILL.md bytes (see
    /// [`estimate_tokens`]).
    pub tokens: u32,
    /// The dry-run's `source_path` (the candidate's SKILL.md on disk).
    pub source_path: String,
    /// The dry-run's `valid`.
    pub valid: bool,
    /// The dry-run's `diagnostics`, joined.
    pub diagnostics: String,
}

/// Check a new skill's name: lowercase letters, digits and hyphens, at most
/// 64 characters, and not taken in the target scope (D61). `Ok` is usable.
pub fn validate_skill_name(name: &str, taken: &[String]) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Give the skill a name.".to_owned());
    }
    if name.len() > 64 {
        return Err("Keep the name to 64 characters.".to_owned());
    }
    let ok = name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !ok {
        return Err("Use lowercase letters, digits and hyphens.".to_owned());
    }
    if taken.iter().any(|other| other == name) {
        return Err(format!("A skill named `{name}` already exists here."));
    }
    Ok(())
}

/// Check a new skill's description: required, at most 1024 characters
/// (D61). `Ok` is usable.
pub fn validate_skill_description(description: &str) -> Result<(), String> {
    if description.trim().is_empty() {
        return Err("Say what it does and when to use it.".to_owned());
    }
    if description.len() > 1024 {
        return Err("Keep the description to 1024 characters.".to_owned());
    }
    Ok(())
}

/// The scaffold a new skill starts from: frontmatter `name` and
/// `description`, then a short body template (D61).
pub fn scaffold_skill_md(name: &str, description: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\nWhat this skill does, and when to use it.\n\n## Workflow\n\n1. One step at a time.\n2. Say what changed.\n"
    )
}

/// The folder a This-project skill lives in: `<root>/.agents/skills/<name>`
/// (D61; the discovery dirs in [`Skill::disk_path`] are read, not written).
pub fn project_skill_dir(root: &str, name: &str) -> std::path::PathBuf {
    std::path::Path::new(root).join(".agents/skills").join(name)
}

/// Create a This-project skill: write the scaffold and answer its SKILL.md.
/// An existing name is refused, never overwritten.
pub fn create_project_skill(root: &str, name: &str, description: &str) -> Result<std::path::PathBuf, String> {
    validate_skill_name(name, &[])?;
    validate_skill_description(description)?;
    let dir = project_skill_dir(root, name);
    let file = dir.join("SKILL.md");
    if file.exists() {
        return Err(format!("A skill named `{name}` already exists here."));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    std::fs::write(&file, scaffold_skill_md(name, description))
        .map_err(|e| format!("Could not write {}: {e}", file.display()))?;
    Ok(file)
}

/// Copy one skill folder over another, creating parents. Files only;
/// symlinks are not followed.
pub fn copy_skill_dir(from: &std::path::Path, to: &std::path::Path) -> Result<(), String> {
    if !from.is_dir() {
        return Err(format!("{} is not a folder.", from.display()));
    }
    let mut stack = vec![from.to_path_buf()];
    while let Some(top) = stack.pop() {
        let relative = top.strip_prefix(from).map_err(|_| "Cannot relativize.".to_owned())?;
        let dest = to.join(relative);
        if top.is_dir() {
            std::fs::create_dir_all(&dest).map_err(|e| format!("Could not create {}: {e}", dest.display()))?;
            let entries = std::fs::read_dir(&top).map_err(|e| format!("Could not read {}: {e}", top.display()))?;
            for entry in entries.flatten() {
                stack.push(entry.path());
            }
        } else if top.is_file() {
            if let Some(parent) = dest.parent() {
                std::fs::create_dir_all(parent).map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
            }
            std::fs::copy(&top, &dest).map_err(|e| format!("Could not copy {}: {e}", top.display()))?;
        }
    }
    Ok(())
}

/// Move `path` into `trash_dir` (renaming past collisions), answering the
/// new location. The Trash move itself — never `rm` (D62).
pub fn move_to_trash_dir(path: &std::path::Path, trash_dir: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let name = path.file_name().and_then(|n| n.to_str()).filter(|n| !n.is_empty()).ok_or_else(|| "Nothing to remove.".to_owned())?;
    std::fs::create_dir_all(trash_dir).map_err(|e| format!("Could not open the Trash: {e}"))?;
    let mut dest = trash_dir.join(name);
    let mut n = 2u32;
    while dest.exists() {
        dest = trash_dir.join(format!("{name} {n}"));
        n += 1;
    }
    std::fs::rename(path, &dest).map_err(|e| format!("Could not move {} to the Trash: {e}", path.display()))?;
    Ok(dest)
}

/// This machine's Trash: `$HOME/.Trash`.
pub fn trash_dir() -> std::path::PathBuf {
    std::env::var_os("HOME").map(std::path::PathBuf::from).unwrap_or_else(|| std::path::PathBuf::from("/tmp")).join(".Trash")
}

/// A token estimate from SKILL.md bytes: four bytes a token, at least one.
/// Muse reports estimates the same way applicants do — the import and
/// install previews say what a candidate *adds*, and a heuristic labelled
/// as one beats a blank cell.
pub fn estimate_tokens(bytes: u64) -> u32 {
    (bytes / 4).max(1).min(u32::MAX as u64) as u32
}

/// A SKILL.md's frontmatter `name` and `description`: single-line values
/// with surrounding quotes stripped, plus indented continuation lines
/// folded with spaces. Anything else answers `None`.
pub fn frontmatter_meta(body: &str) -> (Option<String>, Option<String>) {
    let mut lines = body.lines();
    if lines.next().is_none_or(|first| first.trim() != "---") {
        return (None, None);
    }
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;
    // Which slot a continuation line folds into: 0 is name, 1 is
    // description, `None` is nowhere. A continuation is an indented line —
    // or, leniently, any colon-less line while a slot is open, because a
    // wrapped description is still the description.
    let mut current: Option<u8> = None;
    for line in lines {
        if line.trim() == "---" {
            break;
        }
        let slot = if line.starts_with([' ', '\t']) || (current.is_some() && !line.contains(':')) {
            current
        } else {
            None
        };
        if let Some(which) = slot {
            let trimmed = line.trim();
            if !trimmed.is_empty() {
                let target = if which == 0 { &mut name } else { &mut description };
                if let Some(text) = target {
                    if text.is_empty() {
                        *text = trimmed.to_owned();
                    } else {
                        text.push(' ');
                        text.push_str(trimmed);
                    }
                }
            }
            continue;
        }
        current = None;
        let Some((key, value)) = line.split_once(':') else { continue };
        // A lone `>` / `|` is YAML's folded/literal marker: the value is
        // the indented lines below, not the marker itself.
        let value = value.trim();
        let stored = if value == ">" || value == "|" { String::new() } else { unquote(value) };
        match key.trim() {
            "name" => {
                name = Some(stored);
                current = Some(0);
            }
            "description" => {
                description = Some(stored);
                current = Some(1);
            }
            _ => {}
        }
    }
    // Quotes can straddle the first line and its continuations (`'Does` …
    // `things.'`), so strip one layer at the end as well as at assignment.
    (name.map(|n| unquote(n.trim())), description.map(|d| unquote(d.trim())))
}

/// Strip one layer of surrounding single or double quotes.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        value[1..value.len() - 1].to_owned()
    } else {
        value.to_owned()
    }
}

/// A candidate's display name and description from its source SKILL.md:
/// frontmatter first, folder name and "" when unreadable.
pub fn candidate_meta(source_path: &str) -> (String, String) {
    let fallback = std::path::Path::new(source_path)
        .parent()
        .and_then(|d| d.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| source_path.to_owned());
    let Ok(body) = std::fs::read_to_string(source_path) else {
        return (fallback, String::new());
    };
    let (name, description) = frontmatter_meta(&body);
    (name.filter(|n| !n.trim().is_empty()).unwrap_or(fallback), description.unwrap_or_default())
}

/// Classify one import candidate against the landed catalog (D61): a
/// personal skill under the same name with byte-identical content is
/// Already installed; the same name with different content Replaces yours;
/// anything else is New.
pub fn import_status(name: &str, source_path: &str, catalog: &SkillsCatalog) -> ImportStatus {
    let personal = catalog.rows.iter().find(|row| row.name == name && row.section() == ScopeSection::Personal);
    let Some(row) = personal else { return ImportStatus::New };
    let root = catalog.project_root.clone();
    let same = row
        .disk_path(&root)
        .filter(|installed| installed.is_file())
        .and_then(|installed| std::fs::read(installed).ok())
        .zip(std::fs::read(source_path).ok())
        .is_some_and(|(installed, source)| installed == source);
    if same { ImportStatus::Installed } else { ImportStatus::Replaces }
}

/// The dry-run behind the import preview (D61): `import --from <src>
/// --dry-run --json`, enriched from each candidate's source SKILL.md.
/// Read-only: nothing is copied.
pub fn import_dry_run(program: &str, source: ImportSource) -> Result<Vec<ImportCandidate>, String> {
    let from = source.arg().to_owned();
    let args = ["skills", "import", "--from", from.as_str(), "--dry-run", "--json"];
    let stdout = run_cli(program, &args, None).ok_or_else(|| "`muse skills import --dry-run` did not answer. Try again.".to_owned())?;
    parse_dry_run(&stdout)
}

/// Parse one `import --dry-run --json` payload. Pure, so the preview pins
/// the shape without a CLI.
pub fn parse_dry_run(stdout: &[u8]) -> Result<Vec<ImportCandidate>, String> {
    let payload: serde_json::Value =
        serde_json::from_slice(stdout).map_err(|_| "`muse skills import --dry-run` answered something this build cannot parse.".to_owned())?;
    let mut out = Vec::new();
    let candidates = payload.get("candidates").and_then(|v| v.as_array()).cloned().unwrap_or_default();
    for candidate in candidates {
        let id = candidate.get("id").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let source_path = candidate.get("source_path").and_then(|v| v.as_str()).unwrap_or_default().to_owned();
        let valid = candidate.get("valid").and_then(|v| v.as_bool()).unwrap_or(false);
        out.push((id, source_path, valid, candidate));
    }
    Ok(out
        .into_iter()
        .map(|(id, source_path, valid, candidate)| {
            let (name, description) = if source_path.is_empty() {
                (id.clone(), String::new())
            } else {
                candidate_meta(&source_path)
            };
            let tokens = std::fs::metadata(&source_path).map(|m| estimate_tokens(m.len())).unwrap_or(0);
            let diagnostics = candidate
                .get("diagnostics")
                .and_then(|v| v.as_array())
                .map(|ds| ds.iter().filter_map(|d| d.as_str().or_else(|| d.get("message").and_then(|m| m.as_str()))).collect::<Vec<_>>().join("; "))
                .unwrap_or_default();
            ImportCandidate { id, name, description, tokens, source_path, valid, diagnostics }
        })
        .collect())
}

/// Install one folder as a personal skill: `install <dir> --scope user
/// [--force] --json`. `force` only after an explicit Replace confirm (D61).
pub fn install_skill_dir(program: &str, dir: &std::path::Path, force: bool) -> Result<(), String> {
    let dir = dir.to_string_lossy().into_owned();
    let mut owned = vec!["skills".to_owned(), "install".to_owned(), dir, "--scope".to_owned(), "user".to_owned()];
    if force {
        owned.push("--force".to_owned());
    }
    owned.push("--json".to_owned());
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    run_cli(program, &args, None).map(|_| ()).ok_or_else(|| "`muse skills install` did not answer. Try again.".to_owned())
}

/// Import every candidate from a source: `import --from <src> --json` (D61).
/// A partial selection installs per chosen skill instead (see
/// [`install_skill_dir`); `import` takes no filter (PS3).
pub fn import_source(program: &str, source: ImportSource) -> Result<(), String> {
    let from = source.arg();
    run_cli(program, &["skills", "import", "--from", from, "--json"], None)
        .map(|_| ())
        .ok_or_else(|| "`muse skills import` did not answer. Try again.".to_owned())
}

/// Remove a personal skill: `uninstall <id> --json` (D62). Import and
/// install copy, never link, so this never touches `~/.claude/skills` or
/// `~/.codex/skills` (PS6).
pub fn uninstall_skill(program: &str, id: &str) -> Result<(), String> {
    run_cli(program, &["skills", "uninstall", id, "--json"], None)
        .map(|_| ())
        .ok_or_else(|| "`muse skills uninstall` did not answer. Try again.".to_owned())
}

/// A virtual skill's read-only SKILL.md preview (D60, K2 gap): `inspect
/// --json`, frontmatter stripped, rendered with the library markdown by the
/// caller.
///
/// muse 1.4.0's `inspect --json` carries no body — only metadata — so the
/// preview is the description it does carry. A body under a future key is
/// preferred when one appears.
pub fn inspect_preview(program: &str, id: &str) -> Option<String> {
    let stdout = run_cli(program, &["skills", "inspect", "--json", id], None)?;
    let payload: serde_json::Value = serde_json::from_slice(&stdout).ok()?;
    let skill = payload.get("skill").unwrap_or(&payload);
    for key in ["body", "markdown", "content", "skill_md", "text", "readme", "skillMd"] {
        if let Some(text) = skill.get(key).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
            return Some(strip_frontmatter(text).to_owned());
        }
    }
    let description = skill.get("description").and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty())?;
    Some(strip_frontmatter(description).to_owned())
}

/// The files under a skill's folder, relative to the folder, for the detail
/// pane's file list. Virtual skills and missing folders answer empty.
pub fn skill_files(skill: &Skill, root: &str) -> Vec<String> {
    let Some(file) = skill.disk_path(root) else { return Vec::new() };
    let Some(dir) = file.parent() else { return Vec::new() };
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(top) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&top) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if let Ok(relative) = path.strip_prefix(dir) {
                files.push(relative.to_string_lossy().into_owned());
            }
        }
    }
    files.sort();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(id: &str, description: Option<&str>) -> Skill {
        Skill {
            id: id.to_owned(),
            name: id.split_once(':').map(|(_, n)| n).unwrap_or(id).to_owned(),
            display_name: None,
            description: description.map(str::to_owned),
            short_description: None,
            scope: None,
            source: None,
            path: None,
            activation: Activation::On,
            diagnostics: Vec::new(),
            provenance: None,
            context_cost: ContextCost::default(),
        }
    }

    #[test]
    fn the_scope_is_the_id_prefix() {
        assert_eq!(skill("bundled:plan", None).scope(), "bundled");
        assert_eq!(skill("user:mine", None).scope(), "user");
        assert_eq!(skill("plan", None).scope(), "skill");
    }

    #[test]
    fn an_explicit_scope_beats_the_prefix() {
        let mut s = skill("decoction", None);
        s.scope = Some("user".to_owned());
        assert_eq!(s.scope(), "user");
        assert_eq!(s.section(), ScopeSection::Personal);
    }

    #[test]
    fn the_summary_is_one_sentence() {
        let s = skill("bundled:plan", Some("Create a grounded plan. Then stop.\nMore prose."));
        assert_eq!(s.summary(), "Create a grounded plan.");
    }

    #[test]
    fn a_skill_with_no_description_falls_back_to_its_name() {
        assert_eq!(skill("bundled:plan", None).summary(), "plan");
    }

    #[test]
    fn activation_parses_the_cli_values() {
        assert_eq!(Activation::parse("on"), Activation::On);
        assert_eq!(Activation::parse("user-invocable-only"), Activation::UserInvocableOnly);
        assert_eq!(Activation::parse("off"), Activation::Off);
        assert_eq!(Activation::parse("bogus"), Activation::Off);
    }

    #[test]
    fn frontmatter_strips_to_the_body() {
        let body = "---\nname: demo\ndescription: Demo.\n---\n\n# Demo\n";
        assert_eq!(strip_frontmatter(body), "\n# Demo\n");
        assert_eq!(strip_frontmatter("# No frontmatter\n"), "# No frontmatter\n");
    }

    fn catalog_payload() -> serde_json::Value {
        serde_json::json!({
            "skills": [
                {"id": "git", "name": "git", "scope": "project", "path": ".agents/skills/git/SKILL.md",
                 "activation": "on", "context_cost": {"startup_estimated_tokens": 100}},
                {"id": "decoction", "name": "decoction", "scope": "user", "path": "/home/u/.claude/skills/decoction/SKILL.md",
                 "activation": "user-invocable-only", "context_cost": {"startup_estimated_tokens": 200}},
                {"id": "bundled:plan", "name": "plan", "scope": "bundled", "path": "bundled://x/skills/plan/SKILL.md",
                 "activation": "off", "context_cost": {"startup_estimated_tokens": 300}},
                {"id": "plugin:threejs:threejs", "name": "plugin:threejs:threejs", "scope": "plugin",
                 "path": "plugin://threejs/skills/threejs/SKILL.md",
                 "activation": "on", "context_cost": {"startup_estimated_tokens": 60}}
            ],
            "diagnostics": [
                {"code": "skill-shadowed", "message": "skill `decoction` skipped because a higher-priority source defines the same id",
                 "scope": "user", "path": "$HOME/.claude/skills/decoction/SKILL.md"},
                {"code": "skill-something-new", "message": "kept verbatim"}
            ]
        })
    }

    #[test]
    fn the_catalog_groups_sections_and_overrides() {
        let catalog = build_catalog("/root", catalog_payload());
        assert_eq!(catalog.rows.len(), 4);
        assert_eq!(catalog.section_rows(ScopeSection::Project).len(), 1);
        assert_eq!(catalog.section_rows(ScopeSection::Personal).len(), 1);
        assert_eq!(catalog.section_rows(ScopeSection::Plugins).len(), 1);
        assert_eq!(catalog.section_rows(ScopeSection::Builtin).len(), 1);
        assert_eq!(catalog.overridden.len(), 1);
        let loser = &catalog.overridden[0];
        assert_eq!(loser.name, "decoction");
        assert_eq!(loser.scope, "user");
        assert_eq!(loser.by_scope, "user");
        assert_eq!(loser.chip(), "Overridden by personal decoction");
    }

    #[test]
    fn the_meter_sums_on_only() {
        let catalog = build_catalog("/root", catalog_payload());
        assert_eq!(catalog.on_count(), 2);
        assert_eq!(catalog.meter_tokens(), 160);
        let by_section: Vec<(ScopeSection, u64)> = catalog.meter_by_section().into_iter().collect();
        assert_eq!(by_section[0], (ScopeSection::Project, 100));
        assert_eq!(by_section[1], (ScopeSection::Personal, 0));
        assert_eq!(by_section[2], (ScopeSection::Plugins, 60));
        assert_eq!(by_section[3], (ScopeSection::Builtin, 0));
    }

    #[test]
    fn the_slash_menu_hides_off() {
        let catalog = build_catalog("/root", catalog_payload());
        let names: Vec<&str> =
            catalog.rows.iter().filter(|s| s.in_slash_menu()).map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["git", "decoction", "plugin:threejs:threejs"]);
    }

    #[test]
    fn search_matches_name_and_description() {
        let mut row = skill("bundled:plan", Some("Create a grounded plan."));
        assert!(SkillsCatalog::matches_query(&row, ""));
        assert!(SkillsCatalog::matches_query(&row, "plan"));
        assert!(SkillsCatalog::matches_query(&row, "grounded"));
        assert!(!SkillsCatalog::matches_query(&row, "skill"));
        row.activation = Activation::Off;
        assert!(!row.in_slash_menu());
    }

    #[test]
    fn activation_maps_to_cli_verbs() {
        assert_eq!(Activation::On.cli_verb(), "enable");
        assert_eq!(Activation::UserInvocableOnly.cli_verb(), "user-only");
        assert_eq!(Activation::Off.cli_verb(), "disable");
    }

    #[test]
    fn virtual_paths_have_no_disk_file() {
        let mut row = skill("bundled:plan", None);
        row.path = Some("bundled://x/skills/plan/SKILL.md".to_owned());
        assert!(row.is_virtual());
        assert_eq!(row.disk_path("/root"), None);
        row.path = Some(".agents/skills/git/SKILL.md".to_owned());
        assert!(!row.is_virtual());
    }

    #[test]
    fn names_take_lowercase_hyphens_and_no_collisions() {
        assert!(validate_skill_name("my-skill-2", &[]).is_ok());
        assert!(validate_skill_name("", &[]).is_err());
        assert!(validate_skill_name("Has Caps", &[]).is_err());
        assert!(validate_skill_name("has space", &[]).is_err());
        assert!(validate_skill_name("has_underscore", &[]).is_err());
        assert!(validate_skill_name(&"n".repeat(64), &[]).is_ok());
        assert!(validate_skill_name(&"n".repeat(65), &[]).is_err());
        assert!(validate_skill_name("taken", &["taken".to_owned()]).is_err());
        assert!(validate_skill_name("  trimmed  ", &["trimmed".to_owned()]).is_err());
    }

    #[test]
    fn descriptions_are_required_and_bounded() {
        assert!(validate_skill_description("Does things.").is_ok());
        assert!(validate_skill_description("   ").is_err());
        assert!(validate_skill_description(&"d".repeat(1024)).is_ok());
        assert!(validate_skill_description(&"d".repeat(1025)).is_err());
    }

    #[test]
    fn the_scaffold_carries_frontmatter_then_a_template() {
        let body = scaffold_skill_md("demo", "Does demo things.");
        assert!(body.starts_with("---\nname: demo\ndescription: Does demo things.\n---\n"));
        let (name, description) = frontmatter_meta(&body);
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(description.as_deref(), Some("Does demo things."));
    }

    #[test]
    fn frontmatter_reads_quotes_and_continuations() {
        let (name, description) = frontmatter_meta("---\nname: \"demo\"\ndescription: 'Does\ndemo things.'\n---\n# Demo\n");
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(description.as_deref(), Some("Does demo things."));
        assert_eq!(frontmatter_meta("# No frontmatter\n"), (None, None));
    }

    #[test]
    fn creating_a_project_skill_writes_its_file() {
        let root = std::env::temp_dir().join(format!("baaz-k3-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("scratch root");
        let file = create_project_skill(root.to_str().expect("utf8"), "demo", "Does demo things.").expect("creates");
        assert_eq!(file, root.join(".agents/skills/demo/SKILL.md"));
        let body = std::fs::read_to_string(&file).expect("readable");
        assert!(body.contains("name: demo"));
        assert!(create_project_skill(root.to_str().expect("utf8"), "demo", "Other.").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_dry_run_parses_and_enriches() {
        let dir = std::env::temp_dir().join(format!("baaz-k3-dry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let skill_dir = dir.join("alpha");
        std::fs::create_dir_all(&skill_dir).expect("scratch skill");
        std::fs::write(skill_dir.join("SKILL.md"), "---\nname: alpha\ndescription: Alpha skill.\n---\n# Alpha\n").expect("write");
        let payload = serde_json::json!({
            "candidates": [
                {"id": "alpha", "source_path": skill_dir.join("SKILL.md").to_string_lossy(), "valid": true, "diagnostics": []},
                {"id": "ghost", "source_path": dir.join("ghost/SKILL.md").to_string_lossy(), "valid": false,
                 "diagnostics": [{"message": "no SKILL.md"}]}
            ]
        });
        let candidates = parse_dry_run(serde_json::to_vec(&payload).expect("json").as_slice()).expect("parses");
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].name, "alpha");
        assert_eq!(candidates[0].description, "Alpha skill.");
        assert!(candidates[0].tokens >= 1);
        assert!(candidates[0].valid);
        assert_eq!(candidates[1].name, "ghost");
        assert_eq!(candidates[1].description, "");
        assert_eq!(candidates[1].tokens, 0);
        assert!(!candidates[1].valid);
        assert_eq!(candidates[1].diagnostics, "no SKILL.md");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_status_names_new_replaces_and_installed() {
        let dir = std::env::temp_dir().join(format!("baaz-k3-status-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let personal = dir.join("personal");
        std::fs::create_dir_all(&personal).expect("scratch");
        let installed = personal.join("SKILL.md");
        std::fs::write(&installed, "---\nname: mine\ndescription: Mine.\n---\n").expect("write");
        let source_same = dir.join("same.md");
        let source_diff = dir.join("diff.md");
        std::fs::write(&source_same, "---\nname: mine\ndescription: Mine.\n---\n").expect("write");
        std::fs::write(&source_diff, "---\nname: mine\ndescription: Changed.\n---\n").expect("write");
        let catalog = build_catalog(
            "/root",
            serde_json::json!({"skills": [
                {"id": "mine", "name": "mine", "scope": "user",
                 "path": installed.to_string_lossy(), "activation": "on"}
            ], "diagnostics": []}),
        );
        assert_eq!(import_status("fresh", source_diff.to_str().expect("utf8"), &catalog), ImportStatus::New);
        assert_eq!(
            import_status("mine", source_diff.to_str().expect("utf8"), &catalog),
            ImportStatus::Replaces
        );
        assert_eq!(
            import_status("mine", source_same.to_str().expect("utf8"), &catalog),
            ImportStatus::Installed
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_trash_move_renames_past_collisions() {
        let dir = std::env::temp_dir().join(format!("baaz-k3-trash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let trash = dir.join("Trash");
        let first = dir.join("demo");
        let second = dir.join("work/demo");
        std::fs::create_dir_all(second.parent().expect("parent")).expect("scratch");
        std::fs::create_dir_all(&first).expect("scratch");
        std::fs::create_dir_all(&second).expect("scratch");
        let moved_first = move_to_trash_dir(&first, &trash).expect("moves");
        assert_eq!(moved_first, trash.join("demo"));
        let moved_second = move_to_trash_dir(&second, &trash).expect("moves");
        assert_eq!(moved_second, trash.join("demo 2"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn copying_a_skill_folder_reaches_nested_files() {
        let dir = std::env::temp_dir().join(format!("baaz-k3-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let from = dir.join("from");
        std::fs::create_dir_all(from.join("refs")).expect("scratch");
        std::fs::write(from.join("SKILL.md"), "# Demo\n").expect("write");
        std::fs::write(from.join("refs/notes.md"), "notes\n").expect("write");
        copy_skill_dir(&from, &dir.join("to")).expect("copies");
        assert!(dir.join("to/SKILL.md").is_file());
        assert!(dir.join("to/refs/notes.md").is_file());
        assert!(copy_skill_dir(&dir.join("missing"), &dir.join("nowhere")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn token_estimates_scale_with_bytes() {
        assert_eq!(estimate_tokens(0), 1);
        assert_eq!(estimate_tokens(400), 100);
    }
}
