//! # baaz-browser-core
//!
//! The browser's engine with no window (docs/81 D Q4, docs/82 4.11): the
//! tab and Space model, history and `browser.db`, bookmarks, the Chrome
//! import, favicons, ad blocking's lists and engine, the clone sweep, and
//! the agent page tools with their approval policy. Nothing here names gpui
//! or the aui component library, directly or through a dependency;
//! `crates/baaz/tests/core_ratchet.rs` fails if that changes.
//!
//! The app keeps its `crate::browser::<module>` paths: `browser/mod.rs`
//! imports each module from here under its old name. The stores, the
//! runtime and the session state these modules read come from
//! [`baaz_core`], imported at this crate's root under the names the
//! modules always used.

pub mod adblock;
pub mod automation;
pub mod bookmarks;
pub mod chrome_import;
pub mod clone_sweep;
pub mod favicons;
pub mod history;
pub mod sites;
pub mod tabs;

// The core's modules and logging macro, under the `crate::` paths these
// modules named when they lived in the app.
use baaz_core::{baaz_log, runtime, sessions, store};
