use super::*;

#[test]
fn program_found_through_a_relative_directory_gets_an_absolute_path() {
    // `cargo test` runs in the package directory, which has a `Cargo.toml`.
    let found = find_in(
        "Cargo.toml",
        [PathBuf::from("."), PathBuf::from("no-such-dir")],
    )
    .expect("Cargo.toml is in the current directory");

    assert!(found.is_absolute(), "{found:?}");
    assert!(found.is_file(), "{found:?}");
}

#[test]
fn missing_program_is_reported_by_name() {
    let err = find_in("no-such-program-anywhere", [PathBuf::from(".")]).unwrap_err();

    assert!(matches!(
        err,
        StartError::MissingProgram("no-such-program-anywhere")
    ));
}

#[test]
fn command_that_outlives_its_deadline_is_killed() {
    let mut command = Command::new("sleep");
    command.arg("30");
    let started = Instant::now();

    let err = output_within(command, Duration::from_millis(200)).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}

#[test]
fn command_that_finishes_returns_all_of_its_output() {
    // More than a pipe buffer holds, on both streams.
    let mut command = Command::new("sh");
    command.args([
        "-c",
        "head -c 300000 /dev/zero; head -c 300000 /dev/zero >&2",
    ]);

    let output = output_within(command, Duration::from_secs(30)).unwrap();

    assert!(output.status.success());
    assert_eq!(output.stdout.len(), 300_000);
    assert_eq!(output.stderr.len(), 300_000);
}

/// Another process answering on the port is not this server: `sshd` has not bound it yet,
/// or never will.
#[test]
fn port_held_by_another_process_is_not_ready() {
    let Ok(ssh) = find_program("ssh") else {
        eprintln!("skipping: ssh is not installed");
        return;
    };
    // Accepts connections and says nothing, like any process that is not `sshd`.
    let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = squatter.local_addr().unwrap().port();
    let (user, identity) = ("nobody", Path::new("/nonexistent/id_ed25519"));
    let probe = client_args(port, user, identity, ["true"]);
    // Stands in for an `sshd` that is still running.
    let mut child = Command::new("sleep").arg("30").spawn().unwrap();

    let result = wait_until_ready(&mut child, &ssh, &probe, port, Duration::from_secs(1));

    let _ = child.kill();
    let _ = child.wait();
    let reason = result.expect_err("a listener that is not sshd must not count as ready");
    assert!(
        reason.contains("did not accept the fixture's key"),
        "{reason}"
    );
}

#[test]
fn background_process_holding_the_pipes_does_not_outlast_the_deadline() {
    // The shell exits at once; `sleep` inherits its pipes and would hold them for 30 seconds.
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 &"]);
    let started = Instant::now();

    let result = output_within(command, Duration::from_millis(200));

    assert!(started.elapsed() < Duration::from_secs(10), "{result:?}");
}

#[test]
fn descendant_of_a_killed_command_does_not_outlast_the_deadline() {
    let mut command = Command::new("sh");
    command.args(["-c", "sleep 30 & wait"]);
    let started = Instant::now();

    let err = output_within(command, Duration::from_millis(200)).unwrap_err();

    assert_eq!(err.kind(), io::ErrorKind::TimedOut, "{err}");
    assert!(started.elapsed() < Duration::from_secs(10));
}
