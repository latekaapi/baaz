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
