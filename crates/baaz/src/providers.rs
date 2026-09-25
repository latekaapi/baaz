//! The provider registry: the backends a new session can start on.
//!
//! Three entries — `muse` (existing), `claude-code`, `codex`. Each entry
//! exposes its capability map, mirrored cell-for-cell from the adapter
//! crates that own the evidence (`provider-muse`, `provider-claude-code`
//! and `provider-codex` `caps.rs`): this registry never re-probes, it only
//! repeats what those files declare, so a fixture that upgrades a cell
//! updates the mirror here with it.
//!
//! # One session, one serving lane
//!
//! Two state machines read one event stream today: the legacy pump in
//! `conn.rs` and [`ProviderCall`](crate::wire::ProviderCall). Wiring a
//! session view to both would give two writers to one on-screen state —
//! random-looking UI corruption no unit test can catch. So a session is
//! served by exactly one of them, decided at session creation from the
//! [`SessionHost`](crate::session::SessionHost)'s provider id and then
//! never changed:
//!
//! * `muse` sessions ride the legacy pump (the transitional bundle, until
//!   the last view moves lane by lane).
//! * `claude-code` and `codex` sessions ride the provider lane: neutral
//!   [`Command`](provider::Command)s through the capability gate.
//!
//! There is deliberately no way to switch a live session's provider: the
//! id lives in a private field on the view with no setter, set once from
//! the host at construction. [`check_single_lane`] is the loud detector
//! for the day both lanes claim one session — it returns the violation as
//! text instead of corrupting silently.

use std::collections::HashMap;

use provider::{Capability, CapabilityState};

/// Which backend a session belongs to. Fixed at session creation; a live
/// session never changes lanes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderId {
    /// The existing backend, served by the legacy pump.
    Muse,
    /// Claude Code, served over the provider lane.
    ClaudeCode,
    /// Codex, served over the provider lane.
    Codex,
}

impl ProviderId {
    /// Every entry in the registry, in switcher order.
    pub fn all() -> [ProviderId; 3] {
        [ProviderId::Muse, ProviderId::ClaudeCode, ProviderId::Codex]
    }

    /// The wire id: what the session host and the CLI flag carry.
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderId::Muse => "muse",
            ProviderId::ClaudeCode => "claude-code",
            ProviderId::Codex => "codex",
        }
    }

    /// Parse a wire id back. Unknown ids fall back to `muse`, the lane
    /// every existing session already rides — a mistyped flag still opens
    /// a working session rather than nothing.
    pub fn parse(raw: &str) -> ProviderId {
        match raw {
            "claude-code" => ProviderId::ClaudeCode,
            "codex" => ProviderId::Codex,
            _ => ProviderId::Muse,
        }
    }

    /// The switcher label: a human name, never the wire id.
    pub fn label(self) -> &'static str {
        match self {
            ProviderId::Muse => "Muse",
            ProviderId::ClaudeCode => "Claude Code",
            ProviderId::Codex => "Codex",
        }
    }

    /// One honest line per entry, for the switcher subtitle.
    pub fn blurb(self) -> &'static str {
        match self {
            ProviderId::Muse => "The existing backend, full capability set.",
            ProviderId::ClaudeCode => "Steering and interruption are unverified; questions arrive as prose.",
            ProviderId::Codex => "Native steering and interruption; approvals carry the model's reason.",
        }
    }

    /// The composer placeholder for a session on this provider: the person
    /// is asking this provider, so the empty composer names it.
    pub fn composer_placeholder(self) -> String {
        format!("Ask {}, or type / for commands", self.label())
    }

    /// The hero subtitle for a session on this provider in a workspace
    /// called `display`: the session runs on this provider, so the empty
    /// state names it rather than assuming Muse.
    pub fn hero_subtitle(self, display: &str) -> String {
        format!("{} runs in {display}.", self.label())
    }

    /// The mark beside the composer model chip: each provider's own badge,
    /// so a Codex session never wears the Muse "M".
    pub fn icon(self) -> aui_icons::Provider {
        match self {
            ProviderId::Muse => aui_icons::Provider::Muse,
            ProviderId::ClaudeCode => aui_icons::Provider::Claude,
            ProviderId::Codex => aui_icons::Provider::Codex,
        }
    }

    /// The needs-you banner headline: whoever the session runs on is the
    /// one waiting on the person.
    pub fn waiting_headline(self) -> String {
        format!("{} is waiting for you.", self.label())
    }
}

/// The capability map for one registry entry, mirrored from the adapter
/// crate that owns the evidence (named per arm). A fixture that upgrades
/// a cell there upgrades the mirror here with it.
pub fn capability_state(id: ProviderId, capability: Capability) -> CapabilityState {
    match (id, capability) {
        // `provider-muse` caps: every command-backed capability Native
        // except the shell and reasoning traces (Unverified).
        (ProviderId::Muse, Capability::SessionLifecycle) => CapabilityState::Native,
        (ProviderId::Muse, Capability::ForkSession) => CapabilityState::Native,
        (ProviderId::Muse, Capability::CompactSession) => CapabilityState::Native,
        (ProviderId::Muse, Capability::SessionConfig) => CapabilityState::Native,
        (ProviderId::Muse, Capability::SessionShell) => CapabilityState::Unverified,
        (ProviderId::Muse, Capability::SubmitTurn) => CapabilityState::Native,
        (ProviderId::Muse, Capability::SteerTurn) => CapabilityState::Native,
        (ProviderId::Muse, Capability::TurnControl) => CapabilityState::Native,
        (ProviderId::Muse, Capability::ModelCatalog) => CapabilityState::Native,
        (ProviderId::Muse, Capability::Approvals) => CapabilityState::Native,
        (ProviderId::Muse, Capability::Questions) => CapabilityState::Native,
        (ProviderId::Muse, Capability::Transcript) => CapabilityState::Native,
        (ProviderId::Muse, Capability::Account) => CapabilityState::Native,
        (ProviderId::Muse, Capability::ClientTools) => CapabilityState::Native,
        (ProviderId::Muse, Capability::ReasoningTraces) => CapabilityState::Unverified,
        (ProviderId::Muse, Capability::SubagentTurns) => CapabilityState::Unverified,
        // `provider-claude-code` caps: SteerTurn and TurnControl
        // Unverified (nothing probed them — do not upgrade without a
        // fixture); Questions Unavailable (asks in prose, no id to
        // answer); compaction and the catalog emulated by Baaz.
        (ProviderId::ClaudeCode, Capability::SessionLifecycle) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::ForkSession) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::CompactSession) => CapabilityState::Emulated {
            reason: "No 'compact now' over --print; Baaz sets an autocompact window instead".into(),
        },
        (ProviderId::ClaudeCode, Capability::SessionConfig) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::SessionShell) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::SubmitTurn) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::SteerTurn) => CapabilityState::Unverified,
        (ProviderId::ClaudeCode, Capability::TurnControl) => CapabilityState::Unverified,
        (ProviderId::ClaudeCode, Capability::ModelCatalog) => CapabilityState::Emulated {
            reason: "No fixture enumerates models; Baaz supplies the list".into(),
        },
        (ProviderId::ClaudeCode, Capability::Approvals) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::Questions) => CapabilityState::Unavailable {
            reason: "Claude Code asks in prose; there is no question id to answer".into(),
        },
        (ProviderId::ClaudeCode, Capability::Transcript) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::Account) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::ClientTools) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::ReasoningTraces) => CapabilityState::Native,
        (ProviderId::ClaudeCode, Capability::SubagentTurns) => CapabilityState::Unverified,
        // `provider-codex` caps: steering and interruption proven
        // mid-turn (Native); fork, compaction, transcript paging, client
        // tools and questions read off method names but never executed
        // (Unverified — never quietly upgraded without a fixture).
        (ProviderId::Codex, Capability::SessionLifecycle) => CapabilityState::Native,
        (ProviderId::Codex, Capability::ForkSession) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::CompactSession) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::SessionConfig) => CapabilityState::Native,
        (ProviderId::Codex, Capability::SessionShell) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::SubmitTurn) => CapabilityState::Native,
        (ProviderId::Codex, Capability::SteerTurn) => CapabilityState::Native,
        (ProviderId::Codex, Capability::TurnControl) => CapabilityState::Native,
        (ProviderId::Codex, Capability::ModelCatalog) => CapabilityState::Native,
        (ProviderId::Codex, Capability::Approvals) => CapabilityState::Native,
        (ProviderId::Codex, Capability::Questions) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::Transcript) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::Account) => CapabilityState::Native,
        (ProviderId::Codex, Capability::ClientTools) => CapabilityState::Unverified,
        (ProviderId::Codex, Capability::ReasoningTraces) => CapabilityState::Native,
        (ProviderId::Codex, Capability::SubagentTurns) => CapabilityState::Unverified,
    }
}

/// What the UI may offer for one capability on one provider: `None` means
/// offered normally; `Some` carries the typed reason the control shows
/// beside its disabled state.
///
/// `Unavailable` refuses (the gate never lets the command through, so the
/// control must not invite the press). `Unverified` is attempted everywhere
/// — the seam never refuses it — so it stays offered but visibly marked:
/// honest ignorance, never quietly collapsed into either neighbour.
pub fn gate(id: ProviderId, capability: Capability) -> Option<String> {
    match capability_state(id, capability) {
        CapabilityState::Native | CapabilityState::Emulated { .. } => None,
        CapabilityState::Unavailable { reason } => Some(reason),
        CapabilityState::Unverified => Some(format!(
            "Unverified on {}: nobody has probed it live, so it is attempted, never refused",
            id.label()
        )),
    }
}

/// Whether this provider's sessions ride the legacy pump. `muse` does
/// until the last view moves; every new provider rides the provider lane
/// from its first session. Which lane serves a session is decided at
/// session creation from the host's provider id and then not changed.
pub fn uses_legacy_pump(id: ProviderId) -> bool {
    matches!(id, ProviderId::Muse)
}

// ------------------------------------------------- the supplied model lists

/// One row of Baaz's supplied Claude Code model list: the `ModelCatalog:
/// Emulated` cell made concrete. `--model` takes aliases and ids but no
/// fixture enumerates them, so Baaz owns this list — and it stays owned
/// here, never upgraded into a probed `Native`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SuppliedModel {
    /// What `--model` carries: an alias, never a dated full id.
    pub id: &'static str,
    /// The human label the picker shows; the raw alias never renders.
    pub label: &'static str,
    /// One honest line: what the alias asks for.
    pub detail: &'static str,
}

/// The Claude Code models Baaz offers: aliases `--model` accepts, in
/// picker order. No default is marked — no probe established one — so the
/// current session model marks the active row instead.
pub fn claude_code_models() -> [SuppliedModel; 3] {
    [
        SuppliedModel {
            id: "sonnet",
            label: "Claude Sonnet",
            detail: "The everyday model (--model sonnet; full ids work too)",
        },
        SuppliedModel {
            id: "opus",
            label: "Claude Opus",
            detail: "The largest model (--model opus; full ids work too)",
        },
        SuppliedModel {
            id: "haiku",
            label: "Claude Haiku",
            detail: "The fast model (--model haiku; full ids work too)",
        },
    ]
}

/// Baaz's supplied Claude Code catalog in the seam's neutral shape, with
/// the row matching `current` flagged active. A full dated id still
/// selects (aliases travel) without flagging a row it does not name.
pub fn claude_code_catalog(current: Option<&str>) -> Vec<provider::ModelSummary> {
    claude_code_models()
        .into_iter()
        .map(|row| provider::ModelSummary {
            id: row.id.to_owned(),
            label: row.label.to_owned(),
            active: Some(row.id) == current,
        })
        .collect()
}

// ------------------------------------------------- the last-chosen provider

/// What the store remembers: the backend new sessions start on.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
struct LastProvider {
    /// The wire id (`muse`, `claude-code`, `codex`).
    #[serde(default)]
    provider: String,
}

/// Where the last-chosen provider lives: one small JSON file in Baaz's own
/// store, beside the projects file rather than inside it. Every read is
/// best-effort like every other store read; every write is atomic.
fn last_provider_path() -> std::path::PathBuf {
    crate::store::support_dir().join("provider.json")
}

/// The backend new sessions start on, as last chosen. `None` when nothing
/// was ever chosen or the file names no backend — the caller falls back to
/// the command line.
pub fn read_last_provider() -> Option<ProviderId> {
    let stored: LastProvider = crate::store::read_json(&last_provider_path());
    match stored.provider.as_str() {
        "muse" => Some(ProviderId::Muse),
        "claude-code" => Some(ProviderId::ClaudeCode),
        "codex" => Some(ProviderId::Codex),
        _ => None,
    }
}

/// Remember the backend new sessions start on. Best-effort: a store that
/// cannot be written leaves the in-memory pick, which still names every
/// session this run starts.
pub fn write_last_provider(id: ProviderId) {
    let text = serde_json::to_string(&LastProvider { provider: id.as_str().to_owned() });
    if let Ok(text) = text {
        let _ = crate::store::write_atomic(&last_provider_path(), text.as_bytes());
    }
}

/// The loud detector for the two-writers bug: when both lanes claim the
/// same session, return the violation as text (logged and bannered)
/// instead of letting two state machines drive one screen silently.
/// `None` is the only healthy answer.
pub fn check_single_lane(legacy_active: bool, provider_active: bool, session_id: &str) -> Option<String> {
    if legacy_active && provider_active {
        Some(format!(
            "Session {session_id} is served by two lanes at once; the legacy pump and the provider lane must never both drive one screen"
        ))
    } else {
        None
    }
}

// ------------------------------------------------- external approvals

/// Where an approval waiting on the person came from. Claude Code sends
/// one control request (`can_use_tool`); Codex sends five.
///
/// Constructed by the provider lane decoders when they land (today only
/// by tests and fixtures); the surface already renders every variant.
#[allow(dead_code)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalApprovalKind {
    /// Claude Code `can_use_tool` on the control channel: `tool_name`,
    /// `display_name`, `input`, `tool_use_id`, `permission_suggestions`.
    ClaudeCanUseTool,
    /// Codex `item/commandExecution/requestApproval`.
    CodexCommand,
    /// Codex `item/fileChange/requestApproval`.
    CodexFileChange,
    /// Codex `item/permissions/requestApproval`.
    CodexPermissions,
    /// Codex `item/tool/requestUserInput`.
    CodexUserInput,
    /// Codex `mcpServer/elicitation/request`.
    CodexMcpElicitation,
}

impl ExternalApprovalKind {
    /// The short source tag the card shows beside the headline.
    pub fn tag(self) -> &'static str {
        match self {
            ExternalApprovalKind::ClaudeCanUseTool => "Claude Code tool request",
            ExternalApprovalKind::CodexCommand => "Codex command approval",
            ExternalApprovalKind::CodexFileChange => "Codex file-change approval",
            ExternalApprovalKind::CodexPermissions => "Codex permissions approval",
            ExternalApprovalKind::CodexUserInput => "Codex question",
            ExternalApprovalKind::CodexMcpElicitation => "Codex tool question",
        }
    }
}

/// The person's answer. `decline` ("no, do something else" — the turn
/// continues) and `cancel` ("no, stop" — the turn is interrupted) are
/// different answers and ride as different choices; the card offers both.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApprovalChoice {
    /// Run it, once.
    Accept,
    /// Run it and stop asking for the rest of the session.
    AcceptForSession,
    /// Refuse; the turn continues.
    Decline,
    /// Refuse; the turn is interrupted.
    Cancel,
}

impl ApprovalChoice {
    /// Every choice the card offers, in card order.
    pub fn all() -> [ApprovalChoice; 4] {
        [
            ApprovalChoice::Accept,
            ApprovalChoice::AcceptForSession,
            ApprovalChoice::Decline,
            ApprovalChoice::Cancel,
        ]
    }

    /// The decision token the provider lane sends.
    pub fn choice_id(self) -> &'static str {
        match self {
            ApprovalChoice::Accept => "accept",
            ApprovalChoice::AcceptForSession => "acceptForSession",
            ApprovalChoice::Decline => "decline",
            ApprovalChoice::Cancel => "cancel",
        }
    }

    /// The card button label. Decline and Cancel never share one: "no, do
    /// something else" and "no, stop" are different answers.
    pub fn label(self) -> &'static str {
        match self {
            ApprovalChoice::Accept => "Approve",
            ApprovalChoice::AcceptForSession => "Approve for this session",
            ApprovalChoice::Decline => "Deny",
            ApprovalChoice::Cancel => "Deny and stop",
        }
    }
}

/// One approval from a new provider, waiting on the person. Rendered on
/// the existing approvals surface; decided through the provider lane; the
/// card changes only when the server's notification resolves it — never
/// on the press.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalApproval {
    /// The provider-side id (`tool_use_id` on Claude Code, `itemId` on Codex).
    pub id: String,
    /// The owning session.
    pub session_id: String,
    /// Which backend asked.
    pub provider: ProviderId,
    /// Which of the six request shapes this is.
    pub kind: ExternalApprovalKind,
    /// One-line human summary: the tool name, the command, the question.
    pub headline: String,
    /// The model-written `reason` sentence (Codex), or the tool input
    /// summary (Claude Code). Shown on the card; never empty.
    pub reason: String,
    /// Claude Code's `permission_suggestions`: the "don't ask again"
    /// affordance (`addRules`/`allow`/`localSettings`). Shown beside the
    /// session-scoped choice; `None` on Codex, which scopes
    /// `acceptForSession` itself.
    pub dont_ask_again: Option<String>,
    /// Opaque per-stage token passed back verbatim with the decision.
    /// Codex answers a single yes/no per approval, so this is `None`
    /// there; a backend that stages requires `Some`.
    pub stage_token: Option<String>,
    /// The choice already sent and awaiting the server's notification.
    /// `Some` means the card shows "sent, waiting" and offers no second
    /// press — the card is never ahead of the server.
    pub decision_sent: Option<String>,
}

impl ExternalApproval {
    /// Still waiting on the person (nothing sent yet).
    pub fn is_pending(&self) -> bool {
        self.decision_sent.is_none()
    }
}

/// The pending external approvals of one session, keyed by provider-side
/// id. Pure state — no widgets, no wire — so the decide-then-wait rule is
/// testable without a window:
///
/// * `decide` records the sent choice and returns the decision token. It
///   never settles the card: the card changes only on `resolve`, which is
///   what the server's notification calls.
/// * a second `decide` while one is in flight is refused (`None`): one
///   press, one command, then wait.
#[derive(Clone, Debug, Default)]
pub struct ExternalApprovalStore {
    approvals: HashMap<String, ExternalApproval>,
}

impl ExternalApprovalStore {
    /// Empty, for a session with nothing waiting.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Park a provider approval request on the surface.
    pub fn inject(&mut self, approval: ExternalApproval) {
        self.approvals.insert(approval.id.clone(), approval);
    }

    /// Look one up, for the card.
    pub fn get(&self, id: &str) -> Option<&ExternalApproval> {
        self.approvals.get(id)
    }

    /// The ones still waiting on the person, oldest first by id.
    pub fn pending(&self) -> Vec<&ExternalApproval> {
        let mut out: Vec<&ExternalApproval> =
            self.approvals.values().filter(|a| a.is_pending()).collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Everything the server has not resolved yet: pending presses plus
    /// sent decisions still waiting. Both render; only the former offers
    /// buttons — a sent card shows "waiting for the server" instead.
    pub fn outstanding(&self) -> Vec<&ExternalApproval> {
        let mut out: Vec<&ExternalApproval> = self.approvals.values().collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    /// Record the press: returns the `(choice_id, stage_token)` the
    /// provider lane sends. The card stays pending — the server's
    /// notification settles it through [`Self::resolve`]. A repeat press
    /// while one is in flight, or an unknown id, returns `None` and sends
    /// nothing twice.
    pub fn decide(&mut self, id: &str, choice: ApprovalChoice) -> Option<(String, Option<String>)> {
        let approval = self.approvals.get_mut(id)?;
        if approval.decision_sent.is_some() {
            return None;
        }
        let token = (choice.choice_id().to_owned(), approval.stage_token.clone());
        approval.decision_sent = Some(choice.choice_id().to_owned());
        Some(token)
    }

    /// Settle the card from the server's notification. Returns whether
    /// anything was waiting under that id.
    pub fn resolve(&mut self, id: &str) -> bool {
        self.approvals.remove(id).is_some()
    }

    /// Whether anything the server has not resolved yet is on the card —
    /// a press sent and still waiting counts, because the card has not
    /// moved. Read by the screenshot capture's pending-approval answer.
    pub fn has_pending(&self) -> bool {
        !self.approvals.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_entries_in_the_registry() {
        assert_eq!(ProviderId::all().len(), 3);
        let ids: Vec<&str> = ProviderId::all().iter().map(|p| p.as_str()).collect();
        assert_eq!(ids, vec!["muse", "claude-code", "codex"]);
    }

    #[test]
    fn labels_are_human_names_not_wire_ids() {
        for id in ProviderId::all() {
            assert!(!id.label().contains('-') || id == ProviderId::ClaudeCode);
            assert_ne!(id.label(), id.as_str());
        }
    }

    #[test]
    fn session_chrome_names_the_session_provider() {
        // The hero subtitle, composer placeholder, chip badge and
        // needs-you headline all derive from the session's provider: a
        // future hardcode of any one of them fails here, on every provider.
        for id in ProviderId::all() {
            let label = id.label();
            assert_eq!(id.hero_subtitle("harness"), format!("{label} runs in harness."));
            assert_eq!(
                id.composer_placeholder(),
                format!("Ask {label}, or type / for commands")
            );
            assert_eq!(id.waiting_headline(), format!("{label} is waiting for you."));
        }
        assert_eq!(ProviderId::Muse.icon(), aui_icons::Provider::Muse);
        assert_eq!(ProviderId::ClaudeCode.icon(), aui_icons::Provider::Claude);
        assert_eq!(ProviderId::Codex.icon(), aui_icons::Provider::Codex);
        // No two providers share a badge: the chip mark always identifies
        // the session's lane.
        let icons: Vec<_> = ProviderId::all().iter().map(|p| p.icon()).collect();
        assert_ne!(icons[0], icons[1]);
        assert_ne!(icons[0], icons[2]);
        assert_ne!(icons[1], icons[2]);
    }

    #[test]
    fn unknown_ids_open_a_working_muse_session() {
        assert_eq!(ProviderId::parse(""), ProviderId::Muse);
        assert_eq!(ProviderId::parse("echo"), ProviderId::Muse);
        assert_eq!(ProviderId::parse("claude-code"), ProviderId::ClaudeCode);
        assert_eq!(ProviderId::parse("codex"), ProviderId::Codex);
    }

    #[test]
    fn the_last_chosen_provider_survives_a_relaunch() {
        // Serialized against every other test that points the store at a
        // temp dir: two tests pointing it at two dirs at once would read
        // each other's state.
        let guard = crate::store::test_env_lock();
        let dir = std::env::temp_dir().join(format!("baaz-provider-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let old = std::env::var_os("BAAZ_STATE_DIR");
        std::env::set_var("BAAZ_STATE_DIR", &dir);
        // Nothing chosen yet: no pick, so the boot falls back to the
        // command line rather than inventing one.
        assert_eq!(read_last_provider(), None);
        write_last_provider(ProviderId::Codex);
        assert_eq!(read_last_provider(), Some(ProviderId::Codex));
        write_last_provider(ProviderId::Muse);
        assert_eq!(read_last_provider(), Some(ProviderId::Muse));
        // A file this build cannot parse is ordinary, not a pick.
        std::fs::write(dir.join("provider.json"), b"not json").expect("seed bad json");
        assert_eq!(read_last_provider(), None);
        match old {
            Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
            None => std::env::remove_var("BAAZ_STATE_DIR"),
        }
        drop(guard);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_screen_differs_between_providers() {
        // SteerTurn: Unverified on Claude Code (marked, attempted),
        // Native on Codex (offered plainly). The capability strip reads
        // the gate, so the same screen cannot render the same for both.
        assert!(gate(ProviderId::ClaudeCode, Capability::SteerTurn).is_some());
        assert!(gate(ProviderId::Codex, Capability::SteerTurn).is_none());
        assert!(gate(ProviderId::ClaudeCode, Capability::TurnControl).is_some());
        assert!(gate(ProviderId::Codex, Capability::TurnControl).is_none());
        // Questions: Unavailable on Claude Code (refused, with the prose
        // reason), Unverified on Codex (attempted, marked) — different
        // states, different text, never averaged into one.
        let claude = gate(ProviderId::ClaudeCode, Capability::Questions).expect("gated");
        let codex = gate(ProviderId::Codex, Capability::Questions).expect("gated");
        assert_ne!(claude, codex);
        assert!(claude.contains("prose"));
    }

    #[test]
    fn unverified_is_marked_never_collapsed() {
        // Unverified keeps the typed marker (attempted, not refused);
        // Unavailable carries the provider's own reason.
        let marked = gate(ProviderId::ClaudeCode, Capability::SteerTurn).unwrap();
        assert!(marked.contains("Unverified"));
        let refused = gate(ProviderId::ClaudeCode, Capability::Questions).unwrap();
        assert!(!refused.contains("Unverified"));
    }

    #[test]
    fn muse_keeps_the_legacy_pump_and_new_providers_do_not() {
        assert!(uses_legacy_pump(ProviderId::Muse));
        assert!(!uses_legacy_pump(ProviderId::ClaudeCode));
        assert!(!uses_legacy_pump(ProviderId::Codex));
    }

    #[test]
    fn both_lanes_at_once_is_loud_not_silent() {
        assert!(check_single_lane(false, false, "s").is_none());
        assert!(check_single_lane(true, false, "s").is_none());
        assert!(check_single_lane(false, true, "s").is_none());
        let violation = check_single_lane(true, true, "s").expect("loud");
        assert!(violation.contains('s'));
    }

    #[test]
    fn decline_and_cancel_are_different_answers() {
        assert_ne!(ApprovalChoice::Decline.choice_id(), ApprovalChoice::Cancel.choice_id());
        assert_ne!(ApprovalChoice::Decline.label(), ApprovalChoice::Cancel.label());
        assert_eq!(ApprovalChoice::Decline.choice_id(), "decline");
        assert_eq!(ApprovalChoice::Cancel.choice_id(), "cancel");
        let ids: Vec<&str> =
            ApprovalChoice::all().iter().map(|c| c.choice_id()).collect();
        assert_eq!(ids, vec!["accept", "acceptForSession", "decline", "cancel"]);
    }

    fn sample(kind: ExternalApprovalKind) -> ExternalApproval {
        ExternalApproval {
            id: "appr-1".into(),
            session_id: "s-1".into(),
            provider: ProviderId::Codex,
            kind,
            headline: "Allow creating /tmp/probe.txt?".into(),
            reason: "Allow creating /tmp/probe.txt containing HELLO as requested?".into(),
            dont_ask_again: None,
            stage_token: None,
            decision_sent: None,
        }
    }

    #[test]
    fn the_card_is_never_ahead_of_the_server() {
        let mut store = ExternalApprovalStore::empty();
        store.inject(sample(ExternalApprovalKind::CodexCommand));
        assert!(store.has_pending());
        // The press sends exactly one decision and the card stays pending.
        let sent = store.decide("appr-1", ApprovalChoice::Decline).expect("sent");
        assert_eq!(sent.0, "decline");
        assert!(store.has_pending(), "a sent decision still waits on the server");
        // A repeat press sends nothing twice.
        assert!(store.decide("appr-1", ApprovalChoice::Cancel).is_none());
        // Only the server's notification settles the card.
        assert!(store.resolve("appr-1"));
        assert!(!store.has_pending());
    }

    #[test]
    fn all_six_request_shapes_reach_the_store() {
        let mut store = ExternalApprovalStore::empty();
        let kinds = [
            ExternalApprovalKind::ClaudeCanUseTool,
            ExternalApprovalKind::CodexCommand,
            ExternalApprovalKind::CodexFileChange,
            ExternalApprovalKind::CodexPermissions,
            ExternalApprovalKind::CodexUserInput,
            ExternalApprovalKind::CodexMcpElicitation,
        ];
        for (i, kind) in kinds.into_iter().enumerate() {
            let mut approval = sample(kind);
            approval.id = format!("appr-{i}");
            store.inject(approval);
        }
        assert_eq!(store.pending().len(), 6);
    }
}
