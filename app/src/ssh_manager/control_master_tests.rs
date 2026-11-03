use std::path::Path;

use super::*;
use crate::terminal::ssh::util::parse_interactive_ssh_command;

#[test]
fn with_control_master_inserts_options_after_ssh() {
    let path = Path::new("/home/me/.ssh/warp-cm/abc");
    assert_eq!(
        with_control_master("command ssh -p 2222 -- root@host", path),
        "command ssh -o ControlMaster=auto -o ControlPath=/home/me/.ssh/warp-cm/abc -o ControlPersist=no -p 2222 -- root@host"
    );
    assert_eq!(
        with_control_master("ssh host", path),
        "ssh -o ControlMaster=auto -o ControlPath=/home/me/.ssh/warp-cm/abc -o ControlPersist=no host"
    );
}

#[test]
fn ssh_parser_still_finds_the_target_with_control_master_options() {
    let command = with_control_master(
        "command ssh -p 2222 -- root@10.0.0.5",
        Path::new("/home/me/.ssh/warp-cm/abc"),
    );
    let parsed = parse_interactive_ssh_command(&command).expect("ssh command");
    assert_eq!(parsed.host.as_deref(), Some("root@10.0.0.5"));
}

#[test]
fn remote_image_path_uses_mime_extension() {
    assert!(remote_image_path("image/png").ends_with(".png"));
    assert!(remote_image_path("image/jpeg").ends_with(".jpg"));
    // The bytes are uploaded unchanged, so the suffix must describe their actual encoding.
    assert!(remote_image_path("image/tiff").ends_with(".tiff"));
    assert!(remote_image_path("image/svg+xml").ends_with(".svg"));
    assert!(remote_image_path("image/png").starts_with("/tmp/warp-image-"));
}

#[test]
fn long_socket_paths_are_refused() {
    assert!(fits_socket_path_limit(Path::new(
        "/home/me/.ssh/warp-cm/0123456789ab"
    )));
    let long = format!("/tmp/{}/.ssh/warp-cm/0123456789ab", "x".repeat(80));
    assert!(!fits_socket_path_limit(Path::new(&long)));
}

#[cfg(not(target_family = "wasm"))]
#[test]
fn output_reader_accepts_the_limit_and_rejects_the_next_byte() {
    futures::executor::block_on(async {
        assert_eq!(
            read_limited(futures::io::Cursor::new(b"1234"), 4)
                .await
                .unwrap(),
            b"1234"
        );
        assert!(
            read_limited(futures::io::Cursor::new(b"12345"), 4)
                .await
                .is_err()
        );
    });
}

#[cfg(unix)]
#[test]
fn output_limits_stop_a_running_child_on_either_pipe() {
    for redirect in ["", " >&2"] {
        let started = std::time::Instant::now();
        let pid_file = tempfile::NamedTempFile::new().unwrap();
        let mut command = command::r#async::Command::new("sh");
        command
            .args([
                "-c",
                &format!("echo $$ > \"$1\"; printf '%0100d' 0{redirect}; exec sleep 30"),
                "sh",
            ])
            .arg(pid_file.path());
        let error = futures::executor::block_on(bounded_output(
            &mut command,
            32,
            32,
            std::time::Duration::from_secs(3),
        ))
        .unwrap_err();
        // Overflow must fail while the child is still alive, rather than buffering until exit.
        assert!(error.to_string().contains("exceeds 32 bytes"), "{error:#}");
        assert!(started.elapsed() < std::time::Duration::from_secs(3));
        assert_child_exited(pid_file.path());
    }
}

#[cfg(unix)]
#[test]
fn output_collection_times_out_a_silent_child() {
    let pid_file = tempfile::NamedTempFile::new().unwrap();
    let mut command = command::r#async::Command::new("sh");
    command
        .args(["-c", "echo $$ > \"$1\"; exec sleep 30", "sh"])
        .arg(pid_file.path());
    let error = futures::executor::block_on(bounded_output(
        &mut command,
        32,
        32,
        std::time::Duration::from_millis(50),
    ))
    .unwrap_err();
    assert!(error.to_string().contains("timed out"), "{error:#}");
    assert_child_exited(pid_file.path());
}

#[cfg(unix)]
#[test]
fn missing_master_never_connects_to_a_configured_fallback_host() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let config = dir.path().join("config");
    std::fs::write(
        &config,
        format!(
            "Host warp-control-master.invalid\n HostName 127.0.0.1\n Port {}\n",
            listener.local_addr().unwrap().port(),
        ),
    )
    .unwrap();
    let mut command = command::r#async::Command::new("ssh");
    command
        .arg("-F")
        .arg(&config)
        .args([
            "-o",
            &format!("ControlPath={}", dir.path().join("missing").display()),
        ])
        .args([
            "-o",
            "ControlMaster=no",
            "-o",
            "BatchMode=yes",
            "-o",
            SOCKET_ONLY_PROXY_COMMAND,
            "-o",
            "ProxyJump=none",
            "--",
            "warp-control-master.invalid",
            "true",
        ]);
    let output = futures::executor::block_on(bounded_output(
        &mut command,
        1024,
        4096,
        std::time::Duration::from_secs(3),
    ))
    .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[cfg(unix)]
fn assert_child_exited(pid_file: &Path) {
    let pid: libc::pid_t = std::fs::read_to_string(pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    for _ in 0..100 {
        // SAFETY: signal zero only queries whether this child still exists.
        if unsafe { libc::kill(pid, 0) } == -1 {
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    panic!("SSH helper child {pid} survived cancellation");
}
