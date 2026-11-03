use super::*;

#[test]
fn parses_json_announcement() {
    let ann = OpenCodeAnnouncement::from_line("{\"port\":43123}").unwrap();
    assert_eq!(ann.port, 43123);
}

#[test]
fn parses_announcement_with_address_alias() {
    let ann = OpenCodeAnnouncement::from_line("{\"address\":\"http://127.0.0.1:43123\"}").unwrap();
    assert_eq!(ann.port, 43123);
}

#[test]
fn rejects_zero_port() {
    for line in [
        r#"{"port":0}"#,
        r#"{"url":"http://127.0.0.1:0"}"#,
        "server listening on http://127.0.0.1:0",
    ] {
        assert!(
            OpenCodeAnnouncement::from_line(line).is_err(),
            "{line:?} must not parse as an announcement"
        );
    }
}

fn announced_port(line: &str) -> Option<u16> {
    OpenCodeAnnouncement::from_line(line)
        .ok()
        .map(|ann| ann.port)
}

#[test]
fn only_the_listening_line_of_a_noisy_startup_is_an_announcement() {
    // The lines from the bug report: each of the last four used to be read
    // as a port (33, 33, 5, 443).
    assert_eq!(
        announced_port("opencode server listening on http://127.0.0.1:43123"),
        Some(43123)
    );
    for line in [
        "2026-10-31T11:22:33",
        "INFO  starting at 11:22:33",
        "ETA 00:05",
        "see https://opencode.ai/docs:443 for help",
    ] {
        assert_eq!(announced_port(line), None, "{line:?}");
    }
}

#[test]
fn loose_port_forms_are_not_announcements() {
    for line in [
        "listening on port=54321",
        "port=43123",
        "ready:43123",
        r#"{"msg":"started","detail":"\"port\":43123"}"#,
        r#"{"port":"43123"}"#,
        r#"{"port":70000}"#,
        "{not json, port: 43123}",
    ] {
        assert_eq!(announced_port(line), None, "{line:?}");
    }
}

#[test]
fn listening_marker_only_counts_in_the_announcement_form() {
    // Text before the marker that is not the word `server` makes it some other line.
    for line in [
        "not listening on http://127.0.0.1:43123",
        "proxy listening on http://127.0.0.1:43123",
        "observer listening on http://127.0.0.1:43123",
        "INFO not listening on http://127.0.0.1:43123",
        "server not listening on http://127.0.0.1:43123",
    ] {
        assert_eq!(announced_port(line), None, "{line:?}");
    }
    for line in [
        "listening on http://127.0.0.1:43123",
        "server listening on http://127.0.0.1:43123",
        "Opencode Server Listening On http://127.0.0.1:43123",
        "2026-11-03T04:04:00 INFO server listening on http://127.0.0.1:43123",
    ] {
        assert_eq!(announced_port(line), Some(43123), "{line:?}");
    }
}

#[test]
fn announcement_must_name_this_machine() {
    for line in [
        "server listening on http://example.com:8080",
        "server listening on http://10.0.0.5:4096",
        "server listening on ftp://127.0.0.1:4096",
        "server listening on",
        // `spawn` connects to 127.0.0.1, which a listener on the IPv6 loopback alone
        // does not answer on.
        "server listening on http://[::1]:4096",
        r#"{"url":"http://[::1]:4096"}"#,
        r#"{"url":"http://example.com:8080"}"#,
    ] {
        assert_eq!(announced_port(line), None, "{line:?}");
    }
    for (line, port) in [
        (
            "INFO server listening on http://127.0.0.1:4096/ (pid 7)",
            4096,
        ),
        (r#"{"url":"http://127.0.0.1:43123"}"#, 43123),
    ] {
        assert_eq!(announced_port(line), Some(port), "{line:?}");
    }
}

#[test]
fn parses_url_with_address_field() {
    // The original regex would have matched the IPv4 octet `127`
    // instead of `43123`; the URL-aware parser must skip past
    // the host's `:` and read after it.
    let ann = OpenCodeAnnouncement::from_line(r#"{"address":"http://127.0.0.1:43123"}"#).unwrap();
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
    let server = raw_server(RawOptions::default()).await;
    let args = vec![
        "-c".to_string(),
        format!(
            "echo 'server listening on http://127.0.0.1:{}'",
            server.port
        ),
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

#[test]
fn readiness_rule_agrees_with_request_time_parsing() {
    use crate::settings::LocalProviderKind;
    for model in [
        "ollama/m",
        " ollama/m ",
        "a/b/c",
        "",
        "  ",
        "bare",
        "ollama/",
        "/m",
        "/",
    ] {
        assert_eq!(
            LocalProviderKind::OpenCode.accepts_model(model),
            parse_model_ref(model).is_ok(),
            "{model:?}"
        );
    }
}

const TEST_PASSWORD: &str = "test-password";
// base64("opencode:test-password")
const TEST_BASIC_AUTH: &str = "Basic b3BlbmNvZGU6dGVzdC1wYXNzd29yZA==";
const JSON: &str = "application/json";

async fn run_generate(server: &mockito::ServerGuard, model: &str) -> Result<String, OpenCodeError> {
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

#[test]
fn timestamp_in_a_log_line_is_not_a_port() {
    for line in [
        "2026-04-22T12:34:56Z starting",
        "12:34:56Z",
        "INFO 2026-04-22 12:34:56",
    ] {
        assert!(
            OpenCodeAnnouncement::from_line(line).is_err(),
            "{line:?} must not parse as an announcement"
        );
    }
}

// Shell one-liners standing in for a sidecar: announce `port`, then stay
// alive. `spawn` checks the announced port, so it has to be a live
// `raw_server`'s.
#[cfg(unix)]
fn announce_then_sleep(port: u16, prelude: &str) -> Vec<String> {
    vec![
        "-c".to_string(),
        format!("{prelude} echo 'server listening on http://127.0.0.1:{port}'; sleep 30"),
    ]
}

#[cfg(unix)]
#[tokio::test]
async fn pool_reuses_a_live_sidecar() {
    let server = raw_server(RawOptions::default()).await;
    let args = announce_then_sleep(server.port, "");
    let pool = OpenCodeSidecarPool::new();
    let dir = std::env::temp_dir();

    let first = pool.get_or_spawn("sh", &args, &dir).await.unwrap();
    let second = pool.get_or_spawn("sh", &args, &dir).await.unwrap();

    assert!(Arc::ptr_eq(&first.inner, &second.inner));
}

#[cfg(unix)]
#[tokio::test]
async fn pool_does_not_keep_a_sidecar_spawned_before_clear() {
    let server = raw_server(RawOptions::default()).await;
    let args = announce_then_sleep(server.port, "sleep 0.3;");
    let pool = OpenCodeSidecarPool::new();
    let dir = std::env::temp_dir();

    // `clear()` lands while the sidecar is still starting, as when the
    // settings change mid-spawn.
    let (spawned, ()) = tokio::join!(pool.get_or_spawn("sh", &args, &dir), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        pool.clear();
    });

    // The caller still gets a working sidecar for its in-flight request...
    assert!(spawned.unwrap().is_alive());
    // ...but it is not pooled for later requests to reuse.
    assert!(pool.inner.lock().sidecars.is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn startup_survives_a_flood_of_stderr() {
    // 200 KB on stderr is well over a pipe buffer; if stderr is not
    // drained during startup the child blocks and never announces.
    let server = raw_server(RawOptions::default()).await;
    let args = announce_then_sleep(server.port, "head -c 200000 /dev/zero >&2;");
    let dir = std::env::temp_dir();

    let spawned = tokio::time::timeout(Duration::from_secs(10), spawn("sh", &args, &dir))
        .await
        .expect("startup must not hang on an undrained stderr pipe");

    assert!(spawned.unwrap().is_alive());
}

#[cfg(unix)]
#[tokio::test]
async fn verification_ends_with_the_generated_password() {
    use base64::Engine as _;
    let server = raw_server(RawOptions::default()).await;
    let args = announce_then_sleep(server.port, "");

    let sidecar = spawn("sh", &args, &std::env::temp_dir()).await.unwrap();

    let expected = base64::engine::general_purpose::STANDARD
        .encode(format!("{SERVER_USERNAME}:{}", sidecar.inner.password));
    let seen = server.credentials_seen.lock().unwrap();
    assert_eq!(seen.first(), Some(&wrong_password_credentials()));
    assert_eq!(seen.last(), Some(&expected), "{seen:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_password_is_set_under_both_env_names() {
    // OpenCode reads OPENCODE_PASSWORD before OPENCODE_SERVER_PASSWORD, so
    // setting only the second lets an inherited first one win.
    let out = std::env::temp_dir().join(format!("opencode-env-{}", uuid::Uuid::new_v4()));
    let server = raw_server(RawOptions::default()).await;
    let args = announce_then_sleep(
        server.port,
        &format!(
            "printf '%s\\n%s' \"$OPENCODE_PASSWORD\" \"$OPENCODE_SERVER_PASSWORD\" > '{}';",
            out.display()
        ),
    );

    let sidecar = spawn("sh", &args, &std::env::temp_dir()).await.unwrap();

    let written = std::fs::read_to_string(&out).unwrap();
    let _ = std::fs::remove_file(&out);
    let password = &sidecar.inner.password;
    assert_eq!(written, format!("{password}\n{password}"));
}

#[cfg(unix)]
#[tokio::test]
async fn startup_rejects_a_line_that_never_ends() {
    // Twice the cap with no newline, then the process stays alive. Without
    // a cap the buffer grows until READY_TIMEOUT instead of failing fast.
    let args = vec![
        "-c".to_string(),
        "head -c 140000 /dev/zero | tr '\\0' x; sleep 30".to_string(),
    ];

    let result = tokio::time::timeout(
        Duration::from_secs(10),
        spawn("sh", &args, &std::env::temp_dir()),
    )
    .await
    .expect("an unterminated line must fail startup instead of waiting for the timeout");

    assert!(
        matches!(&result, Err(OpenCodeError::BadAnnouncement(m)) if m.contains("longer than")),
        "{:?}",
        result.err()
    );
}

#[cfg(unix)]
#[tokio::test]
async fn non_utf8_working_dir_is_rejected_before_any_request() {
    use std::os::unix::ffi::OsStrExt;
    // No mocks registered: any request would fail the test with a 501.
    let server = mockito::Server::new_async().await;
    let dir = Path::new(std::ffi::OsStr::from_bytes(b"/work/\xff"));

    let err = generate_via_session(
        &http_client::Client::new(),
        &server.url(),
        TEST_PASSWORD,
        dir,
        "ollama/m",
        "list files",
        Duration::from_secs(5),
    )
    .await
    .unwrap_err();

    assert!(
        matches!(&err, OpenCodeError::Request(m) if m.contains("UTF-8")),
        "{err:?}"
    );
}

/// Minimal HTTP server for what mockito cannot produce: a generate call
/// that never answers, and DELETEs that fail before one succeeds.
struct RawServer {
    url: String,
    port: u16,
    generate_started: Arc<tokio::sync::Notify>,
    deletes: Arc<std::sync::atomic::AtomicUsize>,
    /// The Basic credentials (base64, as sent) of every `GET /api/session`, in order.
    credentials_seen: Arc<std::sync::Mutex<Vec<String>>>,
}

/// How the raw server answers `GET /api/session`, which `spawn` uses to
/// check that an announced port belongs to the sidecar.
#[derive(Clone, Copy, Default)]
enum ProbeReply {
    /// A real sidecar: 401 without Basic auth, an empty session list with it.
    #[default]
    Sidecar,
    /// Some other service that does not know our password.
    Unauthorized,
    /// A listener that ignores authentication and returns a valid session list.
    IgnoresAuth,
    /// A listener that wants some Basic credential but accepts any password.
    AcceptsAnyPassword,
    /// A server that requires authentication but answers with a malformed session list.
    InvalidSessionList,
    /// A web server that answers every path with 200 and an HTML page.
    CatchAll,
}

#[derive(Default)]
struct RawOptions {
    hang_on_generate: bool,
    failing_deletes: usize,
    probe: ProbeReply,
}

/// The base64 `user:password` of a request's `Authorization: Basic` header, if it has one.
fn basic_credentials(request: &str) -> Option<String> {
    request.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if !name.eq_ignore_ascii_case("authorization") {
            return None;
        }
        let value = value.trim();
        let (scheme, credentials) = value.split_once(' ')?;
        scheme
            .eq_ignore_ascii_case("basic")
            .then(|| credentials.to_string())
    })
}

/// `basic_credentials` of the request `verify_sidecar` sends with a wrong password.
fn wrong_password_credentials() -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD
        .encode(format!("{SERVER_USERNAME}:{WRONG_PASSWORD_PROBE}"))
}

async fn raw_server(options: RawOptions) -> RawServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    raw_server_on(listener, options)
}

fn raw_server_on(listener: tokio::net::TcpListener, options: RawOptions) -> RawServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let RawOptions {
        hang_on_generate,
        failing_deletes,
        probe,
    } = options;
    let local_addr = listener.local_addr().unwrap();
    let server = RawServer {
        url: format!("http://{local_addr}"),
        port: local_addr.port(),
        generate_started: Arc::default(),
        deletes: Arc::default(),
        credentials_seen: Arc::default(),
    };
    let started = Arc::clone(&server.generate_started);
    let deletes = Arc::clone(&server.deletes);
    let credentials_seen = Arc::clone(&server.credentials_seen);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let started = Arc::clone(&started);
            let deletes = Arc::clone(&deletes);
            let credentials_seen = Arc::clone(&credentials_seen);
            tokio::spawn(async move {
                // Read the headers, then the body they announce, so closing
                // the socket never resets a request that is still arriving.
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                let (head_len, body_len) = loop {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&chunk[..n]),
                    }
                    if let Some(at) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&request[..at]).to_lowercase();
                        let body_len = head
                            .lines()
                            .find_map(|l| l.strip_prefix("content-length:"))
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        break (at + 4, body_len);
                    }
                };
                while request.len() < head_len + body_len {
                    match socket.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => request.extend_from_slice(&chunk[..n]),
                    }
                }

                let text = String::from_utf8_lossy(&request).into_owned();
                let request_line = text.lines().next().unwrap_or_default();
                let (status, body) = if request_line.starts_with("POST /api/session ") {
                    (200, r#"{"data":{"id":"ses_1"}}"#)
                } else if request_line.contains("/generate") {
                    started.notify_one();
                    if hang_on_generate {
                        std::future::pending::<()>().await;
                    }
                    (200, r#"{"data":{"text":"ls"}}"#)
                } else if request_line.starts_with("GET /api/session") {
                    let credentials = basic_credentials(&text);
                    if let Some(credentials) = &credentials {
                        credentials_seen.lock().unwrap().push(credentials.clone());
                    }
                    // The password `spawn` generated is not known here, but the one it
                    // probes with first is: refuse that one, accept any other.
                    let authorized = credentials
                        .as_ref()
                        .is_some_and(|sent| *sent != wrong_password_credentials());
                    match probe {
                        ProbeReply::Sidecar if authorized => (200, r#"{"data":[]}"#),
                        ProbeReply::Sidecar | ProbeReply::Unauthorized => {
                            (401, r#"{"message":"Authentication required"}"#)
                        }
                        ProbeReply::IgnoresAuth => (200, r#"{"data":[]}"#),
                        ProbeReply::AcceptsAnyPassword if credentials.is_some() => {
                            (200, r#"{"data":[]}"#)
                        }
                        ProbeReply::AcceptsAnyPassword => {
                            (401, r#"{"message":"Authentication required"}"#)
                        }
                        ProbeReply::InvalidSessionList if authorized => {
                            (200, "<!doctype html><html></html>")
                        }
                        ProbeReply::InvalidSessionList => {
                            (401, r#"{"message":"Authentication required"}"#)
                        }
                        ProbeReply::CatchAll => (200, "<!doctype html><html></html>"),
                    }
                } else if request_line.starts_with("DELETE ") {
                    let seen = deletes.fetch_add(1, Ordering::SeqCst);
                    if seen < failing_deletes {
                        (500, r#"{"message":"boom"}"#)
                    } else {
                        (204, "")
                    }
                } else {
                    (404, "")
                };
                let response = format!(
                    "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(response.as_bytes()).await;
            });
        }
    });
    server
}

async fn wait_for_deletes(server: &RawServer, count: usize) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while server.deletes.load(Ordering::SeqCst) < count {
        assert!(
            tokio::time::Instant::now() < deadline,
            "expected {count} DELETE(s), saw {}",
            server.deletes.load(Ordering::SeqCst)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn run_raw_generate(server: &RawServer) -> Result<String, OpenCodeError> {
    let client = http_client::Client::new();
    generate_via_session(
        &client,
        &server.url,
        TEST_PASSWORD,
        Path::new("/work/repo"),
        "ollama/m",
        "list files",
        Duration::from_secs(30),
    )
    .await
}

#[tokio::test]
async fn cancelled_request_still_deletes_its_session() {
    let server = raw_server(RawOptions {
        hang_on_generate: true,
        ..Default::default()
    })
    .await;

    // Drop the request once the generate call is in flight, as aborting a
    // superseded prediction does.
    tokio::select! {
        _ = run_raw_generate(&server) => panic!("generate never answers"),
        _ = server.generate_started.notified() => {}
    }

    wait_for_deletes(&server, 1).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.deletes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn failed_delete_is_retried_once_in_the_background() {
    let server = raw_server(RawOptions {
        failing_deletes: 1,
        ..Default::default()
    })
    .await;

    // The request succeeds even though its inline DELETE got a 500.
    assert_eq!(run_raw_generate(&server).await.unwrap(), "ls");

    wait_for_deletes(&server, 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.deletes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn successful_delete_is_not_repeated() {
    let server = raw_server(RawOptions::default()).await;

    assert_eq!(run_raw_generate(&server).await.unwrap(), "ls");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.deletes.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[tokio::test]
async fn json_port_after_a_timestamped_log_line_is_the_one_used() {
    // A sidecar whose first stdout line is a log line with a timestamp.
    let server = raw_server(RawOptions::default()).await;
    let args = vec![
        "-c".to_string(),
        format!(
            "echo '2026-10-31T11:22:33 starting'; echo '{{\"port\": {}}}'; sleep 30",
            server.port
        ),
    ];

    let sidecar = spawn("sh", &args, &std::env::temp_dir()).await.unwrap();

    assert_eq!(sidecar.inner.base_url, server.url);
}

#[cfg(unix)]
#[tokio::test]
async fn announced_port_that_is_not_our_sidecar_is_refused() {
    for (probe, expected) in [
        (ProbeReply::Unauthorized, "401"),
        (
            ProbeReply::InvalidSessionList,
            "not an OpenCode session list",
        ),
        (ProbeReply::CatchAll, "does not enforce the password"),
    ] {
        let server = raw_server(RawOptions {
            probe,
            ..Default::default()
        })
        .await;
        let args = announce_then_sleep(server.port, "");

        let Err(err) = spawn("sh", &args, &std::env::temp_dir()).await else {
            panic!("a port that does not answer as the sidecar must be refused");
        };

        assert!(
            matches!(
                &err,
                OpenCodeError::PortNotVerified { port, reason }
                    if *port == server.port && reason.contains(expected)
            ),
            "{err:?}"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_that_ignores_authentication_is_refused() {
    let server = raw_server(RawOptions {
        probe: ProbeReply::IgnoresAuth,
        ..Default::default()
    })
    .await;
    let args = announce_then_sleep(server.port, "");

    let Err(err) = spawn("sh", &args, &std::env::temp_dir()).await else {
        panic!("a port that accepts unauthenticated requests must be refused");
    };

    assert!(
        matches!(
            &err,
            OpenCodeError::PortNotVerified { port, reason }
                if *port == server.port && reason.contains("does not enforce the password")
        ),
        "{err:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn listener_that_accepts_any_password_never_sees_the_generated_one() {
    let server = raw_server(RawOptions {
        probe: ProbeReply::AcceptsAnyPassword,
        ..Default::default()
    })
    .await;
    let args = announce_then_sleep(server.port, "");

    let Err(err) = spawn("sh", &args, &std::env::temp_dir()).await else {
        panic!("a listener that accepts any password must be refused");
    };

    assert!(
        matches!(
            &err,
            OpenCodeError::PortNotVerified { port, reason }
                if *port == server.port && reason.contains("accepted a wrong password")
        ),
        "{err:?}"
    );
    // Every credential it was sent was the throwaway one.
    let seen = server.credentials_seen.lock().unwrap();
    assert!(!seen.is_empty());
    assert!(
        seen.iter()
            .all(|sent| *sent == wrong_password_credentials()),
        "the generated password reached the listener: {seen:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn listening_line_of_another_process_before_the_announcement_is_ignored() {
    let server = raw_server(RawOptions::default()).await;
    // A port nothing answers on, named by a line that is not the sidecar's announcement.
    let args = announce_then_sleep(server.port, "echo 'not listening on http://127.0.0.1:1';");

    let sidecar = spawn("sh", &args, &std::env::temp_dir()).await.unwrap();

    assert_eq!(sidecar.inner.base_url, server.url);
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_that_starts_listening_late_is_still_accepted() {
    // Announce a port nothing serves yet, and start serving half a second later. The socket
    // is bound but not listening, so the port stays ours in between and connections to it
    // are refused (which is what the retry in `verify_sidecar` is for) until `listen`.
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    socket.bind("127.0.0.1:0".parse().unwrap()).unwrap();
    let port = socket.local_addr().unwrap().port();
    let args = announce_then_sleep(port, "");

    let dir = std::env::temp_dir();
    let (spawned, ()) = tokio::join!(spawn("sh", &args, &dir), async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let listener = socket.listen(1024).unwrap();
        raw_server_on(listener, RawOptions::default());
    });

    assert!(spawned.unwrap().is_alive());
}
