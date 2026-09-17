//! Errors the transport can produce.

use std::fmt;

use crate::schema::{ErrorObject, ErrorKind};

/// Anything that can go wrong talking to a `muse serve` child.
#[derive(Debug)]
pub enum MuseError {
    /// The server answered a request with a JSON-RPC error object.
    ///
    /// Branch on `error.data.kind` — never on the message, which the wire
    /// contract explicitly refuses to make a branch point (research §1.14).
    ///
    /// Boxed because `ErrorObject` is far larger than every other variant and
    /// `Result<T, MuseError>` is the return type of every method on the client.
    Rpc(Box<ErrorObject>),
    /// The child exited, or its pipes were closed, before the request settled.
    Closed,
    /// The server never answered. Carries the method that went unanswered.
    Timeout(String),
    /// A line arrived that is not a JSON-RPC frame this client understands.
    Protocol(String),
    /// Spawning the child, or reading and writing its pipes, failed.
    Io(std::io::Error),
    /// A payload did not match the typed shape it was decoded into.
    Json(serde_json::Error),
}

impl MuseError {
    /// The stable error category, when this is a server-sent JSON-RPC error.
    pub fn kind(&self) -> Option<&ErrorKind> {
        match self {
            MuseError::Rpc(err) => err.data.as_ref().map(|d| &d.kind),
            _ => None,
        }
    }

    /// Whether the server marked this error retryable.
    ///
    /// `error.data.retryable` overrides the code's table default (research
    /// §1.14); absent means "use the table", which is the caller's job.
    pub fn retryable(&self) -> Option<bool> {
        match self {
            MuseError::Rpc(err) => err.data.as_ref().and_then(|d| d.retryable),
            _ => None,
        }
    }

    /// Whether this is the stale-sidecar `-32603`: a `view/page` (or
    /// `session/read`, or `session/resume`) reached a session whose
    /// `.msp-view-v1` sidecar is stale, before a leased load regenerated it
    /// (muse 1.2.1, #29473).
    ///
    /// The `data.kind` is only `internal` — far too coarse to branch on — and
    /// the code alone covers every internal error, so the message substring is
    /// the one discriminator the server gives. That breaks the "never branch
    /// on the message" rule deliberately and narrowly: the match is a
    /// `contains` on the stable phrase `stale sidecar`, and everything else
    /// about the error keeps branching on `data.kind`.
    pub fn is_stale_sidecar(&self) -> bool {
        match self {
            MuseError::Rpc(err) => err.code == -32603 && err.message.contains("stale sidecar"),
            _ => false,
        }
    }
}



impl fmt::Display for MuseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MuseError::Rpc(err) => write!(f, "muse error {}: {}", err.code, err.message),
            MuseError::Closed => write!(f, "muse serve closed the connection"),
            MuseError::Timeout(method) => write!(f, "muse serve did not answer {method}"),
            MuseError::Protocol(line) => write!(f, "unparseable MSP frame: {line}"),
            MuseError::Io(err) => write!(f, "muse serve io: {err}"),
            MuseError::Json(err) => write!(f, "MSP payload did not match its schema: {err}"),
        }
    }
}

impl std::error::Error for MuseError {}

impl From<std::io::Error> for MuseError {
    fn from(err: std::io::Error) -> Self {
        MuseError::Io(err)
    }
}

impl From<serde_json::Error> for MuseError {
    fn from(err: serde_json::Error) -> Self {
        MuseError::Json(err)
    }
}

/// Shorthand for a transport result.
pub type Result<T> = std::result::Result<T, MuseError>;

#[cfg(test)]
mod tests {
    use super::*;

    fn rpc(code: i32, message: &str) -> MuseError {
        let kind = match code {
            -32021 => "sessionInUse",
            _ => "internal",
        };
        let object: crate::schema::ErrorObject = serde_json::from_value(serde_json::json!({
            "code": code,
            "message": message,
            "data": {"kind": kind},
        }))
        .expect("error object decodes");
        MuseError::Rpc(Box::new(object))
    }

    /// The stale-sidecar `-32603` classifies, and nothing
    /// else does — a different `-32603`, a different code, or a dead child.
    #[test]
    fn only_the_stale_sidecar_32603_classifies() {
        assert!(rpc(
            -32603,
            "internal error: read materialized session view: forward fold range read failed: \
             materialized projection head: stale sidecar generation; a leased load regenerates \
             it at first touch (#29473)"
        )
        .is_stale_sidecar());
        // Same code, another internal error: not it.
        assert!(!rpc(-32603, "internal error: something else broke").is_stale_sidecar());
        // Same message fragment, another code: not it.
        assert!(!rpc(-32021, "stale sidecar generation").is_stale_sidecar());
        // Not server errors at all.
        assert!(!MuseError::Closed.is_stale_sidecar());
        assert!(!MuseError::Timeout("view/page".into()).is_stale_sidecar());
    }
}
