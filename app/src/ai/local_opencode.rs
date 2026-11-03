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
//! Note: today `LocalOpenAIClient` keys the pool on the process's current
//! directory (`set_opencode_working_dir` has no caller yet), so in practice
//! there is one sidecar per Warp process rather than one per repository.
//!
//! Sidecar lifetime: a `Sidecar` keeps the child process alive for as long
//! as the `Sidecar` is referenced. When the last clone is dropped, the child
//! is sent `kill()` (SIGKILL on POSIX, `TerminateProcess` on Windows) and
//! `wait()`ed. `async_process::Child` does not expose `send_signal`, so we
//! cannot send SIGTERM with a grace period; the kill is immediate. A background
//! drain task prevents the child from blocking on a full pipe and owns the
//! kill path so the `Drop` impl stays non-blocking.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures::AsyncReadExt;
use futures::io::{AsyncBufReadExt, BufReader};
use serde::Deserialize;
use thiserror::Error;
/// Default binary name resolved from `$PATH` when the user does not pin one.
pub const DEFAULT_OPENCODE_BINARY: &str = "opencode";

/// Default command-line arguments. `--port 0` lets OpenCode pick a free
/// port and announce it on stdout, which we then parse.
pub const DEFAULT_OPENCODE_ARGS: &[&str] = &["serve", "--port", "0"];

/// Time we wait for the sidecar to print its port after spawn. A cold first
/// start (database migration) has been observed to take ~20s.
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Budget for confirming that the announced port is the sidecar's own. A port
/// that refuses connections is retried every `VERIFY_RETRY_DELAY`, at most
/// `VERIFY_ATTEMPTS` times.
const VERIFY_TIMEOUT: Duration = Duration::from_secs(10);
const VERIFY_RETRY_DELAY: Duration = Duration::from_millis(200);
const VERIFY_ATTEMPTS: u32 = 25;

/// Upper bound on one verification request.
const VERIFY_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);

/// Longest startup line buffered while waiting for the port announcement. The
/// announcement itself is a short line, so a sidecar that writes this much
/// without a newline is not announcing anything.
const MAX_ANNOUNCEMENT_LINE_BYTES: u64 = 64 * 1024;

/// Environment variables OpenCode reads its Basic-auth credentials from. The
/// username is pinned so an `OPENCODE_SERVER_USERNAME` in the user's own
/// environment cannot make our requests fail with 401. OpenCode reads
/// `OPENCODE_PASSWORD` first and only then `OPENCODE_SERVER_PASSWORD`, so the
/// generated password goes into both: an inherited `OPENCODE_PASSWORD` would
/// otherwise win and every request would be rejected.
const SERVER_USERNAME_ENV: &str = "OPENCODE_SERVER_USERNAME";
const SERVER_PASSWORD_ENVS: [&str; 2] = ["OPENCODE_PASSWORD", "OPENCODE_SERVER_PASSWORD"];
const SERVER_USERNAME: &str = "opencode";

/// A password no sidecar was given. `verify_sidecar` sends it before the real one, to find out
/// whether the listener checks the password at all.
const WRONG_PASSWORD_PROBE: &str = "not-the-password-of-this-sidecar";

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
    #[error(
        "port {port} announced by the OpenCode sidecar does not answer as that sidecar: {reason}"
    )]
    PortNotVerified { port: u16, reason: String },
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

/// Hosts an announcement may name. `spawn` always connects to `127.0.0.1`
/// whatever host the sidecar prints, so the host only has to say that the
/// listener is on this machine: a line about some other address is not ours.
///
/// `[::1]` is left out on purpose. A listener that is only on the IPv6 loopback does not
/// answer on `127.0.0.1`, so accepting its announcement would just fail the verification of
/// an address the sidecar never claimed.
const ANNOUNCED_HOSTS: [&str; 4] = ["127.0.0.1", "localhost", "0.0.0.0", "[::]"];

/// Text that precedes the address in OpenCode's own startup line,
/// `server listening on http://127.0.0.1:43123`.
const LISTENING_MARKER: &str = "listening on ";

/// The word that has to come right before [`LISTENING_MARKER`] unless the marker starts the
/// line. OpenCode prints `opencode server listening on <url>`, and a log prefix such as a level
/// or a timestamp may come before that.
const LISTENING_SUBJECT: &str = "server";

/// The port a sidecar announced on stdout.
///
/// Two forms are accepted, and nothing looser: a JSON object with a numeric
/// `port` (or a `url`/`address` string holding a loopback URL), and OpenCode's
/// own `listening on <loopback URL>` line. Startup output is mostly log lines,
/// so a lenient reading (a `port=` scan, the digits after any `:`) takes
/// timestamps and documentation links for ports and sends the prompt there.
/// `spawn` still checks that the port answers as this sidecar before using it.
#[derive(Debug)]
struct OpenCodeAnnouncement {
    port: u16,
}

impl OpenCodeAnnouncement {
    fn from_line(line: &str) -> Result<Self, OpenCodeError> {
        Self::parse(line).ok_or_else(|| OpenCodeError::BadAnnouncement(line.to_string()))
    }

    fn parse(line: &str) -> Option<Self> {
        let line = line.trim();
        let port = if line.starts_with('{') {
            port_from_json(line)?
        } else {
            port_from_listening_line(line)?
        };
        // Port 0 means "pick one for me" to bind(2); an announcement has to
        // name the port that was actually bound.
        (port != 0).then_some(Self { port })
    }
}

fn port_from_json(line: &str) -> Option<u16> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let object = value.as_object()?;
    if let Some(port) = object.get("port") {
        return u16::try_from(port.as_u64()?).ok();
    }
    ["url", "address"]
        .into_iter()
        .find_map(|key| port_from_url(object.get(key)?.as_str()?))
}

/// The port of OpenCode's `server listening on <url>` line. The marker only counts when it starts
/// the line or follows the word `server`, so `not listening on http://127.0.0.1:43123` is some
/// other process's log line and not an announcement.
fn port_from_listening_line(line: &str) -> Option<u16> {
    // ASCII lowercasing keeps byte offsets, so a match position indexes `line`.
    let lowered = line.to_ascii_lowercase();
    lowered
        .match_indices(LISTENING_MARKER)
        .filter(|(marker_at, _)| {
            lowered[..*marker_at]
                .split_whitespace()
                .next_back()
                .is_none_or(|word| word == LISTENING_SUBJECT)
        })
        .find_map(|(marker_at, _)| {
            let url = line[marker_at + LISTENING_MARKER.len()..]
                .split_whitespace()
                .next()?;
            port_from_url(url)
        })
}

/// Port of an `http(s)` URL whose host is one of `ANNOUNCED_HOSTS`.
fn port_from_url(text: &str) -> Option<u16> {
    let url = url::Url::parse(text).ok()?;
    if !matches!(url.scheme(), "http" | "https") || !ANNOUNCED_HOSTS.contains(&url.host_str()?) {
        return None;
    }
    url.port_or_known_default()
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
/// port announcement (see `OpenCodeAnnouncement`) is treated as the bound
/// port, and `spawn` fails unless that port answers as this sidecar.
pub async fn spawn(
    command: &str,
    args: &[String],
    working_dir: &Path,
) -> Result<OpenCodeSidecar, OpenCodeError> {
    if command.trim().is_empty() {
        return Err(OpenCodeError::BinaryNotFound(String::new()));
    }

    let password = generate_password();
    let mut sidecar_command = async_process::Command::new(command);
    sidecar_command
        .args(args)
        .current_dir(working_dir)
        .env(SERVER_USERNAME_ENV, SERVER_USERNAME)
        .stdout(async_process::Stdio::piped())
        .stderr(async_process::Stdio::piped())
        .kill_on_drop(true);
    for name in SERVER_PASSWORD_ENVS {
        sidecar_command.env(name, &password);
    }
    let mut process = sidecar_command.spawn().map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => OpenCodeError::BinaryNotFound(command.to_string()),
        _ => OpenCodeError::Io(err),
    })?;

    let stdout = process
        .stdout
        .take()
        .expect("stdout was piped in the spawn above");
    // Drain stderr from the start: a chatty startup that fills the pipe would
    // otherwise block the child before it can announce readiness.
    if let Some(stderr) = process.stderr.take() {
        discard_in_background(stderr);
    }

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

    // Keep draining stdout after the announcement so the pipe stays open and
    // the sidecar never blocks (or gets EPIPE) writing further stdout.
    discard_in_background(stdout);

    // The announcement is text from a process we do not control. Before any
    // prompt is sent anywhere, check that the port answers to the password
    // only this sidecar was given.
    //
    // The probe goes straight to loopback: through an ambient `HTTP_PROXY` it would either fail a
    // healthy sidecar or hand the proxy the Basic-auth password.
    let verified = tokio::time::timeout(
        VERIFY_TIMEOUT,
        verify_sidecar(
            &http_client::Client::new_external_without_proxy(),
            &base_url,
            &password,
        ),
    )
    .await
    .unwrap_or_else(|_elapsed| Err(format!("no answer within {VERIFY_TIMEOUT:?}")));
    if let Err(reason) = verified {
        let _ = process.kill();
        return Err(OpenCodeError::PortNotVerified { port, reason });
    }

    // Spawn the background task that owns the kill path.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let exited = Arc::new(AtomicBool::new(false));
    tokio::spawn(supervise_sidecar(process, shutdown_rx, Arc::clone(&exited)));

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

/// Check that `base_url` is the sidecar that was just spawned: it must reject
/// unauthenticated requests and a wrong password, accept the Basic-auth password
/// generated for it, and answer with an OpenCode session list. A stray listener on
/// a misread port cannot satisfy all of these. The generated password is sent only
/// after the listener has refused both, so a listener that accepts any credential
/// never sees it. Only a refused connection is retried, since that is what a
/// sidecar that is not yet accepting looks like.
async fn verify_sidecar(
    client: &http_client::Client,
    base_url: &str,
    password: &str,
) -> Result<(), String> {
    let mut last_error = String::new();
    // `limit=1` keeps the reply small for a project with many sessions.
    let endpoint = format!("{base_url}/api/session?limit=1");
    'attempts: for attempt in 0..VERIFY_ATTEMPTS {
        if attempt > 0 {
            tokio::time::sleep(VERIFY_RETRY_DELAY).await;
        }
        for (credentials, what) in [
            (None, "an unauthenticated request"),
            (Some(WRONG_PASSWORD_PROBE), "a wrong password"),
        ] {
            let mut probe = client.get(&endpoint).timeout(VERIFY_REQUEST_TIMEOUT);
            if let Some(wrong_password) = credentials {
                probe = probe.basic_auth(SERVER_USERNAME, Some(wrong_password));
            }
            match probe.send().await {
                Ok(response) if response.status().is_success() => {
                    return Err(format!(
                        "/api/session accepted {what}; the endpoint does not enforce the password"
                    ));
                }
                Ok(response)
                    if response.status() != http_client::StatusCode::UNAUTHORIZED
                        && response.status() != http_client::StatusCode::FORBIDDEN =>
                {
                    return Err(format!(
                        "{} from /api/session for {what}",
                        response.status()
                    ));
                }
                Ok(_) => {}
                Err(err) => {
                    last_error = err.to_string();
                    continue 'attempts;
                }
            }
        }

        let request = client
            .get(&endpoint)
            .basic_auth(SERVER_USERNAME, Some(password))
            .timeout(VERIFY_REQUEST_TIMEOUT);
        match request.send().await {
            Ok(response) if response.status().is_success() => {
                return response
                    .json::<DataEnvelope<Vec<serde::de::IgnoredAny>>>()
                    .await
                    .map(|_| ())
                    .map_err(|err| format!("reply is not an OpenCode session list: {err}"));
            }
            Ok(response) => return Err(format!("{} from /api/session", response.status())),
            Err(err) => last_error = err.to_string(),
        }
    }
    Err(last_error)
}

/// Read `stdout` line by line until one parses as a port announcement.
/// Lines are read as raw bytes (not `String`s) so a non-UTF-8 log line is
/// skipped by `try_parse_buffer` rather than failing the whole startup. A
/// line longer than `MAX_ANNOUNCEMENT_LINE_BYTES` fails the startup instead of
/// growing the buffer until the timeout.
async fn read_announcement(
    stdout: async_process::ChildStdout,
) -> Result<(OpenCodeAnnouncement, async_process::ChildStdout), OpenCodeError> {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::with_capacity(256);
    loop {
        // Only ever parse the current line, so a preceding log line cannot
        // poison the parse and nothing is re-parsed.
        line.clear();
        let read = (&mut reader)
            .take(MAX_ANNOUNCEMENT_LINE_BYTES)
            .read_until(b'\n', &mut line)
            .await;
        match read {
            Ok(0) => break, // EOF before announcement
            Ok(_) if !line.ends_with(b"\n") && line.len() as u64 >= MAX_ANNOUNCEMENT_LINE_BYTES => {
                return Err(OpenCodeError::BadAnnouncement(format!(
                    "startup line longer than {MAX_ANNOUNCEMENT_LINE_BYTES} bytes"
                )));
            }
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

/// Read and discard `reader` until EOF so the child never blocks on a full
/// pipe. Reserved for future log capture (e.g. piping through `tracing`).
fn discard_in_background<R>(mut reader: R)
where
    R: futures::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buffer = [0u8; 4096];
        loop {
            match reader.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_n) => {}
            }
        }
    });
}

/// Long-running task: flags `exited` if the child dies on its own, and on
/// shutdown signal kills it.
async fn supervise_sidecar(
    mut child: async_process::Child,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    exited: Arc<AtomicBool>,
) {
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
/// A fresh session per request keeps requests from sharing context. The
/// session is deleted on every exit, including when the returned future is
/// dropped mid-request (see `SessionCleanup`).
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
    // The directory travels as JSON, which cannot carry arbitrary bytes. A
    // lossy conversion would name a different directory than the one asked for.
    let directory = working_dir.to_str().ok_or_else(|| {
        OpenCodeError::Request(format!(
            "working directory is not valid UTF-8: {}",
            working_dir.display()
        ))
    })?;

    // Deny every permission: OpenCode is an agent with file and shell tools,
    // and this path only ever wants text back.
    let create = client
        .post(format!("{base_url}/api/session"))
        .basic_auth(SERVER_USERNAME, Some(password))
        .timeout(timeout)
        .json(&serde_json::json!({
            "location": { "directory": directory },
            "model": { "id": model_id, "providerID": provider_id },
            "permissions": [{ "action": "*", "resource": "*", "effect": "deny" }],
        }));
    let session: DataEnvelope<SessionInfo> = send_checked(create)
        .await?
        .json()
        .await
        .map_err(|err| OpenCodeError::Request(err.to_string()))?;
    let session_id = session.data.id;
    let mut cleanup = SessionCleanup::new(
        format!("{base_url}/api/session/{session_id}"),
        password,
        timeout,
    );

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

    cleanup.delete(client).await;

    let text = generated?;
    if text.trim().is_empty() {
        return Err(OpenCodeError::EmptyResponse);
    }
    Ok(text)
}

/// Deletes one OpenCode session. A pooled sidecar outlives its requests, so a
/// session that is not deleted stays on it, with the prompt it was given,
/// until the sidecar exits or the pool is cleared.
///
/// Two paths leave a session behind: the request future being dropped after
/// the session was created (a superseded prediction is aborted this way), and
/// the DELETE itself failing. `delete` handles the normal exit; if it did not
/// succeed, `Drop` makes one more attempt in the background.
struct SessionCleanup {
    delete_url: String,
    password: String,
    timeout: Duration,
    /// Captured while the request runs: `Drop` may run on a thread with no
    /// Tokio context, such as the UI thread aborting a prediction.
    runtime: Option<tokio::runtime::Handle>,
    armed: bool,
}

impl SessionCleanup {
    fn new(delete_url: String, password: &str, timeout: Duration) -> Self {
        Self {
            delete_url,
            password: password.to_string(),
            timeout,
            runtime: tokio::runtime::Handle::try_current().ok(),
            armed: true,
        }
    }

    /// Delete the session now. Stays armed on failure so `Drop` retries.
    async fn delete(&mut self, client: &http_client::Client) {
        match delete_session(client, &self.delete_url, &self.password, self.timeout).await {
            Ok(()) => self.armed = false,
            Err(err) => log::warn!(
                "failed to delete OpenCode session {}, retrying once: {err}",
                self.delete_url
            ),
        }
    }
}

impl Drop for SessionCleanup {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(runtime) = self.runtime.take() else {
            log::warn!(
                "no async runtime to delete OpenCode session {}; it stays until the sidecar exits",
                self.delete_url
            );
            return;
        };
        let delete_url = std::mem::take(&mut self.delete_url);
        let password = std::mem::take(&mut self.password);
        let timeout = self.timeout;
        runtime.spawn(async move {
            // `http_client::Client` is not `Clone` and the request's client is
            // borrowed, so the cleanup brings its own. It sends no Warp headers
            // to the sidecar.
            let client = http_client::Client::new_external();
            if let Err(err) = delete_session(&client, &delete_url, &password, timeout).await {
                log::warn!(
                    "failed to delete OpenCode session {delete_url}: {err}; it stays until the sidecar exits"
                );
            }
        });
    }
}

async fn delete_session(
    client: &http_client::Client,
    delete_url: &str,
    password: &str,
    timeout: Duration,
) -> Result<(), OpenCodeError> {
    let request = client
        .delete(delete_url.to_string())
        .basic_auth(SERVER_USERNAME, Some(password))
        .timeout(timeout);
    send_checked(request).await.map(|_| ())
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
    inner: Arc<parking_lot::Mutex<PoolState>>,
}

#[derive(Default)]
struct PoolState {
    sidecars: HashMap<PathBuf, OpenCodeSidecar>,
    /// Bumped by `clear()`. A spawn that began under an older generation was
    /// started with settings that have since changed, so it must not be
    /// pooled for later requests to reuse.
    generation: u64,
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

        let generation = {
            let state = self.inner.lock();
            if let Some(existing) = state.sidecars.get(&canonical) {
                if existing.is_alive() {
                    return Ok(existing.clone());
                }
                // Exited since it was pooled (crash or external kill): fall
                // through and respawn; the insert below replaces the entry.
            }
            state.generation
        };

        let sidecar = spawn(command, args, &canonical).await?;
        let mut state = self.inner.lock();
        if state.generation != generation {
            // `clear()` ran while this sidecar was starting, so it carries
            // the old settings. Serve this request with it, but do not pool
            // it; it exits once the last clone is dropped.
            return Ok(sidecar);
        }
        // Another caller may have raced us to insert; prefer the
        // existing live sidecar so we do not leak a duplicate child.
        if let Some(existing) = state.sidecars.get(&canonical) {
            if existing.is_alive() {
                return Ok(existing.clone());
            }
        }
        state.sidecars.insert(canonical, sidecar.clone());
        Ok(sidecar)
    }

    /// Forget every cached sidecar and invalidate spawns still in flight.
    /// Existing clones continue to live; the next `get_or_spawn` for any key
    /// spawns a fresh sidecar.
    pub fn clear(&self) {
        let mut state = self.inner.lock();
        state.sidecars.clear();
        state.generation += 1;
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
#[path = "local_opencode_tests.rs"]
mod tests;
