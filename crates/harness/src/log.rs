//! The one place that spells the `harness: …` stderr prefix.
//!
//! Every hand-rolled `eprintln!("harness: …")` site used to retype the
//! prefix itself (finding `app-core-16` / `A-MECH-12`); this macro is the
//! only place it is spelled now, so a future change to where these lines go
//! — or whether they carry a timestamp, a level, anything — is one edit.

/// Print one line to stderr, prefixed `harness: `.
#[macro_export]
macro_rules! harness_log {
    ($($arg:tt)*) => {
        eprintln!("harness: {}", format_args!($($arg)*))
    };
}
