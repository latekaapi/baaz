//! # baaz-core
//!
//! The part of Baaz that has no window (docs/81 D Q4, docs/82 4.11): plain
//! data, stores and functions the gpui app in `crates/baaz` reads and
//! drives. Nothing here names gpui or the aui component library, directly
//! or through a dependency; `crates/baaz/tests/core_ratchet.rs` fails if
//! that changes.
//!
//! The app keeps its `crate::<module>` paths: `crates/baaz/src/main.rs`
//! imports each module from here under its old name.

pub mod attachments;
pub mod auth;
pub mod browser_prefs;
pub mod byline;
pub mod clock;
pub mod conn;
pub mod db;
pub mod details;
pub mod files;
pub mod full_output;
pub mod git_ops;
pub mod handoff;
pub mod handoff_snapshot;
pub mod history;
pub mod import;
pub mod index;
pub mod layout;
pub mod lifecycle;
pub mod list_cache;
pub mod log;
pub mod plan;
pub mod projects;
pub mod provider_search;
pub mod provider_sessions;
pub mod provider_status;
pub mod providers;
pub mod render_io;
pub mod right_watch;
pub mod runtime;
pub mod search;
pub mod sent;
pub mod session_migration;
pub mod sessions;
pub mod sidebar;
pub mod skills;
pub mod store;
pub mod system_open;
pub mod terminal_relay;
pub mod tier;
pub mod titles;
pub mod usage;
pub mod utility;
