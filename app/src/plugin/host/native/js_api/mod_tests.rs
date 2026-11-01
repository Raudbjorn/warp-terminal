use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Mutex, PoisonError};

use network_policy::ServicesMode;

use super::*;

/// The services mode is process-wide, so tests that set it take this lock.
static SERVICES_MODE_LOCK: Mutex<()> = Mutex::new(());

/// Runs `test` with the services mode set to `mode`, and restores the previous mode afterwards,
/// including when `test` panics.
fn with_services_mode<T>(mode: ServicesMode, test: impl FnOnce() -> T) -> T {
    struct Restore(ServicesMode);
    impl Drop for Restore {
        fn drop(&mut self) {
            network_policy::set_services_mode(self.0);
        }
    }

    let _guard = SERVICES_MODE_LOCK
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    let _restore = Restore(network_policy::services_mode());
    network_policy::set_services_mode(mode);
    test()
}

/// Serves one canned HTTP response per connection on a loopback port, forever. `respond` gets the
/// port it is served on, so a response can point back at its own server.
fn spawn_server(respond: impl Fn(u16) -> Vec<u8> + Send + 'static) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let _ = stream.read(&mut [0; 1024]);
            // The client may stop reading early (oversized body), so a failed write is fine.
            let _ = stream.write_all(&respond(port));
        }
    });
    port
}

fn ok_response(body: &[u8]) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    response.extend_from_slice(body);
    response
}

/// Declares `declared` bytes of body and sends none of them.
fn headers_only_response(declared: u64) -> Vec<u8> {
    format!("HTTP/1.1 200 OK\r\nContent-Length: {declared}\r\nConnection: close\r\n\r\n")
        .into_bytes()
}

/// A body with no declared length: the end of the connection ends it.
fn unsized_response(body: &[u8]) -> Vec<u8> {
    let mut response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec();
    response.extend_from_slice(body);
    response
}

fn redirect_response(location: &str) -> Vec<u8> {
    format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
        .into_bytes()
}

#[test]
fn fetch_reaches_a_loopback_server_in_local_only_mode() {
    let port = spawn_server(|_| ok_response(b"hello"));

    let response = with_services_mode(ServicesMode::LocalOnly, || {
        fetch_url(&format!("http://127.0.0.1:{port}/")).unwrap()
    });

    assert_eq!(response.status, 200);
    assert_eq!(response.body, "hello");
}

#[test]
fn local_only_mode_refuses_a_public_url_before_connecting() {
    // The refusal is the policy's own error. A request that had been attempted would instead fail
    // with a connection error, or take until `FETCH_TIMEOUT` on a machine with no route.
    let err = with_services_mode(ServicesMode::LocalOnly, || {
        fetch_url("https://example.com/").err()
    })
    .expect("a public URL must be refused");

    assert!(err.contains("NetworkPolicyDenied"), "{err}");
    assert!(err.contains("plugin network fetch"), "{err}");
}

#[test]
fn local_only_mode_refuses_a_redirect_to_a_public_host() {
    let port = spawn_server(|_| redirect_response("https://example.com/"));

    let err = with_services_mode(ServicesMode::LocalOnly, || {
        fetch_url(&format!("http://127.0.0.1:{port}/")).err()
    })
    .expect("a redirect to a public host must be refused");

    assert!(err.contains("NetworkPolicyDenied"), "{err}");
    assert!(err.contains("redirect target"), "{err}");
}

#[test]
fn local_only_mode_follows_a_redirect_to_loopback() {
    let target = spawn_server(|_| ok_response(b"target"));
    let port = spawn_server(move |_| redirect_response(&format!("http://127.0.0.1:{target}/")));

    let response = with_services_mode(ServicesMode::LocalOnly, || {
        fetch_url(&format!("http://127.0.0.1:{port}/")).unwrap()
    });

    assert_eq!(response.body, "target");
}

#[test]
fn a_body_over_the_limit_is_refused_and_one_at_the_limit_is_not() {
    let at_limit = vec![b'x'; MAX_FETCH_BODY_BYTES as usize];
    let at_limit_port = spawn_server(move |_| ok_response(&at_limit));
    let over_limit = vec![b'x'; MAX_FETCH_BODY_BYTES as usize + 1];
    let over_limit_port = spawn_server(move |_| ok_response(&over_limit));

    with_services_mode(ServicesMode::LocalOnly, || {
        let ok = fetch_url(&format!("http://127.0.0.1:{at_limit_port}/")).unwrap();
        assert_eq!(ok.body.len() as u64, MAX_FETCH_BODY_BYTES);

        let err = fetch_url(&format!("http://127.0.0.1:{over_limit_port}/"))
            .err()
            .expect("an oversized body must be refused");
        assert!(err.contains("larger than"), "{err}");
    });
}

#[test]
fn a_declared_length_over_the_limit_is_refused_without_reading_the_body() {
    // The server sends no body. Were the body read, the call would fail on the truncated body
    // ("failed to read the response") instead of on the size.
    let port = spawn_server(|_| headers_only_response(MAX_FETCH_BODY_BYTES + 1));

    let err = with_services_mode(ServicesMode::LocalOnly, || {
        fetch_url(&format!("http://127.0.0.1:{port}/")).err()
    })
    .expect("a declared length over the limit must be refused");

    assert!(err.contains("larger than"), "{err}");
}

#[test]
fn an_undeclared_length_is_capped_while_the_body_is_read() {
    let at_limit = vec![b'x'; MAX_FETCH_BODY_BYTES as usize];
    let at_limit_port = spawn_server(move |_| unsized_response(&at_limit));
    let over_limit = vec![b'x'; MAX_FETCH_BODY_BYTES as usize + 1];
    let over_limit_port = spawn_server(move |_| unsized_response(&over_limit));

    with_services_mode(ServicesMode::LocalOnly, || {
        let ok = fetch_url(&format!("http://127.0.0.1:{at_limit_port}/")).unwrap();
        assert_eq!(ok.body.len() as u64, MAX_FETCH_BODY_BYTES);

        let err = fetch_url(&format!("http://127.0.0.1:{over_limit_port}/"))
            .err()
            .expect("an oversized body must be refused");
        assert!(err.contains("larger than"), "{err}");
    });
}
