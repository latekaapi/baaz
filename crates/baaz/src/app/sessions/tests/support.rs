//! Shared helpers for the session lifecycle tests: lane harnesses, scripted
//! and resumable provider factories, the fake Muse client.

use super::*;

// ------------------------------------------------- W2 provider-lane open

/// A bootable [`crate::Args`] pointed at a hermetic state dir, mirroring
/// the app tests' helper: no store read or write escapes the temp dir.
pub(in crate::app) fn lane_args(dir: &std::path::Path) -> crate::Args {
    crate::Args {
        workspace: dir.to_path_buf(),
        workspace_explicit: true,
        provider: "echo".into(),
        provider_explicit: false,
        program: "muse".into(),
        theme: Some(aui_tokens::ThemeKind::Dark),
        screenshot: None,
        delay: std::time::Duration::from_millis(500),
        session: None,
        send: None,
        offline: true,
        replay: None,
        steps: Vec::new(),
        tier: None,
        print_tier: false,
        approval_mode: None,
        login: crate::LoginSample::Choose,
        login_steps: Vec::new(),
        bench: None,
        bench_cadence: std::time::Duration::from_millis(4),
        bench_scroll: crate::bench::BenchScroll::Sweep,
        bench_frames: 600,
        bench_open_turn: false,
        bench_bare: false,
        bench_shell: false,
        bench_out: None,
        sidebar_fixture: None,
        no_project: false,
        terminal_socket_dir: Some(crate::terminal::service::test_socket_dir()),
    }
}

pub(in crate::app) fn lane_state(
    name: &str,
) -> (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("baaz-w2-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("probe state dir");
    let guard = crate::store::test_env_lock();
    let old = std::env::var_os("BAAZ_STATE_DIR");
    std::env::set_var("BAAZ_STATE_DIR", &dir);
    (guard, old, dir)
}

#[allow(clippy::needless_pass_by_value)]
pub(in crate::app) fn lane_restore(
    state: (std::sync::MutexGuard<'static, ()>, Option<std::ffi::OsString>, std::path::PathBuf),
) {
    let (guard, old, dir) = state;
    let _ = std::fs::remove_dir_all(&dir);
    match old {
        Some(value) => std::env::set_var("BAAZ_STATE_DIR", value),
        None => std::env::remove_var("BAAZ_STATE_DIR"),
    }
    drop(guard);
}

/// A connected scripted provider through the [`crate::providers::ProviderFactory`]
/// seam: what the lane tests drive without spawning a child.
pub(super) fn scripted_factory() -> crate::providers::ProviderFactory {
    use provider::ProviderAdapter as _;
    crate::providers::ProviderFactory::new(|_: ProviderId| {
        let mut adapter = provider::scripted::ScriptedProvider::new();
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    })
}

/// A factory whose provider answers `ResumeSession` by echoing the
/// session: what the forked-open path drives without spawning a child.
pub(super) fn resumable_factory() -> crate::providers::ProviderFactory {
    use provider::ProviderAdapter as _;
    struct Resumable {
        rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
        connected: bool,
    }
    impl provider::ProviderAdapter for Resumable {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Codex
        }
        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            self.connected = true;
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Codex,
                agent_name: "resumable".into(),
                agent_version: "0.0.0".into(),
            })
        }
        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable {
                reason: "the resumable double opens and resumes sessions only".into(),
            };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::ForkSession, off()),
                (Capability::CompactSession, off()),
                (Capability::SessionConfig, off()),
                (Capability::SessionShell, off()),
                (Capability::SubmitTurn, off()),
                (Capability::SteerTurn, off()),
                (Capability::TurnControl, off()),
                (Capability::ModelCatalog, native.clone()),
                (Capability::Approvals, off()),
                (Capability::Questions, off()),
                (Capability::Transcript, off()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
                (Capability::ImageInput, off()),
            ])
        }
        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            if !self.connected {
                return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
            }
            match command {
                provider::Command::OpenSession { .. } => {
                    Ok(provider::Ack::Session { session_id: "s-open".into(), title: None })
                }
                provider::Command::ResumeSession { session_id, .. } => {
                    Ok(provider::Ack::Session { session_id, title: None })
                }
                provider::Command::ListModels { .. } => {
                    Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "resumable".into() })
                }
                provider::Command::ListPending { .. } => {
                    Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                }
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "the resumable double opens and resumes sessions only",
                )),
            }
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.rx.clone()
        }
        fn shutdown(&mut self) {}
    }
    crate::providers::ProviderFactory::new(|_: ProviderId| {
        let (_, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
        let mut adapter = Resumable { rx, connected: false };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    })
}

/// A factory that never connects: the failed-open path.
pub(super) fn failing_factory(reason: &str) -> crate::providers::ProviderFactory {
    let reason = reason.to_owned();
    crate::providers::ProviderFactory::new(move |_: ProviderId| {
        Err(provider::ProviderError::Unavailable { reason: reason.clone() })
    })
}

/// Mark Muse up over a real child with no session behind it: `true`
/// exits at once, so nothing can answer on it — which is exactly what
/// proves the lane path issues no muse `session/start`: had it tried,
/// the dead child would have failed it into a dialog instead of a lane
/// view.
pub(super) fn mark_muse_up(harness: &mut Harness) {
    let hub = provider_muse::MuseHub::spawn_stand_in("true", Vec::new()).expect("a throwaway child spawns");
    harness.muse_slot.set_ready(hub.clone());
    harness.muse_hub = Some(hub);
}

pub(super) fn lane_harness(
    vc: &mut gpui::VisualTestContext,
    dir: &std::path::Path,
) -> gpui::Entity<Harness> {
    vc.update(|window, cx| {
        cx.new(|cx| Harness::new(lane_args(dir), crate::runtime::CaptureToken::default(), window, cx))
    })
}

/// A muse child that answers only `session/start`, echoing the id the
/// start carried, and logs every request line to `log`: what the
/// start-landing tests drive without a real server.
/// The fake child as the hub every Muse open rides: shaken hands,
/// installed as the window's Muse connection — what a live launch has
/// once `muse serve` answered.
pub(super) fn install_fake_muse(
    harness: &mut Harness,
    dir: &std::path::Path,
    root: &std::path::Path,
    log: &std::path::Path,
    answer: Option<&str>,
) {
    use provider::ProviderAdapter as _;
    let hub = starting_client_answering(dir, root, log, answer);
    hub.lane().connect(&crate::conn::connect_info()).expect("the fake child shakes hands");
    harness.muse_slot.set_ready(hub.clone());
    harness.muse_hub = Some(hub);
}

/// Adopt `dir` as the current project and pick muse for new sessions.
pub(super) fn adopt_for_muse(harness: &mut Harness, dir: &std::path::Path, cx: &mut gpui::App) -> String {
    let id = harness.session_list.update(cx, |list, _| list.projects.add(dir).id.clone());
    harness.session_list.update(cx, |list, _| list.projects.current = Some(id.clone()));
    harness.current_project = Some(id.clone());
    harness.new_provider = ProviderId::Muse.as_str().to_owned();
    id
}

// ------------------------------------------------- W6 one session per new

/// A factory that counts every child it spawns, around the scripted
/// stand-in: the W6 tests measure provider opens with it instead of
/// grepping stderr.
pub(super) fn counting_factory() -> (crate::providers::ProviderFactory, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
    use provider::ProviderAdapter as _;
    let opens = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let factory_opens = opens.clone();
    let factory: crate::providers::ProviderFactory = crate::providers::ProviderFactory::new(move |_: ProviderId| {
        factory_opens.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut adapter = provider::scripted::ScriptedProvider::new();
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, opens)
}

pub(super) fn opens_of(opens: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> usize {
    opens.load(std::sync::atomic::Ordering::Relaxed)
}

/// The Harness as the window's root view, so real keystrokes reach the
/// composer, on a Codex default whose child answers through `factory`.
/// The open is NOT run: nothing here parks the executor after the boot.
pub(super) fn provider_draft_window<'a>(
    cx: &'a mut gpui::TestAppContext,
    dir: &std::path::Path,
    factory: crate::providers::ProviderFactory,
) -> (gpui::Entity<Harness>, &'a mut gpui::VisualTestContext, String) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    cx.update(crate::app::bind_keys);
    // Past the login screen, so the session's composer is what paints.
    let mut args = lane_args(dir);
    args.login = crate::LoginSample::SignedIn;
    let (baaz, vc) =
        cx.add_window_view(|window, cx| Harness::new(args, crate::runtime::CaptureToken::default(), window, cx));
    vc.run_until_parked();
    let project = vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            let project = adopt_for_muse(harness, dir, cx);
            harness.new_provider = ProviderId::Codex.as_str().to_owned();
            project
        })
    });
    (baaz, vc, project)
}

/// B3b: a factory minting a distinct session per open, around a quiet
/// adapter that answers the open-time pulls and admits decisions. The
/// scripted double mints one fixed id, so a second open would replace
/// the first instead of parking it.
pub(super) fn parking_factory() -> crate::providers::ProviderFactory {
    use provider::ProviderAdapter as _;
    struct Quiet {
        rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
        connected: bool,
        next: std::sync::Arc<std::sync::atomic::AtomicU32>,
    }
    impl provider::ProviderAdapter for Quiet {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Codex
        }
        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            self.connected = true;
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Codex,
                agent_name: "quiet".into(),
                agent_version: "0.0.0".into(),
            })
        }
        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable {
                reason: "the quiet double opens sessions only".into(),
            };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::ForkSession, off()),
                (Capability::CompactSession, off()),
                (Capability::SessionConfig, off()),
                (Capability::SessionShell, off()),
                (Capability::SubmitTurn, native.clone()),
                (Capability::SteerTurn, off()),
                (Capability::TurnControl, off()),
                (Capability::ModelCatalog, native.clone()),
                (Capability::Approvals, native.clone()),
                (Capability::Questions, off()),
                (Capability::Transcript, off()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
                (Capability::ImageInput, native.clone()),
            ])
        }
        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            if !self.connected {
                return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
            }
            match command {
                provider::Command::OpenSession { .. } => {
                    let n = self.next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    Ok(provider::Ack::Session { session_id: format!("s-park-{n}"), title: None })
                }
                provider::Command::ListModels { .. } => {
                    Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "quiet".into() })
                }
                provider::Command::ListPending { .. } => {
                    Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                }
                provider::Command::DecideApproval { .. } => Ok(provider::Ack::Accepted),
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "the quiet double opens sessions only",
                )),
            }
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.rx.clone()
        }
        fn shutdown(&mut self) {}
    }
    let next = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(1));
    crate::providers::ProviderFactory::new(move |_: ProviderId| {
        let (_, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
        let mut adapter = Quiet { rx, connected: false, next: next.clone() };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    })
}

/// Seed a stored Codex session with history, as a restart would leave
/// it: the record plus its sidebar row.
pub(super) fn z4_seed_stored_codex(harness: &mut Harness, cx: &mut Context<Harness>) {
    use std::collections::HashMap;
    harness.session_list.update(cx, |list, _| {
        list.provider_sessions.insert(
            "s-codex-old".into(),
            crate::provider_sessions::ProviderSessionRecord {
                provider: "codex".into(),
                session_id: "s-codex-old".into(),
                workspace: None,
                project: None,
                created_ms: 1_700_000_000_000,
                updated_ms: 1_700_000_000_000,
                turns: 2,
                title: Some("Inquiry About Current Model".into()),
                first_prompt: Some("which model is this".into()),
                handoff_to: None,
                handoff_from: None,
                handoff_from_provider: None,
                handoff_title: None,
                display_texts: HashMap::new(),
                origin: None,
            },
        );
    });
    harness.merge_provider_rows(cx);
    // The failed session's own right pane stands open: the failure
    // lands on it, like every activation lands on its session's.
    harness.set_override(
        "s-codex-old",
        |meta| {
            meta.right = Some(crate::sessions::RightState {
                open: true,
                ..Default::default()
            })
        },
        cx,
    );
}

/// A factory that records what it was asked and replays one history
/// turn on resume: the reopen path's double, beside `resumable_factory`.
/// The replayed user turn plus its finished assistant turn is what a
/// real adapter replays from its backend store.
pub(super) fn recording_resumable_factory() -> (
    crate::providers::ProviderFactory,
    std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
) {
    recording_resumable_factory_replying(Some("The header is restored"))
}

pub(super) fn y2b2_factory() -> (crate::providers::ProviderFactory, Y2b2Shared) {
    use provider::ProviderAdapter as _;
    let shared = Y2b2Shared {
        inner: std::sync::Arc::new(std::sync::Mutex::new(Y2b2State {
            opens: 0,
            created: Vec::new(),
            shutdowns: Vec::new(),
            modes: Vec::new(),
        })),
    };
    let factory_shared = shared.clone();
    let factory = crate::providers::ProviderFactory::new(move |_: ProviderId| {
        let (_, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
        let mut adapter = Y2b2Adapter {
            rx,
            shared: factory_shared.clone(),
            id: std::sync::Mutex::new(None),
            connected: false,
        };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, shared)
}

/// Open one lane and return its session id.
pub(super) fn y2b2_open(
    vc: &mut gpui::VisualTestContext,
    baaz: &gpui::Entity<Harness>,
    provider: ProviderId,
) -> String {
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            harness.select_new_provider(provider, cx);
            harness.new_session(window, cx);
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).rt(cx).session_id.clone()
    })
}

/// Stage the scheduler until `total` opens have STARTED (their
/// `OpenSession` dispatches ran). A switch's open starts on a
/// background task but lands on a later foreground one, so stopping
/// here holds the switch pending with no thread parked: the landing
/// is queued but cannot run until the test drains again, and the test
/// performs the cancelling action synchronously first — no scheduler
/// turn runs between this return and that action, so the open always
/// lands stale. Each `tick` runs one task and returns, which is what
/// keeps this from deadlocking: `run_until_parked` polls background
/// work on the test thread, so a blocking wait for the open inside
/// the test would strand the release the test itself must send.
pub(super) fn y2b2_drain_until_opens(
    vc: &mut gpui::VisualTestContext,
    shared: &Y2b2Shared,
    total: usize,
) {
    for _ in 0..30_000 {
        if shared.inner.lock().expect("y2b2").created.len() >= total {
            return;
        }
        vc.cx.background_executor.tick();
    }
    panic!("the switch open never started after 30_000 scheduler ticks");
}

/// One muse row for a chain test: the wire lists the head, the stores
/// carry the links.
pub(super) fn chain_muse_entry(id: &str, label: &str, turns: u64) -> crate::sidebar::SessionEntry {
    crate::sidebar::SessionEntry {
        id: id.to_owned(),
        label: label.to_owned(),
        updated: crate::clock::now_local(),
        created: crate::clock::now_local(),
        running: false,
        turns,
        hidden: false,
        pinned: false,
        archived: false,
        description: String::new(),
        replayed: false,
        provider: crate::providers::ProviderId::Muse,
        named: false,
        needs_title: false,
        side_marker: false,
        title_pending: false,
        last_ask: None,
        local: false,
        provisional: false,
        turns_unknown: false,
        workspace: None,
        project: None,
        project_name: None,
        attention: Vec::new(),
        approval_command: None,
        pending_question: None,
        turn_started: None,
        last_error: None,
        unread: false,
        branch: None,
        terminals_running: 0,
    }
}

/// A fake `muse serve` child, answering every start with `answer` as its id
/// instead of the one asked for, when one is given.
pub(super) fn starting_client_answering(
    dir: &std::path::Path,
    root: &std::path::Path,
    log: &std::path::Path,
    answer: Option<&str>,
) -> provider_muse::MuseHub {
    use std::os::unix::fs::PermissionsExt as _;
    let script = dir.join("fake-muse.sh");
    let answered = answer.map_or_else(|| "$sid".to_owned(), str::to_owned);
    let body = format!(
        r#"#!/bin/sh
while IFS= read -r line; do
  printf '%s\n' "$line" >> '{log}'
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || continue
  case "$line" in
*'"method":"initialize"'*)
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"experimentalApi":false,"grantedCapabilities":[],"museHome":"/tmp/fake","platformFamily":"unix","platformOs":"macos","schema":{{"fingerprint":"fake","version":1}},"serverInfo":{{"name":"fake-muse","version":"1.4.11"}},"userAgent":"fake"}}}}\n' "$id";;
*'"method":"session/start"'*)
  sid=$(printf '%s' "$line" | sed -n 's/.*"sessionId":"\([^"]*\)".*/\1/p')
  printf '{{"jsonrpc":"2.0","id":%s,"result":{{"session":{{"sessionId":"%s","createdAt":"2026-10-16T00:00:00Z","updatedAt":"2026-10-16T00:00:00Z","path":"","status":"idle","turnCount":0,"workspaceRoot":"{root}"}},"viewCursor":"c0"}}}}\n' "$id" "{answered}";;
*) printf '{{"jsonrpc":"2.0","id":%s,"error":{{"code":-32601,"message":"not here"}}}}\n' "$id";;
  esac
done
"#,
        log = log.display(),
        root = root.display(),
    );
    std::fs::write(&script, body).expect("fake muse script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    provider_muse::MuseHub::spawn_stand_in(script, Vec::new()).expect("the fake child spawns")
}

/// [`recording_resumable_factory`] whose replayed assistant turn says
/// `reply`, or nothing at all (a turn of tool calls only, as far as the
/// byline can tell) when `None`.
pub(super) fn recording_resumable_factory_replying(
    reply: Option<&'static str>,
) -> (
    crate::providers::ProviderFactory,
    std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
) {
    use provider::ProviderAdapter as _;
    struct RecordingResumable {
        tx: crossbeam_channel::Sender<provider::ProviderEvent>,
        rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
        connected: bool,
        commands: std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
        reply: Option<&'static str>,
    }
    impl provider::ProviderAdapter for RecordingResumable {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Codex
        }
        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            self.connected = true;
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Codex,
                agent_name: "recording-resumable".into(),
                agent_version: "0.0.0".into(),
            })
        }
        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable {
                reason: "the recording double opens and resumes sessions only".into(),
            };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::SubmitTurn, native.clone()),
                (Capability::ModelCatalog, native.clone()),
                (Capability::ForkSession, off()),
                (Capability::CompactSession, off()),
                (Capability::SessionConfig, off()),
                (Capability::SessionShell, off()),
                (Capability::SteerTurn, off()),
                (Capability::TurnControl, off()),
                (Capability::Approvals, off()),
                (Capability::Questions, off()),
                (Capability::Transcript, off()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
                (Capability::ImageInput, native.clone()),
            ])
        }
        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            if !self.connected {
                return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
            }
            self.commands.lock().expect("commands").push(command.clone());
            match command {
                provider::Command::OpenSession { .. } => {
                    Ok(provider::Ack::Session { session_id: "s-open".into(), title: None })
                }
                provider::Command::ResumeSession { session_id, .. } => {
                    // The backend's stored transcript, replayed as
                    // deltas the way a real resume replays them.
                    let mut deltas = vec![
                        aui_protocol::Delta::TurnStarted {
                            turn: aui_protocol::Turn::User {
                                id: "u-old".into(),
                                text: "Restore the header".into(),
                                attachments: Vec::new(),
                                mentions: Vec::new(),
                                timestamp: None,
                            },
                        },
                        aui_protocol::Delta::TurnStarted {
                            turn: aui_protocol::Turn::Assistant {
                                id: "a-old".into(),
                                blocks: Vec::new(),
                                meta: aui_protocol::TurnMeta::default(),
                                timestamp: None,
                            },
                        },
                    ];
                    if let Some(reply) = self.reply {
                        deltas.push(aui_protocol::Delta::BlockAdded {
                            turn_id: "a-old".into(),
                            block: aui_protocol::Block::Text { text: reply.into(), streaming: false },
                        });
                    }
                    let _ = self.tx.send(provider::ProviderEvent::Deltas {
                        session_id: Some(session_id.clone()),
                        deltas: deltas
                            .into_iter()
                            .chain([aui_protocol::Delta::TurnFinished {
                                turn_id: "a-old".into(),
                                meta: aui_protocol::TurnMeta {
                                    model: "codex-mini".into(),
                                    tokens_in: 1200,
                                    tokens_out: 300,
                                    cost_usd: 0.04,
                                    ..Default::default()
                                },
                            }])
                            .collect(),
                    });
                    Ok(provider::Ack::Session { session_id, title: None })
                }
                provider::Command::ListModels { .. } => {
                    Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "recording".into() })
                }
                provider::Command::ListPending { .. } => {
                    Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                }
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "the recording double opens and resumes sessions only",
                )),
            }
        }
        fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
            self.rx.clone()
        }
        fn shutdown(&mut self) {}
    }
    let commands = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let factory_commands = commands.clone();
    let factory = crate::providers::ProviderFactory::new(move |_: ProviderId| {
        let (tx, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
        let mut adapter = RecordingResumable {
            tx,
            rx,
            connected: false,
            commands: factory_commands.clone(),
            reply,
        };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, commands)
}

/// Y2b2 scaffolding: a recording factory. Each `OpenSession` mints a
/// fresh id (so several lanes coexist), records it synchronously, and
/// returns at once; every shutdown is recorded per id (so a discarded
/// open proves its child was hung up and parked nowhere). The open is
/// held pending by STAGING the scheduler (`y2b2_drain_until_opens`),
/// never by blocking inside `dispatch`: the test scheduler polls
/// background work on the test thread, so parking that thread in the
/// adapter strands the test itself — the suite stalls holding the
/// env lock.
#[derive(Clone)]
pub(super) struct Y2b2Shared {
    pub(super) inner: std::sync::Arc<std::sync::Mutex<Y2b2State>>,
}

pub(super) struct Y2b2State {
    pub(super) opens: usize,
    pub(super) created: Vec<String>,
    pub(super) shutdowns: Vec<String>,
    /// The mode each `OpenSession` asked for, in order.
    pub(super) modes: Vec<Option<aui_protocol::PermissionMode>>,
}

pub(super) struct Y2b2Adapter {
    pub(super) rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
    pub(super) shared: Y2b2Shared,
    pub(super) id: std::sync::Mutex<Option<String>>,
    pub(super) connected: bool,
}

impl provider::ProviderAdapter for Y2b2Adapter {
    fn id(&self) -> provider::ProviderId {
        aui_protocol::Provider::Codex
    }
    fn connect(
        &mut self,
        _client: &provider::ConnectInfo,
    ) -> Result<provider::Handshake, provider::ProviderError> {
        self.connected = true;
        Ok(provider::Handshake {
            provider: aui_protocol::Provider::Codex,
            agent_name: "y2b2".into(),
            agent_version: "0.0.0".into(),
        })
    }
    fn capabilities(&self) -> provider::CapabilitySet {
        use provider::{Capability, CapabilityState};
        let native = CapabilityState::Native;
        let off = || CapabilityState::Unavailable {
            reason: "the y2b2 double opens sessions only".into(),
        };
        provider::CapabilitySet::new([
            (Capability::SessionLifecycle, native.clone()),
            (Capability::SubmitTurn, native.clone()),
            (Capability::ModelCatalog, native.clone()),
            (Capability::ForkSession, off()),
            (Capability::CompactSession, off()),
            (Capability::SessionConfig, off()),
            (Capability::SessionShell, off()),
            (Capability::SteerTurn, off()),
            (Capability::TurnControl, off()),
            (Capability::Approvals, off()),
            (Capability::Questions, off()),
            (Capability::Transcript, off()),
            (Capability::Account, off()),
            (Capability::ClientTools, off()),
            (Capability::ReasoningTraces, off()),
            (Capability::SubagentTurns, off()),
            (Capability::ImageInput, native.clone()),
        ])
    }
    fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
        if !self.connected {
            return Err(provider::ProviderError::Unavailable { reason: "not connected".into() });
        }
        match command {
            provider::Command::OpenSession { mode, .. } => {
                let id = {
                    let mut shared = self.shared.inner.lock().expect("y2b2");
                    shared.opens += 1;
                    shared.modes.push(mode);
                    let id = format!("y2b2-{}", shared.opens);
                    shared.created.push(id.clone());
                    id
                };
                *self.id.lock().expect("y2b2") = Some(id.clone());
                Ok(provider::Ack::Session { session_id: id, title: None })
            }
            provider::Command::ListModels { .. } => {
                Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "y2b2".into() })
            }
            provider::Command::ListPending { .. } => {
                Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
            }
            other => Err(provider::ProviderError::unsupported(
                other.capability(),
                "the y2b2 double opens sessions only",
            )),
        }
    }
    fn events(&self) -> crossbeam_channel::Receiver<provider::ProviderEvent> {
        self.rx.clone()
    }
    fn shutdown(&mut self) {
        // Recorded once per child: hanging up is idempotent by
        // contract ([`SessionRuntime::shutdown_lane`], and a dropped view
        // hangs up again), so a re-hangup on drop must not read as a
        // second child shut down.
        if let Some(id) = self.id.lock().expect("y2b2").clone() {
            let mut shared = self.shared.inner.lock().expect("y2b2");
            if !shared.shutdowns.contains(&id) {
                shared.shutdowns.push(id);
            }
        }
    }
}
