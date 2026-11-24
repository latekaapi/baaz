//! docs/85 10.8: a palette keystroke never opens search.db. The palette's
//! query (`refresh_search` in `app/find.rs`) reads through the kept
//! connection (`search::with_reader`); no search path in the app opens the
//! file itself.

#[test]
fn the_palette_path_opens_no_connection() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let find = std::fs::read_to_string(root.join("app/find.rs")).expect("find.rs");
    let start = find.find("fn refresh_search").expect("refresh_search");
    let body = &find[start..start + find[start..].find("\n    }\n").expect("its end")];
    assert!(body.contains("with_reader"), "the palette reads through the kept connection");
    for opener in ["Connection::open", "search::open(", "search::open_at("] {
        assert!(!body.contains(opener), "refresh_search calls {opener}");
    }
    for file in ["app/find.rs", "session/events.rs"] {
        let text = std::fs::read_to_string(root.join(file)).expect("reads");
        assert!(!text.contains("search::open()"), "{file} opens search.db per call");
    }
}
