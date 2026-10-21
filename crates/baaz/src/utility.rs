//! One-shot completions on a session's own provider: the cheap model calls
//! behind generated titles, bylines and handoff summaries (docs/50 P2).
//!
//! Owner decision (2026-10-16): the session's own provider writes them, on
//! its cheapest model — never Muse implicitly, so a Claude Code session's
//! first prompt never leaves for Muse's model.
//!
//! - Muse: a hidden side session on the hub's cheapest model
//!   ([`provider_muse::MuseHub::complete`]) — Muse has no one-shot verb. The
//!   app mints and records the side id first ([`MuseSide`]), so the row
//!   stays hidden even after a crash.
//!
//! - Claude Code: `claude -p` on `haiku`, no tools, no MCP, no settings, no
//!   saved session, in Baaz's private Claude home.
//! - Codex: `codex exec --ephemeral` read-only, on the session's own model
//!   at low reasoning effort (Codex reports no prices to pick a cheapest
//!   model by), in Baaz's private Codex home, the last message to a file.
//!   Never the CLI's own default: `config.toml` may name a model the
//!   account cannot run (`gpt-6.1-sol` → 400 "not supported when using
//!   Codex with a ChatGPT account"), and `minimal` effort is rejected by
//!   every current catalog row ("Supported values are: 'none', 'low', …")
//!   — both reproduced 2026-10-17, and why no Codex session ever got a
//!   title. The app-server session resolves its model from `model/list`,
//!   so the session's model is one the account is known to run.
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

/// How long a Muse completion may run. Longer than the titler's watchdog
/// on purpose: the turn is paid for, so a late answer still lands.
pub(crate) const MUSE_COMPLETE_TIMEOUT: Duration = Duration::from_secs(180);

/// What a Muse completion rides: the hub, the side session the app minted
/// and recorded for it, and the turn's display text.
pub(crate) struct MuseSide {
    pub hub: provider_muse::MuseHub,
    pub side_id: String,
    pub label: &'static str,
}

/// Whether `provider` can write a completion through its CLI: Claude Code
/// and Codex. Muse writes them through the hub ([`MuseSide`]).
pub(crate) fn supports(provider: ProviderId) -> bool {
    matches!(provider, ProviderId::ClaudeCode | ProviderId::Codex)
}

/// Run `prompt` once on `provider` and return the reply text, trimmed.
///
/// Never reaches a real CLI under test or in a deterministic capture: a
/// test that installs a client to reach the live lane path must not spend
/// a real model call, so tests answer through [`set_test_completer`].
///
/// `model` is the session's own model, used by Codex (see the module docs);
/// Claude Code always runs [`CLAUDE_UTILITY_MODEL`].
/// `muse` is the side session a Muse completion rides; `None` for the CLIs
/// (and a Muse completion without one fails: Muse is not up).
pub(crate) fn complete(
    provider: ProviderId,
    model: Option<&str>,
    prompt: &str,
    muse: Option<MuseSide>,
) -> Result<String, String> {
    #[cfg(test)]
    {
        let _ = (model, muse);
        return TEST_COMPLETER.with(|slot| match slot.borrow().as_ref() {
            Some(fake) => fake(provider, prompt),
            None => Err("no completions in tests".into()),
        });
    }
    #[allow(unreachable_code)]
    if crate::clock::deterministic() {
        return Err("no completions in a deterministic capture".into());
    }
    if provider == ProviderId::Muse {
        let muse = muse.ok_or("Muse is not running")?;
        let workspace = crate::titles::side_workspace_dir().to_string_lossy().into_owned();
        return muse
            .hub
            .complete(&muse.side_id, &workspace, prompt, muse.label, MUSE_COMPLETE_TIMEOUT)
            .map_err(|error| error.to_string());
    }
    complete_real(provider, model, prompt)
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

/// The reasoning effort Codex completions run at: the lowest level every
/// current catalog row accepts (`minimal` is refused by all of them).
pub(crate) const CODEX_UTILITY_EFFORT: &str = "low";

/// The `codex exec` arguments for one completion writing its last message
/// to `last`, the prompt on stdin.
fn codex_args(model: Option<&str>, last: &std::path::Path) -> Vec<std::ffi::OsString> {
    let mut args: Vec<std::ffi::OsString> =
        ["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only"].map(Into::into).into();
    if let Some(model) = model.filter(|m| !m.trim().is_empty()) {
        args.extend(["-m".into(), model.into()]);
    }
    args.extend([
        "-c".into(),
        format!("model_reasoning_effort=\"{CODEX_UTILITY_EFFORT}\"").into(),
        "--color".into(),
        "never".into(),
        "-o".into(),
        last.as_os_str().to_owned(),
        "-".into(),
    ]);
    args
}

fn complete_real(provider: ProviderId, model: Option<&str>, prompt: &str) -> Result<String, String> {
    let program = crate::providers::resolve_program(provider)
        .ok_or_else(|| format!("{} is not installed", provider.label()))?;
    let state = crate::store::support_dir();
    let owner = std::env::var_os("HOME").map(PathBuf::from).ok_or("no HOME")?;
    let scratch = std::env::temp_dir().join(format!("baaz-utility-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&scratch);
    let mut command = Command::new(&program);
    let mut out_file: Option<PathBuf> = None;
    let home_env: Vec<(String, String)>;
    match provider {
        ProviderId::ClaudeCode => {
            let home = provider_claude_code::home::ensure_home(&owner, &state).map_err(|e| e.to_string())?;
            // The session children's pair, not `CLAUDE_CONFIG_DIR` alone:
            // without the empty secure-storage dir the CLI looks for a
            // suffixed Keychain item that does not exist and exits "Not
            // logged in" (every title failed so, reproduced 2026-10-17).
            home_env = provider_claude_code::home::child_env(&home);
            command
                .args(["-p", "--model", CLAUDE_UTILITY_MODEL, "--output-format", "text"])
                .args(["--tools", "", "--strict-mcp-config", "--setting-sources", ""])
                .arg("--no-session-persistence");
        }
        ProviderId::Codex => {
            let home = provider_codex::home::ensure_home(&owner, &state).map_err(|e| e.to_string())?;
            let last = scratch.join(format!("codex-last-{}.txt", unique()));
            home_env = vec![("CODEX_HOME".to_owned(), home.to_string_lossy().into_owned())];
            command.args(codex_args(model, &last));
            out_file = Some(last);
        }
        ProviderId::Muse => return Err("Muse completes through the hub".into()),
    }
    command.env("PATH", provider::env_path::login_path());
    provider::child_env::scrub_command(&mut command);
    // After the scrub, which drops an inherited home of the same name.
    command.envs(home_env);
    command.current_dir(&scratch).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|e| format!("could not start {}: {e}", program.display()))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(prompt.as_bytes());
    }
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    // Drained beside stdout so a chatty CLI never blocks on a full pipe;
    // its last line is the failure's reason (Codex prints the API error).
    let mut stderr = child.stderr.take().ok_or("no stderr")?;
    let stderr_reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text);
        text
    });
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
        let stderr = stderr_reader.join().unwrap_or_default();
        let reason = stderr.lines().rev().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
        return Err(format!("{} returned no text ({status}) {reason}", provider.label()).trim_end().to_owned());
    }
    Ok(text)
}

/// A per-call suffix so concurrent Codex completions never share a file.
fn unique() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[std::ffi::OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().into_owned()).collect()
    }

    #[test]
    fn codex_runs_the_sessions_model_at_an_effort_every_row_accepts() {
        let args = strings(&codex_args(Some("gpt-5.6-luna"), std::path::Path::new("/tmp/last.txt")));
        let model = args.iter().position(|a| a == "-m").expect("a model flag");
        assert_eq!(args[model + 1], "gpt-5.6-luna");
        assert!(args.contains(&"model_reasoning_effort=\"low\"".to_owned()), "{args:?}");
        assert!(!args.iter().any(|a| a.contains("minimal")), "minimal is refused by every row: {args:?}");
        assert_eq!(args.last().map(String::as_str), Some("-"), "the prompt rides stdin");
    }

    #[test]
    fn codex_without_a_known_model_names_none() {
        let args = strings(&codex_args(None, std::path::Path::new("/tmp/last.txt")));
        assert!(!args.contains(&"-m".to_owned()), "{args:?}");
        let blank = strings(&codex_args(Some(" "), std::path::Path::new("/tmp/last.txt")));
        assert!(!blank.contains(&"-m".to_owned()), "{blank:?}");
    }
}
