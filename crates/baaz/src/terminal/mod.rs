//! The terminal dock: per-project tabs over the library's pty backend, drawn
//! by the dock's own region entity ([`dock::TerminalDock`], task 4.4).
//!
//! Tabs belong to a project ([`Project::root`][crate::projects::Project]),
//! not to a session, so they outlive session switches (D43). Nothing here
//! persists across app restarts — the tab list dies with the window
//! (`docs/14-terminal.md` §9, decision 1).

pub mod dock;
pub mod host;
pub mod intents;
pub mod run;
pub mod service;
pub mod tasks;

pub use dock::{clamp_dock_height, DOCK_DEFAULT_HEIGHT};
pub(crate) use dock::{DockFacts, TerminalDock, TerminalDockEvent};
pub use host::{Pick, TabOwner, TerminalHost, title_from_command};
#[cfg(feature = "probe")]
pub use host::deterministic_script;
pub use service::TerminalService;
