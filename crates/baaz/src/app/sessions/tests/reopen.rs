//! Tests for reopening stored provider sessions (Z4, P4-B Muse records, replays).

use super::*;

/// Z4: clicking a stored provider session whose child cannot start
/// lands on the clicked session — selected, headered, failed inline —
/// never on the previous session's transcript behind a dialog. Before
/// the fix the click left the old view active (header and composer
/// naming the clicked session over its transcript) and dialogued the
/// reason.
#[gpui::test]
fn z4_a_failed_reopen_lands_inline_on_the_clicked_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("z4-reopen");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            // A tier that banners every session view it touches — the
            // failure state must show none of it.
            harness.tier = Some(crate::tier::Tier::Unavailable("no plan word".into()));
        });
    });
    let first = y2b2_open(vc, &baaz, ProviderId::Codex);
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    // Codex cannot start now.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = failing_factory("no child here");
        });
    });
    // The click on the stored session.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            assert_eq!(
                harness.open_state.pending_id.as_deref(),
                Some("s-codex-old"),
                "the click's target stays selected"
            );
            assert!(
                harness.active.is_none(),
                "no session view is shown — and no view's Muse tier banner with it"
            );
            assert!(
                harness.tier_banner().is_some(),
                "the tier would banner any view shown, so its absence is the failure state's"
            );
            let parked: Vec<String> =
                harness.registry.parked_ids();
            assert_eq!(
                parked,
                vec![first.clone()],
                "the previous session parks exactly like a switch away"
            );
            assert!(
                harness.overlays.read(cx).dialog.is_none(),
                "no modal dialog for a failed reopen"
            );
            let failure = harness.open_state.provider_open_error.clone().expect("the inline failure stands");
            assert_eq!(failure.session_id.as_deref(), Some("s-codex-old"));
            assert_eq!(failure.provider, ProviderId::Codex);
            assert!(
                failure.message.contains("no child here"),
                "the failure carries the error text: {}",
                failure.message
            );
            assert!(!harness.open_state.switch_pending, "the verbs waiting on the switch are released");
            // The header names the clicked session, like the selected
            // row does.
            let head = harness.chain_head("s-codex-old");
            let header = harness.collapsed_head_label(&head, cx).expect("the clicked session has a label");
            let row = harness
                .visible_sessions(cx)
                .iter()
                .find(|entry| entry.id == head)
                .expect("the clicked row is listed")
                .label
                .clone();
            assert_eq!(header, row, "header title == selected row title");
            assert!(crate::layout::Layout::get(cx).right_open, "the failure lands on the failed session's pane");
        });
    });
    lane_restore(state);
}

/// Z4: Retry on the inline failure re-runs the reopen — a factory that
/// answers now replaces the failure with the real session view through
/// the normal activation path.
#[gpui::test]
fn z4_retry_after_a_failed_reopen_opens_the_real_session(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("z4-retry");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _) = y2b2_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    let first = y2b2_open(vc, &baaz, ProviderId::Codex);
    vc.update(|_, cx| {
        baaz.update(cx, z4_seed_stored_codex);
    });
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = failing_factory("no child here");
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        assert!(
            baaz.read(cx).open_state.provider_open_error.is_some(),
            "the setup ends on the inline failure"
        );
    });
    // Codex answers again: Retry re-runs the reopen.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            let (factory, _) = recording_resumable_factory();
            harness.provider_factory = factory;
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.retry_provider_open(window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let view = harness.active.clone().expect("the retried lane is open");
            assert!(view.read(cx).provider_kind() != ProviderId::Muse, "the retry reopens a lane, not a muse view");
            assert_eq!(view.read(cx).session_id, "s-codex-old", "the clicked session itself");
            assert_eq!(view.read(cx).provider_kind(), ProviderId::Codex);
            assert!(
                harness.open_state.provider_open_error.is_none(),
                "the real view replaces the failure state"
            );
            assert!(harness.overlays.read(cx).dialog.is_none(), "still no dialog");
            assert_eq!(harness.open_state.pending_id.as_deref(), Some("s-codex-old"));
            assert!(!harness.open_state.switch_pending, "the switch landed");
            let parked: Vec<String> =
                harness.registry.parked_ids();
            assert_eq!(
                parked,
                vec![first.clone()],
                "the previous session is still parked, never closed or lost"
            );
            let texts: Vec<String> = view
                .read(cx)
                .session()
                .map(|session| {
                    session
                        .turns
                        .iter()
                        .flat_map(|turn| turn.blocks())
                        .filter_map(|block| match block {
                            aui_protocol::Block::Text { text, .. } => Some(text.clone()),
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            assert!(
                texts.iter().any(|text| text.contains("The header is restored")),
                "the view shows the replayed transcript, drew {texts:?}"
            );
        });
    });
    lane_restore(state);
}

/// A reopen replays settled history, and a replayed settle counted
/// nothing, so it starts no model work: no byline rewrite and no title
/// on the session's provider (2026-10-30 — an imported session, with
/// no byline of its own, could fire a paid rewrite just by being
/// opened). The replay here ends on an assistant turn with no text
/// under a stored rewritten byline: exactly the poor, changed pair the
/// rewrite fires on for a live turn. The free excerpt still lands.
#[gpui::test]
fn a_replayed_reopen_asks_no_model_for_anything(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("replay-no-model");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let asked = std::rc::Rc::new(std::cell::RefCell::new(Vec::<(ProviderId, String)>::new()));
    {
        let asked = asked.clone();
        crate::utility::set_test_completer(move |provider, prompt| {
            asked.borrow_mut().push((provider, prompt.to_owned()));
            Ok("ASK: a\nRESULT: b".into())
        });
    }
    let (factory, commands) = recording_resumable_factory_replying(None);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            z4_seed_stored_codex(harness, cx);
            // The record already counted the one exchange the replay
            // folds, as an import or an earlier session counted it.
            if let Some(record) = harness.provider_sessions.get_mut("s-codex-old") {
                record.turns = 1;
                record.title = None;
            }
            crate::layout::Layout::stage(cx, |layout| {
                layout.auto_summary = true;
                layout.auto_title = true;
            });
            harness.set_override(
                "s-codex-old",
                |meta| {
                    meta.last_ask = Some("Rewritten ask".into());
                    meta.last_summary = Some("Rewritten result".into());
                },
                cx,
            );
            assert!(
                harness.utility_available(ProviderId::Codex),
                "the completer is reachable, so only the gate keeps it quiet"
            );
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the reopened lane is open");
        assert_eq!(view.read(cx).session_id, "s-codex-old");
        assert!(
            commands.lock().expect("commands").iter().any(|c| matches!(c, provider::Command::ResumeSession { .. })),
            "the reopen resumed and replayed"
        );
        assert_eq!(
            harness.provider_sessions.get("s-codex-old").map(|record| record.turns),
            Some(1),
            "the replay counted nothing"
        );
        assert!(asked.borrow().is_empty(), "a replayed reopen asks no model: {:?}", asked.borrow());
        assert!(!harness.byline_live.contains("s-codex-old"), "no rewrite started");
        // Opening is not a turn: the row's second line stays what it
        // was (owner, 2026-11-12: clicking a row rewrote its byline).
        assert_eq!(
            harness.overrides.get("s-codex-old").and_then(|meta| meta.last_ask.clone()).as_deref(),
            Some("Rewritten ask"),
            "a reopen leaves the byline alone"
        );
    });
    lane_restore(state);
}

/// An imported session whose folder is no Baaz project shows like any
/// session from an unadopted folder: a row in Unfiled, tagged with its
/// folder (QA 2026-10-30: imported cockpit sessions were nowhere).
#[gpui::test]
fn an_imported_session_from_an_unadopted_folder_shows_in_unfiled(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("import-unfiled");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let folder = state.2.join("cockpit");
    std::fs::create_dir_all(&folder).expect("the folder");
    let folder = folder.to_string_lossy().into_owned();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let mut record = crate::provider_sessions::ProviderSessionRecord {
                provider: "claude-code".into(),
                session_id: "imp-1".into(),
                workspace: Some(folder.clone()),
                project: None,
                created_ms: 1_790_000_000_000,
                updated_ms: 1_790_419_872_640,
                turns: 4,
                title: None,
                first_prompt: Some("Build and run Cockpit".into()),
                handoff_to: None,
                handoff_from: None,
                handoff_from_provider: None,
                handoff_title: None,
                display_texts: std::collections::HashMap::new(),
                origin: Some(crate::provider_sessions::ORIGIN_IMPORTED.into()),
            };
            record.project = None;
            harness.index_loaded = true;
            harness.sessions_loaded = true;
            crate::layout::Layout::stage(cx, |layout| layout.group_by = Some(crate::layout::GroupBy::Project));
            let added = crate::import::land(&mut harness.provider_sessions, vec![record]);
            assert_eq!(added, vec!["imp-1".to_owned()]);
            harness.merge_provider_rows();
        });
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let visible = harness.visible_sessions(cx);
        assert!(visible.iter().any(|row| row.id == "imp-1"), "the imported row is visible: {:?}",
            visible.iter().map(|r| (&r.id, r.turns, &r.workspace, &r.project)).collect::<Vec<_>>());
        let grouping = harness.sidebar_grouping(cx);
        let crate::sidebar::Grouping::Project(groups) = &*grouping else { panic!("project grouping") };
        let unfiled = groups.iter().find(|g| g.id.as_ref() == crate::sidebar::OTHER_GROUP).expect("an Unfiled group");
        assert!(unfiled.sessions.iter().any(|row| row.id.as_ref() == "imp-1"), "the row is in Unfiled");
    });
    lane_restore(state);
}

/// Opening a session is never activity, whatever its record counted:
/// a reopen whose replay folds more exchanges than the record holds (an
/// imported count, a Muse list's `turnCount`, a record synthesised from
/// its row) raised the count AND stamped the record now, so the row
/// read "now" and jumped to the top of its group (QA 2026-10-30, a Muse
/// session last active 22 Oct). The count may rise; the time may not.
#[gpui::test]
fn a_reopen_that_replays_more_than_the_record_counted_keeps_the_rows_time(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("replay-keeps-time");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, _commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
            z4_seed_stored_codex(harness, cx);
            if let Some(record) = harness.provider_sessions.get_mut("s-codex-old") {
                record.turns = 0;
            }
            harness.merge_provider_rows();
        });
    });
    let row_time = |harness: &Harness| {
        harness.sessions.iter().find(|entry| entry.id == "s-codex-old").map(|entry| entry.updated.timestamp_millis())
    };
    let before = vc.update(|_, cx| row_time(baaz.read(cx)));
    assert_eq!(before, Some(1_700_000_000_000), "the row reads its record's time");
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet("s-codex-old".into(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert_eq!(
            harness.active.clone().expect("the reopened lane is open").read(cx).session_id,
            "s-codex-old"
        );
        let record = harness.provider_sessions.get("s-codex-old").expect("the record");
        assert_eq!(record.turns, 1, "the replay may correct the count");
        assert_eq!(record.updated_ms, 1_700_000_000_000, "opening is not activity: the record keeps its time");
        assert_eq!(row_time(harness), before, "the row keeps its time, so it keeps its place");
    });
    lane_restore(state);
}

/// The W5 reopen arm, end to end at the seam: a provider session
/// opened, abandoned (its views dropped like a restart drops them),
/// reinstalled from the record and clicked — travels as
/// `ResumeSession` for the stored id, lands a lane view showing the
/// replayed transcript, and records the replayed turn once.
#[gpui::test]
fn a_provider_session_reopens_with_its_history_after_a_restart(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("reopen");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    let (factory, commands) = recording_resumable_factory();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.provider_factory = factory;
            mark_muse_up(harness);
        });
    });
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| {
            let workspace = harness.workspace();
            harness.open_on_provider(ProviderId::Codex, None, workspace, window, cx);
        });
    });
    vc.run_until_parked();
    let open_id = vc.update(|_, cx| {
        baaz.read(cx).active.clone().expect("the lane opened").read(cx).session_id.clone()
    });
    assert_eq!(open_id, "s-open");
    // One turn settles, so the record counts history: a reopened
    // draft with no turns opens fresh instead (see the next test).
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            crate::provider_sessions::note_settled_turn_counted(
                &mut harness.provider_sessions,
                &open_id,
                1,
                true,
            );
            crate::provider_sessions::write(&harness.provider_sessions);
        });
    });
    // The restart: the harness forgets its views (active and parked
    // alike) and its rows, but the record the open wrote survives on
    // disk. The forgotten views stay alive in this local until
    // teardown: lanes now bridge synchronously in tests (see
    // `conn::gate_sync`), so dropping them is harmless — a real
    // restart drops them with the process instead.
    let mut forgotten = Vec::new();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            if let Some(view) = harness.active.take() {
                forgotten.push(view);
            }
            forgotten.extend(harness.registry.drain_parked());
            harness.sessions.clear();
            harness.provider_sessions = crate::provider_sessions::read();
            harness.merge_provider_rows();
        });
    });
    let _forgotten = forgotten;
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        assert!(
            harness.provider_sessions.contains_key(&open_id),
            "the record survives the restart"
        );
        assert!(
            harness
                .sessions
                .iter()
                .any(|entry| entry.id == open_id && entry.provider == crate::providers::ProviderId::Codex),
            "the sidebar reinstalls the provider row from the record"
        );
    });
    // The click: no parked view, so the record reopens.
    vc.update(|window, cx| {
        baaz.update(cx, |harness, cx| harness.resume_quiet(open_id.clone(), window, cx));
    });
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the resumed lane is open");
        assert!(view.read(cx).provider_kind() != ProviderId::Muse, "the click reopens a lane, not a muse view");
        assert_eq!(view.read(cx).session_id, open_id);
        assert!(!harness.open_state.switch_pending, "the switch landed");
        assert!(
            harness.overlays.read(cx).dialog.is_none(),
            "no failure dialog on a clean resume"
        );
        let texts: Vec<String> = view
            .read(cx)
            .session()
            .map(|session| {
                session
                    .turns
                    .iter()
                    .flat_map(|turn| turn.blocks())
                    .filter_map(|block| match block {
                        aui_protocol::Block::Text { text, .. } => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        assert!(
            texts.iter().any(|text| text.contains("The header is restored")),
            "the view shows the replayed transcript, drew {texts:?}"
        );
    });
    let sent = commands.lock().expect("commands").clone();
    assert!(
        sent.iter().any(|command| matches!(
            command,
            provider::Command::ResumeSession { session_id, metadata_only: false, .. }
            if session_id == &open_id
        )),
        "the reopen travels as ResumeSession for the stored id, drew {sent:?}"
    );
    // The replayed turn lands in the ledger once, tagged with its
    // lane and the adapter-reported cost.
    vc.update(|_, _| {
        let connection = crate::usage::open_at(&crate::usage::db_path()).expect("ledger opens");
        let kept = crate::usage::find_turn(&connection, &open_id, "a-old").expect("replayed turn recorded");
        assert_eq!(kept.provider, "codex");
        assert_eq!(kept.cost_usd, 0.04);
        assert_eq!(crate::usage::count_for_session(&connection, &open_id), 1);
    });
    lane_restore(state);
}

// ------------------------------------------------- P4-B Muse records

/// What a Muse double answers `ResumeSession` with: the session, or a
/// typed refusal the reopen still lands under.
#[derive(Clone, Copy)]
enum MuseResume {
    Ok,
    Refused(provider::RefusalHint),
}

/// A Muse-shaped double behind the factory: answers the reopen's
/// `ResumeSession` per `answer`, accepts every `PageTranscript`, lists an
/// empty catalog and no pending work, and records every command — no
/// child, no server (docs/70 §6).
fn muse_resume_factory(
    answer: MuseResume,
) -> (crate::providers::ProviderFactory, std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>) {
    use provider::ProviderAdapter as _;
    struct MuseDouble {
        rx: crossbeam_channel::Receiver<provider::ProviderEvent>,
        answer: MuseResume,
        commands: std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>,
    }
    impl provider::ProviderAdapter for MuseDouble {
        fn id(&self) -> provider::ProviderId {
            aui_protocol::Provider::Muse
        }
        fn connect(&mut self, _client: &provider::ConnectInfo) -> Result<provider::Handshake, provider::ProviderError> {
            Ok(provider::Handshake {
                provider: aui_protocol::Provider::Muse,
                agent_name: "muse-double".into(),
                agent_version: "0.0.0".into(),
            })
        }
        fn capabilities(&self) -> provider::CapabilitySet {
            use provider::{Capability, CapabilityState};
            let native = CapabilityState::Native;
            let off = || CapabilityState::Unavailable { reason: "the muse double resumes and pages only".into() };
            provider::CapabilitySet::new([
                (Capability::SessionLifecycle, native.clone()),
                (Capability::Transcript, native.clone()),
                (Capability::ModelCatalog, native.clone()),
                (Capability::SubmitTurn, off()),
                (Capability::ForkSession, off()),
                (Capability::CompactSession, off()),
                (Capability::SessionConfig, off()),
                (Capability::SessionShell, off()),
                (Capability::SteerTurn, off()),
                (Capability::TurnControl, off()),
                (Capability::Approvals, off()),
                (Capability::Questions, off()),
                (Capability::Account, off()),
                (Capability::ClientTools, off()),
                (Capability::ReasoningTraces, off()),
                (Capability::SubagentTurns, off()),
                (Capability::ImageInput, off()),
            ])
        }
        fn dispatch(&self, command: provider::Command) -> Result<provider::Ack, provider::ProviderError> {
            self.commands.lock().expect("commands").push(command.clone());
            match command {
                provider::Command::ResumeSession { session_id, .. } => match self.answer {
                    MuseResume::Ok => Ok(provider::Ack::Session { session_id, title: None }),
                    MuseResume::Refused(hint) => Err(provider::ProviderError::Refused(provider::Refusal {
                        hint,
                        title: "Refused".into(),
                        reason: "the double refuses this resume".into(),
                        severity: provider::Severity::Banner,
                        session_scoped: true,
                    })),
                },
                provider::Command::PageTranscript { .. } => Ok(provider::Ack::Accepted),
                provider::Command::ListModels { .. } => {
                    Ok(provider::Ack::ModelCatalog { models: Vec::new(), provider: "muse".into() })
                }
                provider::Command::ListPending { .. } => {
                    Ok(provider::Ack::PendingWork { approvals: Vec::new(), questions: Vec::new() })
                }
                other => Err(provider::ProviderError::unsupported(
                    other.capability(),
                    "the muse double resumes and pages only",
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
    let factory: crate::providers::ProviderFactory = std::sync::Arc::new(move |id: ProviderId| {
        assert!(id.lists_sessions(), "only the listing provider is asked for a lane here");
        let (_, rx) = crossbeam_channel::unbounded::<provider::ProviderEvent>();
        let mut adapter = MuseDouble { rx, answer, commands: factory_commands.clone() };
        adapter.connect(&conn::connect_info())?;
        Ok(provider::Provider::new(adapter))
    });
    (factory, commands)
}

/// A live harness (not scripted chrome) with Muse up on a stand-in hub,
/// the double behind the factory and one listed Muse row, `s-muse-old`,
/// made outside Baaz: 3 turns, no record.
fn p4b_harness(
    vc: &mut gpui::VisualTestContext,
    dir: &std::path::Path,
    answer: MuseResume,
) -> (gpui::Entity<Harness>, std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>) {
    let baaz = lane_harness(vc, dir);
    let (factory, commands) = muse_resume_factory(answer);
    let workspace = dir.to_string_lossy().into_owned();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.args.offline = false;
            mark_muse_up(harness);
            harness.provider_factory = factory;
            harness.sessions.clear();
            let mut row = chain_muse_entry("s-muse-old", "Chart the ferry routes", 3);
            row.workspace = Some(workspace);
            harness.sessions.push(row);
            std::sync::Arc::make_mut(&mut harness.index).insert(
                "s-muse-old".into(),
                IndexEntry {
                    title: "Ferry routes".into(),
                    first_user_prompt: Some("chart the ferry routes".into()),
                    ..Default::default()
                },
            );
            harness.sessions_loaded = true;
        });
    });
    (baaz, commands)
}

fn resumes_of(commands: &std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>) -> Vec<(String, bool)> {
    commands
        .lock()
        .expect("commands")
        .iter()
        .filter_map(|command| match command {
            provider::Command::ResumeSession { session_id, metadata_only, .. } => {
                Some((session_id.clone(), *metadata_only))
            }
            _ => None,
        })
        .collect()
}

fn pages_of(commands: &std::sync::Arc<std::sync::Mutex<Vec<provider::Command>>>) -> usize {
    commands
        .lock()
        .expect("commands")
        .iter()
        .filter(|command| matches!(command, provider::Command::PageTranscript { .. }))
        .count()
}

/// docs/70 §3/§6: the first open in Baaz of a Muse session made
/// elsewhere writes its record from the list row and reopens through
/// `reopen_provider` (a lane view, the history paged in after a
/// metadata-only resume); a second reopen, after a restart that lost
/// the row, is routed by that record alone.
#[gpui::test]
fn a_muse_reopen_without_a_record_writes_one_from_its_row_then_reads_it(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-muse-reopen");
    let vc = cx.add_empty_window();
    let (baaz, commands) = p4b_harness(vc, &state.2, MuseResume::Ok);
    assert!(vc.update(|_, cx| baaz.read(cx).provider_sessions.is_empty()), "no record before the open");
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let record = harness.provider_sessions.get("s-muse-old").expect("the open wrote a record");
        assert_eq!(record.provider, "muse");
        assert_eq!(record.turns, 3, "the row's turns, so the open-fresh rule cannot drop it");
        assert_eq!(record.title.as_deref(), Some("Ferry routes"));
        assert_eq!(record.first_prompt.as_deref(), Some("chart the ferry routes"));
        assert_eq!(record.workspace.as_deref(), Some(state.2.to_string_lossy().as_ref()));
        let view = harness.active.clone().expect("the session opened");
        assert_eq!(view.read(cx).session_id, "s-muse-old");
        assert_eq!(view.read(cx).provider_kind(), ProviderId::Muse);
        assert!(!view.read(cx).is_starting(), "the lane landed");
        assert!(harness.open_state.provider_open_error.is_none(), "no failure");
        assert!(harness.open_state.drafts.values().all(|id| id != "s-muse-old"), "a reopen is never a draft");
        assert_eq!(harness.sessions.iter().filter(|e| e.id == "s-muse-old").count(), 1, "one row");
        assert_eq!(
            harness.sessions.iter().find(|e| e.id == "s-muse-old").map(|e| e.label.as_str()),
            Some("Chart the ferry routes"),
            "the row stays the list's"
        );
    });
    assert_eq!(resumes_of(&commands), vec![("s-muse-old".to_owned(), true)], "one metadata-only resume");
    assert!(pages_of(&commands) >= 1, "the history pages in after the ack");
    let on_disk = crate::provider_sessions::read();
    assert_eq!(on_disk.get("s-muse-old").map(|r| r.provider.as_str()), Some("muse"), "the file holds it");
    // A restart: views and rows gone, the record read back from disk.
    let mut forgotten = Vec::new();
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            forgotten.extend(harness.active.take());
            forgotten.extend(harness.registry.drain_parked());
            harness.sessions.clear();
            std::sync::Arc::make_mut(&mut harness.index).clear();
            harness.provider_sessions = crate::provider_sessions::read();
            harness.merge_provider_rows();
            assert!(harness.sessions.is_empty(), "a Muse record never builds a row: the list owns it");
        });
    });
    let _forgotten = forgotten;
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    assert_eq!(
        resumes_of(&commands),
        vec![("s-muse-old".to_owned(), true), ("s-muse-old".to_owned(), true)],
        "the record alone routes the second reopen"
    );
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("reopened");
        assert_eq!(view.read(cx).session_id, "s-muse-old");
        assert_eq!(harness.provider_sessions.get("s-muse-old").map(|r| r.turns), Some(3));
    });
    lane_restore(state);
}

/// A Muse session another window holds still opens through the one
/// reopen landing: the lane attaches, read-only under the lease notice,
/// no inline failure, no dialog — and its history pages in.
#[gpui::test]
fn a_held_elsewhere_muse_reopen_lands_read_only_under_the_lease_notice(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-held");
    let vc = cx.add_empty_window();
    let (baaz, commands) =
        p4b_harness(vc, &state.2, MuseResume::Refused(provider::RefusalHint::HeldElsewhere));
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the session opened");
        assert_eq!(view.read(cx).session_id, "s-muse-old");
        assert_eq!(view.read(cx).banner_text(), Some(crate::app::LEASE_NOTICE));
        assert!(harness.open_state.provider_open_error.is_none(), "not a failed open");
        assert!(harness.overlays.read(cx).dialog.is_none(), "no dialog");
    });
    assert!(pages_of(&commands) >= 1, "the history still pages in");
    lane_restore(state);
}

/// A stale history the resume already regenerated pages in anyway:
/// the view lands normally, with no lease notice and no failure.
#[gpui::test]
fn a_stale_history_muse_reopen_pages_in_anyway(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-stale");
    let vc = cx.add_empty_window();
    let (baaz, commands) =
        p4b_harness(vc, &state.2, MuseResume::Refused(provider::RefusalHint::StaleHistory));
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let harness = baaz.read(cx);
        let view = harness.active.clone().expect("the session opened");
        assert_eq!(view.read(cx).session_id, "s-muse-old");
        assert_ne!(view.read(cx).banner_text(), Some(crate::app::LEASE_NOTICE));
        assert!(harness.open_state.provider_open_error.is_none(), "not a failed open");
    });
    assert!(pages_of(&commands) >= 1, "the history pages in");
    lane_restore(state);
}

/// Any other refusal fails the reopen inline, like a Codex reopen's.
#[gpui::test]
fn a_refused_muse_reopen_fails_inline(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-gone");
    let vc = cx.add_empty_window();
    let (baaz, _commands) =
        p4b_harness(vc, &state.2, MuseResume::Refused(provider::RefusalHint::SessionGone));
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    vc.update(|_, cx| {
        let failure = baaz.read(cx).open_state.provider_open_error.clone().expect("an inline failure");
        assert_eq!(failure.provider, ProviderId::Muse);
        assert_eq!(failure.session_id.as_deref(), Some("s-muse-old"));
    });
    lane_restore(state);
}

/// docs/70 §4: a Muse turn moves one row through the view's own lane
/// events, like Codex — the record counts it, the ledger row is tagged
/// `muse` — and the hub's `TurnRunning` / `TurnSettled` move only a
/// viewless session's row. Nothing relists (`load_sessions`).
#[gpui::test]
fn a_muse_turn_moves_one_row_and_never_relists(cx: &mut gpui::TestAppContext) {
    use crate::session::SessionEvent;
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-turn");
    let vc = cx.add_empty_window();
    let (baaz, _commands) = p4b_harness(vc, &state.2, MuseResume::Ok);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.sessions.push(chain_muse_entry("s-muse-cli", "Run from the CLI", 2));
        });
    });
    vc.update(|window, cx| baaz.update(cx, |harness, cx| harness.resume_quiet("s-muse-old".into(), window, cx)));
    vc.run_until_parked();
    let view = vc.update(|_, cx| baaz.read(cx).active.clone().expect("opened"));
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            let event = SessionEvent::ProviderTurnAccepted {
                session_id: "s-muse-old".into(),
                prompt: "and the night ferries".into(),
                turn_id: "t-4".into(),
            };
            harness.on_session_event(view.clone(), &event, cx);
            assert!(!harness.sessions_list_in_flight, "an admission never relists");
            assert_eq!(
                harness.provider_sessions.get("s-muse-old").and_then(|r| r.first_prompt.clone()).as_deref(),
                Some("chart the ferry routes"),
                "the record keeps the session's own first words"
            );
            // The hub hears the same turn: a viewed session ignores it.
            harness.on_hub_event(
                provider::ProviderEvent::TurnSettled {
                    session_id: "s-muse-old".into(),
                    turn_id: "t-4".into(),
                    outcome: provider::facts::TurnOutcome::Completed,
                },
                cx,
            );
            assert!(!harness.sessions_list_in_flight, "a settle never relists");
        });
    });
    // The view settles the turn: four folded exchanges would be the
    // fold's word; here the record's count only ever moves up.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            if let Some(record) = harness.provider_sessions.get_mut("s-muse-old") {
                record.turns = 4;
            }
            let event = SessionEvent::ProviderTurnFinished {
                session_id: "s-muse-old".into(),
                turn_id: "t-4".into(),
                meta: aui_protocol::TurnMeta::default(),
            };
            harness.on_session_event(view.clone(), &event, cx);
            assert!(!harness.sessions_list_in_flight, "a finished turn never relists");
            let row = harness.sessions.iter().find(|e| e.id == "s-muse-old").expect("the row");
            assert_eq!(row.turns, 4, "the one row moved");
            assert_eq!(row.label, "Chart the ferry routes", "and stays the list's row");
        });
    });
    // A session with no view here (the muse CLI): the hub moves its row.
    vc.update(|_, cx| {
        baaz.update(cx, |harness, cx| {
            harness.on_hub_event(
                provider::ProviderEvent::TurnRunning { session_id: "s-muse-cli".into(), turn_id: "t-9".into() },
                cx,
            );
            assert!(harness.sessions.iter().any(|e| e.id == "s-muse-cli" && e.running), "it reads running");
            harness.on_hub_event(
                provider::ProviderEvent::TurnSettled {
                    session_id: "s-muse-cli".into(),
                    turn_id: "t-9".into(),
                    outcome: provider::facts::TurnOutcome::Completed,
                },
                cx,
            );
            assert!(!harness.sessions_list_in_flight, "a settle never relists");
            // It stands down on the idle `statusChanged`, as before P4-B
            // (docs/64 §4 item 4) — still one row, still no relist.
            harness.on_hub_event(
                provider::ProviderEvent::StatusChanged {
                    session_id: "s-muse-cli".into(),
                    running: false,
                    attention: Vec::new(),
                },
                cx,
            );
            assert!(harness.sessions.iter().any(|e| e.id == "s-muse-cli" && !e.running), "and stands down");
            assert!(!harness.sessions_list_in_flight, "with no relist");
            assert!(!harness.provider_sessions.contains_key("s-muse-cli"), "and writes no record");
        });
    });
    vc.run_until_parked();
    lane_restore(state);
}

/// Live check of 7ff7f92: a state dir holding a Muse record but not
/// the Claude Code / Codex records drew every chain member those
/// records anchor as a "New session · now" row — `merge_provider_rows`
/// synthesised a row for any override id a handoff link names, once
/// any record existed (before P4-B only a Claude/Codex record did
/// that). Only a destination's own override (it carries
/// `handoff_from`) may stand in for its row.
#[gpui::test]
fn a_record_never_conjures_rows_for_chain_members_it_does_not_hold(cx: &mut gpui::TestAppContext) {
    cx.update(|cx| aui::init(aui_tokens::ThemeKind::Dark, cx));
    let state = lane_state("p4b-ghost-rows");
    let vc = cx.add_empty_window();
    let baaz = lane_harness(vc, &state.2);
    vc.update(|_, cx| {
        baaz.update(cx, |harness, _| {
            harness.sessions.clear();
            harness.overrides.clear();
            // The trigger: one Muse record (a reopened session).
            crate::provider_sessions::upsert_listed(
                &mut harness.provider_sessions,
                "muse",
                "s-muse-opened",
                None,
                None,
                Some("Opened".into()),
                None,
                2,
                1,
                1,
            );
            harness.sessions.push(chain_muse_entry("s-muse-opened", "Opened", 2));
            // A Muse source handed off to a Claude Code session whose
            // record this state dir does not hold…
            harness.overrides.insert(
                "s-muse-src".into(),
                crate::sessions::SessionMeta { handoff_to: Some("s-claude-missing".into()), ..Default::default() },
            );
            harness.sessions.push(chain_muse_entry("s-muse-src", "Source", 1));
            // Both members keep overrides of their own (a project, a
            // derived title), as the owner's do.
            for id in ["s-claude-missing", "s-codex-missing"] {
                harness.overrides.insert(
                    id.into(),
                    crate::sessions::SessionMeta { project: Some("p-harness".into()), ..Default::default() },
                );
            }
            // …and a Muse destination of a Codex source with no record:
            // the destination stands in for itself, the source does not.
            harness.overrides.insert(
                "s-muse-dest".into(),
                crate::sessions::SessionMeta {
                    handoff_from: Some("s-codex-missing".into()),
                    handoff_from_provider: Some("codex".into()),
                    handoff_title: Some("Chain title".into()),
                    ..Default::default()
                },
            );
            harness.merge_provider_rows();
            let ids: Vec<&str> = harness.sessions.iter().map(|e| e.id.as_str()).collect();
            assert!(!ids.contains(&"s-claude-missing"), "no ghost for a member named by a link: {ids:?}");
            assert!(!ids.contains(&"s-codex-missing"), "no ghost for a source named by a link: {ids:?}");
            let dest = harness.sessions.iter().find(|e| e.id == "s-muse-dest").expect("the destination's row");
            assert_eq!(dest.label, "Chain title");
        });
    });
    lane_restore(state);
}

/// An unknown provider id in a record is refused as unknown — never
/// reopened on `ProviderId::parse`'s Muse fallback — while a Muse
/// record parses as Muse.
#[test]
fn a_records_provider_parses_strictly() {
    assert_eq!(ProviderId::parse_known("muse"), Some(ProviderId::Muse));
    assert_eq!(ProviderId::parse_known("codex"), Some(ProviderId::Codex));
    assert_eq!(ProviderId::parse_known("claude-code"), Some(ProviderId::ClaudeCode));
    assert_eq!(ProviderId::parse_known("echo"), None);
    assert_eq!(ProviderId::parse_known(""), None);
    assert_eq!(ProviderId::parse("echo"), ProviderId::Muse, "the flag's fallback is unchanged");
}
