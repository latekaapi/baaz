//! Cross-block text selection: the app-side fold over the library's span events.
//!
//! The library owns the span model (`aui::transcript::MessageSelection`,
//! `SpanEvent`, `SpanSession`) and the rendering; this module owns the one
//! rule the app adds: **one span at a time**. A hover or pick that commits a
//! span in one turn clears whatever another turn held, so ⌘C always copies
//! exactly what is highlighted. Presses never disturb held state (a link
//! click is a press and a release with no hover in between), plain clicks
//! clear, and events for a turn with no open drag leave everything alone.
//!
//! Everything here is pure — no `Window`, no `Context` — so the unit tests
//! below pin the fold without a gpui harness. [`crate::session::SessionView`]
//! holds the two maps and notifies only when [`apply_span_event`] reports a
//! change.

use std::collections::HashMap;

use aui::transcript::{MessageSelection, SpanEvent, SpanSession};

/// What one turn holds: its markdown source (so ⌘C slices the exact view the
/// person dragged in) and the span over it.
pub type HeldSpan = (String, MessageSelection);

/// Fold one [`SpanEvent`] for `turn_id` into the held spans and drag
/// sessions. Returns whether held state changed — the view notifies only
/// then, so hovers that commit the same span do not earn a frame.
pub fn apply_span_event(
    held: &mut HashMap<String, HeldSpan>,
    sessions: &mut HashMap<String, SpanSession>,
    turn_id: String,
    source: String,
    event: &SpanEvent,
) -> bool {
    match event {
        SpanEvent::Press { .. } => {
            // Opens the drag session; held state is untouched, so a link
            // click (press, release, no hover) never disturbs a held span.
            let before = held_snapshot(held, &turn_id);
            let session = sessions.entry(turn_id).or_default();
            session.apply(before, event);
            false
        }
        SpanEvent::Hover { .. } | SpanEvent::Pick { .. } => {
            let before = held.get(&turn_id).map(|(_, span)| span.clone());
            let session = sessions.entry(turn_id.clone()).or_default();
            let next = session.apply(before, event);
            match next {
                Some(span) => {
                    let changed = held.get(&turn_id).map(|(_, held)| held != &span).unwrap_or(true);
                    if changed {
                        // One span at a time: the commit clears every other
                        // turn first — a drag that starts in one paragraph
                        // and ends in another clears the old span.
                        held.clear();
                        held.insert(turn_id, (source, span));
                    }
                    changed
                }
                None => {
                    // Back on the anchor (a caret is not a selection), or a
                    // pick of nothing: this turn holds nothing now.
                    held.remove(&turn_id).is_some()
                }
            }
        }
        SpanEvent::Release { .. } => {
            let before = held.get(&turn_id).map(|(_, span)| span.clone());
            let mut session = sessions.remove(&turn_id).unwrap_or_default();
            let next = session.apply(before, event);
            if session.is_active() {
                // A foreign release: some other cell's press is still open,
                // so the session goes back — held state is untouched.
                sessions.insert(turn_id.clone(), session);
                return false;
            }
            match next {
                Some(_) => false,
                // A plain click clears this turn; anything else keeps what
                // was held (a link click keeps the span, a release outside
                // the press cell keeps it too).
                None => held.remove(&turn_id).is_some(),
            }
        }
    }
}

/// The span `turn_id` holds now, if any, for [`SpanSession::apply`]'s `held`
/// argument.
fn held_snapshot(held: &HashMap<String, HeldSpan>, turn_id: &str) -> Option<MessageSelection> {
    held.get(turn_id).map(|(_, span)| span.clone())
}

/// Copy text for the single held span, sliced out of its own turn's source
/// through the library's joining rules (document order, a blank line between
/// blocks, list markers kept on wholly selected items, code byte-exact).
/// `None` when nothing is held or the span addresses no cell.
pub fn span_copy_text(held: &HashMap<String, HeldSpan>) -> Option<String> {
    let (source, span) = held.values().next()?;
    aui::transcript::turn_span_selected_text(source, span)
}

/// Hold `span` over `turn_id`, clearing whatever was held — the scripted
/// equivalent of a committed drag (`select-text:`, `select-span:`).
/// Returns whether held state changed.
pub fn hold_span(
    held: &mut HashMap<String, HeldSpan>,
    turn_id: String,
    source: String,
    span: Option<MessageSelection>,
) -> bool {
    match span {
        Some(span) => {
            let changed = held.get(&turn_id).map(|(_, held)| held != &span).unwrap_or(true)
                || held.keys().any(|id| id != &turn_id);
            if changed {
                held.clear();
                held.insert(turn_id, (source, span));
            }
            changed
        }
        None => {
            if held.is_empty() {
                false
            } else {
                held.clear();
                true
            }
        }
    }
}

/// Clear every held span. Returns whether one was held, so Escape prefers
/// this over heavier dismissals.
pub fn clear_spans(held: &mut HashMap<String, HeldSpan>, sessions: &mut HashMap<String, SpanSession>) -> bool {
    let had = !held.is_empty() || sessions.values().any(SpanSession::is_active);
    held.clear();
    sessions.clear();
    had
}

#[cfg(test)]
mod tests {
    use super::*;
    use aui::transcript::{SelectionEndpoint, SelectionKey};

    fn endpoint(cell: SelectionKey, offset: usize) -> SelectionEndpoint {
        SelectionEndpoint { cell, offset }
    }

    fn para(offset: usize) -> SelectionEndpoint {
        endpoint(SelectionKey::paragraph("", 0), offset)
    }

    fn press(cell: SelectionKey, offset: usize) -> SpanEvent {
        SpanEvent::Press { cell, offset }
    }

    fn hover(cell: SelectionKey, offset: usize) -> SpanEvent {
        SpanEvent::Hover { cell, offset }
    }

    fn release(cell: SelectionKey) -> SpanEvent {
        SpanEvent::Release { cell, hovered: true, link: false }
    }

    fn source() -> String {
        "first paragraph here\n\n- alpha\n- beta\n\n```rust\nlet x = 1;\n```\n".to_owned()
    }

    #[test]
    fn a_press_alone_holds_nothing_and_disturbs_nothing() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        assert!(!apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            2
        )));
        assert!(held.is_empty());
        // The drag session is open, though: a hover now commits.
        assert!(apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &hover(SelectionKey::paragraph("", 0), 8)
        ));
        assert!(held.contains_key("t"));
    }

    #[test]
    fn a_drag_across_cells_holds_one_span() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            2
        ));
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &hover(
            SelectionKey::list_item("", 1, false, 0),
            4
        ));
        let (_, span) = held.get("t").expect("the drag commits a span");
        assert_eq!(span.anchor, endpoint(SelectionKey::paragraph("", 0), 2));
        assert_eq!(span.focus, endpoint(SelectionKey::list_item("", 1, false, 0), 4));
        // The press cell's release ends the drag and keeps the span.
        assert!(!apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &release(SelectionKey::paragraph("", 0))
        ));
        assert!(held.contains_key("t"));
    }

    #[test]
    fn a_plain_click_clears() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            2
        ));
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &hover(
            SelectionKey::paragraph("", 0),
            8
        ));
        assert!(held.contains_key("t"));
        // A new press and release with no hover in between: a plain click.
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            0
        ));
        assert!(apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &release(SelectionKey::paragraph("", 0))
        ));
        assert!(held.is_empty());
    }

    #[test]
    fn a_new_drag_clears_the_old_span() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        hold_span(
            &mut held,
            "a".into(),
            source(),
            MessageSelection::new(para(0), para(5)),
        );
        apply_span_event(&mut held, &mut sessions, "b".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            1
        ));
        // The press alone disturbs nothing — the old span survives a link
        // click's press.
        assert!(held.contains_key("a"));
        apply_span_event(&mut held, &mut sessions, "b".into(), source(), &hover(
            SelectionKey::paragraph("", 0),
            6
        ));
        assert!(!held.contains_key("a"));
        assert!(held.contains_key("b"));
    }

    #[test]
    fn a_foreign_release_is_ignored() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        hold_span(
            &mut held,
            "t".into(),
            source(),
            MessageSelection::new(para(0), para(5)),
        );
        // A press in "t", then a release naming some other cell: every
        // cell's release handler fires window-wide, only the press cell's
        // folds.
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            0
        ));
        assert!(!apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &release(SelectionKey::list_item("", 1, false, 0))
        ));
        assert!(held.contains_key("t"));
    }

    #[test]
    fn a_hover_with_no_open_drag_leaks_nothing_in() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        assert!(!apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &hover(SelectionKey::paragraph("", 0), 4)
        ));
        assert!(held.is_empty());
    }

    #[test]
    fn a_link_click_keeps_the_held_span() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        hold_span(
            &mut held,
            "t".into(),
            source(),
            MessageSelection::new(para(0), para(5)),
        );
        apply_span_event(&mut held, &mut sessions, "t".into(), source(), &press(
            SelectionKey::paragraph("", 0),
            1
        ));
        assert!(!apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &SpanEvent::Release {
                cell: SelectionKey::paragraph("", 0),
                hovered: true,
                link: true,
            }
        ));
        assert!(held.contains_key("t"));
    }

    #[test]
    fn a_double_click_pick_replaces_the_held_span() {
        let mut held = HashMap::new();
        let mut sessions = HashMap::new();
        hold_span(
            &mut held,
            "t".into(),
            source(),
            MessageSelection::new(para(0), para(5)),
        );
        let pick = MessageSelection::new(para(6), para(11));
        assert!(apply_span_event(
            &mut held,
            &mut sessions,
            "t".into(),
            source(),
            &SpanEvent::Pick { selection: pick.clone() }
        ));
        assert_eq!(held.get("t").map(|(_, span)| span), pick.as_ref());
    }

    #[test]
    fn copy_covers_a_paragraph_a_list_and_a_fence() {
        let text = "Ship the fix today\n\n- first item\n- second item\n\n```rust\nlet x = 1;\n```\n";
        let all = aui::transcript::message_select_all(text).expect("a message selects all");
        let mut held = HashMap::new();
        hold_span(&mut held, "t".into(), text.to_owned(), Some(all));
        let copied = span_copy_text(&held).expect("a held span copies");
        assert!(copied.contains("Ship the fix today"), "{copied:?}");
        // Wholly selected list items keep their markers.
        assert!(copied.contains("- first item"), "{copied:?}");
        assert!(copied.contains("- second item"), "{copied:?}");
        // Code is byte-exact.
        assert!(copied.contains("let x = 1;"), "{copied:?}");
        // Blocks join with a blank line.
        assert!(copied.contains("\n\n"), "{copied:?}");
    }

    #[test]
    fn copy_with_nothing_held_copies_nothing() {
        assert_eq!(span_copy_text(&HashMap::new()), None);
    }

    #[test]
    fn reversed_drags_copy_in_document_order() {
        let text = "alpha line\n\nbeta line\n";
        let forward = MessageSelection::new(para(0), endpoint(SelectionKey::paragraph("", 1), 4));
        let backward = MessageSelection::new(endpoint(SelectionKey::paragraph("", 1), 4), para(0));
        let mut held = HashMap::new();
        hold_span(&mut held, "t".into(), text.to_owned(), forward);
        let first = span_copy_text(&held).expect("forward copies");
        hold_span(&mut held, "t".into(), text.to_owned(), backward);
        let second = span_copy_text(&held).expect("reversed copies");
        assert_eq!(first, second);
    }
}
