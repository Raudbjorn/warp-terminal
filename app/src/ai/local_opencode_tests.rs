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

// Shell one-liners standing in for a sidecar: announce, then stay alive.
#[cfg(unix)]
fn announce_then_sleep(prelude: &str) -> Vec<String> {
    vec![
        "-c".to_string(),
        format!("{prelude} echo 'server listening on http://127.0.0.1:43123'; sleep 30"),
    ]
}

#[cfg(unix)]
#[tokio::test]
async fn pool_reuses_a_live_sidecar() {
    let args = announce_then_sleep("");
    let pool = OpenCodeSidecarPool::new();
    let dir = std::env::temp_dir();

    let first = pool.get_or_spawn("sh", &args, &dir).await.unwrap();
    let second = pool.get_or_spawn("sh", &args, &dir).await.unwrap();

    assert!(Arc::ptr_eq(&first.inner, &second.inner));
}

#[cfg(unix)]
#[tokio::test]
async fn pool_does_not_keep_a_sidecar_spawned_before_clear() {
    let args = announce_then_sleep("sleep 0.3;");
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
    let args = announce_then_sleep("head -c 200000 /dev/zero >&2;");
    let dir = std::env::temp_dir();

    let spawned = tokio::time::timeout(Duration::from_secs(10), spawn("sh", &args, &dir))
        .await
        .expect("startup must not hang on an undrained stderr pipe");

    assert!(spawned.unwrap().is_alive());
}

#[cfg(unix)]
#[tokio::test]
async fn sidecar_password_is_set_under_both_env_names() {
    // OpenCode reads OPENCODE_PASSWORD before OPENCODE_SERVER_PASSWORD, so
    // setting only the second lets an inherited first one win.
    let out = std::env::temp_dir().join(format!("opencode-env-{}", uuid::Uuid::new_v4()));
    let args = announce_then_sleep(&format!(
        "printf '%s\\n%s' \"$OPENCODE_PASSWORD\" \"$OPENCODE_SERVER_PASSWORD\" > '{}';",
        out.display()
    ));

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
    generate_started: Arc<tokio::sync::Notify>,
    deletes: Arc<std::sync::atomic::AtomicUsize>,
}

async fn raw_server(hang_on_generate: bool, failing_deletes: usize) -> RawServer {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server = RawServer {
        url: format!("http://{}", listener.local_addr().unwrap()),
        generate_started: Arc::default(),
        deletes: Arc::default(),
    };
    let started = Arc::clone(&server.generate_started);
    let deletes = Arc::clone(&server.deletes);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let started = Arc::clone(&started);
            let deletes = Arc::clone(&deletes);
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
    let server = raw_server(true, 0).await;

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
    let server = raw_server(false, 1).await;

    // The request succeeds even though its inline DELETE got a 500.
    assert_eq!(run_raw_generate(&server).await.unwrap(), "ls");

    wait_for_deletes(&server, 2).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(server.deletes.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn successful_delete_is_not_repeated() {
    let server = raw_server(false, 0).await;

    assert_eq!(run_raw_generate(&server).await.unwrap(), "ls");

    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(server.deletes.load(Ordering::SeqCst), 1);
}
