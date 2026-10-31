// This crate is test support for Unix hosts: it runs the system `ssh` and `sshd`, and is never built
// for wasm or shipped in the app, so the wasm `Instant` and Windows console-flash concerns behind
// these lints do not apply.
#![allow(clippy::disallowed_types)]

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use ssh_test_server::LocalSshd;

fn ssh(server: &LocalSshd, remote_command: &str) -> Output {
    Command::new("ssh")
        .args(server.ssh_args())
        .arg(remote_command)
        .output()
        .expect("ssh should run")
}

#[test]
fn client_with_the_fixture_key_can_run_a_command() {
    let Some(server) = LocalSshd::start_or_skip("client_with_the_fixture_key_can_run_a_command")
    else {
        return;
    };

    let output = ssh(&server, "echo hello from sshd");

    assert!(
        output.status.success(),
        "ssh failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "hello from sshd\n");
}

#[test]
fn client_with_another_key_is_refused() {
    let Some(server) = LocalSshd::start_or_skip("client_with_another_key_is_refused") else {
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let other_key = dir.path().join("other");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(&other_key)
        .status()
        .unwrap();
    assert!(keygen.success());

    // The fixture's arguments, with the key swapped for one the server has not been given.
    let mut args = server.ssh_args();
    let identity = args.iter().position(|arg| arg == "-i").unwrap() + 1;
    args[identity] = other_key.display().to_string();
    let output = Command::new("ssh")
        .args(args)
        .arg("echo should not run")
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Permission denied"), "{stderr}");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("should not run"));
}

#[test]
fn dropping_the_server_stops_it() {
    let Some(server) = LocalSshd::start_or_skip("dropping_the_server_stops_it") else {
        return;
    };
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, server.port()));
    assert!(TcpStream::connect(address).is_ok());

    drop(server);

    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(address).is_ok() {
        assert!(
            Instant::now() < deadline,
            "the port still accepts connections after the server was dropped"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn servers_do_not_share_a_port_or_a_key() {
    let Some(first) = LocalSshd::start_or_skip("servers_do_not_share_a_port_or_a_key") else {
        return;
    };
    let second = LocalSshd::start().expect("a second server should start");

    assert_ne!(first.port(), second.port());
    assert_ne!(first.identity_file(), second.identity_file());
}
