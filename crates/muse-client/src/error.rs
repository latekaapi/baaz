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
