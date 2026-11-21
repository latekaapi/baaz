//! Per-site settings (docs/82 8.6): what the person answered a site's
//! permission asks for good ("Always allow" / "Always block"), and the
//! site's page zoom. Keyed by origin (`https://example.com:8443`), each
//! setting one `(origin, key, value)` row in `browser.db`'s
//! `site_settings` table, written by the `BrowserData` worker
//! ([`crate::history::BrowserDb::set_site`]) and read whole at load into
//! the front the UI reads. This module is the model: no SQL, no window.
//!
//! A permission key is the asked kind's stable key (`camera`,
//! `microphone`, `geolocation`, `notifications`, `clipboard`, `midi`,
//! `other:<bit>`); its value is `allow` or `block`. The zoom key is
//! [`ZOOM_KEY`], its value a percent. A missing key is "Ask" (or 100 %).

use std::collections::BTreeMap;

/// The key a site's page zoom is stored under.
pub const ZOOM_KEY: &str = "zoom";

/// A stored answer to one kind of permission ask.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Answer {
    /// Always allow: the ask is answered yes without asking.
    Allow,
    /// Always block: the ask is answered no without asking.
    Block,
}

impl Answer {
    /// The stored spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Answer::Allow => "allow",
            Answer::Block => "block",
        }
    }

    /// The stored spelling read back; anything else is no answer.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "allow" => Some(Answer::Allow),
            "block" => Some(Answer::Block),
            _ => None,
        }
    }
}

/// Every site's stored settings, by origin then key.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SiteSettings {
    sites: BTreeMap<String, BTreeMap<String, String>>,
}

impl SiteSettings {
    /// The settings from stored `(origin, key, value)` rows.
    pub fn from_rows(rows: impl IntoIterator<Item = (String, String, String)>) -> Self {
        let mut out = Self::default();
        for (origin, key, value) in rows {
            out.set(&origin, &key, Some(&value));
        }
        out
    }

    /// Set (`Some`) or clear (`None`) one setting. A site left with no
    /// settings is dropped.
    pub fn set(&mut self, origin: &str, key: &str, value: Option<&str>) {
        match value {
            Some(value) => {
                self.sites.entry(origin.to_owned()).or_default().insert(key.to_owned(), value.to_owned());
            }
            None => {
                if let Some(site) = self.sites.get_mut(origin) {
                    site.remove(key);
                    if site.is_empty() {
                        self.sites.remove(origin);
                    }
                }
            }
        }
    }

    /// Forget everything stored for `origin`.
    pub fn clear_site(&mut self, origin: &str) {
        self.sites.remove(origin);
    }

    /// The stored answer for `kind` on `origin`, if any.
    pub fn answer(&self, origin: &str, kind: &str) -> Option<Answer> {
        self.sites.get(origin)?.get(kind).and_then(|value| Answer::parse(value))
    }

    /// The answer for an ask of `kinds` on `origin` that needs no person:
    /// `Block` when any asked kind is always blocked, `Allow` when every
    /// one is always allowed, else `None` (the strip asks).
    pub fn decide(&self, origin: &str, kinds: &[String]) -> Option<Answer> {
        if kinds.is_empty() {
            return None;
        }
        let answers: Vec<Option<Answer>> = kinds.iter().map(|kind| self.answer(origin, kind)).collect();
        if answers.contains(&Some(Answer::Block)) {
            return Some(Answer::Block);
        }
        answers.iter().all(|answer| *answer == Some(Answer::Allow)).then_some(Answer::Allow)
    }

    /// The site's page zoom in percent, if one is stored.
    pub fn zoom(&self, origin: &str) -> Option<u16> {
        self.sites.get(origin)?.get(ZOOM_KEY).and_then(|value| value.parse().ok())
    }

    /// The origins with anything stored, sorted.
    pub fn origins(&self) -> Vec<String> {
        self.sites.keys().cloned().collect()
    }

    /// The permission answers stored for `origin`, by kind key, sorted.
    pub fn answers(&self, origin: &str) -> Vec<(String, Answer)> {
        self.sites
            .get(origin)
            .map(|site| {
                site.iter()
                    .filter(|(key, _)| key.as_str() != ZOOM_KEY)
                    .filter_map(|(key, value)| Answer::parse(value).map(|answer| (key.clone(), answer)))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// The origin a URL belongs to (`https://example.com:8443/a?b` →
/// `https://example.com:8443`): scheme lower-cased, userinfo dropped. `None`
/// for anything that is not a web page (`about:`, `file:`, `chrome:`…),
/// which keeps no site settings.
pub fn origin_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "https" && scheme != "http" {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let authority = authority.rsplit_once('@').map(|(_, host)| host).unwrap_or(authority);
    if authority.is_empty() {
        return None;
    }
    Some(format!("{scheme}://{}", authority.to_ascii_lowercase()))
}

/// The page zoom steps the ⋯ menu's − and + walk (Chrome's).
pub const ZOOM_STEPS: [u16; 15] = [25, 33, 50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// The next step from `current` in `direction` (+1 in, −1 out), held at
/// the ends. A level between steps moves to the neighbouring step.
pub fn zoom_step(current: u16, direction: i8) -> u16 {
    if direction > 0 {
        ZOOM_STEPS.iter().copied().find(|step| *step > current).unwrap_or(ZOOM_STEPS[ZOOM_STEPS.len() - 1])
    } else {
        ZOOM_STEPS.iter().rev().copied().find(|step| *step < current).unwrap_or(ZOOM_STEPS[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(list: &[&str]) -> Vec<String> {
        list.iter().map(|kind| kind.to_string()).collect()
    }

    #[test]
    fn stored_answers_decide_an_ask_only_when_they_cover_it() {
        let mut sites = SiteSettings::default();
        let cam = "https://cam.example";
        assert_eq!(sites.decide(cam, &kinds(&["camera"])), None, "nothing stored asks");
        sites.set(cam, "camera", Some(Answer::Allow.as_str()));
        assert_eq!(sites.decide(cam, &kinds(&["camera"])), Some(Answer::Allow));
        assert_eq!(sites.decide(cam, &kinds(&["camera", "microphone"])), None, "the microphone was never answered");
        sites.set(cam, "microphone", Some(Answer::Block.as_str()));
        assert_eq!(sites.decide(cam, &kinds(&["camera", "microphone"])), Some(Answer::Block), "one block blocks the ask");
        assert_eq!(sites.decide("https://other.example", &kinds(&["camera"])), None, "per origin");
        sites.set(cam, "camera", None);
        assert_eq!(sites.answer(cam, "camera"), None, "back to Ask");
        assert_eq!(sites.answers(cam), vec![(String::from("microphone"), Answer::Block)]);
    }

    #[test]
    fn zoom_is_stored_beside_the_answers() {
        let mut sites = SiteSettings::from_rows([
            (String::from("https://a.example"), String::from(ZOOM_KEY), String::from("125")),
            (String::from("https://a.example"), String::from("camera"), String::from("allow")),
        ]);
        assert_eq!(sites.zoom("https://a.example"), Some(125));
        assert_eq!(sites.answers("https://a.example").len(), 1, "zoom is not a permission");
        assert_eq!(sites.zoom("https://b.example"), None);
        sites.clear_site("https://a.example");
        assert!(sites.origins().is_empty());
    }

    #[test]
    fn origins_are_scheme_host_and_port() {
        assert_eq!(origin_of("https://Example.com/a?b#c").as_deref(), Some("https://example.com"));
        assert_eq!(origin_of("http://user:pw@localhost:5173/x").as_deref(), Some("http://localhost:5173"));
        assert_eq!(origin_of("about:blank"), None);
        assert_eq!(origin_of("file:///tmp/a.html"), None);
    }

    #[test]
    fn zoom_steps_walk_and_hold_at_the_ends() {
        assert_eq!(zoom_step(100, 1), 110);
        assert_eq!(zoom_step(100, -1), 90);
        assert_eq!(zoom_step(115, 1), 125, "between steps moves to the next");
        assert_eq!(zoom_step(300, 1), 300);
        assert_eq!(zoom_step(25, -1), 25);
    }
}
