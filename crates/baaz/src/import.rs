//! P7: import earlier Claude Code and Codex sessions (docs/72).
//!
//! An imported session is a [`ProviderSessionRecord`] with
//! `origin: "imported"` plus its search row. Its transcript stays where the
//! CLI wrote it (`~/.claude/projects/<slug>/<id>.jsonl`,
//! `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`): nothing here copies,
//! moves or deletes a file, ever. Resuming one pins the child on the owner's
//! home (`session_migration::gate_resume` answers `LegacyOwner` for every
//! imported record, whatever the migration consent).
//!
//! Headless and pure over explicit dirs, like [`crate::session_migration`]:
//!
//! * [`scan_dirs`] walks one Claude config dir and one Codex home (through
//!   [`provider_claude_code::history::stored_transcripts`] and
//!   [`provider_codex::rollout::stored_rollouts`]) and reads only the **head**
//!   of each file (cwd, entrypoint/originator, first prompt, first
//!   timestamp) and a short **tail** (the last timestamp), so 1,900 files
//!   and 3.6 GB scan in well under a second;
//! * [`plan`] groups the scan by folder (cwd), drops ids Baaz already has,
//!   ticks the folders that are Baaz projects and counts scripted runs
//!   apart — the dry run;
//! * [`execute_batch`] turns chosen sessions into records (reading each
//!   whole file once for its turn count); the search words come later
//!   through the P3 path ([`crate::provider_search`] reads owner-home
//!   transcripts for ids not in Baaz's homes);
//! * the [`Journal`] (`import-journal.json` in the state dir) is what Undo
//!   reads — exactly the ids a run added — and what the card's tally reads.
//!
//! Tests use temp homes and never the real `HOME` ([`owner_home`] is the
//! window's only door to it, and `BAAZ_IMPORT_OWNER_HOME` points it at a
//! fixture for the probe entries).

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Read, Seek};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::provider_sessions::{ProviderSessionRecord, ProviderSessionStore, ORIGIN_IMPORTED};

/// The registry wire id for Claude Code sessions.
pub const CLAUDE: &str = "claude-code";
/// The registry wire id for Codex sessions.
pub const CODEX: &str = "codex";
/// The journal file's name under the state dir.
pub const JOURNAL_FILE_NAME: &str = "import-journal.json";
/// Bytes of a file's head the scan reads at most: the facts it wants sit in
/// the first few lines, but Codex opens with its whole instructions block.
const HEAD_BYTES: u64 = 512 * 1024;
/// Bytes of a file's tail the scan reads for the last timestamp.
const TAIL_BYTES: u64 = 64 * 1024;
/// The first prompt is a label, not a transcript.
const PROMPT_CHARS: usize = 200;

/// The owner's `$HOME` the import reads: `BAAZ_IMPORT_OWNER_HOME` when set
/// (the probe's fixture home — never a real one), else the real `HOME`.
/// Tests pass temp dirs explicitly and never call this.
pub fn owner_home() -> PathBuf {
    if let Some(dir) = std::env::var_os("BAAZ_IMPORT_OWNER_HOME") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Whether a window may scan at all: always live; under
/// `BAAZ_DETERMINISTIC=1` only with a fixture owner home, so a capture never
/// reads (or paints) the real `~/.claude` and `~/.codex`.
pub fn scan_allowed() -> bool {
    !crate::clock::deterministic() || std::env::var_os("BAAZ_IMPORT_OWNER_HOME").is_some_and(|dir| !dir.is_empty())
}

// ------------------------------------------------------------------- scan

/// One stored session the scan found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    /// [`CLAUDE`] or [`CODEX`].
    pub provider: &'static str,
    /// The session id (Claude's uuid, Codex's thread id).
    pub session_id: String,
    /// The transcript file.
    pub path: PathBuf,
    /// The folder it ran in, when the head names one.
    pub cwd: Option<String>,
    /// The first timestamp, else the file's mtime, as Unix ms.
    pub created_ms: i64,
    /// The last timestamp, else the file's mtime, as Unix ms.
    pub updated_ms: i64,
    /// The first real prompt, one line, capped.
    pub first_prompt: Option<String>,
    /// A scripted run: `claude -p` (`entrypoint: "sdk-cli"`), `codex exec`
    /// (`originator: "codex_exec"` / `source: "exec"`), or anything that
    /// ran under a temp dir.
    pub scripted: bool,
}

/// What one scan found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Scan {
    /// Every session with a prompt, in no particular order.
    pub sessions: Vec<Found>,
    /// Files that could not be read or carried no parseable line, by path:
    /// reported, never dropped silently.
    pub unreadable: Vec<PathBuf>,
    /// Sessions with no prompt at all (opened and left): never offered.
    pub empty: usize,
}

/// Scan the owner's homes: `<home>/.claude` and `<home>/.codex`.
pub fn scan_home(owner_home: &Path) -> Scan {
    scan_dirs(
        &provider_claude_code::home::owner_config_dir(owner_home),
        &provider_codex::home::owner_home_dir(owner_home),
    )
}

/// Scan one Claude config dir (holding `projects/`) and one Codex home
/// (holding `sessions/`). Read-only; heads and tails only.
pub fn scan_dirs(claude_config: &Path, codex_home: &Path) -> Scan {
    let temp = temp_roots();
    let mut scan = Scan::default();
    let mut claude: Vec<(String, PathBuf)> =
        provider_claude_code::history::stored_transcripts(claude_config).into_iter().collect();
    claude.sort();
    for (id, path) in claude {
        match scan_file(CLAUDE, &id, &path, &temp) {
            Ok(Some(found)) => scan.sessions.push(found),
            Ok(None) => scan.empty += 1,
            Err(()) => scan.unreadable.push(path),
        }
    }
    let mut codex: Vec<(String, PathBuf)> =
        provider_codex::rollout::stored_rollouts(codex_home).into_iter().collect();
    codex.sort();
    for (id, path) in codex {
        match scan_file(CODEX, &id, &path, &temp) {
            Ok(Some(found)) => scan.sessions.push(found),
            Ok(None) => scan.empty += 1,
            Err(()) => scan.unreadable.push(path),
        }
    }
    scan
}

/// The roots a run under counts as scripted: `$TMPDIR`, `/tmp`,
/// `/private/tmp`, `/private/var`, `/var/folders`.
fn temp_roots() -> Vec<String> {
    let mut roots: Vec<String> =
        ["/tmp/", "/private/tmp/", "/private/var/", "/var/folders/"].iter().map(|s| (*s).to_owned()).collect();
    if let Some(tmp) = std::env::var_os("TMPDIR") {
        let mut tmp = tmp.to_string_lossy().into_owned();
        if !tmp.ends_with('/') {
            tmp.push('/');
        }
        if tmp.len() > 1 {
            roots.push(tmp);
        }
    }
    roots
}

/// Whether `cwd` is a temp dir (or under one).
fn under_temp(cwd: &str, roots: &[String]) -> bool {
    let with_slash = format!("{}/", cwd.trim_end_matches('/'));
    roots.iter().any(|root| with_slash.starts_with(root.as_str()))
}

/// What the head and tail of one file say.
#[derive(Default)]
struct Facts {
    cwd: Option<String>,
    first_ts: Option<i64>,
    last_ts: Option<i64>,
    first_prompt: Option<String>,
    scripted: bool,
    parsed: usize,
}

/// One file: `Ok(Some)` a session, `Ok(None)` an empty one (no prompt),
/// `Err` unreadable.
fn scan_file(provider: &'static str, id: &str, path: &Path, temp: &[String]) -> Result<Option<Found>, ()> {
    let file = std::fs::File::open(path).map_err(|_| ())?;
    let meta = file.metadata().map_err(|_| ())?;
    let len = meta.len();
    let mtime = meta
        .modified()
        .ok()
        .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|at| at.as_millis() as i64)
        .unwrap_or(0);
    let mut facts = Facts::default();
    let mut reader = std::io::BufReader::new(file);
    let mut consumed: u64 = 0;
    let mut line = String::new();
    // The head: line by line until every fact is known or the budget is
    // spent. A line longer than the budget is read whole once (a JSON line
    // cannot be judged by half), then the head stops.
    loop {
        if consumed >= HEAD_BYTES || head_complete(provider, &facts) {
            break;
        }
        line.clear();
        let read = match reader.read_line(&mut line) {
            Ok(0) => break,
            Ok(read) => read,
            Err(_) => {
                // Not UTF-8 or an IO error mid-file: judge what we have.
                break;
            }
        };
        consumed += read as u64;
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim_end()) else { continue };
        facts.parsed += 1;
        note_line(provider, &value, &mut facts);
    }
    if facts.parsed == 0 {
        return if len == 0 { Ok(None) } else { Err(()) };
    }
    // The tail: the last timestamp, unless the head already read the file
    // to its end.
    if consumed < len {
        if let Some(ts) = tail_timestamp(reader.into_inner(), len) {
            facts.last_ts = Some(ts);
        }
    }
    let Some(first_prompt) = facts.first_prompt.take() else { return Ok(None) };
    let cwd = facts.cwd.take();
    let scripted = facts.scripted || cwd.as_deref().is_some_and(|cwd| under_temp(cwd, temp));
    let created_ms = facts.first_ts.unwrap_or(mtime);
    let updated_ms = facts.last_ts.unwrap_or(mtime).max(created_ms);
    Ok(Some(Found {
        provider,
        session_id: id.to_owned(),
        path: path.to_path_buf(),
        cwd,
        created_ms,
        updated_ms,
        first_prompt: Some(first_prompt),
        scripted,
    }))
}

/// Whether the head has told us all it can.
fn head_complete(provider: &str, facts: &Facts) -> bool {
    // Claude names its entrypoint on the user lines, so the first prompt's
    // line carries it too; Codex names its originator on line one.
    let _ = provider;
    facts.cwd.is_some() && facts.first_prompt.is_some() && facts.first_ts.is_some()
}

/// Fold one parsed line into the facts.
fn note_line(provider: &str, value: &serde_json::Value, facts: &mut Facts) {
    if let Some(ts) = timestamp_of(value) {
        if facts.first_ts.is_none() {
            facts.first_ts = Some(ts);
        }
        facts.last_ts = Some(ts);
    }
    if provider == CLAUDE {
        if facts.cwd.is_none() {
            if let Some(cwd) = value.get("cwd").and_then(|v| v.as_str()).filter(|cwd| !cwd.is_empty()) {
                facts.cwd = Some(cwd.to_owned());
            }
        }
        if value.get("entrypoint").and_then(|v| v.as_str()) == Some("sdk-cli") {
            facts.scripted = true;
        }
        if facts.first_prompt.is_none() {
            facts.first_prompt = claude_prompt(value).map(clip_prompt);
        }
    } else {
        let kind = value.get("type").and_then(|v| v.as_str());
        let payload = value.get("payload");
        if kind == Some("session_meta") {
            if let Some(payload) = payload {
                if facts.cwd.is_none() {
                    facts.cwd = payload.get("cwd").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_owned);
                }
                let originator = payload.get("originator").and_then(|v| v.as_str());
                let source = payload.get("source").and_then(|v| v.as_str());
                if originator == Some("codex_exec") || source == Some("exec") {
                    facts.scripted = true;
                }
            }
        }
        if facts.first_prompt.is_none() {
            facts.first_prompt = codex_prompt(value).map(clip_prompt);
        }
    }
}

/// A line's `timestamp` as Unix ms.
fn timestamp_of(value: &serde_json::Value) -> Option<i64> {
    let text = value.get("timestamp").and_then(|v| v.as_str())?;
    chrono::DateTime::parse_from_rfc3339(text).ok().map(|at| at.timestamp_millis())
}

/// The prompt a Claude line carries, if it is a real one: a `user` line,
/// not `isMeta`, not a sidechain, whose text is not a tag block
/// (`<command-name>`, `<local-command-stdout>`, `<system-reminder>`) and
/// not a tool result.
pub fn claude_prompt(value: &serde_json::Value) -> Option<&str> {
    if value.get("type").and_then(|v| v.as_str()) != Some("user") {
        return None;
    }
    if value.get("isMeta").and_then(|v| v.as_bool()) == Some(true)
        || value.get("isSidechain").and_then(|v| v.as_bool()) == Some(true)
    {
        return None;
    }
    let content = value.get("message")?.get("content")?;
    fn real(text: &str) -> Option<&str> {
        let text = text.trim();
        (!text.is_empty() && !text.starts_with('<') && !text.starts_with("Caveat:")).then_some(text)
    }
    match content {
        serde_json::Value::String(text) => real(text),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .filter(|block| block.get("type").and_then(|v| v.as_str()) == Some("text"))
            .filter_map(|block| block.get("text").and_then(|v| v.as_str()))
            .find_map(real),
        _ => None,
    }
}

/// The prompt a Codex line carries: an `event_msg` `user_message` (what the
/// person typed; the `response_item` copy is wrapped in injected context).
pub fn codex_prompt(value: &serde_json::Value) -> Option<&str> {
    if value.get("type").and_then(|v| v.as_str()) != Some("event_msg") {
        return None;
    }
    let payload = value.get("payload")?;
    if payload.get("type").and_then(|v| v.as_str()) != Some("user_message") {
        return None;
    }
    let text = payload.get("message").and_then(|v| v.as_str())?.trim();
    (!text.is_empty() && !text.starts_with('<')).then_some(text)
}

/// One line, at most [`PROMPT_CHARS`] characters.
fn clip_prompt(text: &str) -> String {
    let line = crate::sidebar::one_line(text);
    match line.char_indices().nth(PROMPT_CHARS) {
        Some((at, _)) => line[..at].to_owned(),
        None => line,
    }
}

/// The last timestamp in the final [`TAIL_BYTES`] of a file.
fn tail_timestamp(mut file: std::fs::File, len: u64) -> Option<i64> {
    let from = len.saturating_sub(TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::with_capacity((len - from) as usize);
    file.take(TAIL_BYTES).read_to_end(&mut bytes).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|value| timestamp_of(&value))
}

// ------------------------------------------------------------------- plan

/// One folder row of the import sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Folder {
    /// The folder's key: the cwd the sessions ran in (`""` when no head
    /// named one).
    pub cwd: String,
    /// The folder's own name (`harness`).
    pub name: String,
    /// Its parent, `~`-abbreviated (`~/Projects`).
    pub parent: String,
    /// The Baaz project it resolves to: ticked by default.
    pub project: Option<String>,
    /// Sessions offered per provider, scripted runs excluded.
    pub claude: usize,
    /// See [`Folder::claude`].
    pub codex: usize,
    /// Scripted runs per provider.
    pub claude_scripted: usize,
    /// See [`Folder::claude_scripted`].
    pub codex_scripted: usize,
    /// The newest activity in it, Unix ms.
    pub latest_ms: i64,
}

impl Folder {
    /// Sessions this folder offers: scripted runs too unless skipped.
    pub fn count(&self, skip_scripted: bool) -> (usize, usize) {
        if skip_scripted {
            (self.claude, self.codex)
        } else {
            (self.claude + self.claude_scripted, self.codex + self.codex_scripted)
        }
    }
}

/// Per-provider tallies of one plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Offered: not in Baaz, not scripted.
    pub offered: usize,
    /// Scripted runs not in Baaz.
    pub scripted: usize,
    /// Already in Baaz (Baaz's own sessions and earlier imports).
    pub known: usize,
    /// Folders holding at least one offered session.
    pub folders: usize,
}

/// The dry run: what the sheet offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Plan {
    /// Folder rows: Baaz projects first, then newest activity first.
    pub folders: Vec<Folder>,
    /// Every session not in Baaz yet, scripted ones included (the sheet's
    /// option decides), keyed to its folder by `cwd`.
    pub sessions: Vec<Found>,
    /// Claude Code's tallies.
    pub claude: Tally,
    /// Codex's tallies.
    pub codex: Tally,
    /// Files the scan could not read.
    pub unreadable: Vec<PathBuf>,
}

impl Plan {
    /// The tallies for `provider` ([`CLAUDE`] / [`CODEX`]).
    pub fn tally(&self, provider: &str) -> Tally {
        if provider == CODEX { self.codex } else { self.claude }
    }

    /// Scripted runs not in Baaz, both providers.
    pub fn scripted(&self) -> usize {
        self.claude.scripted + self.codex.scripted
    }

    /// The folders ticked by default: the Baaz projects.
    pub fn default_ticked(&self) -> HashSet<String> {
        self.folders.iter().filter(|f| f.project.is_some()).map(|f| f.cwd.clone()).collect()
    }

    /// The folder `cwd` names.
    pub fn folder(&self, cwd: &str) -> Option<&Folder> {
        self.folders.iter().find(|f| f.cwd == cwd)
    }
}

/// The Baaz projects a plan resolves folders against: `(adoption id,
/// root)`. Canonicalised inside [`plan`], off the UI thread.
pub type ProjectRoots = Vec<(String, PathBuf)>;

/// Group `scan` by folder, drop the ids `known` holds (Baaz's own sessions
/// and earlier imports), resolve each folder against `projects` (exact
/// canonical root, never a prefix — [`crate::projects::Projects::resolve`]'s
/// rule) and count. `home` abbreviates parents to `~`. Pure over its
/// inputs: the dry run.
pub fn plan(scan: &Scan, known: &HashSet<String>, projects: &ProjectRoots, home: Option<&Path>) -> Plan {
    let canonical: Vec<(String, PathBuf)> = projects
        .iter()
        .map(|(id, root)| (id.clone(), std::fs::canonicalize(root).unwrap_or_else(|_| root.clone())))
        .collect();
    let mut resolved: HashMap<String, Option<String>> = HashMap::new();
    let mut folders: HashMap<String, Folder> = HashMap::new();
    let mut out = Plan { unreadable: scan.unreadable.clone(), ..Plan::default() };
    for found in &scan.sessions {
        let tally = if found.provider == CODEX { &mut out.codex } else { &mut out.claude };
        if known.contains(&found.session_id) {
            tally.known += 1;
            continue;
        }
        if found.scripted {
            tally.scripted += 1;
        } else {
            tally.offered += 1;
        }
        let cwd = found.cwd.clone().unwrap_or_default();
        let project = resolved
            .entry(cwd.clone())
            .or_insert_with(|| {
                if cwd.is_empty() {
                    return None;
                }
                let path = PathBuf::from(&cwd);
                let path = std::fs::canonicalize(&path).unwrap_or(path);
                canonical.iter().find(|(_, root)| *root == path).map(|(id, _)| id.clone())
            })
            .clone();
        let folder = folders.entry(cwd.clone()).or_insert_with(|| {
            let (name, parent) = folder_names(&cwd, home);
            Folder {
                cwd: cwd.clone(),
                name,
                parent,
                project,
                claude: 0,
                codex: 0,
                claude_scripted: 0,
                codex_scripted: 0,
                latest_ms: 0,
            }
        });
        match (found.provider == CODEX, found.scripted) {
            (false, false) => folder.claude += 1,
            (false, true) => folder.claude_scripted += 1,
            (true, false) => folder.codex += 1,
            (true, true) => folder.codex_scripted += 1,
        }
        folder.latest_ms = folder.latest_ms.max(found.updated_ms);
        out.sessions.push(found.clone());
    }
    let mut folders: Vec<Folder> = folders.into_values().collect();
    folders.sort_by(|a, b| {
        b.project
            .is_some()
            .cmp(&a.project.is_some())
            .then_with(|| b.latest_ms.cmp(&a.latest_ms))
            .then_with(|| a.cwd.cmp(&b.cwd))
    });
    out.claude.folders = folders.iter().filter(|f| f.claude > 0).count();
    out.codex.folders = folders.iter().filter(|f| f.codex > 0).count();
    out.folders = folders;
    out.sessions.sort_by(|a, b| b.updated_ms.cmp(&a.updated_ms).then_with(|| a.session_id.cmp(&b.session_id)));
    out
}

/// A folder's name and its `~`-abbreviated parent.
fn folder_names(cwd: &str, home: Option<&Path>) -> (String, String) {
    if cwd.is_empty() {
        return ("Unknown folder".to_owned(), String::new());
    }
    let path = Path::new(cwd);
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| cwd.to_owned());
    let parent = path.parent().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = match home.map(|h| h.to_string_lossy().into_owned()) {
        Some(home) if !home.is_empty() && parent == home => "~".to_owned(),
        Some(home) if !home.is_empty() && parent.starts_with(&format!("{home}/")) => {
            format!("~{}", &parent[home.len()..])
        }
        _ => parent,
    };
    (name, parent)
}

/// What the sheet's controls say.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Choice {
    /// "Skip scripted runs".
    pub skip_scripted: bool,
    /// The ticked folders, by cwd.
    pub ticked: HashSet<String>,
}

impl Choice {
    /// The approved defaults: Baaz projects ticked, scripted runs skipped.
    pub fn defaults(plan: &Plan) -> Self {
        Self { skip_scripted: true, ticked: plan.default_ticked() }
    }
}

/// The sessions `choice` picks from `plan`, newest first.
pub fn chosen<'a>(plan: &'a Plan, choice: &Choice) -> Vec<&'a Found> {
    plan.sessions
        .iter()
        .filter(|found| !(choice.skip_scripted && found.scripted))
        .filter(|found| choice.ticked.contains(found.cwd.as_deref().unwrap_or("")))
        .collect()
}

// ---------------------------------------------------------------- execute

/// Count a transcript's turns (real prompts) by reading it whole, line by
/// line, parsing only the lines that can be one. `None` when unreadable.
pub fn count_turns(provider: &str, path: &Path) -> Option<u64> {
    let file = std::fs::File::open(path).ok()?;
    let reader = std::io::BufReader::new(file);
    let mut turns = 0;
    for line in reader.split(b'\n') {
        let line = line.ok()?;
        let needle: &[u8] = if provider == CODEX { b"\"user_message\"" } else { b"\"user\"" };
        if !contains(&line, needle) || (provider != CODEX && contains(&line, b"\"tool_result\"")) {
            continue;
        }
        let Ok(value) = serde_json::from_slice::<serde_json::Value>(&line) else { continue };
        let prompt = if provider == CODEX { codex_prompt(&value) } else { claude_prompt(&value) };
        if prompt.is_some() {
            turns += 1;
        }
    }
    Some(turns)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|window| window == needle)
}

/// The record one imported session becomes: `origin: imported`, workspace
/// the folder it ran in, the project it resolves to, the scan's times, the
/// counted turns (never below one: a session the scan offered has a
/// prompt), and the first prompt. No title: the label ladder falls back to
/// the first prompt until the auto-title lands.
pub fn record_for(found: &Found, project: Option<String>, turns: u64) -> ProviderSessionRecord {
    ProviderSessionRecord {
        provider: found.provider.to_owned(),
        session_id: found.session_id.clone(),
        workspace: found.cwd.clone(),
        project,
        created_ms: found.created_ms,
        updated_ms: found.updated_ms,
        turns: turns.max(1),
        title: None,
        first_prompt: found.first_prompt.clone(),
        handoff_to: None,
        handoff_from: None,
        handoff_from_provider: None,
        handoff_title: None,
        display_texts: HashMap::new(),
        origin: Some(ORIGIN_IMPORTED.to_owned()),
    }
}

/// What one batch produced.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BatchResult {
    /// The new records, in batch order.
    pub records: Vec<ProviderSessionRecord>,
    /// Files that could not be read now, by path.
    pub unreadable: Vec<PathBuf>,
}

/// Turn one batch of chosen sessions into records, skipping any id `known`
/// already holds (a re-run writes nothing: idempotent). Blocking — it reads
/// each file whole for its turns; run it off the UI thread.
pub fn execute_batch(batch: &[(Found, Option<String>)], known: &HashSet<String>) -> BatchResult {
    let mut out = BatchResult::default();
    for (found, project) in batch {
        if known.contains(&found.session_id) {
            continue;
        }
        match count_turns(found.provider, &found.path) {
            Some(turns) => out.records.push(record_for(found, project.clone(), turns)),
            None => out.unreadable.push(found.path.clone()),
        }
    }
    out
}

/// Land a batch in the store: inserts only ids it does not hold (never
/// overwrites a Baaz session's record). Returns the ids added.
pub fn land(store: &mut ProviderSessionStore, records: Vec<ProviderSessionRecord>) -> Vec<String> {
    let mut added = Vec::new();
    for record in records {
        if store.contains_key(&record.session_id) {
            continue;
        }
        added.push(record.session_id.clone());
        store.insert(record.session_id.clone(), record);
    }
    added
}

// ---------------------------------------------------------------- journal

/// One session a run added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalSession {
    /// [`CLAUDE`] or [`CODEX`].
    pub provider: String,
    /// The session id.
    pub session_id: String,
}

/// One import run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalRun {
    /// The run's id (`import-<ms>`).
    pub id: String,
    /// When it ran, Unix ms.
    pub at_ms: i64,
    /// Exactly the sessions it added — what Undo removes.
    pub sessions: Vec<JournalSession>,
}

/// `import-journal.json`: every run not undone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Journal {
    /// Oldest first.
    #[serde(default)]
    pub runs: Vec<JournalRun>,
}

impl Journal {
    /// Open a run at `at_ms`; the caller adds sessions as batches land.
    pub fn begin(&mut self, at_ms: i64) -> String {
        let mut id = format!("import-{at_ms}");
        while self.runs.iter().any(|run| run.id == id) {
            id.push('+');
        }
        self.runs.push(JournalRun { id: id.clone(), at_ms, sessions: Vec::new() });
        id
    }

    /// Note sessions `run` added.
    pub fn note(&mut self, run: &str, sessions: impl IntoIterator<Item = JournalSession>) {
        if let Some(run) = self.runs.iter_mut().find(|r| r.id == run) {
            run.sessions.extend(sessions);
        }
    }

    /// Drop runs that added nothing (a re-run of an imported plan).
    pub fn drop_empty(&mut self) {
        self.runs.retain(|run| !run.sessions.is_empty());
    }

    /// How many `provider` sessions every run added, and when the newest
    /// such run was: the card's "122 imported 30 Oct".
    pub fn imported(&self, provider: &str, store: &ProviderSessionStore) -> (usize, Option<i64>) {
        let mut count = 0;
        let mut latest = None;
        for run in &self.runs {
            let n = run
                .sessions
                .iter()
                .filter(|s| s.provider == provider && store.get(&s.session_id).is_some_and(|r| r.is_imported()))
                .count();
            if n > 0 {
                count += n;
                latest = Some(run.at_ms);
            }
        }
        (count, latest)
    }
}

/// The journal's path under the state dir.
pub fn journal_path(state_dir: &Path) -> PathBuf {
    state_dir.join(JOURNAL_FILE_NAME)
}

/// Read the journal; missing or unparseable is empty.
pub fn read_journal(state_dir: &Path) -> Journal {
    crate::store::read_json(&journal_path(state_dir))
}

/// Write the journal atomically. Best-effort: an unwritable journal loses
/// the tally and the Undo, never a session.
pub fn write_journal(state_dir: &Path, journal: &Journal) {
    if let Ok(text) = serde_json::to_vec_pretty(journal) {
        let _ = std::fs::create_dir_all(state_dir);
        let _ = crate::store::write_atomic(&journal_path(state_dir), &text);
    }
}

/// Undo `run`: remove exactly the records it added — only while they are
/// still imported records (a session Baaz itself registered since is never
/// touched) — and the run itself. Never a file. Returns the ids removed.
pub fn undo(store: &mut ProviderSessionStore, journal: &mut Journal, run: &str) -> Vec<String> {
    let Some(at) = journal.runs.iter().position(|r| r.id == run) else { return Vec::new() };
    let run = journal.runs.remove(at);
    let mut removed = Vec::new();
    for session in run.sessions {
        if store.get(&session.session_id).is_some_and(|r| r.is_imported()) {
            store.remove(&session.session_id);
            removed.push(session.session_id);
        }
    }
    removed
}

// --------------------------------------------------------------- dry run

/// `baaz --import-dry-run`: scan the owner's homes and print the plan as one
/// JSON object — per-provider tallies, the folders (top first), unreadable
/// files and the scan time. Read-only: it reads Baaz's own registry to say
/// what is already in Baaz, writes nothing anywhere, and resolves folders
/// against no projects (the projects file is read, never written).
pub fn print_dry_run() {
    let home = owner_home();
    let at = std::time::Instant::now();
    let scan = scan_home(&home);
    let scan_ms = at.elapsed().as_secs_f64() * 1000.0;
    let known: HashSet<String> = crate::provider_sessions::read().into_keys().collect();
    let projects: ProjectRoots = crate::projects::read()
        .sorted()
        .into_iter()
        .map(|p| (p.id.clone(), p.root.clone()))
        .collect();
    let plan = plan(&scan, &known, &projects, Some(&home));
    let folders: Vec<serde_json::Value> = plan
        .folders
        .iter()
        .map(|f| {
            serde_json::json!({
                "folder": f.cwd, "project": f.project.is_some(),
                "claude": f.claude, "claudeScripted": f.claude_scripted,
                "codex": f.codex, "codexScripted": f.codex_scripted,
            })
        })
        .collect();
    let tally = |t: Tally| serde_json::json!({"offered": t.offered, "scripted": t.scripted, "known": t.known, "folders": t.folders});
    let out = serde_json::json!({
        "home": home,
        "scanMs": (scan_ms * 10.0).round() / 10.0,
        "files": scan.sessions.len() + scan.unreadable.len() + scan.empty,
        "empty": scan.empty,
        "unreadable": scan.unreadable,
        "claude": tally(plan.claude),
        "codex": tally(plan.codex),
        "folders": folders,
    });
    println!("{out}");
}

// ------------------------------------------------------------- fixtures

/// Write `count` sessions into a fixture owner home, in each CLI's real
/// on-disk shape: Claude Code transcripts under `.claude/projects/<slug>/`
/// and Codex rollouts under `.codex/sessions/2026/10/…`, spread over
/// `folders` cwds, every seventh Claude one a `claude -p` run and every
/// Codex one a `codex exec` run on odd indices. Each carries a tool result
/// of `pad` bytes between its prompt and answer, so the head/tail reads are
/// measured against files that are mostly body. The perf budget's fixture
/// and the tests'.
pub fn write_fixture_home(home: &Path, count: usize, folders: usize, pad: usize) {
    let _ = std::fs::remove_dir_all(home.join(".claude"));
    let _ = std::fs::remove_dir_all(home.join(".codex"));
    let folders = folders.max(1);
    for index in 0..count {
        let cwd = format!("/work/folder-{}", index % folders);
        let ts = |minute: usize| format!("2026-10-{:02}T10:{:02}:00.000Z", 1 + index % 28, minute % 60);
        if index % 4 != 3 {
            let id = format!("{index:08}-aaaa-4000-8000-00000000c1a0");
            let slug = cwd.replace('/', "-");
            let dir = home.join(".claude").join("projects").join(&slug);
            let _ = std::fs::create_dir_all(&dir);
            let entry = if index % 7 == 6 { "sdk-cli" } else { "cli" };
            let lines = [
                serde_json::json!({"type":"queue-operation","operation":"enqueue","timestamp":ts(0),"sessionId":id}),
                serde_json::json!({"type":"user","isMeta":true,"message":{"role":"user","content":"<local-command-caveat>x"},"cwd":cwd,"entrypoint":entry,"timestamp":ts(1),"sessionId":id}),
                serde_json::json!({"type":"user","message":{"role":"user","content":format!("fixture prompt {index} about the parser")},"cwd":cwd,"entrypoint":entry,"timestamp":ts(2),"sessionId":id}),
                serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x".repeat(pad)}]},"cwd":cwd,"timestamp":ts(3),"sessionId":id}),
                serde_json::json!({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":format!("fixture answer {index}")}]},"cwd":cwd,"timestamp":ts(4),"sessionId":id}),
                serde_json::json!({"type":"user","message":{"role":"user","content":"and a second prompt"},"cwd":cwd,"entrypoint":entry,"timestamp":ts(5),"sessionId":id}),
            ];
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            let _ = std::fs::write(dir.join(format!("{id}.jsonl")), text);
        } else {
            let id = format!("{index:08}-bbbb-7000-8000-00000000c0de");
            let dir = home.join(".codex").join("sessions").join("2026").join("10").join(format!("{:02}", 1 + index % 28));
            let _ = std::fs::create_dir_all(&dir);
            let exec = index % 2 == 1;
            let lines = [
                serde_json::json!({"timestamp":ts(0),"type":"session_meta","payload":{"id":id,"cwd":cwd,"originator": if exec {"codex_exec"} else {"codex_cli_rs"},"source": if exec {"exec"} else {"cli"},"base_instructions":{"text":"y".repeat(2048)}}}),
                serde_json::json!({"timestamp":ts(1),"type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>x"}]}}),
                serde_json::json!({"timestamp":ts(2),"type":"event_msg","payload":{"type":"user_message","message":format!("codex fixture prompt {index}")}}),
                serde_json::json!({"timestamp":ts(3),"type":"response_item","payload":{"type":"function_call_output","output":"z".repeat(pad)}}),
                serde_json::json!({"timestamp":ts(4),"type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":format!("codex fixture answer {index}")}]}}),
            ];
            let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
            let _ = std::fs::write(dir.join(format!("rollout-2026-10-01T10-00-00-{id}.jsonl")), text);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp root holding a fixture owner home. Never the real `HOME`.
    fn temp(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "baaz-import-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp root");
        root
    }

    fn write(path: &Path, lines: &[serde_json::Value]) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(path, text).expect("write");
    }

    /// One Claude transcript, one `claude -p` run, one meta-only (empty),
    /// one garbage file; one Codex exec rollout and one interactive one.
    fn seed(home: &Path) -> (PathBuf, PathBuf) {
        let projects = home.join(".claude").join("projects");
        write(
            &projects.join("-work-app").join("c-real.jsonl"),
            &[
                serde_json::json!({"type":"summary","summary":"x"}),
                serde_json::json!({"type":"user","isMeta":true,"message":{"content":"<command-name>/clear</command-name>"},"cwd":"/work/app","timestamp":"2026-10-01T10:00:00Z","entrypoint":"claude-desktop"}),
                serde_json::json!({"type":"user","message":{"content":"<local-command-stdout>ok</local-command-stdout>"},"cwd":"/work/app","timestamp":"2026-10-01T10:00:01Z"}),
                serde_json::json!({"type":"user","isSidechain":true,"message":{"content":"a subagent's brief"},"cwd":"/work/app","timestamp":"2026-10-01T10:00:02Z"}),
                serde_json::json!({"type":"user","message":{"content":[{"type":"text","text":"Fix the ledger\nchart please"}]},"cwd":"/work/app","entrypoint":"claude-desktop","timestamp":"2026-10-01T10:00:03Z"}),
                serde_json::json!({"type":"user","message":{"content":[{"type":"tool_result","content":"Fix nothing"}]},"cwd":"/work/app","timestamp":"2026-10-01T10:00:04Z"}),
                serde_json::json!({"type":"assistant","message":{"content":[{"type":"text","text":"Done."}]},"timestamp":"2026-10-01T10:00:05Z"}),
                serde_json::json!({"type":"user","message":{"content":"and the legend"},"cwd":"/work/app","timestamp":"2026-10-02T09:00:00Z"}),
            ],
        );
        write(
            &projects.join("-work-app").join("c-sdk.jsonl"),
            &[serde_json::json!({"type":"user","message":{"content":"summarise this"},"cwd":"/work/app","entrypoint":"sdk-cli","timestamp":"2026-10-03T10:00:00Z"})],
        );
        write(
            &projects.join("-private-tmp-x").join("c-tmp.jsonl"),
            &[serde_json::json!({"type":"user","message":{"content":"probe"},"cwd":"/private/tmp/x","entrypoint":"cli","timestamp":"2026-10-03T10:00:00Z"})],
        );
        write(
            &projects.join("-work-app").join("c-empty.jsonl"),
            &[serde_json::json!({"type":"queue-operation","timestamp":"2026-10-03T10:00:00Z"})],
        );
        let garbage = projects.join("-work-app").join("c-garbage.jsonl");
        std::fs::write(&garbage, "this is not json\nnor this\n").expect("garbage");
        let day = home.join(".codex").join("sessions").join("2026").join("10").join("04");
        write(
            &day.join("rollout-2026-10-04T10-00-00-01a15e17-bdc8-7393-bb12-89d784546a80.jsonl"),
            &[
                serde_json::json!({"timestamp":"2026-10-04T10:00:00Z","type":"session_meta","payload":{"id":"01a15e17-bdc8-7393-bb12-89d784546a80","cwd":"/work/art","originator":"codex_exec","source":"exec"}}),
                serde_json::json!({"timestamp":"2026-10-04T10:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"make one image"}}),
            ],
        );
        write(
            &day.join("rollout-2026-10-04T11-00-00-01a15e17-bdc8-7393-bb12-000000000002.jsonl"),
            &[
                serde_json::json!({"timestamp":"2026-10-04T11:00:00Z","type":"session_meta","payload":{"id":"01a15e17-bdc8-7393-bb12-000000000002","cwd":"/work/app","originator":"codex_cli_rs","source":"cli"}}),
                serde_json::json!({"timestamp":"2026-10-04T11:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"<environment_context>"}]}}),
                serde_json::json!({"timestamp":"2026-10-04T11:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"rename the router"}}),
                serde_json::json!({"timestamp":"2026-10-04T11:30:00Z","type":"event_msg","payload":{"type":"user_message","message":"and test it"}}),
            ],
        );
        (projects, garbage)
    }

    fn by_id<'a>(scan: &'a Scan, id: &str) -> &'a Found {
        scan.sessions.iter().find(|f| f.session_id == id).unwrap_or_else(|| panic!("{id} scanned"))
    }

    #[test]
    fn the_scan_reads_both_formats_and_marks_scripted_runs() {
        let home = temp("scan");
        let (_, garbage) = seed(&home);
        let scan = scan_home(&home);
        assert_eq!(scan.sessions.len(), 5, "{:#?}", scan.sessions);
        let real = by_id(&scan, "c-real");
        assert_eq!(real.provider, CLAUDE);
        assert_eq!(real.cwd.as_deref(), Some("/work/app"));
        assert_eq!(
            real.first_prompt.as_deref(),
            Some("Fix the ledger chart please"),
            "not the meta line, not the command output, not the sidechain, one line"
        );
        assert!(!real.scripted, "claude-desktop is a person's session");
        assert_eq!(real.created_ms, chrono::DateTime::parse_from_rfc3339("2026-10-01T10:00:00Z").unwrap().timestamp_millis());
        assert_eq!(
            real.updated_ms,
            chrono::DateTime::parse_from_rfc3339("2026-10-02T09:00:00Z").unwrap().timestamp_millis(),
            "the last timestamp, read from the tail"
        );
        assert!(by_id(&scan, "c-sdk").scripted, "entrypoint sdk-cli is `claude -p`");
        assert!(by_id(&scan, "c-tmp").scripted, "a run under /private/tmp is scripted");
        let exec = by_id(&scan, "01a15e17-bdc8-7393-bb12-89d784546a80");
        assert_eq!(exec.provider, CODEX);
        assert!(exec.scripted, "originator codex_exec is `codex exec`");
        assert_eq!(exec.cwd.as_deref(), Some("/work/art"));
        let cli = by_id(&scan, "01a15e17-bdc8-7393-bb12-000000000002");
        assert!(!cli.scripted);
        assert_eq!(cli.first_prompt.as_deref(), Some("rename the router"), "the user_message, not the context block");
        assert_eq!(scan.empty, 1, "a session with no prompt is counted, never offered");
        assert_eq!(scan.unreadable, vec![garbage], "an unparseable file is reported by path");
    }

    #[test]
    fn a_file_that_cannot_be_opened_is_reported_never_dropped() {
        let home = temp("unreadable");
        seed(&home);
        let dir = home.join(".claude").join("projects").join("-work-app");
        // A directory named like a transcript cannot be read as one.
        std::fs::create_dir_all(dir.join("c-dir.jsonl")).expect("dir");
        let scan = scan_home(&home);
        assert!(scan.unreadable.iter().any(|p| p.ends_with("c-dir.jsonl")), "{:?}", scan.unreadable);
    }

    #[test]
    fn the_plan_groups_by_folder_ticks_projects_and_skips_known_ids() {
        let home = temp("plan");
        seed(&home);
        std::fs::create_dir_all(home.join("work-app")).expect("project root");
        let scan = scan_home(&home);
        // `/work/app` cannot be canonicalised here; the project's root is
        // given exactly as the sessions name it.
        let projects: ProjectRoots = vec![("p-app".to_owned(), PathBuf::from("/work/app"))];
        let known: HashSet<String> = ["01a15e17-bdc8-7393-bb12-000000000002".to_owned()].into_iter().collect();
        let plan = plan(&scan, &known, &projects, Some(Path::new("/work")));
        assert_eq!(plan.claude, Tally { offered: 1, scripted: 2, known: 0, folders: 1 });
        assert_eq!(plan.codex, Tally { offered: 0, scripted: 1, known: 1, folders: 0 });
        let names: Vec<&str> = plan.folders.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names[0], "app", "a Baaz project sorts first: {names:?}");
        let app = plan.folder("/work/app").expect("app folder");
        assert_eq!(app.project.as_deref(), Some("p-app"));
        assert_eq!(app.parent, "~", "the parent is ~-abbreviated");
        assert_eq!((app.claude, app.claude_scripted, app.codex), (1, 1, 0), "the known Codex id is not offered");
        let art = plan.folder("/work/art").expect("art folder");
        assert_eq!(art.project, None);
        assert_eq!(art.count(true), (0, 0), "all scripted: nothing offered with the box ticked");
        assert_eq!(art.count(false), (0, 1));
        // The defaults: projects ticked, scripted skipped → just c-real.
        let choice = Choice::defaults(&plan);
        let picked: Vec<&str> = chosen(&plan, &choice).iter().map(|f| f.session_id.as_str()).collect();
        assert_eq!(picked, vec!["c-real"]);
        let all = Choice { skip_scripted: false, ticked: plan.folders.iter().map(|f| f.cwd.clone()).collect() };
        assert_eq!(chosen(&plan, &all).len(), 4, "every session not in Baaz, scripted too");
    }

    #[test]
    fn execute_is_idempotent_and_undo_removes_exactly_the_journal_ids() {
        let home = temp("execute");
        let state = temp("execute-state");
        seed(&home);
        let scan = scan_home(&home);
        let plan = plan(&scan, &HashSet::new(), &vec![("p-app".into(), PathBuf::from("/work/app"))], None);
        let choice = Choice { skip_scripted: true, ticked: plan.folders.iter().map(|f| f.cwd.clone()).collect() };
        let batch: Vec<(Found, Option<String>)> = chosen(&plan, &choice)
            .into_iter()
            .map(|f| (f.clone(), plan.folder(f.cwd.as_deref().unwrap_or("")).and_then(|d| d.project.clone())))
            .collect();
        assert_eq!(batch.len(), 2);
        // A Baaz session already in the store.
        let mut store = ProviderSessionStore::new();
        crate::provider_sessions::upsert_open(&mut store, CLAUDE, "baaz-own", None, None, None);
        let known: HashSet<String> = store.keys().cloned().collect();
        let result = execute_batch(&batch, &known);
        assert!(result.unreadable.is_empty());
        let real = result.records.iter().find(|r| r.session_id == "c-real").expect("record");
        assert!(real.is_imported());
        assert_eq!(real.turns, 2, "two real prompts: not the meta, command, sidechain or tool result lines");
        assert_eq!(real.workspace.as_deref(), Some("/work/app"));
        assert_eq!(real.project.as_deref(), Some("p-app"));
        assert_eq!(real.first_prompt.as_deref(), Some("Fix the ledger chart please"));
        assert_eq!(real.title, None);
        let codex = result.records.iter().find(|r| r.provider == CODEX).expect("codex record");
        assert_eq!(codex.turns, 2);
        let mut journal = Journal::default();
        let run = journal.begin(1_790_000_000_000);
        let added = land(&mut store, result.records);
        journal.note(&run, added.iter().map(|id| JournalSession { provider: store[id].provider.clone(), session_id: id.clone() }));
        write_journal(&state, &journal);
        assert_eq!(read_journal(&state), journal, "the journal round-trips");
        assert_eq!(journal.imported(CLAUDE, &store), (1, Some(1_790_000_000_000)));
        // Re-running the same plan writes nothing.
        let known: HashSet<String> = store.keys().cloned().collect();
        let again = execute_batch(&batch, &known);
        assert!(again.records.is_empty(), "a re-run writes nothing");
        let rerun = journal.begin(1_790_000_000_001);
        let added_again = land(&mut store, again.records);
        journal.note(&rerun, added_again.iter().map(|id| JournalSession { provider: CLAUDE.into(), session_id: id.clone() }));
        journal.drop_empty();
        assert_eq!(journal.runs.len(), 1, "an empty run leaves no trace");
        // A Baaz session that later took an imported id's place is never
        // removed by Undo; here the store also keeps Baaz's own record.
        let removed = undo(&mut store, &mut journal, &run);
        let mut removed_sorted = removed.clone();
        removed_sorted.sort();
        let mut expected = added.clone();
        expected.sort();
        assert_eq!(removed_sorted, expected, "exactly the journal's ids");
        assert!(store.contains_key("baaz-own"), "Baaz's own session is untouched");
        assert_eq!(store.len(), 1);
        assert!(journal.runs.is_empty());
        // Undo never touches a file.
        assert!(home.join(".claude/projects/-work-app/c-real.jsonl").exists());
        assert_eq!(provider_codex::rollout::stored_rollouts(&home.join(".codex")).len(), 2);
    }

    #[test]
    fn undo_spares_a_record_that_is_no_longer_imported() {
        let mut store = ProviderSessionStore::new();
        let found = Found {
            provider: CLAUDE,
            session_id: "s1".into(),
            path: PathBuf::from("/nowhere"),
            cwd: None,
            created_ms: 1,
            updated_ms: 2,
            first_prompt: Some("hi".into()),
            scripted: false,
        };
        let mut record = record_for(&found, None, 3);
        record.origin = None;
        store.insert("s1".into(), record);
        let mut journal = Journal::default();
        let run = journal.begin(5);
        journal.note(&run, [JournalSession { provider: CLAUDE.into(), session_id: "s1".into() }]);
        assert!(undo(&mut store, &mut journal, &run).is_empty());
        assert!(store.contains_key("s1"));
    }

    #[test]
    fn land_never_overwrites_a_record() {
        let mut store = ProviderSessionStore::new();
        crate::provider_sessions::upsert_open(&mut store, CLAUDE, "s1", Some("/mine".into()), None, None);
        let found = Found {
            provider: CLAUDE,
            session_id: "s1".into(),
            path: PathBuf::from("/nowhere"),
            cwd: Some("/theirs".into()),
            created_ms: 1,
            updated_ms: 2,
            first_prompt: Some("hi".into()),
            scripted: false,
        };
        assert!(land(&mut store, vec![record_for(&found, None, 1)]).is_empty());
        assert_eq!(store["s1"].workspace.as_deref(), Some("/mine"));
        assert!(!store["s1"].is_imported());
    }

    #[test]
    fn the_fixture_home_scans_to_its_shape() {
        let home = temp("fixture");
        write_fixture_home(&home, 40, 5, 4096);
        let scan = scan_home(&home);
        assert_eq!(scan.sessions.len(), 40);
        assert!(scan.unreadable.is_empty());
        assert!(scan.sessions.iter().any(|f| f.scripted) && scan.sessions.iter().any(|f| !f.scripted));
        let plan = plan(&scan, &HashSet::new(), &Vec::new(), None);
        assert_eq!(plan.folders.len(), 5);
    }

    #[test]
    fn temp_dirs_are_scripted_and_lookalikes_are_not() {
        let roots = vec!["/tmp/".to_owned(), "/private/var/".to_owned()];
        assert!(under_temp("/tmp", &roots));
        assert!(under_temp("/tmp/x/y", &roots));
        assert!(under_temp("/private/var/folders/q", &roots));
        assert!(!under_temp("/tmpfoo", &roots));
        assert!(!under_temp("/Users/a/tmp", &roots));
    }
}
