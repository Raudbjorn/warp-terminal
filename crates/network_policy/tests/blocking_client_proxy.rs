//! In local-only mode a client built with [`apply_to_blocking_client_builder`] must not use the
//! proxy from `HTTP_PROXY`: the loopback check looks at the request URL, so a proxied request would
//! send the destination off the machine. This is its own test binary so that changing the
//! environment cannot affect other tests.
#![cfg(not(target_family = "wasm"))]

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use network_policy::{ServicesMode, apply_to_blocking_client_builder, set_services_mode};

/// Answers every connection with `name` as the body, and returns its port and a hit counter.
fn spawn_server(name: &'static str) -> (u16, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            counter.fetch_add(1, Ordering::SeqCst);
            let _ = stream.read(&mut [0; 1024]);
            let _ = stream.write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{name}",
                    name.len()
                )
                .as_bytes(),
            );
        }
    });
    (port, hits)
}

fn get(client: &reqwest::blocking::Client, port: u16) -> String {
    client
        .get(format!("http://127.0.0.1:{port}/"))
        .send()
        .unwrap()
        .text()
        .unwrap()
}

#[test]
fn local_only_mode_ignores_the_proxy_environment() {
    let (proxy_port, proxy_hits) = spawn_server("proxy");
    let (target_port, target_hits) = spawn_server("target");
    // SAFETY: this is the only test in the binary, so nothing else reads or writes the
    // environment while it runs.
    unsafe {
        for name in ["HTTP_PROXY", "http_proxy"] {
            std::env::set_var(name, format!("http://127.0.0.1:{proxy_port}"));
        }
        for name in ["NO_PROXY", "no_proxy"] {
            std::env::remove_var(name);
        }
    }

    // Control: without the policy, reqwest sends the request to the proxy. If this stops being
    // true, the assertions below prove nothing.
    let proxied = reqwest::blocking::Client::builder().build().unwrap();
    assert_eq!(get(&proxied, target_port), "proxy");
    assert_eq!(proxy_hits.swap(0, Ordering::SeqCst), 1);

    set_services_mode(ServicesMode::LocalOnly);
    let client = apply_to_blocking_client_builder(reqwest::blocking::Client::builder())
        .build()
        .unwrap();
    assert_eq!(get(&client, target_port), "target");
    assert_eq!(proxy_hits.load(Ordering::SeqCst), 0);
    assert_eq!(target_hits.load(Ordering::SeqCst), 1);

    // Online mode keeps the proxy, which is the user's to configure.
    set_services_mode(ServicesMode::Online);
    let client = apply_to_blocking_client_builder(reqwest::blocking::Client::builder())
        .build()
        .unwrap();
    assert_eq!(get(&client, target_port), "proxy");
}
