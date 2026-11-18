//! One line of a provider's output, folded without taking the pump down.
//!
//! Every adapter reads its child's stdout on a pump thread and folds one
//! line at a time. A panic in that fold used to end the thread silently:
//! no `ConnectionLost`, the session read Working forever, and the mutex the
//! fold held was poisoned for every later command (docs/81 B PM-3). Each
//! pump runs its per-line step through [`guard_line`] and, when it caught a
//! panic, clears the poison it may have left and ends the session the way a
//! lost child does.

/// Run `step`, catching a panic. `Err` carries the panic's message.
pub fn guard_line(step: impl FnOnce()) -> Result<(), String> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(step)).map_err(|payload| {
        payload
            .downcast_ref::<&str>()
            .map(|text| (*text).to_owned())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "an unknown panic".to_owned())
    })
}

/// The reason a pump reports when its fold panicked: the session ends as
/// a lost connection, so the next send revives it.
pub fn panic_reason(provider: &str, message: &str) -> String {
    eprintln!("[{provider}] the output reader failed and stopped: {message}");
    format!("Baaz stopped reading {provider}'s output after an internal error")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_panic_is_caught_with_its_message() {
        assert_eq!(guard_line(|| {}), Ok(()));
        assert_eq!(guard_line(|| panic!("bad frame")), Err("bad frame".to_owned()));
        let owned = String::from("owned");
        assert_eq!(guard_line(move || panic!("{owned}")), Err("owned".to_owned()));
    }
}
