//! Signing in and out of Muse (spec §3.2).
//!
//! MSP has no auth method, so this module is entirely out-of-band: it reads the
//! identity out of `~/.config/muse/auth.json` and it drives the `muse login`
//! child by parsing its **stderr**, which is where the launcher's device-code
//! flow prints. The second half of the boot probe — `model/list` reporting
//! `source: "providerCatalog"` — needs the wire and lives in [`crate::app`].
//!
//! Two rules this module keeps:
//!
//! * `muse login` refuses to prompt when stderr is not a tty unless
//!   `MUSE_LOGIN=1`, so the child is always spawned with it set.
//! * **The verification URL and the user code are never logged.** They are
//!   secrets for the length of the flow; they go into [`LoginEvent`] and from
//!   there straight onto the screen.

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use crossbeam_channel::{unbounded, Receiver};

/// Who the stored credential belongs to, as the sidebar footer shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Identity {
    /// `providers.meta.user_full_name`, or `"API key"` for an ambient
    /// `META_API_KEY`.
    pub name: String,
    /// `providers.meta.user_email`; empty for an API-key identity.
    pub email: String,
    /// Whether this identity came from `META_API_KEY` rather than a login.
    pub api_key: bool,
}

impl Identity {
    /// The avatar initial for the sidebar footer.
    pub fn initial(&self) -> String {
        self.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_else(|| "?".into())
    }
}

/// `~/.config/muse/auth.json`, honouring `MUSE_AUTH_PATH` and
/// `XDG_CONFIG_HOME` the way the launcher does.
pub fn auth_path() -> PathBuf {
    if let Some(path) = std::env::var_os("MUSE_AUTH_PATH") {
        return PathBuf::from(path);
    }
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("muse").join("auth.json")
}

/// The stored identity, or `None` when `providers.meta` is absent.
///
/// `META_API_KEY` takes priority over the stored credential — both `muse login`
/// and `muse logout` say so — so it is checked first and reported as the
/// "API key" identity.
pub fn identity() -> Option<Identity> {
    if std::env::var_os("META_API_KEY").is_some() {
        return Some(Identity { name: "API key".into(), email: String::new(), api_key: true });
    }
    let text = std::fs::read_to_string(auth_path()).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let meta = value.get("providers")?.get("meta")?;
    let field = |name: &str| meta.get(name).and_then(|v| v.as_str()).unwrap_or_default().to_owned();
    let name = field("user_full_name");
    let email = field("user_email");
    Some(Identity {
        name: if name.is_empty() { "Signed in".into() } else { name },
        email,
        api_key: false,
    })
}

/// One thing the `muse login` child said.
///
/// The child prints the URL and the code on separate lines after their own
/// headings, so the parser is a small state machine over stderr.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LoginEvent {
    /// The verification URL arrived.
    Url(String),
    /// The user code arrived.
    Code(String),
    /// `Waiting for approval (link expires in N minutes)...`.
    Waiting {
        /// The expiry line as the child phrased it, e.g. `"link expires in 15 minutes"`.
        expires: Option<String>,
    },
    /// `Signed in.`
    Success,
    /// A `muse: …` line, or a non-zero exit with nothing better to say.
    Failed(String),
}

/// Spawn `muse login` and stream what it says.
///
/// The child owns its own lifetime: the returned receiver closes when the child
/// exits, and dropping the receiver does not kill it (a login half-finished in
/// the browser should still be allowed to complete).
pub fn spawn_login(program: &str) -> std::io::Result<Receiver<LoginEvent>> {
    let mut child = Command::new(program)
        .arg("login")
        // Without this the launcher refuses to prompt when stderr is not a tty.
        .env("MUSE_LOGIN", "1")
        // The code is printed bold through `tput` when colour is allowed; the
        // parser strips SGR anyway, but plain output is cheaper to trust.
        .env("NO_COLOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()?;
    let stderr = child.stderr.take().expect("piped stderr");
    let (tx, rx) = unbounded();
    std::thread::Builder::new().name("muse-login".into()).spawn(move || {
        let mut want = Want::Nothing;
        let mut saw_outcome = false;
        for line in BufReader::new(stderr).lines() {
            let Ok(line) = line else { break };
            let line = strip_sgr(&line);
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            // A heading claims the next non-empty line as its value.
            if trimmed.starts_with("Open this page to sign in") {
                want = Want::Url;
                continue;
            }
            if trimmed.starts_with("Confirm this code matches") || trimmed.starts_with("Enter this code") {
                want = Want::Code;
                continue;
            }
            match std::mem::replace(&mut want, Want::Nothing) {
                Want::Url => {
                    let _ = tx.send(LoginEvent::Url(trimmed.to_owned()));
                    continue;
                }
                Want::Code => {
                    let _ = tx.send(LoginEvent::Code(trimmed.to_owned()));
                    continue;
                }
                Want::Nothing => {}
            }
            if trimmed.starts_with("Waiting for approval") {
                let _ = tx.send(LoginEvent::Waiting { expires: expiry(trimmed) });
                continue;
            }
            if trimmed == "Signed in." {
                saw_outcome = true;
                let _ = tx.send(LoginEvent::Success);
                continue;
            }
            if let Some(rest) = trimmed.strip_prefix("muse:") {
                saw_outcome = true;
                let _ = tx.send(LoginEvent::Failed(rest.trim().to_owned()));
            }
        }
        let status = child.wait();
        if !saw_outcome {
            let message = match status {
                Ok(status) if status.success() => "the sign-in ended without saying whether it worked".to_owned(),
                Ok(status) => format!("muse login exited with status {}", status.code().unwrap_or(-1)),
                Err(err) => format!("muse login could not be run: {err}"),
            };
            let _ = tx.send(LoginEvent::Failed(message));
        }
    })?;
    Ok(rx)
}

/// Run `muse logout` and report what it said. Blocking; call it off the UI
/// thread.
pub fn logout(program: &str) -> Result<(), String> {
    let output = Command::new(program).arg("logout").output().map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let text = String::from_utf8_lossy(&output.stderr);
    let line = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("muse logout failed");
    Err(strip_sgr(line).trim().to_owned())
}

/// Hand the verification page to the browser (`open <url>`).
///
/// The URL never reaches a log; it goes straight into the child's argv.
pub fn open_in_browser(url: &str) -> std::io::Result<()> {
    Command::new("open").arg(url).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// What the previous stderr line promised the next one would be.
enum Want {
    Nothing,
    Url,
    Code,
}

/// `Waiting for approval (link expires in 15 minutes)...` → `link expires in 15 minutes`.
fn expiry(line: &str) -> Option<String> {
    let start = line.find('(')? + 1;
    let end = line[start..].find(')')? + start;
    Some(line[start..end].to_owned())
}

/// Drop SGR escapes, which the launcher emits around the code when colour is on.
fn strip_sgr(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        // `ESC [ … <final>`: swallow through the first byte in `@`..`~`.
        if chars.next() != Some('[') {
            continue;
        }
        for c in chars.by_ref() {
            if ('@'..='~').contains(&c) {
                break;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_sgr_around_a_code() {
        assert_eq!(strip_sgr("\u{1b}[1mWXYZ-2946\u{1b}[0m"), "WXYZ-2946");
    }

    #[test]
    fn reads_the_expiry_out_of_the_waiting_line() {
        assert_eq!(
            expiry("Waiting for approval (link expires in 15 minutes)...").as_deref(),
            Some("link expires in 15 minutes")
        );
        assert_eq!(expiry("Waiting for approval..."), None);
    }
}
