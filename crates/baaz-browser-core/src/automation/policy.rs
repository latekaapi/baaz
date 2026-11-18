//! Which agent actions always ask the person (docs/27 §6.2): Send, Pay, Delete,
//! Upload, sign-in. Detected structurally, with no model in the loop, so the same
//! page always gets the same answer.

use serde_json::{json, Value};

/// Why an action needs the person.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    Send,
    Pay,
    Delete,
    Upload,
    SignIn,
    /// A script on a page that is not this machine's: it could do any of the others.
    Script,
    /// Any action on a page, under Settings › Browser › Agents › Ask (R3).
    Act,
    /// The first action in one of the person's own tabs at a host the
    /// agent has not been allowed on there (Your own tabs › Ask first).
    YourTab,
    /// Opening or fetching a page on another host, when a rule above asks.
    Open,
}

impl Reason {
    /// The word on the approval card's chip and in the tool's answer.
    pub fn word(self) -> &'static str {
        match self {
            Reason::Send => "Send",
            Reason::Pay => "Pay",
            Reason::Delete => "Delete",
            Reason::Upload => "Upload",
            Reason::SignIn => "Sign in",
            Reason::Script => "Run a script",
            Reason::Act => "Act",
            Reason::YourTab => "Use your tab",
            Reason::Open => "Open",
        }
    }
}

/// Button words, in the seven languages of docs/27 §6.2: English, Spanish, French,
/// German, Portuguese, Italian, Hindi. Lower case; matched on whole words.
const SEND: &[&str] = &["send", "submit", "post", "publish", "reply", "enviar", "publicar", "responder", "envoyer", "publier", "répondre", "senden", "absenden", "veröffentlichen", "antworten", "invia", "inviare", "pubblica", "rispondi", "भेजें", "प्रकाशित"];
const PAY: &[&str] = &["pay", "buy", "purchase", "checkout", "order", "subscribe", "pagar", "comprar", "pedir", "suscribirse", "payer", "acheter", "commander", "zahlen", "bezahlen", "kaufen", "bestellen", "paga", "acquista", "ordina", "भुगतान", "खरीदें"];
const DELETE: &[&str] = &["delete", "remove", "erase", "destroy", "eliminar", "borrar", "supprimer", "effacer", "löschen", "entfernen", "excluir", "apagar", "remover", "elimina", "cancella", "rimuovi", "हटाएं", "मिटाएं"];
const UPLOAD: &[&str] = &["upload", "subir", "téléverser", "hochladen", "carregar", "carica", "अपलोड"];
const SIGN_IN: &[&str] = &["login", "signin", "acceder", "connexion", "anmelden", "entrar", "accedi", "लॉगिन"];
const SIGN_IN_PHRASES: &[&str] = &["sign in", "log in", "iniciar sesión", "se connecter", "einloggen", "fazer login", "साइन इन"];

/// Whether `url` is this machine's: scripts there are the person's own dev work.
pub fn is_local(url: &str) -> bool {
    let host = url.split("://").nth(1).unwrap_or("").split(['/', ':', '?', '#']).next().unwrap_or("");
    url.starts_with("file:") || matches!(host, "localhost" | "127.0.0.1" | "[") || host.ends_with(".localhost") || host.ends_with(".test")
}

/// What's known about the element an action targets.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Target {
    /// Its accessible name or visible text.
    pub name: String,
    /// `input type`, lower case, when it is an input.
    pub input_type: String,
    /// Whether it submits or belongs to a form.
    pub in_form: bool,
    /// Its form has a password field.
    pub form_has_password: bool,
    /// Its form has a card field (`autocomplete=cc-*`, `name=cardnumber`, …).
    pub form_has_card: bool,
    /// Its form posts to another origin.
    pub cross_origin_post: bool,
}

fn has_word(text: &str, words: &[&str]) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && !matches!(c, '\u{0900}'..='\u{097F}'))
        .filter(|word| !word.is_empty())
        .any(|word| words.contains(&word))
}

/// The reason `target` must ask, or `None` when the agent may go on.
pub fn classify(target: &Target) -> Option<Reason> {
    let name = target.name.to_lowercase();
    if target.input_type == "file" || has_word(&name, UPLOAD) {
        return Some(Reason::Upload);
    }
    if target.form_has_card || has_word(&name, PAY) {
        return Some(Reason::Pay);
    }
    if target.form_has_password
        || target.input_type == "password"
        || has_word(&name, SIGN_IN)
        || SIGN_IN_PHRASES.iter().any(|phrase| name.contains(phrase))
    {
        return Some(Reason::SignIn);
    }
    if has_word(&name, DELETE) {
        return Some(Reason::Delete);
    }
    if target.cross_origin_post || has_word(&name, SEND) {
        return Some(Reason::Send);
    }
    None
}

/// Run on the target element: reports what [`classify`] needs. `this` is the node.
pub const INSPECT_FUNCTION: &str = r#"function () {
  const el = this;
  const form = el.form || (el.closest && el.closest('form')) || null;
  const fields = form ? Array.from(form.elements || []) : [];
  const auto = (f) => (f.getAttribute && (f.getAttribute('autocomplete') || '')) + ' ' + (f.name || '') + ' ' + (f.id || '');
  let cross = false;
  if (form && (form.method || '').toLowerCase() === 'post' && form.action) {
    try { cross = new URL(form.action, location.href).origin !== location.origin; } catch (e) {}
  }
  return {
    name: ((el.getAttribute && (el.getAttribute('aria-label') || el.getAttribute('value'))) || el.innerText || el.value || el.title || '').slice(0, 300),
    input_type: (el.type || '').toLowerCase(),
    in_form: !!form,
    form_has_password: fields.some((f) => f.type === 'password'),
    form_has_card: fields.some((f) => /\bcc-|card.?num|cardnumber|cvc|cvv/i.test(auto(f))),
    cross_origin_post: cross,
  };
}"#;

/// Reads [`INSPECT_FUNCTION`]'s answer, falling back to the snapshot's name.
pub fn target_from(value: &Value, snapshot_name: &str) -> Target {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or("").to_owned();
    let flag = |key: &str| value.get(key).and_then(Value::as_bool).unwrap_or(false);
    let name = text("name");
    Target {
        name: if name.trim().is_empty() { snapshot_name.to_owned() } else { name },
        input_type: text("input_type"),
        in_form: flag("in_form"),
        form_has_password: flag("form_has_password"),
        form_has_card: flag("form_has_card"),
        cross_origin_post: flag("cross_origin_post"),
    }
}

/// The host of `url` (`shop.example`), lower case; empty for none.
pub fn host_of(url: &str) -> String {
    url.split("://").nth(1).unwrap_or("").split(['/', '?', '#']).next().unwrap_or("").to_lowercase()
}

/// What the agent is about to do, for [`decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// A click, a key or a value on the page; `Some` when [`classify`] says
    /// it is one of the always-ask kinds.
    Page(Option<Reason>),
    /// Opening a URL in the tab, or a cookie-bearing fetch from it.
    Open,
}

/// What [`decide`] says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Go on.
    Go,
    /// A site rule refuses agents here.
    Refuse,
    /// Ask the person first, for this reason.
    Ask(Reason),
}

/// The one gate every agent action passes (docs/82 0.12), with no model in
/// the loop:
///
/// 1. A site's Never refuses and its Always goes on.
/// 2. In one of the person's own tabs under **Ask first**, the first action
///    at a host the agent was not allowed on in that tab asks ("Use your
///    tab"); **Yes** treats the tab as the agent's own. (**Read only** is
///    refused before any job starts.)
/// 3. The always-ask kinds (Send, Pay, Delete, Upload, Sign in) ask.
/// 4. Under **Ask** every page action asks, and opening another page asks.
///
/// Opening this machine's own pages (`localhost`, `file:`) skips 2–4.
pub fn decide(
    prefs: &baaz_core::browser_prefs::agents::AgentsPrefs,
    host: &str,
    local: bool,
    persons_tab: bool,
    allowed_in_tab: bool,
    action: Action,
) -> Decision {
    use baaz_core::browser_prefs::agents::{ActMode, SiteRule, YourTabs};
    match prefs.site(host) {
        Some(SiteRule::Never) => return Decision::Refuse,
        Some(SiteRule::Always) => return Decision::Go,
        None => {}
    }
    if action == Action::Open && local {
        return Decision::Go;
    }
    if persons_tab && prefs.your_tabs == YourTabs::AskFirst && !allowed_in_tab {
        return Decision::Ask(Reason::YourTab);
    }
    match action {
        Action::Page(Some(reason)) => Decision::Ask(reason),
        Action::Page(None) if prefs.act == ActMode::Ask => Decision::Ask(Reason::Act),
        Action::Open if prefs.act == ActMode::Ask => Decision::Ask(Reason::Open),
        _ => Decision::Go,
    }
}

/// The tool's answer when a site rule refuses agents (Never here).
pub fn refused_answer(host: &str) -> Value {
    json!({
        "stopped": "site_refused",
        "message": format!("The person does not let agents act on {host} (Settings › Browser › Agents › Sites). Nothing was done."),
    })
}

/// The tool's answer when an action is stopped for the person.
pub fn stopped_answer(reason: Reason, target: &Target) -> Value {
    json!({
        "stopped": "needs_approval",
        "reason": reason.word(),
        "target": target.name,
        "message": match reason {
            Reason::Act => "The person asked to approve actions on this page and declined this one. Nothing was done.".to_owned(),
            Reason::YourTab => "This is one of the person's own tabs and they did not allow you to use it here. Nothing was done; open your own tab with browser_tabs.".to_owned(),
            Reason::Open => "The person did not allow opening this page. Nothing was done.".to_owned(),
            _ => format!("{} always asks the person. Nothing was done.", reason.word()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Target {
        Target { name: name.into(), ..Target::default() }
    }

    #[test]
    fn the_four_and_sign_in_ask_by_their_words() {
        assert_eq!(classify(&named("Pay €97")), Some(Reason::Pay));
        assert_eq!(classify(&named("Place your order")), Some(Reason::Pay));
        assert_eq!(classify(&named("Send")), Some(Reason::Send));
        assert_eq!(classify(&named("Delete account")), Some(Reason::Delete));
        assert_eq!(classify(&named("Upload file")), Some(Reason::Upload));
        assert_eq!(classify(&named("Sign in")), Some(Reason::SignIn));
    }

    #[test]
    fn the_seven_languages_ask_too() {
        for (name, reason) in [
            ("Pagar ahora", Reason::Pay),
            ("Acheter", Reason::Pay),
            ("Jetzt kaufen", Reason::Pay),
            ("Enviar", Reason::Send),
            ("Supprimer", Reason::Delete),
            ("Löschen", Reason::Delete),
            ("Iniciar sesión", Reason::SignIn),
            ("Paga ora", Reason::Pay),
            ("भुगतान करें", Reason::Pay),
        ] {
            assert_eq!(classify(&named(name)), Some(reason), "{name}");
        }
    }

    #[test]
    fn whole_words_only_and_ordinary_buttons_go_on() {
        for name in ["Next", "Add to cart", "Payload inspector", "Sender details", "Show more", "Removed items", ""] {
            assert_eq!(classify(&named(name)), None, "{name}");
        }
    }

    #[test]
    fn structure_asks_with_no_word_at_all() {
        let target = |change: fn(&mut Target)| {
            let mut target = named("Continue");
            change(&mut target);
            target
        };
        assert_eq!(classify(&target(|t| t.form_has_password = true)), Some(Reason::SignIn));
        assert_eq!(classify(&target(|t| t.form_has_card = true)), Some(Reason::Pay));
        assert_eq!(classify(&target(|t| t.input_type = "file".into())), Some(Reason::Upload));
        assert_eq!(classify(&target(|t| t.cross_origin_post = true)), Some(Reason::Send));
    }

    /// docs/82 0.12: the three "Your own tabs" values each change what the
    /// gate does in one of the person's tabs; the agent's own tabs never
    /// ask for being used.
    #[test]
    fn your_own_tabs_values_each_change_the_gate() {
        use baaz_core::browser_prefs::agents::{AgentsPrefs, YourTabs};
        let with = |tabs| AgentsPrefs { your_tabs: tabs, ..AgentsPrefs::default() };
        let click = Action::Page(None);
        // Ask first (the default): the first action at a new host asks…
        assert_eq!(decide(&with(YourTabs::AskFirst), "shop.example", false, true, false, click), Decision::Ask(Reason::YourTab));
        // …and once allowed in that tab, the next goes on.
        assert_eq!(decide(&with(YourTabs::AskFirst), "shop.example", false, true, true, click), Decision::Go);
        // Navigating the person's tab to a new host asks too.
        assert_eq!(decide(&with(YourTabs::AskFirst), "other.example", false, true, false, Action::Open), Decision::Ask(Reason::YourTab));
        // Yes: the person's tab is used like the agent's own.
        assert_eq!(decide(&with(YourTabs::Yes), "shop.example", false, true, false, click), Decision::Go);
        // The agent's own tab never asks for being used.
        assert_eq!(decide(&with(YourTabs::AskFirst), "shop.example", false, false, false, click), Decision::Go);
        // Read only is refused before the gate (`read_only_tool`); here the
        // gate itself adds no prompt for it.
        assert_eq!(decide(&with(YourTabs::ReadOnly), "shop.example", false, true, false, click), Decision::Go);
    }

    #[test]
    fn site_rules_and_ask_mode_cover_opening_pages_too() {
        use baaz_core::browser_prefs::agents::{ActMode, AgentsPrefs, SiteRule};
        let mut prefs = AgentsPrefs::default();
        prefs.set_site("bank.example", Some(SiteRule::Never));
        prefs.set_site("docs.example", Some(SiteRule::Always));
        assert_eq!(decide(&prefs, "bank.example", false, false, true, Action::Open), Decision::Refuse);
        assert_eq!(decide(&prefs, "docs.example", false, true, false, Action::Page(Some(Reason::Pay))), Decision::Go);
        // Auto: opening a page in the agent's own tab goes on; Ask asks.
        assert_eq!(decide(&prefs, "news.example", false, false, true, Action::Open), Decision::Go);
        prefs.act = ActMode::Ask;
        assert_eq!(decide(&prefs, "news.example", false, false, true, Action::Open), Decision::Ask(Reason::Open));
        // This machine's own pages are the person's dev work: never asked.
        assert_eq!(decide(&prefs, "localhost:5173", true, true, false, Action::Open), Decision::Go);
        // The always-ask kinds still ask once the tab is allowed.
        prefs.act = ActMode::Auto;
        assert_eq!(decide(&prefs, "shop.example", false, true, true, Action::Page(Some(Reason::Pay))), Decision::Ask(Reason::Pay));
    }

    #[test]
    fn only_this_machine_counts_as_local() {
        for url in ["http://localhost:5173/", "http://127.0.0.1:8080/x", "file:///tmp/a.html", "https://app.localhost/", "http://shop.test/"] {
            assert!(is_local(url), "{url}");
        }
        for url in ["https://localhost.evil.com/", "https://example.com/localhost", "about:blank"] {
            assert!(!is_local(url), "{url}");
        }
    }

    #[test]
    fn the_inspector_answer_reads_back_and_falls_back_to_the_snapshot() {
        let value = json!({"name":"  ","form_has_card":true,"input_type":"submit"});
        let target = target_from(&value, "Continue");
        assert_eq!(target.name, "Continue");
        assert!(target.form_has_card);
        assert_eq!(classify(&target), Some(Reason::Pay));
    }
}
