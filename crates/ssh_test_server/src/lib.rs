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
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;

/// How long to wait for `sshd` to start accepting connections.
const STARTUP_TIMEOUT: Duration = Duration::from_secs(10);
const STARTUP_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_millis(200);

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
    port: u16,
    identity: PathBuf,
    /// Held so the keys and config live as long as the server.
    _dir: TempDir,
}

impl LocalSshd {
    /// Starts a server, or says why not.
    pub fn start() -> Result<Self, StartError> {
        let sshd = find_program("sshd")?;
        let ssh_keygen = find_program("ssh-keygen")?;
        // Every user of the fixture runs the client too, so a machine without it skips as well.
        find_program("ssh")?;

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
            match wait_until_accepting(&mut child, port) {
                Ok(()) => {
                    return Ok(Self {
                        child,
                        port,
                        identity,
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
    /// no other), no host-key checking, and the host. No user is named, so `ssh` uses the current
    /// one, the only one the server accepts. Append the remote command, if any.
    pub fn ssh_args(&self) -> Vec<String> {
        let mut args = vec![
            "-p".to_string(),
            self.port.to_string(),
            "-i".to_string(),
            self.identity.display().to_string(),
        ];
        for option in [
            "IdentitiesOnly=yes",
            "IdentityAgent=none",
            "BatchMode=yes",
            "StrictHostKeyChecking=no",
            "UserKnownHostsFile=/dev/null",
            "LogLevel=ERROR",
        ] {
            args.push("-o".to_string());
            args.push(option.to_string());
        }
        args.push(self.host().to_string());
        args
    }
}

impl Drop for LocalSshd {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Waits until something accepts connections on `port`, or `sshd` exits, or the time is up.
fn wait_until_accepting(child: &mut Child, port: u16) -> Result<(), String> {
    let address = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let deadline = Instant::now() + STARTUP_TIMEOUT;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|err| format!("checking on sshd: {err}"))?
        {
            return Err(format!("sshd exited during startup ({status})"));
        }
        if TcpStream::connect_timeout(&address, CONNECT_TIMEOUT).is_ok() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "sshd was not accepting connections after {STARTUP_TIMEOUT:?}"
            ));
        }
        std::thread::sleep(STARTUP_POLL_INTERVAL);
    }
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
    std::env::split_paths(&path)
        .chain(EXTRA_PROGRAM_DIRS.iter().map(PathBuf::from))
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
        .ok_or(StartError::MissingProgram(name))
}
