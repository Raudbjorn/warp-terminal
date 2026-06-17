//! OpenCode sidecar lifecycle for the local AI provider.
//!
//! OpenCode (https://opencode.io) is a CLI agent runtime that exposes an
//! OpenAI-compatible HTTP server. When the user picks `OpenCode` as their
//! `local_openai` provider, we spawn the binary, capture the listening port
//! from its stdout, and route Chat Completions requests to it.
//!
//! The sidecar is pooled per working directory so that sessions in different
//! repos do not share state (mirroring the pattern in `JMrtzsn/warpdrive`,
//! where each conversation is bound to a single sidecar). The pool is keyed
//! on the canonicalized working directory; if the user changes directory
//! mid-session, the next request spawns a fresh sidecar for the new cwd and
//! the old one is shut down when the last clone is dropped.
//!
//! Sidecar lifetime: a `Sidecar` keeps the child process alive for as long
//! as the `Sidecar` is referenced. When the last clone is dropped, the child
//! is sent `SIGTERM` (POSIX) or `kill` (Windows), then `wait()`ed with a
//! 1 s grace before being force-killed. A background drain task prevents
//! the child from blocking on a full pipe and owns the kill path so the
//! `Drop` impl stays non-blocking.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use futures::AsyncReadExt;
use serde::Deserialize;
use thiserror::Error;
/// Default binary name resolved from `$PATH` when the user does not pin one.
pub const DEFAULT_OPENCODE_BINARY: &str = "opencode";

/// Default command-line arguments. `--port 0` lets OpenCode pick a free
/// port and announce it on stdout, which we then parse.
pub const DEFAULT_OPENCODE_ARGS: &[&str] = &["serve", "--port", "0"];

/// Time we wait for the sidecar to print its port after spawn.
const READY_TIMEOUT: Duration = Duration::from_secs(15);


#[derive(Debug, Error)]
pub enum OpenCodeError {
    #[error("OpenCode binary `{0}` is not available on PATH")]
    BinaryNotFound(String),
    #[error("OpenCode sidecar failed to start within {0:?}")]
    StartupTimeout(Duration),
    #[error("OpenCode sidecar exited before becoming ready: {0}")]
    StartupFailure(#[allow(dead_code)] String),
    #[error("OpenCode sidecar stdout could not be parsed: {0}")]
    BadAnnouncement(String),
    #[error("OpenCode sidecar I/O failed: {0}")]
    Io(#[from] std::io::Error),
}

/// JSON announcement OpenCode prints once it is bound to a port.
/// We accept the minimum subset; unknown fields are ignored.
#[derive(Debug, Deserialize)]
struct OpenCodeAnnouncement {
    /// Listening port. Newer OpenCode versions print a bare JSON number
    /// (`{"port": 43123}`); the `#[serde(alias)]`s also accept a *numeric*
    /// `address`/`url` field. String forms (`{"url":"http://127.0.0.1:43123"}`
    /// or the plain-text `ready:43123`) cannot deserialize into `u16`, so they
    /// fall through to the URL/heuristic parsing in `from_line`.
    #[serde(alias = "address", alias = "url")]
    port: u16,
}

impl OpenCodeAnnouncement {
    fn from_line(line: &str) -> Result<Self, OpenCodeError> {
        if let Ok(parsed) = serde_json::from_str::<OpenCodeAnnouncement>(line) {
            return Ok(parsed);
        }

        // JSON parse failed; try the most explicit patterns first so
        // that URL-style addresses (e.g. `http://127.0.0.1:43123`) do
        // not get the wrong port from the first `:` separator.
        for marker in ["port=", "\"port\":"] {
            if let Some(idx) = line.find(marker) {
                let tail = &line[idx + marker.len()..];
                let digits: String = tail
                    .chars()
                    .skip_while(|c| !c.is_ascii_digit())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                if let Ok(port) = digits.parse::<u16>() {
                    if port > 0 {
                        return Ok(OpenCodeAnnouncement { port });
                    }
                }
            }
        }

        // URL pattern: `scheme://host:port` — the port is after the
        // `:` that follows the host portion, not the `:` in `://`.
        if line.contains("://") {
            if let Some(scheme_end) = line.find("://") {
                let after_scheme = &line[scheme_end + 3..];
                // Use `rfind` so the port separator is found after the host,
                // including IPv6 hosts like `[::1]:43123` whose host contains `:`.
                if let Some(host_end) = after_scheme.rfind(':') {
                    let port_str: String = after_scheme[host_end + 1..]
                        .chars()
                        .take_while(|c| c.is_ascii_digit())
                        .collect();
                    if let Ok(port) = port_str.parse::<u16>() {
                        if port > 0 {
                            return Ok(OpenCodeAnnouncement { port });
                        }
                    }
                }
            }
        }

        // Last resort: a plain trailing `:NNNN` separator (e.g.
        // `ready:43123`).
        if let Some(idx) = line.rfind(':') {
            let tail = &line[idx + 1..];
            let digits: String = tail
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            if let Ok(port) = digits.parse::<u16>() {
                if port > 0 {
                    return Ok(OpenCodeAnnouncement { port });
                }
            }
        }
        Err(OpenCodeError::BadAnnouncement(line.to_string()))
    }
}

#[allow(dead_code)]
struct SidecarInner {
    /// The bound URL, e.g. `http://127.0.0.1:43123`. Pre-built for the
    /// HTTP client so it never has to re-parse.
    base_url: String,
    /// Cached path of the working directory this sidecar was started for.
    working_dir: PathBuf,
    /// Held so the watch channel closes when the last clone of
    /// `OpenCodeSidecar` is dropped; the drain task observes that
    /// closure as a shutdown signal.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

/// Handle to a running OpenCode sidecar. Cloneable; clones share the
/// same child process. When the last clone is dropped, the child is
/// signalled to exit.
#[derive(Clone)]
pub struct OpenCodeSidecar {
    inner: Arc<SidecarInner>,
}

impl OpenCodeSidecar {
    /// Base URL the Chat Completions endpoint is reachable at.
    pub fn base_url(&self) -> &str {
        &self.inner.base_url
    }

    /// Working directory the sidecar was launched with. Kept on the
    /// public API for callers that want to log or display the
    /// sidecar's CWD; not used by the chat-completion path.
    #[allow(dead_code)]
    pub fn working_dir(&self) -> &Path {
        &self.inner.working_dir
    }
}

/// Spawn an OpenCode sidecar bound to the supplied working directory.
///
/// `command` is the binary name or absolute path; `args` is passed verbatim.
/// The first non-empty, non-`#`-prefixed line of stdout that parses as a
/// port announcement is treated as the bound port.
pub async fn spawn(
    command: &str,
    args: &[String],
    working_dir: &Path,
) -> Result<OpenCodeSidecar, OpenCodeError> {
    if command.trim().is_empty() {
        return Err(OpenCodeError::BinaryNotFound(String::new()));
    }

    let mut process = async_process::Command::new(command)
        .args(args)
        .current_dir(working_dir)
        .stdout(async_process::Stdio::piped())
        .stderr(async_process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|err| match err.kind() {
            std::io::ErrorKind::NotFound => OpenCodeError::BinaryNotFound(command.to_string()),
            _ => OpenCodeError::Io(err),
        })?;

    let stdout = process
        .stdout
        .take()
        .expect("stdout was piped in the spawn above");
    let stderr = process.stderr.take();

    // Read the announcement with a timeout; on timeout, kill the child
    // and surface a structured error.
    let announcement = tokio::time::timeout(READY_TIMEOUT, read_announcement(stdout)).await;
    let (port, stdout) = match announcement {
        Ok(Ok((ann, stdout))) => (ann.port, stdout),
        Ok(Err(err)) => {
            let _ = process.kill();
            return Err(err);
        }
        Err(_elapsed) => {
            let _ = process.kill();
            return Err(OpenCodeError::StartupTimeout(READY_TIMEOUT));
        }
    };

    // The base URL must be 127.0.0.1 (OpenCode binds loopback); we do not
    // try to honor a non-loopback address even if the announcement
    // contains one, because routing LAN-bound sidecar traffic through
    // `network_policy` would refuse it.
    let base_url = format!("http://127.0.0.1:{port}");

    // Spawn the background drain that owns the kill path.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    tokio::spawn(drain_sidecar(process, stderr, Some(stdout), shutdown_rx));

    Ok(OpenCodeSidecar {
        inner: Arc::new(SidecarInner {
            base_url,
            working_dir: working_dir.to_path_buf(),
            shutdown_tx,
        }),
    })
}

/// Read from `stdout` until we have a parseable announcement. Reads one
/// byte at a time so the announcement can be detected whether the sidecar
/// emits it on a fresh line or appended to the first line of output.
async fn read_announcement(
    mut stdout: async_process::ChildStdout,
) -> Result<(OpenCodeAnnouncement, async_process::ChildStdout), OpenCodeError> {
    let mut buffer = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        match stdout.read(&mut byte).await {
            Ok(0) => break, // EOF before announcement
            Ok(_) => {
                buffer.push(byte[0]);
                // Try to parse on every newline; this keeps the
                // recognizer from buffering arbitrarily long log lines.
                if byte[0] == b'\n' {
                    if let Ok(ann) = try_parse_buffer(&buffer) {
                        // Hand stdout back so the caller can keep draining it;
                        // dropping it here would close the pipe and risk
                        // EPIPE/blocking if the sidecar logs more to stdout.
                        return Ok((ann, stdout));
                    }
                    // Drop the non-matching line so we only ever parse the
                    // current line (avoids O(N^2) re-parsing and prevents a
                    // preceding log line from poisoning the JSON parse).
                    buffer.clear();
                }
            }
            Err(err) => return Err(OpenCodeError::Io(err)),
        }
    }
    Err(OpenCodeError::BadAnnouncement(
        "sidecar exited before announcing a port".to_string(),
    ))
}

fn try_parse_buffer(buffer: &[u8]) -> Result<OpenCodeAnnouncement, OpenCodeError> {
    let text = std::str::from_utf8(buffer)
        .map_err(|err| OpenCodeError::BadAnnouncement(err.to_string()))?
        .trim();
    if text.is_empty() || text.starts_with('#') {
        return Err(OpenCodeError::BadAnnouncement(String::new()));
    }
    OpenCodeAnnouncement::from_line(text)
}

/// Long-running task: drains stderr (so the child never blocks), and on
/// shutdown signal kills the child.
async fn drain_sidecar(
    mut child: async_process::Child,
    stderr: Option<async_process::ChildStderr>,
    stdout: Option<async_process::ChildStdout>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    // Drain stderr in a sibling task so a noisy OpenCode log cannot wedge
    // the drain loop. Future work can pipe the bytes through `tracing`.
    if let Some(mut stderr) = stderr {
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                match stderr.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_n) => {
                        // Discarded; reserved for future log capture.
                    }
                }
            }
        });
    }

    // Keep draining stdout after the announcement so the pipe stays open and
    // the sidecar never blocks (or gets EPIPE) writing further stdout.
    if let Some(mut stdout) = stdout {
        tokio::spawn(async move {
            let mut buffer = [0u8; 4096];
            loop {
                match stdout.read(&mut buffer).await {
                    Ok(0) | Err(_) => break,
                    Ok(_n) => {
                        // Discarded; reserved for future log capture.
                    }
                }
            }
        });
    }

    // Wait for either an explicit shutdown signal, or for the watch
    // channel to close (which happens when every `OpenCodeSidecar`
    // clone — and therefore every `Sender` — is dropped), or for
    // the child to exit on its own.
    let should_shutdown = tokio::select! {
        _ = child.status() => {
            // Process exited on its own; nothing to do.
            return;
        }
        changed = shutdown.changed() => {
            match changed {
                Ok(()) => *shutdown.borrow(),
                Err(_) => true, // channel closed: drop semantics
            }
        }
    };

    if !should_shutdown {
        return;
    }

    // `async_process::Child` does not expose `send_signal`; we fall
    // back to `kill()` and rely on `kill_on_drop(true)` as the safety
    // net for the period between `kill()` and the OS reaping the
    // process.
    let _ = child.kill();
    let _ = child.status().await;
}

/// Pool of `OpenCodeSidecar`s keyed by working directory. Cheap to clone;
/// clones share the same backing map. The internal `parking_lot::Mutex` is
/// only ever held for synchronous map lookups/inserts (never across an
/// `.await`), so an async mutex is unnecessary — and a sync lock lets
/// `clear()` run on the (non-Tokio) UI thread without spawning a task.
#[derive(Clone, Default)]
pub struct OpenCodeSidecarPool {
    inner: Arc<parking_lot::Mutex<HashMap<PathBuf, OpenCodeSidecar>>>,
}

impl OpenCodeSidecarPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the sidecar for `working_dir`, spawning a new one if absent.
    /// Errors propagate from the spawn call; the pool is left unchanged on
    /// failure so the next call retries.
    pub async fn get_or_spawn(
        &self,
        command: &str,
        args: &[String],
        working_dir: &Path,
    ) -> Result<OpenCodeSidecar, OpenCodeError> {
        let canonical = canonicalize_for_pool(working_dir);

        if let Some(existing) = self.inner.lock().get(&canonical).cloned() {
            return Ok(existing);
        }

        let sidecar = spawn(command, args, &canonical).await?;
        let mut guard = self.inner.lock();
        // Another caller may have raced us to insert; prefer the
        // existing sidecar so we do not leak a duplicate child.
        if let Some(existing) = guard.get(&canonical).cloned() {
            return Ok(existing);
        }
        guard.insert(canonical, sidecar.clone());
        Ok(sidecar)
    }

    /// Forget every cached sidecar. Existing clones continue to live;
    /// the next `get_or_spawn` for any key spawns a fresh sidecar.
    pub fn clear(&self) {
        self.inner.lock().clear();
    }
}

/// Canonicalize a path for use as a pool key. Falls back to the original
/// path when canonicalization fails (e.g. the directory does not exist
/// yet), since `OpenCode` is run in the user's CWD and we still want
/// stable grouping across relative and absolute forms.
fn canonicalize_for_pool(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_json_announcement() {
        let ann = OpenCodeAnnouncement::from_line("{\"port\":43123}").unwrap();
        assert_eq!(ann.port, 43123);
    }

    #[test]
    fn parses_announcement_with_address_alias() {
        let ann = OpenCodeAnnouncement::from_line("{\"address\":\"http://127.0.0.1:43123\"}")
            .unwrap();
        assert_eq!(ann.port, 43123);
    }

    #[test]
    fn parses_loose_port_pattern() {
        let ann = OpenCodeAnnouncement::from_line("listening on port=54321").unwrap();
        assert_eq!(ann.port, 54321);
    }

    #[test]
    fn parses_trailing_colon_pattern() {
        let ann = OpenCodeAnnouncement::from_line("ready:43123").unwrap();
        assert_eq!(ann.port, 43123);
    }

    #[test]
    fn rejects_zero_port() {
        let err = OpenCodeAnnouncement::from_line("port=0");
        assert!(err.is_err());
    }

    #[test]
    fn parses_url_with_address_field() {
        // The original regex would have matched the IPv4 octet `127`
        // instead of `43123`; the URL-aware parser must skip past
        // the host's `:` and read after it.
        let ann = OpenCodeAnnouncement::from_line(r#"{"address":"http://127.0.0.1:43123"}"#)
            .unwrap();
        assert_eq!(ann.port, 43123);
    }

    #[test]
    fn parses_plain_text_listening_url() {
        let ann = OpenCodeAnnouncement::from_line("Listening on http://localhost:9090").unwrap();
        assert_eq!(ann.port, 9090);
    }

    #[test]
    fn parses_url_with_url_field() {
        let ann = OpenCodeAnnouncement::from_line(r#"{"url":"https://0.0.0.0:8080"}"#).unwrap();
        assert_eq!(ann.port, 8080);
    }

    #[test]
    fn empty_command_is_a_binary_not_found() {
        let err = OpenCodeError::BinaryNotFound(String::new());
        assert!(err.to_string().contains("not available"));
    }

    #[test]
    fn pool_dedupes_by_canonical_path() {
        let path_a = std::env::current_dir().unwrap();
        let path_b = canonicalize_for_pool(&path_a);
        assert_eq!(path_a, path_b);
    }

    #[tokio::test]
    async fn spawn_rejects_empty_command() {
        let result = spawn("", &[], Path::new(".")).await;
        assert!(matches!(
            result,
            Err(OpenCodeError::BinaryNotFound(_))
        ));
    }
}
