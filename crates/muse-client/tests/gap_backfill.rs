//! **client-adapter-2 / A-MECH-2.** `Inner::dispatch` must park a
//! `ServerRequest` carrying a `sessionId` in `params` (`approval/request`,
//! `userInput/request`) the same way it parks a `Notification`, while that
//! session is mid `view/gap` backfill — and release it through the same
//! cursor-dedup pass. Before the fix, `dispatch`'s session match only covered
//! `MuseEvent::Notification`, so a `ServerRequest` went straight to the event
//! channel: an approval could fold before (or instead of alongside) the item
//! it gates, still catching up from the page.
//!
//! No real `muse serve` involved: `fixtures/gap_backfill_server.py` is a
//! scripted stdin/stdout puppet run as the client's child process (its argv,
//! always led by `serve`, is ignored) so this test spends nothing and needs
//! no server on `PATH`.

use std::path::PathBuf;
use std::time::Duration;

use muse_client::{MuseClient, MuseConfig, MuseEvent};

fn server_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/gap_backfill_server.py")
}

#[test]
fn a_server_request_parks_during_a_gap_backfill_and_releases_after_the_page() {
    let script = server_script();
    assert!(script.exists(), "fake server script missing: {}", script.display());

    let mut client = MuseClient::spawn(&MuseConfig {
        program: script,
        trust_workspace: false,
        no_session_log: false,
        extra_args: Vec::new(),
    })
    .expect("fake server spawns — is python3 on PATH?");
    let events = client.events();

    // Collect events until nothing new arrives for a bit: the fake server's
    // script is entirely deterministic and finite (one `view/gap`, two
    // server requests, one paged item), so a short quiet window after the
    // last of it lands is enough to know nothing more is coming — in
    // particular, that the deduped request never shows up.
    let mut seen: Vec<MuseEvent> = Vec::new();
    loop {
        let timeout = if seen.is_empty() { Duration::from_secs(5) } else { Duration::from_millis(500) };
        match events.recv_timeout(timeout) {
            Ok(event) => seen.push(event),
            Err(_) => break,
        }
    }

    client.shutdown();

    let paged_index = seen
        .iter()
        .position(|event| {
            matches!(
                event,
                MuseEvent::Notification { method, .. } if method == "item/completed"
            )
        })
        .expect("the page's item/completed notification never arrived");
    let released_index = seen
        .iter()
        .position(|event| {
            matches!(event, MuseEvent::ServerRequest { id, .. } if id.as_str() == Some("srv-undeduped"))
        })
        .expect("the parked server request was never released");

    assert!(
        paged_index < released_index,
        "the server request arrived before the backfill it should have waited behind: {seen:?}"
    );

    let deduped_arrived = seen.iter().any(|event| {
        matches!(event, MuseEvent::ServerRequest { id, .. } if id.as_str() == Some("srv-deduped"))
    });
    assert!(
        !deduped_arrived,
        "a server request whose viewCursor the page already served was not deduped: {seen:?}"
    );
}
