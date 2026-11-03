//! A throwaway OpenSSH server on loopback, for tests that need a real SSH connection.
//!
//! [`LocalSshd::start`] runs the system `sshd` unprivileged, on a free port, with a host key and a
//! client key generated into a temporary directory. Only public-key login with that client key
//! works, and only as the user running the test: an unprivileged `sshd` cannot switch users, so
//! the remote shell is that user's login shell. Dropping the value stops the server and deletes
//! the keys.
//!
//! Tests should call [`LocalSshd::start_or_skip`], which skips the test, with a message, on a
//! machine where OpenSSH is not installed.

// This crate is test support for Unix hosts: it runs the system `ssh` and `sshd`, and is never built
// for wasm or shipped in the app, so the wasm `Instant` and Windows console-flash concerns behind
// these lints do not apply.
#![allow(clippy::disallowed_types)]

use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// How long to wait for `sshd` to start accepting connections.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

/// How long one `ssh` run may take before it is killed. A server that stops answering during
/// negotiation would otherwise block the test until the whole run is killed.
pub const SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// The same bound for the readiness probe, which only runs `true`.
const SSH_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// `ConnectTimeout` for the `ssh` client, in seconds: it bounds the connection and the protocol
/// banner exchange, which [`SSH_COMMAND_TIMEOUT`] bounds as a whole.
const SSH_CONNECT_TIMEOUT_SECS: u32 = 5;

/// A free port can be taken between choosing it and `sshd` binding it, so startup is attempted
/// this many times.
const START_ATTEMPTS: usize = 5;

/// Directories searched for `sshd` after `PATH`: it lives in an `sbin` directory that an
/// ordinary user's `PATH` often lacks.
const EXTRA_PROGRAM_DIRS: [&str; 3] = ["/usr/sbin", "/usr/local/sbin", "/sbin"];

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// A program the fixture needs is not installed. Tests skip on this.
    #[error("`{0}` is not installed")]
    MissingProgram(&'static str),
    #[error("{0}")]
    Failed(String),
}

impl StartError {
    fn failed(context: &str, err: impl std::fmt::Display) -> Self {
        Self::Failed(format!("{context}: {err}"))
    }
}

/// A running `sshd` bound to `127.0.0.1`.
pub struct LocalSshd {
    child: Child,
    ssh: PathBuf,
    port: u16,
    identity: PathBuf,
    /// The user the client logs in as, the only one the server accepts.
    user: String,
    /// Held so the keys and config live as long as the server.
    _dir: TempDir,
}

impl LocalSshd {
    /// Starts a server, or says why not.
    pub fn start() -> Result<Self, StartError> {
        let sshd = find_program("sshd")?;
        let ssh_keygen = find_program("ssh-keygen")?;
        // Every user of the fixture runs the client too, so a machine without it skips as well.
        let ssh = find_program("ssh")?;
        let user = current_user(&find_program("id")?)?;

        let dir = tempfile::Builder::new()
            .prefix("ssh-test-server-")
            .tempdir()
            .map_err(|err| StartError::failed("creating a temporary directory", err))?;
        let host_key = dir.path().join("host_key");
        let identity = dir.path().join("id_ed25519");
        generate_key(&ssh_keygen, &host_key)?;
        generate_key(&ssh_keygen, &identity)?;
        let public_key = fs::read_to_string(identity.with_extension("pub"))
            .map_err(|err| StartError::failed("reading the client public key", err))?;
        let authorized_keys = dir.path().join("authorized_keys");
        fs::write(&authorized_keys, public_key)
            .map_err(|err| StartError::failed("writing authorized_keys", err))?;

        let config = dir.path().join("sshd_config");
        let log_path = dir.path().join("sshd.log");
        let mut last_error = String::new();
        for _ in 0..START_ATTEMPTS {
            let port = free_port()?;
            fs::write(&config, sshd_config(dir.path(), port))
                .map_err(|err| StartError::failed("writing sshd_config", err))?;
            let log = File::create(&log_path)
                .map_err(|err| StartError::failed("creating the sshd log", err))?;
            let mut child = Command::new(&sshd)
                .args(["-D", "-e", "-f"])
                .arg(&config)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .map_err(|err| StartError::failed("starting sshd", err))?;
            let probe = client_args(port, &user, &identity, ["true"]);
            match wait_until_ready(&mut child, &ssh, &probe, port, STARTUP_TIMEOUT) {
                Ok(()) => {
                    return Ok(Self {
                        child,
                        ssh,
                        port,
                        identity,
                        user,
                        _dir: dir,
                    });
                }
                Err(reason) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let log_text = fs::read_to_string(&log_path).unwrap_or_default();
                    last_error = format!("{reason}\n{log_text}");
                    // Only a port taken in the meantime is worth another attempt.
                    if !log_text.contains("Address already in use") {
                        break;
                    }
                }
            }
        }
        Err(StartError::Failed(last_error))
    }

    /// Starts a server for `test`, or prints why `test` is being skipped and returns `None`. Any
    /// failure other than a missing program panics: a machine that has OpenSSH should be able to
    /// run the fixture.
    pub fn start_or_skip(test: &str) -> Option<Self> {
        match Self::start() {
            Ok(server) => Some(server),
            Err(StartError::MissingProgram(program)) => {
                eprintln!("skipping {test}: {program} is not installed");
                None
            }
            Err(err) => panic!("could not start the local sshd for {test}: {err}"),
        }
    }

    pub fn host(&self) -> &'static str {
        "127.0.0.1"
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// The private key whose public half the server accepts.
    pub fn identity_file(&self) -> &Path {
        &self.identity
    }

    /// Arguments for `ssh` that connect to this server without prompting: the port, the key (and
    /// no other), no host-key checking, and the host. Append the remote command, if any.
    ///
    /// The client reads no configuration file at all (`-F none`, understood since OpenSSH 6.3,
    /// which is older than the ed25519 keys this fixture already needs), so a `User`, `ProxyJump`
    /// or `IdentityFile` in the caller's `~/.ssh/config` or in `/etc/ssh/ssh_config` cannot change
    /// the connection. The login user is named explicitly (`-l`) because the server accepts only
    /// the user running the test.
    pub fn ssh_args(&self) -> Vec<String> {
        client_args(self.port, &self.user, &self.identity, [])
    }

    /// Runs `remote_command` on the server with the system `ssh`, killing the client if it takes
    /// longer than [`SSH_COMMAND_TIMEOUT`].
    pub fn run_ssh(&self, remote_command: &str) -> io::Result<Output> {
        let mut command = Command::new(&self.ssh);
        command.args(self.ssh_args()).arg(remote_command);
        output_within(command, SSH_COMMAND_TIMEOUT)
    }
}

impl Drop for LocalSshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The arguments of the `ssh` client for a server on `port`, followed by `remote_command`.
fn client_args<'a>(
    port: u16,
    user: &str,
    identity: &Path,
    remote_command: impl IntoIterator<Item = &'a str>,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        port.to_string(),
        "-l".to_string(),
        user.to_string(),
        "-i".to_string(),
        identity.display().to_string(),
        "-F".to_string(),
        "none".to_string(),
    ];
    for option in [
        "IdentitiesOnly=yes",
        "IdentityAgent=none",
        "BatchMode=yes",
        "StrictHostKeyChecking=no",
        "UserKnownHostsFile=/dev/null",
        "LogLevel=ERROR",
        &format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"),
    ] {
        args.push("-o".to_string());
        args.push(option.to_string());
    }
    args.push(Ipv4Addr::LOCALHOST.to_string());
    args.extend(remote_command.into_iter().map(str::to_string));
    args
}

/// Waits until the `sshd` that `child` runs serves `port`, or exits, or `timeout` is up.
///
/// A connection that is merely accepted proves nothing: between choosing the port and `sshd`
/// binding it, another process can take it, and that process accepts connections too while `sshd`
/// is about to exit with `Address already in use`. So once the port accepts, `ssh` logs in with
/// this server's own client key (`probe` holds its arguments); only the server that was given
/// that key's public half can let it in.
fn wait_until_ready(
    child: &mut Child,
    ssh: &Path,
    probe: &[String],
    port: u16,
    timeout: Duration,
) -> Result<(), String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + timeout;
    let mut last_probe_failure = String::new();
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("checking on sshd: {err}"))?
        {
            return Err(format!("sshd exited during startup ({status})"));
        }
        if TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).is_ok() {
            let mut command = Command::new(ssh);
            command.args(probe);
            match output_within(command, SSH_PROBE_TIMEOUT) {
                Ok(output) if output.status.success() => return Ok(()),
                Ok(output) => {
                    last_probe_failure = format!(
                        "{}: {}",
                        output.status,
                        String::from_utf8_lossy(&output.stderr).trim()
                    );
                }
                Err(err) => last_probe_failure = err.to_string(),
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "sshd did not accept the fixture's key within {timeout:?} (last probe: {last_probe_failure:?})"
            ));
        }
        std::thread::sleep(STARTUP_POLL_INTERVAL);
    }
}

/// Runs `command` to completion and collects its output like [`Command::output`], but kills it and
/// fails with [`io::ErrorKind::TimedOut`] if it takes longer than `timeout`. Standard input is
/// closed.
pub fn output_within(mut command: Command, timeout: Duration) -> io::Result<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    // Read both pipes while waiting, so a child that writes more than a pipe holds is not stuck.
    let readers = [
        child.stdout.take().map(spawn_reader),
        child.stderr.take().map(spawn_reader),
    ];
    let deadline = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break Ok(status);
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "{} did not finish within {timeout:?}",
                    command.get_program().to_string_lossy()
                ),
            ));
        }
        std::thread::sleep(STARTUP_POLL_INTERVAL);
    };
    // The pipes close when the child is gone, so the readers end.
    let [stdout, stderr] = readers.map(|reader| {
        reader
            .and_then(|reader| reader.join().ok())
            .unwrap_or_default()
    });
    Ok(Output {
        status: status?,
        stdout,
        stderr,
    })
}

fn spawn_reader(mut pipe: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        bytes
    })
}

/// A port nothing is listening on right now.
fn free_port() -> Result<u16, StartError> {
    TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
        .map_err(|err| StartError::failed("finding a free port", err))
}

fn generate_key(ssh_keygen: &Path, path: &Path) -> Result<(), StartError> {
    let output = Command::new(ssh_keygen)
        .args([
            "-q",
            "-t",
            "ed25519",
            "-N",
            "",
            "-C",
            "ssh_test_server",
            "-f",
        ])
        .arg(path)
        .output()
        .map_err(|err| StartError::failed("running ssh-keygen", err))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(StartError::Failed(format!(
            "ssh-keygen failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// `sshd` configuration for an unprivileged server: public-key login only, files in `dir`.
/// `StrictModes` is off because it also checks the permissions of the user's home directory,
/// which says nothing about the security of a key in a private temporary directory.
fn sshd_config(dir: &Path, port: u16) -> String {
    let dir = dir.display();
    format!(
        "Port {port}\n\
         ListenAddress 127.0.0.1\n\
         HostKey {dir}/host_key\n\
         PidFile {dir}/sshd.pid\n\
         AuthorizedKeysFile {dir}/authorized_keys\n\
         PubkeyAuthentication yes\n\
         PasswordAuthentication no\n\
         KbdInteractiveAuthentication no\n\
         UsePAM no\n\
         StrictModes no\n\
         PrintMotd no\n\
         LogLevel ERROR\n"
    )
}

/// The absolute path of `name`, searching `PATH` and then the usual `sbin` directories.
/// `sshd` must be started by absolute path.
fn find_program(name: &'static str) -> Result<PathBuf, StartError> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    find_in(
        name,
        std::env::split_paths(&path).chain(EXTRA_PROGRAM_DIRS.iter().map(PathBuf::from)),
    )
}

/// [`find_program`] over `dirs`. A relative directory (`PATH=bin`, or an empty entry, which means
/// the current directory) still yields an absolute path, made without resolving symlinks: a
/// symlinked tool may behave differently under another name, `id` among them.
fn find_in(
    name: &'static str,
    dirs: impl IntoIterator<Item = PathBuf>,
) -> Result<PathBuf, StartError> {
    let found = dirs
        .into_iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or(StartError::MissingProgram(name))?;
    std::path::absolute(&found).map_err(|err| StartError::failed(&format!("locating {name}"), err))
}

/// The name of the user the test runs as, which is the only one the server accepts.
///
/// `id -un` asks the system user database for it, the same source `ssh` itself uses to pick the
/// user to log in as. `USER` and `LOGNAME` are not used: they are unset in some environments and
/// stale in others.
fn current_user(id: &Path) -> Result<String, StartError> {
    let output = Command::new(id)
        .arg("-un")
        .output()
        .map_err(|err| StartError::failed("running id", err))?;
    if !output.status.success() {
        return Err(StartError::Failed(format!(
            "id -un failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let user = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if user.is_empty() {
        return Err(StartError::Failed(
            "id -un printed no user name".to_string(),
        ));
    }
    Ok(user)
}

#[cfg(test)]
mod tests {
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
}
