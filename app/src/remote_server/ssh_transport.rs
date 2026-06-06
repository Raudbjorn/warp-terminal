//! SSH-specific implementation of [`RemoteTransport`].
//!
//! [`SshTransport`] uses an existing SSH ControlMaster socket to check/install
//! the remote server binary and to launch the `remote-server-proxy` process
//! whose stdin/stdout become the protocol channel.
use std::fmt;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use anyhow::Result;
use remote_server::auth::RemoteServerAuthContext;
use remote_server::client::RemoteServerClient;
use remote_server::manager::RemoteServerExitStatus;
use remote_server::protocol::REMOTE_SERVER_PROTOCOL_VERSION;
use remote_server::setup::{
    parse_uname_output, remote_server_daemon_dir, PreinstallCheckResult, RemotePlatform,
};
use remote_server::ssh::{run_ssh_command, ssh_args};
use remote_server::transport::{
    BinaryCheckStatus, BinaryUpdateReason, Connection, Error, InstallOutcome, RemoteTransport,
};
use warp_core::channel::ChannelState;
use warpui::r#async::executor;

#[path = "ssh_transport/installation.rs"]
pub(crate) mod installation;

/// SSH transport: connects via a ControlMaster socket.
///
/// `socket_path` is the local Unix socket created by the ControlMaster
/// process (`ssh -N -o ControlMaster=yes -o ControlPath=<path>`). All SSH
/// commands (binary check, install, proxy launch) are multiplexed through
/// this socket without re-authenticating.
#[derive(Clone)]
pub struct SshTransport {
    socket_path: PathBuf,
    auth_context: Arc<RemoteServerAuthContext>,
    install_options: InstallScriptOptions,
    allow_tagged_server_with_untagged_client: bool,
}

impl fmt::Debug for SshTransport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SshTransport")
            .field("socket_path", &self.socket_path)
            .field("warp_owns_control_master", &self.warp_owns_control_master)
            .finish_non_exhaustive()
    }
}

impl SshTransport {
    pub fn new(
        socket_path: PathBuf,
        auth_context: Arc<RemoteServerAuthContext>,
        warp_owns_control_master: bool,
    ) -> Self {
        Self {
            socket_path,
            auth_context,
            install_options,
            allow_tagged_server_with_untagged_client: false,
        }
    }

    pub fn with_tagged_server_untagged_client_trust(mut self) -> Self {
        self.allow_tagged_server_with_untagged_client = true;
        self
    }

    pub fn socket_path(&self) -> &PathBuf {
        &self.socket_path
    }

    pub fn warp_owns_control_master(&self) -> bool {
        self.warp_owns_control_master
    }

    pub fn remote_daemon_socket_path(&self) -> String {
        format!(
            "{}/{}",
            remote_server_daemon_dir(&self.auth_context.remote_server_identity_key()),
            remote_server::setup::daemon_socket_name(),
        )
    }

    pub fn remote_daemon_pid_path(&self) -> String {
        format!(
            "{}/{}",
            remote_server_daemon_dir(&self.auth_context.remote_server_identity_key()),
            remote_server::setup::daemon_pid_name(),
        )
    }

    fn remote_proxy_command(&self) -> String {
        let binary = remote_server::setup::remote_server_binary();
        let identity_key = self.auth_context.remote_server_identity_key();
        let quoted_identity_key = shell_words::quote(&identity_key);
        format!("{binary} remote-server-proxy --identity-key {quoted_identity_key}")
    }
}

/// Runs `uname -sm` on the remote host via the ControlMaster socket and
/// parses the output into a [`RemotePlatform`].
async fn detect_remote_platform(socket_path: &Path) -> Result<RemotePlatform, Error> {
    let output = run_ssh_command(
        socket_path,
        "uname -sm",
        remote_server::setup::CHECK_TIMEOUT,
    )
    .await?;
    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        parse_uname_output(&stdout)
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Other(anyhow::anyhow!(
            "uname -sm exited with code {code}: {stderr}"
        )))
    }
}

fn parse_remote_server_version(stdout: &str) -> Option<&str> {
    stdout.lines().rev().find_map(|line| {
        line.split_whitespace().find_map(|token| {
            let token = token.trim_matches(|c: char| {
                matches!(c, '"' | '\'' | ',' | ';' | '(' | ')' | '[' | ']')
            });
            token.starts_with('v').then_some(token).filter(|token| {
                token
                    .as_bytes()
                    .get(1)
                    .is_some_and(|byte| byte.is_ascii_digit())
            })
        })
    })
}

fn parse_remote_server_protocol_version(stdout: &str) -> Option<&str> {
    stdout
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| line.starts_with("remote-server-protocol-"))
}

fn classify_binary_check_success(version_stdout: &str) -> BinaryCheckStatus {
    classify_binary_check_success_with_client_version(version_stdout, ChannelState::app_version())
}

fn classify_binary_check_success_with_client_version(
    version_stdout: &str,
    client_version: Option<&str>,
) -> BinaryCheckStatus {
    let installed_version = parse_remote_server_version(version_stdout).map(str::to_string);

    match (client_version, installed_version.as_deref()) {
        (Some(expected), Some(found)) if found == expected => BinaryCheckStatus::Installed,
        (Some(expected), Some(found)) => BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::VersionMismatch {
                expected: expected.to_string(),
                found: found.to_string(),
            },
            installed_version,
        },
        (Some(expected), None) => BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::MissingVersion {
                expected: expected.to_string(),
            },
            installed_version,
        },
        // Local dev loops can have neither side reporting a release tag.
        (None, None) => BinaryCheckStatus::Installed,
        (None, Some(_)) => BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::MissingClientVersion,
            installed_version,
        },
    }
}

fn classify_protocol_check_success(protocol_stdout: &str) -> Result<(), BinaryUpdateReason> {
    match parse_remote_server_protocol_version(protocol_stdout) {
        Some(found) if found == REMOTE_SERVER_PROTOCOL_VERSION => Ok(()),
        found => Err(BinaryUpdateReason::ProtocolMismatch {
            expected: REMOTE_SERVER_PROTOCOL_VERSION.to_string(),
            found: found.map(str::to_string),
        }),
    }
}

async fn check_remote_server_protocol(socket_path: &Path) -> Result<(), Error> {
    let cmd = remote_server::setup::protocol_check_command();
    let output = run_ssh_command(socket_path, &cmd, remote_server::setup::CHECK_TIMEOUT).await?;
    let code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout);
    log::info!("Protocol check result: exit={code:?} stdout={stdout}");
    match code {
        Some(0) => classify_protocol_check_success(&stdout)
            .map_err(|reason| Error::IncompatibleBinary { reason }),
        Some(_) | None => Err(Error::IncompatibleBinary {
            reason: BinaryUpdateReason::ProtocolMismatch {
                expected: REMOTE_SERVER_PROTOCOL_VERSION.to_string(),
                found: parse_remote_server_protocol_version(&stdout).map(str::to_string),
            },
        }),
    }
}

async fn check_installed_binary_status(socket_path: &Path) -> Result<BinaryCheckStatus, Error> {
    let cmd = remote_server::setup::binary_check_command();
    let output = run_ssh_command(socket_path, &cmd, remote_server::setup::CHECK_TIMEOUT).await?;
    let code = output.status.code();
    let stdout = String::from_utf8_lossy(&output.stdout);
    log::info!("Binary check result: exit={code:?} stdout={stdout}");
    match code {
        Some(0) => {
            let status = classify_binary_check_success(&stdout);
            match check_remote_server_protocol(socket_path).await {
                Ok(()) => Ok(status),
                Err(Error::IncompatibleBinary { reason }) => match status {
                    BinaryCheckStatus::UpdateRequired {
                        reason:
                            BinaryUpdateReason::VersionMismatch { .. }
                            | BinaryUpdateReason::MissingVersion { .. },
                        ..
                    } if ChannelState::app_version().is_some() => Ok(status),
                    _ => Err(Error::IncompatibleBinary { reason }),
                },
                Err(err) => Err(err),
            }
        }
        Some(126) | Some(127) => Ok(BinaryCheckStatus::Missing),
        Some(code) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::Other(anyhow::anyhow!(
                "remote-server binary check exited with code {code}: {stderr}"
            )))
        }
        None => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            Err(Error::Other(anyhow::anyhow!(
                "remote-server binary check terminated without exit code: {stderr}"
            )))
        }
    }
}

async fn verify_installed_binary_compatible(socket_path: &Path) -> Result<(), Error> {
    match check_installed_binary_status(socket_path).await? {
        BinaryCheckStatus::Installed => Ok(()),
        BinaryCheckStatus::UpdateRequired {
            reason: BinaryUpdateReason::MissingClientVersion,
            ..
        } => Ok(()),
        BinaryCheckStatus::UpdateRequired { reason, .. } => {
            Err(Error::IncompatibleBinary { reason })
        }
        BinaryCheckStatus::Missing => Err(Error::Other(anyhow::anyhow!(
            "post-install compatibility check unexpectedly reported missing binary"
        ))),
    }
}

fn mark_incompatible_install(outcome: &mut InstallOutcome, error: Error) {
    if outcome.result.is_ok() {
        outcome.result = Err(error);
    }
}

async fn verify_install_outcome(socket_path: &Path, mut outcome: InstallOutcome) -> InstallOutcome {
    if outcome.result.is_ok() {
        match verify_installed_binary_compatible(socket_path).await {
            Ok(()) => {}
            Err(error) => {
                log::warn!("Remote server post-install compatibility check failed: {error}");
                mark_incompatible_install(&mut outcome, error);
            }
        }
    }
    outcome
}

fn known_remote_server_install_dirs() -> [&'static str; 5] {
    [
        "~/.warp/remote-server",
        "~/.warp-preview/remote-server",
        "~/.warp-dev/remote-server",
        "~/.warp-local/remote-server",
        "~/.slipstream/remote-server",
    ]
}

fn old_binary_check_command() -> String {
    format!(
        "for d in {}; do if [ -d \"$d\" ]; then exit 0; fi; done; exit 1",
        known_remote_server_install_dirs().join(" ")
    )
}

fn cleanup_remote_daemons_command(identity_key: &str) -> String {
    let identity_dir = remote_server::setup::remote_server_identity_dir_name(identity_key);
    let daemon_dirs = known_remote_server_install_dirs()
        .map(|dir| format!("{dir}/{identity_dir}"))
        .join(" ");
    format!(
        "for d in {daemon_dirs}; do \
           [ -d \"$d\" ] || continue; \
           for p in \"$d\"/server*.pid; do \
             [ -e \"$p\" ] || continue; \
             pid=$(cat \"$p\" 2>/dev/null || true); \
             case \"$pid\" in ''|*[!0-9]*) continue ;; esac; \
             cmdline=$(ps -p \"$pid\" -o args= 2>/dev/null || true); \
             case \"$cmdline\" in *remote-server-daemon*) kill \"$pid\" 2>/dev/null || true ;; esac; \
           done; \
           rm -f \"$d\"/server*.pid \"$d\"/server*.sock 2>/dev/null || true; \
         done"
    )
}

async fn cleanup_remote_daemons(socket_path: &Path, identity_key: &str) -> Result<(), Error> {
    let cmd = cleanup_remote_daemons_command(identity_key);
    log::info!("Cleaning up stale remote-server daemons before install");
    let output = run_ssh_command(socket_path, &cmd, remote_server::setup::CHECK_TIMEOUT).await?;
    if output.status.success() {
        Ok(())
    } else {
        let code = output.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(Error::Other(anyhow::anyhow!(
            "daemon cleanup exited with code {code}: {stderr}"
        )))
    }
}

impl RemoteTransport for SshTransport {
    fn detect_platform(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<RemotePlatform, Error>> + Send>> {
        let socket_path = self.socket_path.clone();
        Box::pin(async move { detect_remote_platform(&socket_path).await })
    }

    fn run_preinstall_check(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<PreinstallCheckResult, Error>> + Send>> {
        let socket_path = self.socket_path.clone();
        Box::pin(async move {
            match remote_server::ssh::run_ssh_script(
                &socket_path,
                remote_server::setup::PREINSTALL_CHECK_SCRIPT,
                remote_server::setup::CHECK_TIMEOUT,
            )
            .await
            {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    Ok(PreinstallCheckResult::parse(&stdout))
                }
                Ok(output) => {
                    let exit_code = output.status.code().unwrap_or(-1);
                    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                    Err(Error::ScriptFailed { exit_code, stderr })
                }
                Err(e) => Err(e.into()),
            }
        })
    }

    fn check_binary(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<BinaryCheckStatus, Error>> + Send>> {
        let socket_path = self.socket_path.clone();
        Box::pin(async move {
            let cmd = remote_server::setup::binary_check_command();
            log::info!("Running binary check: {cmd}");
            let status = check_installed_binary_status(&socket_path).await?;
            log::info!("Binary check status: {status:?}");
            Ok(status)
        })
    }

    fn check_has_old_binary(&self) -> Pin<Box<dyn Future<Output = anyhow::Result<bool>> + Send>> {
        let socket_path = self.socket_path.clone();
        Box::pin(async move {
            // Treat any known remote-server install directory as evidence of
            // a prior install. Slipstream/OSS can encounter stale binaries
            // from another channel, so this intentionally scans every
            // channel-specific install root instead of only the current one.
            let cmd = old_binary_check_command();
            let output =
                run_ssh_command(&socket_path, &cmd, remote_server::setup::CHECK_TIMEOUT).await?;
            // `test -d` exits 0 when present, 1 when missing.
            // Anything else is treated as a check failure.
            match output.status.code() {
                Some(0) => Ok(true),
                Some(1) => Ok(false),
                Some(code) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(anyhow::anyhow!(
                        "remote-server dir check exited with code {code}: {stderr}"
                    ))
                }
                None => Err(anyhow::anyhow!(
                    "remote-server dir check terminated by signal"
                )),
            }
        })
    }

    fn install_binary(&self) -> Pin<Box<dyn Future<Output = InstallOutcome> + Send>> {
        let socket_path = self.socket_path.clone();
        let install_options = self.install_options.clone();
        let identity_key = self.auth_context.remote_server_identity_key();
        Box::pin(async move {
            if let Err(e) = cleanup_remote_daemons(&socket_path, &identity_key).await {
                log::warn!("Failed to clean up stale remote-server daemons before install: {e}");
            }
            let outcome = installation::install_binary(&socket_path, &install_options).await;
            verify_install_outcome(&socket_path, outcome).await
        })
    }

    fn connect(
        &self,
        executor: Arc<executor::Background>,
    ) -> Pin<Box<dyn Future<Output = Result<Connection>> + Send>> {
        let socket_path = self.socket_path.clone();
        let warp_owns_control_master = self.warp_owns_control_master;
        let remote_proxy_command = self.remote_proxy_command();
        Box::pin(async move {
            let mut args = ssh_args(&socket_path);
            args.push(remote_proxy_command);

            // `kill_on_drop(true)` pairs with ownership of the `Child` being
            // returned in the [`Connection`] below: the
            // [`RemoteServerManager`] holds the `Child` on its per-session
            // state, and dropping that state (on explicit teardown or
            // spontaneous disconnect) sends SIGKILL to this ssh process.
            let mut child = command::r#async::Command::new("ssh")
                .args(&args)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .kill_on_drop(true)
                .spawn()?;

            let stdin = child
                .stdin
                .take()
                .ok_or_else(|| anyhow::anyhow!("Failed to capture child stdin"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| anyhow::anyhow!("Failed to capture child stdout"))?;
            let stderr = child
                .stderr
                .take()
                .ok_or_else(|| anyhow::anyhow!("Failed to capture child stderr"))?;

            let (client, event_rx, failure_rx, host_response_rx, stderr_tail) =
                RemoteServerClient::from_child_streams(stdin, stdout, stderr, &executor);
            Ok(Connection {
                client,
                event_rx,
                failure_rx,
                host_response_rx,
                child,
                // Tag the socket with master ownership. Teardown only runs
                // `ssh -O exit` against Warp-managed masters; a user-owned
                // (external) master must be left running when the Warp
                // session exits.
                control_path: if warp_owns_control_master {
                    ControlPath::WarpManaged(socket_path)
                } else {
                    ControlPath::UserOwned(socket_path)
                },
                stderr_tail,
            })
        })
    }

    fn allow_tagged_server_with_untagged_client(&self) -> bool {
        self.allow_tagged_server_with_untagged_client
    }

    fn remove_remote_server_binary(
        &self,
    ) -> Pin<Box<dyn Future<Output = anyhow::Result<()>> + Send>> {
        let socket_path = self.socket_path.clone();
        Box::pin(async move {
            let cmd = remote_server::setup::remote_server_removal_command();
            log::info!("Removing stale remote server binary: {cmd}");
            let output =
                run_ssh_command(&socket_path, &cmd, remote_server::setup::CHECK_TIMEOUT).await?;
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(anyhow::anyhow!("Failed to remove binary: {stderr}"))
            }
        })
    }

    /// SSH exit code 255 indicates a connection-level error (broken pipe,
    /// connection reset, host unreachable) — the ControlMaster's TCP
    /// connection is dead. A signal kill also suggests the transport was
    /// torn down. In either case, reconnecting through the same
    /// ControlMaster is futile.
    fn is_reconnectable(&self, exit_status: Option<&RemoteServerExitStatus>) -> bool {
        match exit_status {
            Some(s) => s.code != Some(255) && !s.signal_killed,
            // No exit status available — optimistically allow reconnect.
            None => true,
        }
    }
}

#[cfg(test)]
#[path = "ssh_transport_tests.rs"]
mod tests;
