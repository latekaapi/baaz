//! The neutral store a view renders must show exactly what Muse's fold
//! folded: every capture in `fixtures/msp`, mirrored event by event into one
//! [`SessionStore`] per session (and the other capture folders beside it), ends equal to the fold's own session.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use muse_adapter::{Mirror, MuseFold};
use muse_client::{frame, MuseEvent};
use session_store::SessionStore;

fn captures() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
    let mut paths: Vec<PathBuf> = ["msp", "msp-experimental", "msp-ts", "msp-ts-experimental"]
        .iter()
        .filter_map(|dir| std::fs::read_dir(root.join(dir)).ok())
        .flatten()
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    paths.sort();
    assert!(!paths.is_empty());
    paths
}

fn events(path: &Path) -> Vec<MuseEvent> {
    let text = std::fs::read_to_string(path).expect("capture is readable");
    text.lines()
        .filter_map(|line| line.strip_prefix("<-- "))
        .filter_map(|body| frame::parse_line(body).ok().flatten())
        .filter_map(MuseEvent::from_frame)
        .collect()
}

#[test]
fn every_capture_mirrors_into_an_identical_store() {
    for path in captures() {
        let mut plain = MuseFold::new();
        let mut fold = MuseFold::new();
        let mut stores: BTreeMap<String, (SessionStore, Mirror)> = BTreeMap::new();
        for event in events(&path) {
            plain.apply(event.clone());
            let deltas = fold.apply(event);
            let Some(id) = fold.last_touched().map(str::to_owned) else { continue };
            let (store, mirror) = stores
                .entry(id.clone())
                .or_insert_with(|| (SessionStore::new(&id, aui_protocol::Provider::Muse, ""), Mirror::new()));
            mirror.sync(&mut fold, &id, deltas, store);
        }
        for id in plain.session_ids() {
            let (store, _) = stores.get(id).unwrap_or_else(|| panic!("{}: no store for {id}", path.display()));
            assert_eq!(
                store.session(),
                plain.session(id),
                "{}: session {id} differs between the fold and its mirrored store",
                path.display()
            );
            let side = muse_adapter::neutral_side(plain.side(id).unwrap());
            assert_eq!(store.side().queued, side.queued, "{}: queue", path.display());
            assert_eq!(store.side().pending_prompts, side.pending_prompts, "{}: prompts", path.display());
            assert_eq!(store.side().pending_approvals, side.pending_approvals, "{}: approvals", path.display());
            assert_eq!(store.side().cursor, side.cursor, "{}: cursor", path.display());
        }
    }
}
