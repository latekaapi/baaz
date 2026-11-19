//! Tool cards: the verb table and its tense, the baaz terminal tools, and
//! wall-clock durations (docs/81 B F-1, F-2).

use std::time::Instant;

use aui_protocol::{Block, DiffStat, ToolBody, ToolKind, ToolStatus};
use serde_json::Value;

/// The running verb of a baaz terminal tool card (D51).
pub const TERMINAL_RUNNING_VERB: &str = "Running in terminal";

/// The settled verb of a baaz terminal tool card (D51).
pub const TERMINAL_RAN_VERB: &str = "Ran in terminal";

/// The verb a card that never ran reads, whatever its kind: the person or
/// the provider refused it.
pub const DENIED_VERB: &str = "Denied";

/// Whether `tool` is one of the baaz terminal tools (`docs/14-terminal.md`
/// §4), by its bare name. A call to one folds to a shell card with
/// terminal verbs (D51) — the command in the header, the run's output as
/// the body — never the raw request/response JSON. The one list: a change
/// to Baaz's own terminal tools is made here.
pub fn is_terminal_tool(tool: &str) -> bool {
    matches!(
        tool,
        "terminal_list"
            | "terminal_open"
            | "terminal_run"
            | "terminal_read"
            | "terminal_screen"
            | "terminal_send"
            | "terminal_close"
    )
}

/// Wall-clock milliseconds since `start`, saturating at `u64`: the clock a
/// card falls back to when the wire timed nothing.
pub fn wall_ms_since(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A card's two verbs: what it reads while it has not finished, and once it
/// has. A card that never ran reads [`DENIED_VERB`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Verbs {
    /// Pending or running: the call has not run yet ("Run", "Edit").
    pub present: &'static str,
    /// Finished, either way ("Ran", "Edited").
    pub past: &'static str,
}

impl Verbs {
    /// The one table: every provider's cards read the same verbs for the
    /// same kind of work.
    pub fn of(kind: &ToolKind) -> Self {
        let (present, past) = match kind {
            ToolKind::Shell | ToolKind::Mcp { .. } => ("Run", "Ran"),
            ToolKind::Read => ("Read", "Read"),
            ToolKind::Edit => ("Edit", "Edited"),
            ToolKind::Write => ("Write", "Wrote"),
            ToolKind::Search => ("Search", "Searched"),
            ToolKind::Web => ("Fetch", "Fetched"),
            ToolKind::Browser => ("Browse", "Browsed"),
            ToolKind::SubAgent => ("Delegate", "Delegated"),
        };
        Self { present, past }
    }

    /// A baaz terminal tool's verbs.
    pub const TERMINAL: Self = Self { present: TERMINAL_RUNNING_VERB, past: TERMINAL_RAN_VERB };

    /// A web search: a web card that searches rather than fetches.
    pub const WEB_SEARCH: Self = Self { present: "Search the web", past: "Searched the web" };

    /// Input written to a running process (an interactive shell).
    pub const SEND_INPUT: Self = Self { present: "Send input", past: "Sent input" };

    /// A verb that reads the same in every tense ("Loaded skill").
    pub const fn fixed(verb: &'static str) -> Self {
        Self { present: verb, past: verb }
    }

    /// The verb a card in `status` reads.
    pub fn verb(&self, status: ToolStatus) -> &'static str {
        match status {
            ToolStatus::Pending | ToolStatus::Running => self.present,
            ToolStatus::Cancelled => DENIED_VERB,
            ToolStatus::Success | ToolStatus::Error => self.past,
        }
    }
    /// The verb a `kind` card reading `verb` reads once it is `status`: the
    /// same pair in the other tense (a card settled by its turn's end, not
    /// by its own result). A verb no pair knows ("Loaded skill", "$") keeps
    /// its words.
    pub fn retense(kind: &ToolKind, verb: &str, status: ToolStatus) -> String {
        [Self::of(kind), Self::TERMINAL, Self::WEB_SEARCH, Self::SEND_INPUT]
            .into_iter()
            .find(|verbs| verbs.present == verb || verbs.past == verb || verb == DENIED_VERB && verbs == &Self::of(kind))
            .map_or(verb, |verbs| verbs.verb(status))
            .to_owned()
    }
}

/// One tool call as a fold reports it: what kind of work, on what, how it
/// stands. [`ToolEvent::block`] presents it — the verb comes from the
/// table, never from the fold.
#[derive(Clone, Debug, PartialEq)]
pub struct ToolEvent {
    /// The call's stable id (the wire's item or tool-use id).
    pub id: String,
    /// What kind of work it is.
    pub kind: ToolKind,
    /// The verbs it reads; [`Verbs::of`] the kind unless a fold names a
    /// different pair (a terminal tool, a web search, a skill load).
    pub verbs: Verbs,
    /// What it works on: the command, the path, the query.
    pub target: String,
    /// How it stands.
    pub status: ToolStatus,
    /// How long it took, when known.
    pub duration_ms: Option<u64>,
    /// What it produced.
    pub body: ToolBody,
    /// The `+N/−N` chip of a file change.
    pub diff_stat: Option<DiffStat>,
}

impl ToolEvent {
    /// A call with no body, no duration and the kind's own verbs.
    pub fn new(id: impl Into<String>, kind: ToolKind, target: impl Into<String>, status: ToolStatus) -> Self {
        let verbs = Verbs::of(&kind);
        Self {
            id: id.into(),
            kind,
            verbs,
            target: target.into(),
            status,
            duration_ms: None,
            body: ToolBody::None,
            diff_stat: None,
        }
    }

    /// The same call reading `verbs`.
    pub fn verbs(mut self, verbs: Verbs) -> Self {
        self.verbs = verbs;
        self
    }

    /// The same call with `body`.
    pub fn body(mut self, body: ToolBody) -> Self {
        self.body = body;
        self
    }

    /// The same call timed at `duration_ms`.
    pub fn duration(mut self, duration_ms: Option<u64>) -> Self {
        self.duration_ms = duration_ms;
        self
    }

    /// The same call with a diff chip.
    pub fn diff_stat(mut self, stat: Option<DiffStat>) -> Self {
        self.diff_stat = stat;
        self
    }

    /// The card.
    pub fn block(self) -> Block {
        Block::ToolCall {
            id: self.id,
            verb: self.verbs.verb(self.status).to_owned(),
            kind: self.kind,
            target: self.target,
            status: self.status,
            duration_ms: self.duration_ms,
            body: self.body,
            diff_stat: self.diff_stat,
        }
    }
}

/// One terminal tool's JSON answer (`docs/14-terminal.md` §4): `{tab,
/// block, status, exit_code?, duration_ms, output, …}`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TerminalAnswer {
    /// The run's output, when the answer carries one (`terminal_run`,
    /// `terminal_read`); `None` for list, open, send and close.
    pub output_lines: Option<Vec<String>>,
    /// The run's exit code, when it reported one.
    pub exit_code: Option<i32>,
    /// The tab it ran in.
    pub tab: Option<String>,
    /// How long it ran.
    pub duration_ms: Option<u64>,
}

impl TerminalAnswer {
    /// Read the answer off a result's text. `None` when the text is not a
    /// JSON object — the card then bodies the text itself, never a forged
    /// parse.
    pub fn parse(text: &str) -> Option<Self> {
        let value = serde_json::from_str::<Value>(text.trim()).ok()?;
        let object = value.as_object()?;
        Some(Self {
            output_lines: object.get("output").and_then(Value::as_str).map(split_lines),
            exit_code: object.get("exit_code").and_then(Value::as_i64).and_then(|code| i32::try_from(code).ok()),
            tab: object
                .get("tab")
                .and_then(Value::as_str)
                .filter(|tab| !tab.trim().is_empty())
                .map(str::to_owned),
            duration_ms: object.get("duration_ms").and_then(Value::as_u64),
        })
    }
}

/// A terminal card's header: the command, then the tab it ran in when the
/// tab says something the command does not.
pub fn terminal_target(command: String, tab: Option<&str>) -> String {
    match tab {
        Some(tab) if tab.trim() != command.trim() => format!("{command} · {tab}"),
        _ => command,
    }
}

/// A terminal call's card: a shell card with the terminal verbs, the
/// command (and tab) as its header and the run's output as its body.
pub fn terminal_event(
    id: impl Into<String>,
    target: String,
    status: ToolStatus,
    output_lines: Vec<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u64>,
) -> ToolEvent {
    ToolEvent::new(id, ToolKind::Shell, target, status)
        .verbs(Verbs::TERMINAL)
        .body(ToolBody::Shell { output_lines, exit_code, live: false })
        .duration(duration_ms)
}

/// `text` as display lines.
pub fn split_lines(text: &str) -> Vec<String> {
    text.lines().map(str::to_owned).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_table_reads_every_kind_in_both_tenses_and_denied() {
        let shell = Verbs::of(&ToolKind::Shell);
        assert_eq!(shell.verb(ToolStatus::Running), "Run");
        assert_eq!(shell.verb(ToolStatus::Error), "Ran");
        assert_eq!(shell.verb(ToolStatus::Cancelled), "Denied");
        assert_eq!(Verbs::of(&ToolKind::Write).verb(ToolStatus::Success), "Wrote");
        assert_eq!(Verbs::TERMINAL.verb(ToolStatus::Pending), TERMINAL_RUNNING_VERB);
        assert_eq!(Verbs::fixed("Loaded skill").verb(ToolStatus::Success), "Loaded skill");
        assert_eq!(Verbs::retense(&ToolKind::Shell, "Run", ToolStatus::Success), "Ran");
        assert_eq!(Verbs::retense(&ToolKind::Read, "Read", ToolStatus::Cancelled), "Denied");
        assert_eq!(Verbs::retense(&ToolKind::Shell, TERMINAL_RUNNING_VERB, ToolStatus::Error), TERMINAL_RAN_VERB);
        assert_eq!(Verbs::retense(&ToolKind::Read, "Loaded skill", ToolStatus::Success), "Loaded skill");
    }

    #[test]
    fn a_terminal_answer_parses_or_is_not_one() {
        let answer = TerminalAnswer::parse(r#"{"tab":"t1","exit_code":2,"duration_ms":40,"output":"a\nb"}"#).unwrap();
        assert_eq!(answer.output_lines, Some(vec!["a".into(), "b".into()]));
        assert_eq!((answer.exit_code, answer.tab.as_deref(), answer.duration_ms), (Some(2), Some("t1"), Some(40)));
        assert!(TerminalAnswer::parse("plain words").is_none());
        assert_eq!(terminal_target("ls".into(), Some("t1")), "ls · t1");
        assert_eq!(terminal_target("t1".into(), Some("t1")), "t1");
    }

    #[test]
    fn baaz_terminal_tools_are_one_list() {
        assert!(is_terminal_tool("terminal_run"));
        assert!(!is_terminal_tool("terminal_paint"));
    }
}
