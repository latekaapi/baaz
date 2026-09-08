//! NDJSON framing: one compact JSON-RPC 2.0 value per line, no headers.
//!
//! `muse serve` speaks newline-delimited JSON over stdio — write
//! `serde_json::to_string(&frame) + "\n"`, read with `BufReader::lines()`. The
//! server writes compact JSON and `stderr` carries no protocol at all
//! (research §1.1).
//!
//! Classification is the load-bearing part, because **both directions send
//! requests**: the server issues `approval/request` and `userInput/request` as
//! real JSON-RPC requests with its *own* id space. So a frame is dispatched on
//! shape, never on the id alone:
//!
//! | `id` | `method` | `result`/`error` | frame |
//! |---|---|---|---|
//! | yes | yes | — | [`Frame::ServerRequest`] |
//! | yes | no | `result` | [`Frame::Response`] |
//! | yes or `null` | no | `error` | [`Frame::ErrorResponse`] |
//! | no | yes | — | [`Frame::Notification`] |

use serde_json::Value;

use crate::error::MuseError;
use crate::schema::ErrorObject;

/// One decoded server→client line.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A response to one of our requests.
    Response {
        /// The client id we minted for the request.
        id: i64,
        /// The result object. MSP results are *always* objects, possibly `{}`.
        result: Value,
    },
    /// A JSON-RPC error answering one of our requests.
    ErrorResponse {
        /// The client id, or `None` for an unrecoverable parse error, which the
        /// wire contract says is the only case with a `null` id.
        id: Option<i64>,
        /// The error object, boxed: it dwarfs every other frame's payload.
        error: Box<ErrorObject>,
    },
    /// A server→client notification.
    Notification {
        /// Method name, e.g. `"item/delta"`.
        method: String,
        /// Params object; `{}` when the server omitted it.
        params: Value,
    },
    /// A server→client **request**: `approval/request` or `userInput/request`.
    ///
    /// These carry the server's own id and are **never answered with a
    /// JSON-RPC result** — settle them with `approval/decide` or
    /// `userInput/answer|cancel|clarify` (research §1.7, §1.8).
    ServerRequest {
        /// The server's id, kept verbatim so it can be echoed in diagnostics.
        /// It lives in a different id space from ours and must never be matched
        /// against our pending map.
        id: Value,
        /// Method name.
        method: String,
        /// Params object.
        params: Value,
    },
}

/// Decode one line from the child's stdout.
///
/// Blank lines yield `Ok(None)`; anything else that is not a frame this client
/// understands is a [`MuseError::Protocol`], which the caller should surface
/// rather than swallow.
pub fn parse_line(line: &str) -> crate::error::Result<Option<Frame>> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: Value = serde_json::from_str(trimmed)
        .map_err(|_| MuseError::Protocol(truncate(trimmed)))?;
    let Some(obj) = value.as_object() else {
        return Err(MuseError::Protocol(truncate(trimmed)));
    };

    let has_id = obj.contains_key("id");
    let method = obj.get("method").and_then(Value::as_str).map(str::to_owned);

    match (has_id, method) {
        (true, Some(method)) => Ok(Some(Frame::ServerRequest {
            id: obj.get("id").cloned().unwrap_or(Value::Null),
            method,
            params: obj.get("params").cloned().unwrap_or_else(empty_object),
        })),
        (false, Some(method)) => Ok(Some(Frame::Notification {
            method,
            params: obj.get("params").cloned().unwrap_or_else(empty_object),
        })),
        (true, None) => {
            let id = obj.get("id").and_then(Value::as_i64);
            if let Some(error) = obj.get("error") {
                let error: ErrorObject =
                    serde_json::from_value(error.clone()).map_err(MuseError::Json)?;
                Ok(Some(Frame::ErrorResponse { id, error: Box::new(error) }))
            } else if let Some(result) = obj.get("result") {
                let Some(id) = id else {
                    return Err(MuseError::Protocol(truncate(trimmed)));
                };
                Ok(Some(Frame::Response { id, result: result.clone() }))
            } else {
                Err(MuseError::Protocol(truncate(trimmed)))
            }
        }
        (false, None) => Err(MuseError::Protocol(truncate(trimmed))),
    }
}

/// Encode a client→server request as one NDJSON line, newline included.
pub fn request_line(id: i64, method: &str, params: Option<&Value>) -> String {
    let mut frame = serde_json::Map::new();
    frame.insert("jsonrpc".into(), Value::String("2.0".into()));
    frame.insert("id".into(), Value::from(id));
    frame.insert("method".into(), Value::String(method.into()));
    // `params` is omitted entirely when empty, never sent as `null`.
    if let Some(params) = params {
        if !is_empty_params(params) {
            frame.insert("params".into(), params.clone());
        }
    }
    let mut line = Value::Object(frame).to_string();
    line.push('\n');
    line
}

/// Encode a client→server notification (only `initialized` in v1) as one line.
pub fn notification_line(method: &str, params: Option<&Value>) -> String {
    let mut frame = serde_json::Map::new();
    frame.insert("jsonrpc".into(), Value::String("2.0".into()));
    frame.insert("method".into(), Value::String(method.into()));
    if let Some(params) = params {
        if !is_empty_params(params) {
            frame.insert("params".into(), params.clone());
        }
    }
    let mut line = Value::Object(frame).to_string();
    line.push('\n');
    line
}

fn is_empty_params(params: &Value) -> bool {
    match params {
        Value::Null => true,
        Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn truncate(line: &str) -> String {
    const LIMIT: usize = 200;
    if line.len() <= LIMIT {
        return line.to_owned();
    }
    let mut end = LIMIT;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_response() {
        let frame = parse_line(r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#)
            .unwrap()
            .unwrap();
        assert!(matches!(frame, Frame::Response { id: 1, .. }));
    }

    #[test]
    fn classifies_a_notification() {
        let frame = parse_line(r#"{"jsonrpc":"2.0","method":"item/delta","params":{"delta":"Hi."}}"#)
            .unwrap()
            .unwrap();
        let Frame::Notification { method, params } = frame else {
            panic!("expected a notification");
        };
        assert_eq!(method, "item/delta");
        assert_eq!(params["delta"], "Hi.");
    }

    #[test]
    fn a_frame_with_both_id_and_method_is_a_server_request() {
        let frame = parse_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"userInput/request","params":{"userInputId":"u"}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(frame, Frame::ServerRequest { .. }));
    }

    #[test]
    fn a_null_id_error_is_still_an_error_response() {
        let frame = parse_line(
            r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32700,"message":"bad json"}}"#,
        )
        .unwrap()
        .unwrap();
        assert!(matches!(frame, Frame::ErrorResponse { id: None, .. }));
    }

    #[test]
    fn blank_lines_are_skipped() {
        assert_eq!(parse_line("   ").unwrap(), None);
    }

    #[test]
    fn garbage_is_a_protocol_error() {
        assert!(parse_line("not json").is_err());
        assert!(parse_line(r#"{"jsonrpc":"2.0"}"#).is_err());
    }

    #[test]
    fn empty_params_are_omitted_not_null() {
        let line = request_line(3, "model/list", Some(&serde_json::json!({})));
        assert_eq!(line, "{\"jsonrpc\":\"2.0\",\"id\":3,\"method\":\"model/list\"}\n");
    }

    #[test]
    fn frames_are_compact_and_newline_terminated() {
        let line = request_line(1, "initialize", Some(&serde_json::json!({"a": 1})));
        assert!(line.ends_with('\n'));
        assert!(!line.trim_end().contains(": "));
    }
}
