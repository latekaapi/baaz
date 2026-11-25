//! Keep the Mac awake while a goal or a delegated worker works (docs/85
//! 15.5): one power assertion, taken when work starts and released when it
//! settles. The assertion is `caffeinate -i -w <our pid>`'s (an
//! IOPMAssertion "PreventUserIdleSystemSleep", listed by `pmset -g
//! assertions`): it also ends with Baaz, so a crash never leaves the Mac
//! held awake. Spawned plainly (rule 5: never `pre_exec`).
//!
//! The one holder is this module's static: no entity owns it.

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

static HOLD: Mutex<Option<Child>> = Mutex::new(None);

/// Take the assertion (`on`) or release it; repeated calls change nothing.
/// Never in a capture or a test: neither should hold the Mac awake.
pub(crate) fn hold(on: bool) {
    if on && (cfg!(test) || crate::runtime::captures()) {
        return;
    }
    let mut held = HOLD.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    // A child that ended by itself (killed from outside) holds nothing.
    if held.as_mut().is_some_and(|child| child.try_wait().ok().flatten().is_some()) {
        *held = None;
    }
    match (on, held.is_some()) {
        (true, false) => {
            let spawned = Command::new("/usr/bin/caffeinate")
                .args(["-i", "-w", &std::process::id().to_string()])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
            match spawned {
                Ok(child) => {
                    crate::baaz_log!("keep-awake: held (caffeinate {})", child.id());
                    *held = Some(child);
                }
                Err(why) => crate::baaz_log!("keep-awake: could not hold ({why})"),
            }
        }
        (false, true) => {
            if let Some(mut child) = held.take() {
                let _ = child.kill();
                let _ = child.wait();
                crate::baaz_log!("keep-awake: released");
            }
        }
        _ => {}
    }
}

/// Whether the assertion is held now.
#[cfg(feature = "probe")]
pub(crate) fn held() -> bool {
    HOLD.lock().map(|h| h.is_some()).unwrap_or(false)
}
