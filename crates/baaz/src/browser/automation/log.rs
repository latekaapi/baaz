//! What a page said while an agent watched: console, uncaught errors and
//! network, read from DevTools events and bounded per tab.

use std::collections::{HashMap, VecDeque};

use serde_json::{json, Value};

const MAX_CONSOLE: usize = 500;
const MAX_ERRORS: usize = 200;
const MAX_REQUESTS: usize = 500;
const MAX_TEXT: usize = 2_000;

/// One console line.
#[derive(Debug, Clone, PartialEq)]
pub struct ConsoleLine {
    pub seq: u64,
    pub level: String,
    pub text: String,
    pub source: String,
}

/// One uncaught error.
#[derive(Debug, Clone, PartialEq)]
pub struct PageError {
    pub seq: u64,
    pub text: String,
    pub source: String,
    pub stack: String,
}

/// One network request.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    pub seq: u64,
    pub id: String,
    pub method: String,
    pub url: String,
    pub status: Option<i64>,
    pub mime: String,
    pub failed: Option<String>,
    pub done: bool,
}

/// One tab's log.
#[derive(Debug, Default)]
pub struct PageLog {
    next_seq: u64,
    console: VecDeque<ConsoleLine>,
    errors: VecDeque<PageError>,
    requests: VecDeque<Request>,
    by_id: HashMap<String, u64>,
    /// `seq` at the agent's last action: what `since: last_action` means.
    action_mark: u64,
    /// Whether the main frame navigated since the last [`Self::take_navigated`].
    navigated: bool,
    /// How many main-frame load events have fired.
    loads: u64,
    /// How many main-frame documents have loaded: refs live for one.
    documents: u64,
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_TEXT {
        return text.to_owned();
    }
    let cut: String = text.chars().take(MAX_TEXT).collect();
    format!("{cut}…")
}

fn console_argument(argument: &Value) -> String {
    if let Some(text) = argument.get("value") {
        return match text {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
    }
    argument.get("description").and_then(Value::as_str).unwrap_or("").to_owned()
}

fn frame_location(frame: &Value) -> String {
    let url = frame.get("url").and_then(Value::as_str).unwrap_or("");
    let line = frame.get("lineNumber").and_then(Value::as_i64).unwrap_or(0) + 1;
    let column = frame.get("columnNumber").and_then(Value::as_i64).unwrap_or(0) + 1;
    format!("{url}:{line}:{column}")
}

impl PageLog {
    fn seq(&mut self) -> u64 {
        self.next_seq += 1;
        self.next_seq
    }

    /// Reads one DevTools event. Unknown events are ignored.
    pub fn absorb(&mut self, method: &str, params: &Value) {
        match method {
            "Runtime.consoleAPICalled" => {
                let level = params.get("type").and_then(Value::as_str).unwrap_or("log").to_owned();
                let text = params
                    .get("args")
                    .and_then(Value::as_array)
                    .map(|args| args.iter().map(console_argument).collect::<Vec<_>>().join(" "))
                    .unwrap_or_default();
                let source = params.pointer("/stackTrace/callFrames/0").map(frame_location).unwrap_or_default();
                self.push_console(level, text, source);
            }
            "Log.entryAdded" => {
                let entry = params.get("entry").unwrap_or(&Value::Null);
                let level = entry.get("level").and_then(Value::as_str).unwrap_or("info").to_owned();
                let text = entry.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
                let source = entry.get("url").and_then(Value::as_str).unwrap_or("").to_owned();
                self.push_console(level, text, source);
            }
            "Runtime.exceptionThrown" => {
                let details = params.get("exceptionDetails").unwrap_or(&Value::Null);
                let text = details
                    .pointer("/exception/description")
                    .or_else(|| details.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or("Uncaught error");
                let source = format!(
                    "{}:{}:{}",
                    details.get("url").and_then(Value::as_str).unwrap_or(""),
                    details.get("lineNumber").and_then(Value::as_i64).unwrap_or(0) + 1,
                    details.get("columnNumber").and_then(Value::as_i64).unwrap_or(0) + 1
                );
                let stack = details
                    .pointer("/stackTrace/callFrames")
                    .and_then(Value::as_array)
                    .map(|frames| frames.iter().map(frame_location).collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                let seq = self.seq();
                self.errors.push_back(PageError { seq, text: clip(text), source, stack: clip(&stack) });
                if self.errors.len() > MAX_ERRORS {
                    self.errors.pop_front();
                }
            }
            "Network.requestWillBeSent" => {
                let id = params.get("requestId").and_then(Value::as_str).unwrap_or("").to_owned();
                let seq = self.seq();
                self.by_id.insert(id.clone(), seq);
                self.requests.push_back(Request {
                    seq,
                    id,
                    method: params.pointer("/request/method").and_then(Value::as_str).unwrap_or("GET").to_owned(),
                    url: clip(params.pointer("/request/url").and_then(Value::as_str).unwrap_or("")),
                    status: None,
                    mime: String::new(),
                    failed: None,
                    done: false,
                });
                if self.requests.len() > MAX_REQUESTS {
                    if let Some(gone) = self.requests.pop_front() {
                        self.by_id.remove(&gone.id);
                    }
                }
            }
            "Network.responseReceived" => {
                if let Some(request) = self.request_mut(params) {
                    request.status = params.pointer("/response/status").and_then(Value::as_i64);
                    request.mime = params.pointer("/response/mimeType").and_then(Value::as_str).unwrap_or("").to_owned();
                }
            }
            "Network.loadingFinished" => {
                if let Some(request) = self.request_mut(params) {
                    request.done = true;
                }
            }
            "Network.loadingFailed" => {
                let reason = params.get("errorText").and_then(Value::as_str).unwrap_or("failed").to_owned();
                if let Some(request) = self.request_mut(params) {
                    request.done = true;
                    request.failed = Some(reason);
                }
            }
            "Page.frameNavigated" => {
                if params.pointer("/frame/parentId").is_none() {
                    self.navigated = true;
                    self.documents += 1;
                }
            }
            "Page.loadEventFired" => self.loads += 1,
            _ => {}
        }
    }

    fn request_mut(&mut self, params: &Value) -> Option<&mut Request> {
        let id = params.get("requestId").and_then(Value::as_str)?;
        let seq = *self.by_id.get(id)?;
        self.requests.iter_mut().rev().find(|request| request.seq == seq)
    }

    fn push_console(&mut self, level: String, text: String, source: String) {
        let seq = self.seq();
        self.console.push_back(ConsoleLine { seq, level, text: clip(&text), source });
        if self.console.len() > MAX_CONSOLE {
            self.console.pop_front();
        }
    }

    /// How many main-frame documents have loaded.
    pub fn documents(&self) -> u64 {
        self.documents
    }

    /// How many load events have fired: `navigate` waits for this to move.
    pub fn loads(&self) -> u64 {
        self.loads
    }

    /// Marks the start of an agent action.
    pub fn mark_action(&mut self) {
        self.action_mark = self.next_seq;
    }

    /// Whether the main frame navigated since this was last asked.
    pub fn take_navigated(&mut self) -> bool {
        std::mem::take(&mut self.navigated)
    }

    /// Requests started and not finished.
    pub fn in_flight(&self) -> usize {
        self.requests.iter().filter(|request| !request.done).count()
    }

    /// Requests since the agent's last action.
    pub fn requests_since_action(&self) -> usize {
        self.requests.iter().filter(|request| request.seq > self.action_mark).count()
    }

    /// Whether anything but a GET left since the last action.
    pub fn wrote_since_action(&self) -> bool {
        self.requests.iter().any(|request| request.seq > self.action_mark && request.method != "GET")
    }

    fn floor(&self, since_action: bool) -> u64 {
        if since_action { self.action_mark } else { 0 }
    }

    /// The console, oldest first, for `browser_console`.
    pub fn console_json(&self, since_action: bool, errors_only: bool) -> Value {
        let floor = self.floor(since_action);
        let lines: Vec<Value> = self
            .console
            .iter()
            .filter(|line| line.seq > floor)
            .filter(|line| !errors_only || matches!(line.level.as_str(), "error" | "assert" | "warning" | "warn"))
            .map(|line| json!({"level": line.level, "text": line.text, "source": line.source}))
            .collect();
        json!({ "lines": lines })
    }

    /// Uncaught errors, for `browser_errors`.
    pub fn errors_json(&self, since_action: bool) -> Value {
        let floor = self.floor(since_action);
        let errors: Vec<Value> = self
            .errors
            .iter()
            .filter(|error| error.seq > floor)
            .map(|error| json!({"text": error.text, "source": error.source, "stack": error.stack}))
            .collect();
        json!({ "errors": errors })
    }

    /// Requests, for `browser_network`. `id` is what fetches a body on demand.
    pub fn network_json(&self, since_action: bool, failed_only: bool) -> Value {
        let floor = self.floor(since_action);
        let requests: Vec<Value> = self
            .requests
            .iter()
            .filter(|request| request.seq > floor)
            .filter(|request| !failed_only || request.failed.is_some() || request.status.is_some_and(|status| status >= 400))
            .map(|request| {
                json!({"id": request.id, "method": request.method, "url": request.url, "status": request.status,
                    "mime": request.mime, "failed": request.failed, "done": request.done})
            })
            .collect();
        json!({ "requests": requests })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn console(log: &mut PageLog, level: &str, text: &str) {
        log.absorb("Runtime.consoleAPICalled", &json!({"type": level, "args": [{"type":"string","value": text}]}));
    }

    #[test]
    fn console_and_log_entries_read_as_lines() {
        let mut log = PageLog::default();
        console(&mut log, "error", "boom");
        log.absorb("Log.entryAdded", &json!({"entry": {"level":"warning","text":"slow","url":"https://a/x.js"}}));
        let out = log.console_json(false, false);
        assert_eq!(out["lines"][0]["text"], "boom");
        assert_eq!(out["lines"][1]["level"], "warning");
        assert_eq!(out["lines"][1]["source"], "https://a/x.js");
    }

    #[test]
    fn since_last_action_only_shows_what_came_after() {
        let mut log = PageLog::default();
        console(&mut log, "log", "before");
        log.mark_action();
        console(&mut log, "error", "after");
        let out = log.console_json(true, false);
        assert_eq!(out["lines"].as_array().unwrap().len(), 1);
        assert_eq!(out["lines"][0]["text"], "after");
        assert_eq!(log.console_json(false, false)["lines"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn errors_only_filters_the_console() {
        let mut log = PageLog::default();
        console(&mut log, "log", "fine");
        console(&mut log, "error", "bad");
        assert_eq!(log.console_json(false, true)["lines"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn an_uncaught_error_keeps_its_place_and_stack() {
        let mut log = PageLog::default();
        log.absorb(
            "Runtime.exceptionThrown",
            &json!({"exceptionDetails": {"url":"http://localhost:5173/app.js","lineNumber":9,"columnNumber":2,
                "exception":{"description":"TypeError: x is not a function"},
                "stackTrace":{"callFrames":[{"url":"http://localhost:5173/app.js","lineNumber":9,"columnNumber":2}]}}}),
        );
        let out = log.errors_json(false);
        assert_eq!(out["errors"][0]["text"], "TypeError: x is not a function");
        assert_eq!(out["errors"][0]["source"], "http://localhost:5173/app.js:10:3");
        assert!(out["errors"][0]["stack"].as_str().unwrap().contains(":10:3"));
    }

    #[test]
    fn a_request_follows_its_events_to_done() {
        let mut log = PageLog::default();
        log.absorb("Network.requestWillBeSent", &json!({"requestId":"1","request":{"method":"POST","url":"https://a/pay"}}));
        assert_eq!(log.in_flight(), 1);
        assert!(log.wrote_since_action());
        log.absorb("Network.responseReceived", &json!({"requestId":"1","response":{"status":500,"mimeType":"application/json"}}));
        log.absorb("Network.loadingFinished", &json!({"requestId":"1"}));
        assert_eq!(log.in_flight(), 0);
        let failed = log.network_json(false, true);
        assert_eq!(failed["requests"][0]["status"], 500);
        log.mark_action();
        assert!(!log.wrote_since_action());
    }

    #[test]
    fn a_failed_request_is_done_and_says_why() {
        let mut log = PageLog::default();
        log.absorb("Network.requestWillBeSent", &json!({"requestId":"2","request":{"method":"GET","url":"https://a/"}}));
        log.absorb("Network.loadingFailed", &json!({"requestId":"2","errorText":"net::ERR_FAILED"}));
        assert_eq!(log.in_flight(), 0);
        assert_eq!(log.network_json(false, true)["requests"][0]["failed"], "net::ERR_FAILED");
    }

    #[test]
    fn every_buffer_stays_bounded() {
        let mut log = PageLog::default();
        for n in 0..(MAX_CONSOLE + 10) {
            console(&mut log, "log", &n.to_string());
        }
        for n in 0..(MAX_REQUESTS + 10) {
            log.absorb("Network.requestWillBeSent", &json!({"requestId": n.to_string(), "request":{"method":"GET","url":"u"}}));
        }
        assert_eq!(log.console.len(), MAX_CONSOLE);
        assert_eq!(log.requests.len(), MAX_REQUESTS);
        assert_eq!(log.by_id.len(), MAX_REQUESTS, "the id index shrinks with it");
    }

    #[test]
    fn only_a_main_frame_navigation_counts() {
        let mut log = PageLog::default();
        log.absorb("Page.frameNavigated", &json!({"frame":{"id":"f2","parentId":"f1"}}));
        assert!(!log.take_navigated());
        log.absorb("Page.frameNavigated", &json!({"frame":{"id":"f1"}}));
        assert!(log.take_navigated());
        assert!(!log.take_navigated(), "asked once");
    }
}
