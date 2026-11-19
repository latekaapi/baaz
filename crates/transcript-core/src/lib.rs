//! One transcript builder under every provider fold (docs/82 5.3, docs/81 B
//! F-1…F-4, §c.1).
//!
//! A provider fold decodes its wire and says what happened — a turn opened,
//! a tool started or finished, an approval was raised or settled, the child
//! was lost — by **stable id**: the item, tool-call or request id every wire
//! already carries. The [`TranscriptBuilder`] turns that into the positional
//! [`aui_protocol::Delta`]s the app folds, and owns everything the three
//! folds used to keep a private copy of:
//!
//! * block positions ([`builder`]): which block of which turn an id renders
//!   as, so no fold keeps a `turn → block count` or `id → (turn, index)` map;
//! * tool cards ([`tool`]): the verb table and its tense, the baaz terminal
//!   tools and their cards, wall-clock durations;
//! * diffs and their `+N/−N` stats ([`diff`]);
//! * the todo status vocabulary ([`todo`]);
//! * approval cards and choices ([`approval`]), and settling them when the
//!   child is lost ([`TranscriptBuilder::abandon`]).
//!
//! The builder keeps one materialised transcript (an
//! [`aui_protocol::Session`] built from exactly the deltas it emits), which is
//! how it knows positions and can re-face a card without the fold storing a
//! copy. The deltas stay positional because `aui-protocol` is (F-3's root
//! fix, stable block ids in the protocol, is a later two-repo change); the
//! builder hides that from every fold.
//!
//! Depends on `aui-protocol` (and `serde_json` for the terminal tools' JSON
//! answers) only.

pub mod approval;
pub mod builder;
pub mod diff;
pub mod failure;
pub mod todo;
pub mod tool;

pub use builder::TranscriptBuilder;
pub use tool::{
    is_terminal_tool, wall_ms_since, ToolEvent, Verbs, TERMINAL_RAN_VERB, TERMINAL_RUNNING_VERB,
};
