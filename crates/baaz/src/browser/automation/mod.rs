//! The agent's control of a page (docs/27 §6, R2): tools written as linear async
//! jobs over [`aui_webview::PageAutomation`].
//!
//! A tool is an `async fn` that awaits DevTools calls through a [`Ctx`]. The
//! service's drain polls each [`Job`] on the UI thread, sends the one call it is
//! waiting on and hands back the answer on a later pass, so a slow page never
//! stalls a frame, and every tool can be tested by scripting answers
//! ([`script`]) with no browser at all.
//!
//! What a tool sees about one tab lives in [`TabAgent`]: the ref table, the last
//! snapshot, and the console, network and error log. A tab's refs are valid for
//! its current document; a main-frame navigation clears them.

pub mod log;
pub mod policy;
pub mod snapshot;
pub mod tools;

use std::cell::RefCell;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::task::{Context as TaskContext, Poll};
use std::time::{Duration, Instant};

use serde_json::Value;

pub use log::PageLog;
pub use snapshot::RefTable;

/// What a tool knows about one tab across calls.
#[derive(Debug, Default)]
pub struct TabAgent {
    /// `eN` ↔ backend node id, for the current document.
    pub refs: RefTable,
    /// The last snapshot's lines, for `delta` and the post-action diff.
    pub last_snapshot: Vec<String>,
    /// Console, network and errors, bounded.
    pub log: PageLog,
    /// Whether the DevTools domains the log reads are enabled.
    pub domains_on: bool,
    /// The last screenshot's hash, for `if_changed`.
    pub last_shot: Option<u64>,
    /// Which document [`Self::refs`] belongs to ([`PageLog::documents`]).
    pub refs_document: u64,
}

/// A shared handle to one tab's [`TabAgent`].
pub type TabHandle = Rc<RefCell<TabAgent>>;

#[derive(Default)]
struct Slot {
    /// The call the job is waiting to have sent.
    outgoing: Option<(String, Value)>,
    /// The answer to the call in flight.
    answer: Option<Result<Value, String>>,
    /// Whether a call is out and unanswered.
    in_flight: bool,
    /// Set each poll: what a timed wait compares against.
    now: Option<Instant>,
    /// What the job wants the person to decide.
    ask: Option<PersonAsk>,
    /// The person's answer.
    decision: Option<bool>,
}

/// A question for the person, shown on the line as a card.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonAsk {
    /// "Pay", "Send", "Sign in"…, or the agent's own for `browser_ask`.
    pub kind: String,
    /// What it is about, such as the button's name.
    pub detail: String,
    /// Whether it needs Allow / Don't allow, or only Done (login, OTP…).
    pub approval: bool,
}

/// What a running tool awaits through.
#[derive(Clone)]
pub struct Ctx {
    slot: Rc<RefCell<Slot>>,
    /// The tab this job acts on.
    pub tab: TabHandle,
}

struct CallFuture {
    slot: Rc<RefCell<Slot>>,
    request: Option<(String, Value)>,
}

impl Future for CallFuture {
    type Output = Result<Value, String>;

    fn poll(mut self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let mut slot = self.slot.borrow_mut();
        if let Some(answer) = slot.answer.take() {
            return Poll::Ready(answer);
        }
        drop(slot);
        if let Some(request) = self.request.take() {
            self.slot.borrow_mut().outgoing = Some(request);
        }
        Poll::Pending
    }
}

struct AskFuture {
    slot: Rc<RefCell<Slot>>,
    ask: Option<PersonAsk>,
}

impl Future for AskFuture {
    type Output = bool;

    fn poll(mut self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<bool> {
        if let Some(decision) = self.slot.borrow_mut().decision.take() {
            return Poll::Ready(decision);
        }
        if let Some(ask) = self.ask.take() {
            self.slot.borrow_mut().ask = Some(ask);
        }
        Poll::Pending
    }
}

struct SleepUntil {
    slot: Rc<RefCell<Slot>>,
    at: Instant,
}

impl Future for SleepUntil {
    type Output = ();

    fn poll(self: Pin<&mut Self>, _: &mut TaskContext<'_>) -> Poll<()> {
        match self.slot.borrow().now {
            Some(now) if now >= self.at => Poll::Ready(()),
            _ => Poll::Pending,
        }
    }
}

impl Ctx {
    /// Sends one DevTools call and waits for its `result`.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, String> {
        CallFuture { slot: self.slot.clone(), request: Some((method.to_owned(), params)) }.await
    }

    /// Asks the person and waits for as long as they take.
    pub async fn ask_person(&self, ask: PersonAsk) -> bool {
        AskFuture { slot: self.slot.clone(), ask: Some(ask) }.await
    }

    /// The instant of this poll.
    pub fn now(&self) -> Instant {
        self.slot.borrow().now.unwrap_or_else(Instant::now)
    }

    /// Waits without holding anything up: the job is polled again each drain.
    pub async fn sleep(&self, wait: Duration) {
        let at = self.now() + wait;
        SleepUntil { slot: self.slot.clone(), at }.await
    }
}

/// What a [`Job`] needs next.
#[derive(Debug, PartialEq)]
pub enum JobStep {
    /// Send this call, then [`Job::answer`] with what comes back.
    Send(String, Value),
    /// Show this to the person, then [`Job::decide`] with their answer. The job's
    /// deadline does not run while the person is being asked.
    Ask(PersonAsk),
    /// Nothing to do this pass: waiting on an answer or a timer.
    Waiting,
    /// Finished.
    Done(Result<Value, String>),
}

/// One running tool.
pub struct Job {
    future: Pin<Box<dyn Future<Output = Result<Value, String>>>>,
    slot: Rc<RefCell<Slot>>,
}

impl Job {
    /// Starts `tool` against `tab`. Nothing runs until the first [`Self::poll`].
    pub fn new<F, Fut>(tab: TabHandle, tool: F) -> Self
    where
        F: FnOnce(Ctx) -> Fut,
        Fut: Future<Output = Result<Value, String>> + 'static,
    {
        let slot: Rc<RefCell<Slot>> = Rc::default();
        let ctx = Ctx { slot: slot.clone(), tab };
        Self { future: Box::pin(tool(ctx)), slot }
    }

    /// Advances the job as far as it can go at `now`.
    pub fn poll(&mut self, now: Instant) -> JobStep {
        self.slot.borrow_mut().now = Some(now);
        let waker = futures::task::noop_waker();
        let mut task = TaskContext::from_waker(&waker);
        if let Poll::Ready(result) = self.future.as_mut().poll(&mut task) {
            return JobStep::Done(result);
        }
        let mut slot = self.slot.borrow_mut();
        if let Some(ask) = slot.ask.take() {
            return JobStep::Ask(ask);
        }
        match slot.outgoing.take() {
            Some((method, params)) => {
                slot.in_flight = true;
                JobStep::Send(method, params)
            }
            None => JobStep::Waiting,
        }
    }

    /// Hands the job the person's answer to [`JobStep::Ask`].
    pub fn decide(&mut self, allowed: bool) {
        self.slot.borrow_mut().decision = Some(allowed);
    }

    /// Hands the job the answer to the call [`JobStep::Send`] asked for.
    pub fn answer(&mut self, answer: Result<Value, String>) {
        let mut slot = self.slot.borrow_mut();
        slot.in_flight = false;
        slot.answer = Some(answer);
    }
}

/// Runs a [`Job`] against scripted answers, for tests.
#[cfg(test)]
pub mod script {
    use super::*;

    /// Drives `job`, answering each call through `answer`, allowing every ask,
    /// and advancing a fake clock while it waits.
    pub fn run(
        job: Job,
        answer: impl FnMut(&str, &Value) -> Result<Value, String>,
    ) -> (Vec<(String, Value)>, Result<Value, String>) {
        run_deciding(job, answer, |_| true).map_asks()
    }

    /// What [`run_deciding`] saw.
    pub struct Outcome {
        pub calls: Vec<(String, Value)>,
        pub asks: Vec<PersonAsk>,
        pub result: Result<Value, String>,
    }

    impl Outcome {
        fn map_asks(self) -> (Vec<(String, Value)>, Result<Value, String>) {
            (self.calls, self.result)
        }
    }

    /// [`run`], with the person's answers scripted.
    pub fn run_deciding(
        mut job: Job,
        mut answer: impl FnMut(&str, &Value) -> Result<Value, String>,
        mut decide: impl FnMut(&PersonAsk) -> bool,
    ) -> Outcome {
        let mut calls = Vec::new();
        let mut asks = Vec::new();
        let mut now = Instant::now();
        for _ in 0..100_000 {
            match job.poll(now) {
                JobStep::Send(method, params) => {
                    let result = answer(&method, &params);
                    calls.push((method, params));
                    job.answer(result);
                }
                JobStep::Ask(ask) => {
                    let allowed = decide(&ask);
                    asks.push(ask);
                    job.decide(allowed);
                }
                JobStep::Waiting => now += Duration::from_millis(50),
                JobStep::Done(result) => return Outcome { calls, asks, result },
            }
        }
        panic!("the job never finished; calls: {calls:?}");
    }
}
