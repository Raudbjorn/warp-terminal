//! OpenCode sidecar lifecycle for the local AI provider.
//!
//! OpenCode (https://opencode.ai) is a CLI agent runtime whose `serve`
//! command exposes an HTTP API. It does **not** serve OpenAI Chat
//! Completions; it is session based. When the user picks `OpenCode` as their
//! `local_openai` provider, we spawn the binary, capture the listening port
//! from its stdout, and for each request create a throwaway session, call
//! `POST /api/session/{id}/generate`, and delete the session again.
//!
//! The server listens on loopback and refuses unauthenticated requests. We
//! pick a random password per sidecar, hand it over through
//! `OPENCODE_SERVER_PASSWORD`, and authenticate with HTTP Basic auth, so
//! other local processes cannot drive the sidecar. Sessions are created with
//! a deny-all permission ruleset: OpenCode is a coding agent, and a
//! command-generation prompt must never be able to run tools.
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
//! is sent `kill()` (SIGKILL on POSIX, `TerminateProcess` on Windows) and
//! `wait()`ed. `async_process::Child` does not expose `send_signal`, so we
//! cannot send SIGTERM with a grace period; the kill is immediate. A background
//! drain task prevents the child from blocking on a full pipe and owns the
//! kill path so the `Drop` impl stays non-blocking.

use futures::io::{AsyncBufReadExt, BufReader};
use futures::AsyncReadExt;
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;
/// Default binary name resolved from `$PATH` when the user does not pin one.
pub const DEFAULT_OPENCODE_BINARY: &str = "opencode";

/// Default command-line arguments. `--port 0` lets OpenCode pick a free
/// port and announce it on stdout, which we then parse.
pub const DEFAULT_OPENCODE_ARGS: &[&str] = &["serve", "--port", "0"];

/// Time we wait for the sidecar to print its port after spawn. A cold first
/// start (database migration) has been observed to take ~20s.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Environment variables OpenCode reads its Basic-auth credentials from. The
/// username is pinned so an `OPENCODE_SERVER_USERNAME` in the user's own
/// environment cannot make our requests fail with 401.
const SERVER_USERNAME_ENV: &str = "OPENCODE_SERVER_USERNAME";
const SERVER_PASSWORD_ENV: &str = "OPENCODE_SERVER_PASSWORD";
const SERVER_USERNAME: &str = "opencode";

/// Longest slice of an unparseable error body we echo into an error message.
const ERROR_BODY_PREVIEW_CHARS: usize = 200;

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
    #[error("No working directory available for OpenCode sidecar")]
    NoWorkingDirectory,
    #[error("OpenCode sidecar I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(
        "OpenCode model `{0}` must be written `providerID/modelID`, e.g. `ollama/qwen2.5-coder:7b`"
    )]
    InvalidModel(String),
    #[error("OpenCode request failed: {0}")]
    Request(String),
    #[error("OpenCode returned no text")]
    EmptyResponse,
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
            // Reject port 0 even in JSON form (bind(2) would assign a random
            // port, but the announcement should report the actual bound port).
            if parsed.port == 0 {
                return Err(OpenCodeError::BadAnnouncement(line.to_string()));
            }
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
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
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
    /// Set by the drain task once the child has exited on its own (crash,
    /// external kill). The pool uses it to evict and respawn instead of
    /// handing out a URL nothing is listening on.
    exited: Arc<AtomicBool>,
    /// Basic-auth password this sidecar was started with.
    password: String,
}

/// Handle to a running OpenCode sidecar. Cloneable; clones share the
/// same child process. When the last clone is dropped, the child is
/// signalled to exit.
#[derive(Clone)]
pub struct OpenCodeSidecar {
    inner: Arc<SidecarInner>,
}

impl OpenCodeSidecar {
    /// Run one prompt through the sidecar and return the generated text.
    /// `model` must be `providerID/modelID`. `timeout` applies to each of the
    /// (up to three) HTTP calls, not to the whole exchange.
    pub async fn generate(
        &self,
        client: &http_client::Client,
        model: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String, OpenCodeError> {
        generate_via_session(
            client,
            &self.inner.base_url,
            &self.inner.password,
            &self.inner.working_dir,
            model,
            prompt,
            timeout,
        )
        .await
    }

    /// Whether the child process is still running. `false` once it has
    /// exited on its own; the pool replaces such sidecars on the next lookup.
    pub fn is_alive(&self) -> bool {
        !self.inner.exited.load(Ordering::Acquire)
    }

    /// Working directory the sidecar was launched with. Kept on the
    /// public API for callers that want to log or display the
    /// sidecar's CWD; the session path passes it to OpenCode as the
    /// session location.
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

    let password = generate_password();
    let mut process = async_process::Command::new(command)
        .args(args)
        .current_dir(working_dir)
        .env(SERVER_USERNAME_ENV, SERVER_USERNAME)
        .env(SERVER_PASSWORD_ENV, &password)
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
    let exited = Arc::new(AtomicBool::new(false));
    tokio::spawn(drain_sidecar(
        process,
        stderr,
        Some(stdout),
        shutdown_rx,
        Arc::clone(&exited),
    ));

    Ok(OpenCodeSidecar {
        inner: Arc::new(SidecarInner {
            base_url,
            working_dir: working_dir.to_path_buf(),
            shutdown_tx,
            exited,
            password,
        }),
    })
}

/// Read `stdout` line by line until one parses as a port announcement.
/// Lines are read as raw bytes (not `String`s) so a non-UTF-8 log line is
/// skipped by `try_parse_buffer` rather than failing the whole startup.
async fn read_announcement(
    stdout: async_process::ChildStdout,
) -> Result<(OpenCodeAnnouncement, async_process::ChildStdout), OpenCodeError> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::with_capacity(256);
    loop {
        // Only ever parse the current line, so a preceding log line cannot
        // poison the parse and nothing is re-parsed.
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) => break, // EOF before announcement
            Ok(_) => {
                if let Ok(ann) = try_parse_buffer(&line) {
                    // Hand stdout back so the caller can keep draining it;
                    // dropping it here would close the pipe and risk
                    // EPIPE/blocking if the sidecar logs more to stdout.
                    // Bytes the reader buffered past this line are dropped,
                    // which is fine: the drain task discards stdout anyway.
                    return Ok((ann, reader.into_inner()));
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

/// Long-running task: drains stderr (so the child never blocks), flags
/// `exited` if the child dies on its own, and on shutdown signal kills it.
async fn drain_sidecar(
    mut child: async_process::Child,
    stderr: Option<async_process::ChildStderr>,
    stdout: Option<async_process::ChildStdout>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    exited: Arc<AtomicBool>,
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
            // Process exited on its own; mark the handle dead so the pool
            // evicts it rather than reusing a stale URL.
            exited.store(true, Ordering::Release);
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

/// Random Basic-auth password for one sidecar (~240 bits from the OS RNG).
/// The sidecar listens on loopback, so without a password any local process
/// could drive it.
fn generate_password() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Split `providerID/modelID`. Only the first `/` separates the two, since
/// model IDs can contain slashes themselves (`openrouter/anthropic/claude`).
/// A bare model name is rejected on purpose: OpenCode would otherwise fall
/// back to its own default model, which may be a hosted one, and a "Local AI"
/// request must go where the user pointed it.
fn parse_model_ref(model: &str) -> Result<(&str, &str), OpenCodeError> {
    match model.trim().split_once('/') {
        Some((provider_id, model_id)) if !provider_id.is_empty() && !model_id.is_empty() => {
            Ok((provider_id, model_id))
        }
        _ => Err(OpenCodeError::InvalidModel(model.to_string())),
    }
}

#[derive(Deserialize)]
struct DataEnvelope<T> {
    data: T,
}

#[derive(Deserialize)]
struct SessionInfo {
    id: String,
}

#[derive(Deserialize)]
struct GenerateData {
    text: String,
}

#[derive(Deserialize)]
struct ErrorBody {
    message: String,
}

/// Create a session, generate once, delete the session. Split out from
/// `OpenCodeSidecar::generate` so tests can point it at a mock server.
///
/// A fresh session per request keeps requests from sharing context. If the
/// returned future is dropped between create and delete, the session is left
/// behind until the sidecar exits.
async fn generate_via_session(
    client: &http_client::Client,
    base_url: &str,
    password: &str,
    working_dir: &Path,
    model: &str,
    prompt: &str,
    timeout: Duration,
) -> Result<String, OpenCodeError> {
    let (provider_id, model_id) = parse_model_ref(model)?;

    // Deny every permission: OpenCode is an agent with file and shell tools,
    // and this path only ever wants text back.
    let create = client
        .post(format!("{base_url}/api/session"))
        .basic_auth(SERVER_USERNAME, Some(password))
        .timeout(timeout)
        .json(&serde_json::json!({
            "location": { "directory": working_dir.to_string_lossy() },
            "model": { "id": model_id, "providerID": provider_id },
            "permissions": [{ "action": "*", "resource": "*", "effect": "deny" }],
        }));
    let session: DataEnvelope<SessionInfo> = send_checked(create)
        .await?
        .json()
        .await
        .map_err(|err| OpenCodeError::Request(err.to_string()))?;
    let session_id = session.data.id;

    let generate = client
        .post(format!("{base_url}/api/session/{session_id}/generate"))
        .basic_auth(SERVER_USERNAME, Some(password))
        .timeout(timeout)
        .json(&serde_json::json!({ "prompt": prompt }));
    let generated = async {
        let response: DataEnvelope<GenerateData> = send_checked(generate)
            .await?
            .json()
            .await
            .map_err(|err| OpenCodeError::Request(err.to_string()))?;
        Ok::<_, OpenCodeError>(response.data.text)
    }
    .await;

    // Best effort: the sidecar goes away with its pool entry, so a failed
    // delete only leaves one idle session behind.
    let delete = client
        .delete(format!("{base_url}/api/session/{session_id}"))
        .basic_auth(SERVER_USERNAME, Some(password))
        .timeout(timeout);
    if let Err(err) = send_checked(delete).await {
        log::debug!("failed to delete OpenCode session {session_id}: {err}");
    }

    let text = generated?;
    if text.trim().is_empty() {
        return Err(OpenCodeError::EmptyResponse);
    }
    Ok(text)
}

/// Send the request and turn a non-2xx response into an `OpenCodeError`
/// carrying OpenCode's own `message` (e.g. "Authentication required").
async fn send_checked(
    request: http_client::RequestBuilder<'_>,
) -> Result<http_client::Response, OpenCodeError> {
    let response = request
        .send()
        .await
        .map_err(|err| OpenCodeError::Request(err.to_string()))?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<ErrorBody>(&body)
        .map(|parsed| parsed.message)
        .unwrap_or_else(|_| body.chars().take(ERROR_BODY_PREVIEW_CHARS).collect());
    Err(OpenCodeError::Request(format!("{status}: {message}")))
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

    /// Borrow the sidecar for `working_dir`, spawning a new one if absent or
    /// if the pooled one has exited. Errors propagate from the spawn call;
    /// the pool is left unchanged on failure so the next call retries.
    pub async fn get_or_spawn(
        &self,
        command: &str,
        args: &[String],
        working_dir: &Path,
    ) -> Result<OpenCodeSidecar, OpenCodeError> {
        let canonical = canonicalize_for_pool(working_dir);

        if let Some(existing) = self.inner.lock().get(&canonical) {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
            // Exited since it was pooled (crash or external kill): fall
            // through and respawn; the insert below replaces the entry.
        }

        let sidecar = spawn(command, args, &canonical).await?;
        let mut guard = self.inner.lock();
        // Another caller may have raced us to insert; prefer the
        // existing live sidecar so we do not leak a duplicate child.
        if let Some(existing) = guard.get(&canonical) {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
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
        let ann =
            OpenCodeAnnouncement::from_line("{\"address\":\"http://127.0.0.1:43123\"}").unwrap();
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
        let ann =
            OpenCodeAnnouncement::from_line(r#"{"address":"http://127.0.0.1:43123"}"#).unwrap();
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
        // Use a tempdir to avoid symlink issues with CWD; canonicalize
        // should return the same path for an already-canonical directory.
        let tmp = std::env::temp_dir();
        let canonical = canonicalize_for_pool(&tmp);
        // Both should resolve to the same canonical form.
        assert_eq!(canonicalize_for_pool(&tmp), canonical);
    }

    #[tokio::test]
    async fn spawn_rejects_empty_command() {
        let result = spawn("", &[], Path::new(".")).await;
        assert!(matches!(result, Err(OpenCodeError::BinaryNotFound(_))));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pool_replaces_exited_sidecar() {
        // `sh` announces a port and exits straight away, standing in for a
        // sidecar that crashes after becoming ready.
        let args = vec![
            "-c".to_string(),
            "echo 'server listening on http://127.0.0.1:43123'".to_string(),
        ];
        let pool = OpenCodeSidecarPool::new();
        let dir = std::env::temp_dir();

        let first = pool.get_or_spawn("sh", &args, &dir).await.unwrap();
        for _ in 0..100 {
            if !first.is_alive() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(!first.is_alive());

        let second = pool.get_or_spawn("sh", &args, &dir).await.unwrap();
        assert!(!Arc::ptr_eq(&first.inner, &second.inner));
    }

    #[test]
    fn parse_model_ref_splits_on_first_slash() {
        assert_eq!(
            parse_model_ref("ollama/qwen2.5-coder:7b").unwrap(),
            ("ollama", "qwen2.5-coder:7b")
        );
        assert_eq!(
            parse_model_ref(" openrouter/anthropic/claude ").unwrap(),
            ("openrouter", "anthropic/claude")
        );
    }

    #[test]
    fn parse_model_ref_rejects_bare_and_empty_halves() {
        for bad in ["qwen2.5-coder:7b", "", "/model", "provider/", "/"] {
            assert!(
                matches!(parse_model_ref(bad), Err(OpenCodeError::InvalidModel(_))),
                "{bad:?} should be rejected"
            );
        }
    }

    #[test]
    fn generated_passwords_are_long_and_distinct() {
        let first = generate_password();
        assert!(first.len() >= 64);
        assert_ne!(first, generate_password());
    }

    const TEST_PASSWORD: &str = "test-password";
    // base64("opencode:test-password")
    const TEST_BASIC_AUTH: &str = "Basic b3BlbmNvZGU6dGVzdC1wYXNzd29yZA==";
    const JSON: &str = "application/json";

    async fn run_generate(
        server: &mockito::ServerGuard,
        model: &str,
    ) -> Result<String, OpenCodeError> {
        generate_via_session(
            &http_client::Client::new(),
            &server.url(),
            TEST_PASSWORD,
            Path::new("/work/repo"),
            model,
            "list files",
            Duration::from_secs(5),
        )
        .await
    }

    #[tokio::test]
    async fn generate_creates_session_generates_and_deletes() {
        let mut server = mockito::Server::new_async().await;
        let create = server
            .mock("POST", "/api/session")
            .match_header("authorization", TEST_BASIC_AUTH)
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "location": { "directory": "/work/repo" },
                "model": { "id": "qwen2.5-coder:7b", "providerID": "ollama" },
                "permissions": [{ "action": "*", "resource": "*", "effect": "deny" }],
            })))
            .with_header("content-type", JSON)
            .with_body(r#"{"data":{"id":"ses_1"}}"#)
            .create_async()
            .await;
        let generate = server
            .mock("POST", "/api/session/ses_1/generate")
            .match_header("authorization", TEST_BASIC_AUTH)
            .match_body(mockito::Matcher::Json(
                serde_json::json!({ "prompt": "list files" }),
            ))
            .with_header("content-type", JSON)
            .with_body(r#"{"data":{"text":"ls -la"}}"#)
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/api/session/ses_1")
            .match_header("authorization", TEST_BASIC_AUTH)
            .with_status(204)
            .create_async()
            .await;

        let text = run_generate(&server, "ollama/qwen2.5-coder:7b")
            .await
            .unwrap();

        assert_eq!(text, "ls -la");
        create.assert_async().await;
        generate.assert_async().await;
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn generate_failure_surfaces_message_and_still_deletes_session() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/session")
            .with_header("content-type", JSON)
            .with_body(r#"{"data":{"id":"ses_2"}}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/api/session/ses_2/generate")
            .with_status(503)
            .with_header("content-type", JSON)
            .with_body(
                r#"{"_tag":"ServiceUnavailableError","message":"Free usage limit reached","service":"session generation"}"#,
            )
            .create_async()
            .await;
        let delete = server
            .mock("DELETE", "/api/session/ses_2")
            .with_status(204)
            .create_async()
            .await;

        let err = run_generate(&server, "opencode/flint-40b-free")
            .await
            .unwrap_err();

        match err {
            OpenCodeError::Request(message) => {
                assert!(message.contains("503"), "{message}");
                assert!(message.contains("Free usage limit reached"), "{message}");
            }
            other => panic!("unexpected error: {other:?}"),
        }
        delete.assert_async().await;
    }

    #[tokio::test]
    async fn unauthorized_create_reports_opencode_message() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/session")
            .with_status(401)
            .with_header("content-type", JSON)
            .with_body(r#"{"_tag":"UnauthorizedError","message":"Authentication required"}"#)
            .create_async()
            .await;

        let err = run_generate(&server, "ollama/m").await.unwrap_err();

        assert!(
            matches!(&err, OpenCodeError::Request(m) if m.contains("Authentication required")),
            "{err:?}"
        );
    }

    #[tokio::test]
    async fn empty_generated_text_is_an_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/session")
            .with_header("content-type", JSON)
            .with_body(r#"{"data":{"id":"ses_3"}}"#)
            .create_async()
            .await;
        server
            .mock("POST", "/api/session/ses_3/generate")
            .with_header("content-type", JSON)
            .with_body(r#"{"data":{"text":"  \n"}}"#)
            .create_async()
            .await;
        server
            .mock("DELETE", "/api/session/ses_3")
            .with_status(204)
            .create_async()
            .await;

        let err = run_generate(&server, "ollama/m").await.unwrap_err();

        assert!(matches!(err, OpenCodeError::EmptyResponse), "{err:?}");
    }

    #[tokio::test]
    async fn bare_model_is_rejected_before_any_request() {
        // No mocks registered: any request would fail the test with a 501.
        let server = mockito::Server::new_async().await;
        let err = run_generate(&server, "qwen2.5-coder:7b").await.unwrap_err();
        assert!(matches!(err, OpenCodeError::InvalidModel(_)), "{err:?}");
    }
}
