//! The right pane's content: a stub in task T1, the four real panes in T2.
//!
//! [`render`] is T2's seam. Its signature is stable — the shown
//! [`RightKind`](crate::layout::RightKind) in, an element out — so T2
//! replaces only the body with the Browser, Diff, Git and Files panes. If
//! T2 finds it needs more than the kind (the open session, the window, a
//! focus handle), it extends this signature then; nothing here guesses.

use gpui::{div, prelude::*, AnyElement, Context};

use crate::app::Harness;
use crate::layout::RightKind;

/// Render the right pane for `kind`: in this task, a labelled placeholder
/// carrying the kind's [`RightKind::label`] as visible text.
pub(crate) fn render(kind: RightKind, cx: &mut Context<Harness>) -> AnyElement {
    let _ = cx;
    div().size_full().child(kind.label()).into_any_element()
}
