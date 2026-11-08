//! Settings › Browser › Agents (docs/27 §6.2, R3; owner 8 Nov: built in R3):
//! how far a browser agent may go on its own.
//!
//! - **Before acting on a page**: Ask (also before using your tabs) or Auto
//!   (only the always-ask actions: Send, Pay, Delete, Upload, sign-in).
//! - **Your own tabs**: Yes, Ask first, or Read only.
//! - **Sites**: per host, Always here (never asks) or Never here (refused).

use serde::{Deserialize, Serialize};

/// Ask or Auto.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ActMode {
    /// Asks before acting on a page, and before using your tabs.
    Ask,
    /// Asks only for the always-ask actions.
    #[default]
    Auto,
}

/// What a chat may do with the tabs you opened.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum YourTabs {
    /// Use them like its own.
    Yes,
    /// Ask once per tab before using it.
    #[default]
    AskFirst,
    /// Read them, never act on them.
    ReadOnly,
}

/// A per-site rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SiteRule {
    /// Never asks here (the always-ask actions included).
    Always,
    /// Agents may not act here at all.
    Never,
}

/// The live Agents settings the page tools read (they run off the window's
/// state, in the automation jobs): set from the layout at boot and on every
/// change ([`set_current`]).
static CURRENT: std::sync::Mutex<Option<AgentsPrefs>> = std::sync::Mutex::new(None);

/// The Agents settings in force.
pub fn current() -> AgentsPrefs {
    CURRENT.lock().map(|prefs| prefs.clone().unwrap_or_default()).unwrap_or_default()
}

/// Put `prefs` in force for the page tools.
pub fn set_current(prefs: &AgentsPrefs) {
    if let Ok(mut current) = CURRENT.lock() {
        *current = Some(prefs.clone());
    }
}

/// The Agents block's settings.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentsPrefs {
    /// Ask or Auto.
    #[serde(default)]
    pub act: ActMode,
    /// Your own tabs.
    #[serde(default)]
    pub your_tabs: YourTabs,
    /// Per-site rules, by host.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sites: Vec<(String, SiteRule)>,
}

impl AgentsPrefs {
    /// The rule for `host`, if any.
    pub fn site(&self, host: &str) -> Option<SiteRule> {
        self.sites.iter().find(|(h, _)| h == host).map(|(_, rule)| *rule)
    }

    /// Set (or clear, with `None`) the rule for `host`.
    pub fn set_site(&mut self, host: &str, rule: Option<SiteRule>) {
        self.sites.retain(|(h, _)| h != host);
        if let Some(rule) = rule {
            self.sites.push((host.to_owned(), rule));
        }
    }

    /// One line for the Sites row: `Always here: a, b · Never here: c`.
    pub fn sites_summary(&self) -> String {
        let list = |want: SiteRule| self.sites.iter().filter(|(_, r)| *r == want).map(|(h, _)| h.as_str()).collect::<Vec<_>>().join(", ");
        let (always, never) = (list(SiteRule::Always), list(SiteRule::Never));
        match (always.is_empty(), never.is_empty()) {
            (true, true) => "No site rules yet. An approval's \u{201C}Always on this site\u{201D} adds one.".into(),
            (false, true) => format!("Always here: {always}"),
            (true, false) => format!("Never here: {never}"),
            (false, false) => format!("Always here: {always} · Never here: {never}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn site_rules_replace_and_clear() {
        let mut prefs = AgentsPrefs::default();
        prefs.set_site("github.com", Some(SiteRule::Always));
        prefs.set_site("bank.example", Some(SiteRule::Never));
        prefs.set_site("github.com", Some(SiteRule::Never));
        assert_eq!(prefs.site("github.com"), Some(SiteRule::Never));
        assert_eq!(prefs.sites_summary(), "Never here: bank.example, github.com");
        prefs.set_site("github.com", None);
        prefs.set_site("bank.example", None);
        assert!(prefs.sites_summary().starts_with("No site rules"));
    }

    #[test]
    fn defaults_are_auto_and_ask_first() {
        let prefs = AgentsPrefs::default();
        assert_eq!((prefs.act, prefs.your_tabs), (ActMode::Auto, YourTabs::AskFirst));
        let back: AgentsPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(back, prefs);
    }
}
