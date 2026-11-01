//! A ControlMaster socket per SSH manager connection, so Warp can send files over the
//! connection's existing authentication.

use std::path::{Path, PathBuf};

use uuid::Uuid;

use super::profile::shell_quote_arg;

/// Returns a fresh socket path under `~/.ssh/warp-cm`, or `None` where OpenSSH multiplexing is
/// unavailable.
pub fn new_control_path() -> Option<PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    let dir = match private_dir() {
        Ok(dir) => dir,
        Err(err) => {
            log::warn!("Not using an SSH ControlMaster: {err:#}");
            return None;
        }
    };
    let name = Uuid::new_v4().simple().to_string();
    let path = dir.join(&name[..12]);
    if !fits_socket_path_limit(&path) {
        log::warn!("Not using an SSH ControlMaster: the socket path {path:?} is too long");
        return None;
    }
    Some(path)
}

/// `~/.ssh/warp-cm`, created if needed and private to the current user.
pub(super) fn private_dir() -> anyhow::Result<PathBuf> {
    use anyhow::Context as _;
    let dir = dirs::home_dir()
        .context("no home directory")?
        .join(".ssh")
        .join("warp-cm");
    create_private_dir(&dir).with_context(|| format!("failed to create {dir:?}"))?;
    Ok(dir)
}

/// Unix socket paths are limited to 104 bytes on macOS and 108 on Linux, and ssh appends a
/// 17-byte random suffix while it creates the socket.
fn fits_socket_path_limit(path: &Path) -> bool {
    path.as_os_str().len() + 17 < 104
}

#[cfg(unix)]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // Another local user who can reach the directory could hijack the sockets in it.
    let metadata = std::fs::symlink_metadata(dir)?;
    // SAFETY: `getuid` has no preconditions and cannot fail.
    let uid = unsafe { libc::getuid() };
    if !metadata.is_dir() || metadata.uid() != uid {
        return Err(std::io::Error::other(
            "not a directory owned by the current user",
        ));
    }
    if metadata.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// Makes the `ssh` in `command` the ControlMaster at `control_path`.
pub fn with_control_master(command: &str, control_path: &Path) -> String {
    let Some((launcher, args)) = command.split_once("ssh ") else {
        return command.to_string();
    };
    format!(
        "{launcher}ssh -o ControlMaster=auto -o ControlPath={} -o ControlPersist=no {args}",
        shell_quote_arg(&escape_ssh_tokens(control_path))
    )
}

/// ssh expands `%` tokens in ControlPath.
pub(super) fn escape_ssh_tokens(path: &Path) -> String {
    path.to_string_lossy().replace('%', "%%")
}

/// Returns a unique remote path for a pasted image with the given MIME type.
pub fn remote_image_path(mime_type: &str) -> String {
    let extension = match mime_type {
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/tiff" => "tiff",
        "image/svg+xml" => "svg",
        _ => "png",
    };
    format!("/tmp/warp-image-{}.{extension}", Uuid::new_v4().simple())
}

/// Largest clipboard image Warp sends over SSH.
pub const MAX_UPLOAD_BYTES: usize = 20 * 1024 * 1024;
pub(super) const SOCKET_ONLY_PROXY_COMMAND: &str = "ProxyCommand=false";
const UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Collect both pipes concurrently, terminating the child on timeout or either byte limit.
#[cfg(not(target_family = "wasm"))]
pub(super) async fn bounded_output(
    command: &mut command::r#async::Command,
    stdout_limit: usize,
    stderr_limit: usize,
    timeout: std::time::Duration,
) -> anyhow::Result<std::process::Output> {
    use std::process::Stdio;

    use anyhow::Context as _;
    use warpui::r#async::FutureExt as _;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to start SSH helper")?;
    let stdout = child
        .stdout
        .take()
        .context("SSH helper has no stdout pipe")?;
    let stderr = child
        .stderr
        .take()
        .context("SSH helper has no stderr pipe")?;
    let collect = async {
        let (stdout, stderr, status) = futures::try_join!(
            read_limited(stdout, stdout_limit),
            read_limited(stderr, stderr_limit),
            async {
                child
                    .status()
                    .await
                    .context("failed to wait for SSH helper")
            },
        )?;
        Ok::<_, anyhow::Error>(std::process::Output {
            status,
            stdout,
            stderr,
        })
    };
    collect
        .with_timeout(timeout)
        .await
        .context("SSH helper timed out")?
}

#[cfg(not(target_family = "wasm"))]
async fn read_limited(
    stream: impl futures::io::AsyncRead + Unpin,
    limit: usize,
) -> anyhow::Result<Vec<u8>> {
    use futures::io::AsyncReadExt as _;
    let mut bytes = Vec::new();
    stream
        .take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .await?;
    anyhow::ensure!(
        bytes.len() <= limit,
        "SSH helper output exceeds {limit} bytes"
    );
    Ok(bytes)
}

/// Copies `data` to `remote_path` through the ControlMaster at `control_path`, failing instead of
/// opening a new connection if the master is gone.
#[cfg(not(target_family = "wasm"))]
pub async fn upload(
    control_path: PathBuf,
    data: Vec<u8>,
    remote_path: String,
) -> anyhow::Result<()> {
    use std::io::Write as _;

    use anyhow::bail;

    // The temp file is 0600 and `scp -p` keeps that mode, so the copy isn't readable by other
    // users of the remote host.
    let mut local = tempfile::NamedTempFile::new()?;
    local.write_all(&data)?;
    local.flush()?;
    let mut command = command::r#async::Command::new("scp");
    command
        .arg("-p")
        .arg("-q")
        .args([
            "-o",
            &format!("ControlPath={}", escape_ssh_tokens(&control_path)),
        ])
        .args(["-o", "ControlMaster=no"])
        .args(["-o", "BatchMode=yes"])
        .args(["-o", SOCKET_ONLY_PROXY_COMMAND])
        .args(["-o", "ProxyJump=none"])
        .args(["-o", "ConnectTimeout=15"])
        .arg("--")
        .arg(local.path())
        // The failing proxy forbids a fresh connection if the original master has gone away.
        .arg(format!("warp-control-master.invalid:{remote_path}"));
    let output = bounded_output(&mut command, 64 * 1024, 16 * 1024, UPLOAD_TIMEOUT).await?;
    if !output.status.success() {
        bail!(
            "scp exited with {:?}: {}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

#[cfg(target_family = "wasm")]
pub async fn upload(_: PathBuf, _: Vec<u8>, _: String) -> anyhow::Result<()> {
    anyhow::bail!("SSH uploads are not supported on this platform")
}

#[cfg(test)]
#[path = "control_master_tests.rs"]
mod tests;
