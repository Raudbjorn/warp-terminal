//! warpkeep attach: the client the shell-spawn wrap actually invokes. Picks an
//! orphaned session to recover (or creates a fresh master), then relays bytes
//! verbatim between Warp's PTY (our stdin/stdout) and the master's socket.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use nix::fcntl::{flock, FlockArg};

use super::{get_winsize, set_nonblocking, READ_CHUNK, TAG_DATA, TAG_WINCH};

/// Attach entry point. `dir` holds the session sockets; `command` is the shell
/// (plus its args) to run if a new master must be created.
pub fn run_attach(dir: PathBuf, command: Vec<String>) -> Result<()> {
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;

    let (mut stream, _socket, _lock) = connect_or_spawn(&dir, &command)?;

    // Put Warp's PTY (our controlling tty) into raw mode so bytes pass through
    // untouched; restore on exit no matter what.
    let restore = RawModeGuard::enter(libc::STDIN_FILENO)?;
    let status = relay(&mut stream);
    drop(restore);

    std::process::exit(status);
}

/// A connected session, plus the lock we hold for its lifetime. While a client
/// holds the lock, the session is "in use"; once the client dies the lock frees,
/// marking the (still-alive) master as an orphan another attach can recover.
struct SessionLock {
    _file: std::fs::File,
}

fn connect_or_spawn(dir: &Path, command: &[String]) -> Result<(UnixStream, PathBuf, SessionLock)> {
    // 1. Try to recover an orphaned session: a socket whose lock is free and
    //    whose master still answers.
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            let Some(lock) = try_lock(&path) else {
                continue; // a live client holds it
            };
            match UnixStream::connect(&path) {
                Ok(stream) => return Ok((stream, path, lock)),
                Err(_) => {
                    // Master is gone; clean up the stale socket + lock and move on.
                    let _ = std::fs::remove_file(&path);
                    let _ = std::fs::remove_file(lock_path(&path));
                }
            }
        }
    }

    // 2. Otherwise create a fresh session.
    let socket = dir.join(format!(
        "wk-{}-{}.sock",
        std::process::id(),
        nanos_since_epoch()
    ));
    spawn_master(&socket, command)?;

    // Wait for the master to come up (bounded), then connect + lock.
    for _ in 0..200 {
        if let Ok(stream) = UnixStream::connect(&socket) {
            let lock = try_lock(&socket).context("failed to lock new session")?;
            return Ok((stream, socket, lock));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    bail!("warpkeep: master did not come up at {}", socket.display());
}

/// Spawn the detached master process: re-invoke our own binary as the hidden
/// `warpkeep-master` worker, fully detached from Warp's PTY (`setsid` + std fds
/// to `/dev/null`) so it outlives the GUI.
fn spawn_master(socket: &Path, command: &[String]) -> Result<()> {
    let exe = std::env::current_exe().context("current_exe")?;
    let mut cmd = command::blocking::Command::new(exe);
    cmd.arg("warpkeep-master")
        .arg("--socket")
        .arg(socket)
        .arg("--")
        .args(command);
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            let devnull = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if devnull >= 0 {
                libc::dup2(devnull, libc::STDIN_FILENO);
                libc::dup2(devnull, libc::STDOUT_FILENO);
                libc::dup2(devnull, libc::STDERR_FILENO);
                if devnull > libc::STDERR_FILENO {
                    libc::close(devnull);
                }
            }
            Ok(())
        });
    }
    cmd.spawn().context("spawn warpkeep-master")?;
    Ok(())
}

/// Relay loop: pump stdin→master and master→stdout, forwarding window-size
/// changes. Returns the process exit status to propagate.
fn relay(stream: &mut UnixStream) -> i32 {
    let stdin = libc::STDIN_FILENO;
    let stdout = libc::STDOUT_FILENO;

    // Send our initial window size, then drain the replay block to stdout.
    let (rows, cols) = get_winsize(stdin);
    let mut header = Vec::with_capacity(4);
    header.extend_from_slice(&rows.to_ne_bytes());
    header.extend_from_slice(&cols.to_ne_bytes());
    if stream.write_all(&header).is_err() {
        return 1;
    }
    if drain_replay(stream, stdout).is_err() {
        return 0;
    }

    // SIGWINCH self-pipe: the handler writes a byte we poll for.
    let winch_fd = install_winch_pipe();

    // Keep stdin/stdout/socket BLOCKING: reads are gated by `poll` (so they
    // never block), and writes block until fully drained. A non-blocking
    // `write_all` of a large burst (e.g. Warp's shell-bootstrap init script)
    // would hit `WouldBlock` and silently drop bytes, breaking the bootstrap.
    let sock_fd = stream.as_raw_fd();

    let mut buf = [0u8; READ_CHUNK];
    loop {
        let mut fds = [
            libc::pollfd {
                fd: stdin,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: sock_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: winch_fd,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, -1) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return 1;
        }

        // stdin -> master (framed data packet).
        if fds[0].revents & libc::POLLIN != 0 {
            match read_fd(stdin, &mut buf) {
                Ok(0) => {}
                Ok(len) => {
                    if send_packet(stream, TAG_DATA, &buf[..len]).is_err() {
                        return 0;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return 1,
            }
        }

        // master -> stdout (raw).
        if fds[1].revents & (libc::POLLIN | libc::POLLHUP) != 0 {
            match stream.read(&mut buf) {
                Ok(0) => return 0, // master/shell gone
                Ok(len) => write_fd(stdout, &buf[..len]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return 0,
            }
        }

        // SIGWINCH -> forward new size.
        if fds[2].revents & libc::POLLIN != 0 {
            let mut drain = [0u8; 16];
            let _ = read_fd(winch_fd, &mut drain);
            let (rows, cols) = get_winsize(stdin);
            let mut payload = Vec::with_capacity(4);
            payload.extend_from_slice(&rows.to_ne_bytes());
            payload.extend_from_slice(&cols.to_ne_bytes());
            if send_packet(stream, TAG_WINCH, &payload).is_err() {
                return 0;
            }
        }
    }
}

/// Read the `[len:u32][bytes]` replay block and write the bytes to stdout.
fn drain_replay(stream: &mut UnixStream, stdout: RawFd) -> std::io::Result<()> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let mut remaining = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = [0u8; READ_CHUNK];
    while remaining > 0 {
        let want = remaining.min(buf.len());
        let got = stream.read(&mut buf[..want])?;
        if got == 0 {
            break;
        }
        write_fd(stdout, &buf[..got]);
        remaining -= got;
    }
    Ok(())
}

fn send_packet(stream: &mut UnixStream, tag: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut frame = Vec::with_capacity(5 + payload.len());
    frame.push(tag);
    frame.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    frame.extend_from_slice(payload);
    stream.write_all(&frame)
}

// --- locking -------------------------------------------------------------

fn lock_path(socket: &Path) -> PathBuf {
    socket.with_extension("lock")
}

/// Try to exclusively lock a session's lock file without blocking. `Some` means
/// no live client holds it (we now do, for our lifetime); `None` means in use.
fn try_lock(socket: &Path) -> Option<SessionLock> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(lock_path(socket))
        .ok()?;
    match flock(file.as_raw_fd(), FlockArg::LockExclusiveNonblock) {
        Ok(()) => Some(SessionLock { _file: file }),
        Err(_) => None,
    }
}

// --- raw mode ------------------------------------------------------------

struct RawModeGuard {
    fd: RawFd,
    original: libc::termios,
}

impl RawModeGuard {
    fn enter(fd: RawFd) -> Result<Self> {
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(fd, &mut original) } != 0 {
            bail!("warpkeep: tcgetattr failed");
        }
        let mut raw = original;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(fd, libc::TCSANOW, &raw) } != 0 {
            bail!("warpkeep: tcsetattr failed");
        }
        Ok(Self { fd, original })
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        unsafe {
            libc::tcsetattr(self.fd, libc::TCSANOW, &self.original);
        }
    }
}

// --- SIGWINCH self-pipe --------------------------------------------------

static mut WINCH_WRITE_FD: RawFd = -1;

extern "C" fn handle_winch(_sig: libc::c_int) {
    unsafe {
        if WINCH_WRITE_FD >= 0 {
            let byte = [1u8];
            libc::write(WINCH_WRITE_FD, byte.as_ptr() as *const libc::c_void, 1);
        }
    }
}

/// Install a SIGWINCH handler backed by a self-pipe and return the read end.
fn install_winch_pipe() -> RawFd {
    let mut pipe_fds = [0 as RawFd; 2];
    if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
        return -1;
    }
    let (read_fd, write_fd) = (pipe_fds[0], pipe_fds[1]);
    set_nonblocking(read_fd);
    set_nonblocking(write_fd);
    unsafe {
        WINCH_WRITE_FD = write_fd;
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle_winch as usize;
        action.sa_flags = libc::SA_RESTART;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGWINCH, &action, std::ptr::null_mut());
    }
    read_fd
}

// --- small fd helpers ----------------------------------------------------

fn read_fd(fd: RawFd, buf: &mut [u8]) -> std::io::Result<usize> {
    let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
    if n < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(n as usize)
    }
}

fn write_fd(fd: RawFd, data: &[u8]) {
    let mut written = 0;
    while written < data.len() {
        let n = unsafe {
            libc::write(
                fd,
                data[written..].as_ptr() as *const libc::c_void,
                data.len() - written,
            )
        };
        if n <= 0 {
            break;
        }
        written += n as usize;
    }
}

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
