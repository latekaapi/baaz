//! The terminal dock's state: per-project tabs over the library's pty backend.
//!
//! Tabs belong to a project ([`Project::root`][crate::projects::Project]),
//! not to a session, so they outlive session switches (D43). Nothing here
//! persists across app restarts — the tab list dies with the window
//! (`docs/14-terminal.md` §9, decision 1).

pub mod dock;
pub mod host;
pub mod intents;

pub use dock::{clamp_dock_height, DOCK_DEFAULT_HEIGHT};
pub(crate) use host::key_input;
pub use host::{Pick, TabOwner, TerminalHost, deterministic_script, title_from_command};
