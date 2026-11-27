//! docs/87 18.8 (11.2's "one query"): every provider's sessions are rows
//! of baaz.db's one `session` table, and a window builds its sidebar's own
//! maps — the facts (overrides) and the provider records — from one read
//! of it (`sessions::read_front`, one `load_front`). The boot used to run
//! that same query twice on the UI thread, once per map. Muse's kept list
//! and index facts come off the UI thread in `load_index`'s own single
//! query (`listing::load_both`).

fn src(path: &str) -> String {
    std::fs::read_to_string(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path)).expect(path)
}

/// The body of the fn at `signature`, up to its closing brace at `indent`.
fn body_of<'a>(text: &'a str, signature: &str, indent: &str) -> &'a str {
    let start = text.find(signature).unwrap_or_else(|| panic!("{signature}"));
    let end = format!("\n{indent}}}\n");
    &text[start..start + text[start..].find(&end).expect("its end")]
}

#[test]
fn a_window_reads_the_session_tables_front_once() {
    let list = src("src/session_list.rs");
    let new = body_of(&list, "pub(crate) fn new() -> Self", "    ");
    assert_eq!(new.matches("read_front()").count(), 1, "the list's maps come from one read");
    for second in ["sessions::read()", "provider_sessions::read()"] {
        assert!(!new.contains(second), "SessionList::new reads {second} as well");
    }
    let app = src("src/app.rs");
    let boot = body_of(&app, "pub fn new(args: Args, capture: CaptureToken", "    ");
    for second in ["sessions::read()", "provider_sessions::read()", "read_front()"] {
        assert!(!boot.contains(second), "Harness::new reads {second} again after the list did");
    }
    let core = src("../baaz-core/src/sessions.rs");
    let front = body_of(&core, "pub fn read_front()", "");
    assert_eq!(front.matches("load_front(").count(), 1, "read_front is one query");
    let listing = src("../baaz-core/src/db/listing.rs");
    let both = body_of(&listing, "pub fn load_both(", "");
    assert_eq!(both.matches("prepare_cached(").count(), 1, "the Muse list and index facts are one query");
}
