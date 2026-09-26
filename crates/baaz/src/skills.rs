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
    /// no reveal, no editor, read-only preview from `inspect` (D60).
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

/// A virtual skill's SKILL.md through `muse skills inspect --json`: the body
/// text when the payload carries one, `None` when it does not.
pub fn inspect_body(program: &str, id: &str) -> Option<String> {
    let stdout = run_cli(program, &["skills", "inspect", "--json", id], None)?;
    let payload: serde_json::Value = serde_json::from_slice(&stdout).ok()?;
    let skill = payload.get("skill").unwrap_or(&payload);
    for key in ["body", "markdown", "content", "skill_md", "text"] {
        if let Some(text) = skill.get(key).and_then(|v| v.as_str()).filter(|s| !s.trim().is_empty()) {
            return Some(text.to_owned());
        }
    }
    None
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

/// A skill's SKILL.md body: the file for real paths, `inspect` for virtual
/// ones (D60). Frontmatter stripped.
pub fn skill_body(program: &str, skill: &Skill, root: &str) -> Option<String> {
    if skill.is_virtual() {
        return inspect_body(program, &skill.id).map(|body| strip_frontmatter(&body).to_owned());
    }
    let path = skill.disk_path(root)?;
    std::fs::read_to_string(path).ok().map(|body| strip_frontmatter(&body).to_owned())
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
}
