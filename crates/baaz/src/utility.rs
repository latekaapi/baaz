//! One-shot completions on a session's own provider: the cheap model calls
//! behind generated titles, bylines and handoff summaries (docs/50 P2).
//!
//! Owner decision (2026-10-16): the session's own provider writes them, on
//! its cheapest model — never Muse implicitly, so a Claude Code session's
//! first prompt never leaves for Muse's model. Muse sessions keep Muse's
//! side-session route (`app/titles.rs`), which is Muse's own way to do this.
//!
//! - Claude Code: `claude -p` on `haiku`, no tools, no MCP, no settings, no
//!   saved session, in Baaz's private Claude home.
//! - Codex: `codex exec --ephemeral` read-only, on the person's default model
//!   at minimal reasoning effort (Codex reports no prices to pick a cheapest
//!   model by), in Baaz's private Codex home, the last message to a file.
//!
//! Blocking, with a deadline that kills the child: run it off the UI thread.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::providers::ProviderId;

/// The longest one completion may take before it is abandoned.
pub(crate) const COMPLETE_TIMEOUT: Duration = Duration::from_secs(60);

/// The model Claude Code completions run on.
pub(crate) const CLAUDE_UTILITY_MODEL: &str = "haiku";

/// Whether `provider` can write a completion here: Claude Code and Codex
/// (through their CLIs). Muse uses its side sessions instead.
pub(crate) fn supports(provider: ProviderId) -> bool {
    matches!(provider, ProviderId::ClaudeCode | ProviderId::Codex)
}

/// Run `prompt` once on `provider` and return the reply text, trimmed.
///
/// Never reaches a real CLI under test or in a deterministic capture: a
/// test that installs a client to reach the live lane path must not spend
/// a real model call, so tests answer through [`set_test_completer`].
pub(crate) fn complete(provider: ProviderId, prompt: &str) -> Result<String, String> {
    #[cfg(test)]
    {
        return TEST_COMPLETER.with(|slot| match slot.borrow().as_ref() {
            Some(fake) => fake(provider, prompt),
            None => Err("no completions in tests".into()),
        });
    }
    #[allow(unreachable_code)]
    if crate::clock::deterministic() {
        return Err("no completions in a deterministic capture".into());
    }
    complete_real(provider, prompt)
}

/// A test's stand-in for the CLIs.
#[cfg(test)]
type TestCompleter = Box<dyn Fn(ProviderId, &str) -> Result<String, String>>;

#[cfg(test)]
thread_local! {
    static TEST_COMPLETER: std::cell::RefCell<Option<TestCompleter>> = std::cell::RefCell::new(None);
}

/// Answer [`complete`] calls on this thread with `fake` (tests only).
#[cfg(test)]
pub(crate) fn set_test_completer(fake: impl Fn(ProviderId, &str) -> Result<String, String> + 'static) {
    TEST_COMPLETER.with(|slot| *slot.borrow_mut() = Some(Box::new(fake)));
}

fn complete_real(provider: ProviderId, prompt: &str) -> Result<String, String> {
    let program = crate::providers::resolve_program(provider)
        .ok_or_else(|| format!("{} is not installed", provider.label()))?;
    let state = crate::store::support_dir();
    let owner = std::env::var_os("HOME").map(PathBuf::from).ok_or("no HOME")?;
    let scratch = std::env::temp_dir().join(format!("baaz-utility-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let mut command = Command::new(&program);
    let mut out_file: Option<PathBuf> = None;
    match provider {
        ProviderId::ClaudeCode => {
            let home = provider_claude_code::home::ensure_home(&owner, &state).map_err(|e| e.to_string())?;
            command
                .env("CLAUDE_CONFIG_DIR", home)
                .args(["-p", "--model", CLAUDE_UTILITY_MODEL, "--output-format", "text"])
                .args(["--tools", "", "--strict-mcp-config", "--setting-sources", ""])
                .arg("--no-session-persistence");
        }
        ProviderId::Codex => {
            let home = provider_codex::home::ensure_home(&owner, &state).map_err(|e| e.to_string())?;
            let last = scratch.join(format!("codex-last-{}.txt", unique()));
            command
                .env("CODEX_HOME", home)
                .args(["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only"])
                .args(["-c", "model_reasoning_effort=\"minimal\"", "--color", "never", "-o"])
                .arg(&last)
                .arg("-");
            out_file = Some(last);
        }
        ProviderId::Muse => return Err("Muse writes these through its own side sessions".into()),
    }
    command.env("PATH", provider::env_path::login_path());
    provider::child_env::scrub_command(&mut command);
    command.current_dir(&scratch).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    let mut child = command.spawn().map_err(|e| format!("could not start {}: {e}", program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });
    let deadline = Instant::now() + COMPLETE_TIMEOUT;
    let Ok(text) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("{} took longer than {}s", provider.label(), COMPLETE_TIMEOUT.as_secs()));
    };
    let status = child.wait().map_err(|e| e.to_string())?;
    let text = match out_file {
        Some(path) => {
            let read = std::fs::read_to_string(&path).unwrap_or_default();
            let _ = std::fs::remove_file(&path);
            read
        }
        None => text,
    };
    let text = text.trim().to_owned();
    if !status.success() || text.is_empty() {
        return Err(format!("{} returned no text ({status})", provider.label()));
    }
    Ok(text)
}

/// A per-call suffix so concurrent Codex completions never share a file.
fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
