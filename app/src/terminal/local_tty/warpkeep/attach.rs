//! warpkeep attach: the client the shell-spawn wrap actually invokes. Picks an
//! orphaned session to recover (or creates a fresh master), then relays bytes
//! verbatim between Warp's PTY (our stdin/stdout) and the master's socket.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use instant::Instant;

use anyhow::{Context, Result, bail};
use nix::fcntl::{FlockArg, flock};

use super::pump::{
    NonblockingGuard, PendingWrites, interest, poll_fds, read_fd, readable, write_all_until,
};
use super::{
    FINAL_FLUSH_TIMEOUT, HANDSHAKE_READ_TIMEOUT, PENDING_WRITE_CAP_BYTES, READ_CHUNK,
    REPLAY_LEN_BYTES, TAG_DATA, TAG_WINCH, encode_winsize, get_winsize, set_nonblocking,
};

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

    // Handshake: send our initial window size, then copy the replay block to
    // stdout. These steps use plain blocking I/O on purpose; they cannot
    // deadlock because we send nothing but the header until it's done:
    // - the header goes into a freshly connected, empty socket buffer;
    // - the master queues the replay (at most RING_CAPACITY bytes) without
    //   ever blocking, so it keeps serving its shell while we read; we only
    //   read it and write to Warp's PTY, whose reader doesn't depend on us.
    // Socket reads are still bounded by HANDSHAKE_READ_TIMEOUT so a wedged
    // master (e.g. one started by an older build) can't hang the tab forever.
    let (rows, cols) = get_winsize(stdin);
    if stream.write_all(&encode_winsize(rows, cols)).is_err() {
        return 1;
    }
    if stream
        .set_read_timeout(Some(HANDSHAKE_READ_TIMEOUT))
        .is_err()
        || drain_replay(stream, stdout).is_err()
        || stream.set_read_timeout(None).is_err()
    {
        return 0;
    }

    // SIGWINCH self-pipe: the handler writes a byte we poll for.
    let winch_fd = install_winch_pipe();

    pump(stream, stdin, stdout, winch_fd)
}

/// Relay core: pump `stdin`→`stream` (framed) and `stream`→`stdout` (raw),
/// forwarding window-size changes signalled on `winch_fd` (or none if `-1`).
/// Returns the process exit status to propagate.
///
/// Never blocks on a write (see `pump` module): each direction has a
/// [`PendingWrites`] buffer flushed on `POLLOUT`, the other direction keeps
/// being read meanwhile, and a full buffer pauses reading from its source.
pub(super) fn pump(stream: &mut UnixStream, stdin: RawFd, stdout: RawFd, winch_fd: RawFd) -> i32 {
    let sock_fd = stream.as_raw_fd();

    // stdin is included because on a tty it shares stdout's open file
    // description anyway; reads are poll-gated and tolerate EAGAIN. The guard
    // restores the original flags so we don't leave Warp's PTY non-blocking.
    // Refuse to relay through a blocking fd rather than risk the deadlock.
    let Ok(_nonblocking) = NonblockingGuard::set(&[stdin, stdout, sock_fd]) else {
        return 1;
    };
    // A 0-byte read on a tty means the terminal hung up (BSD/macOS report
    // it this way rather than EIO); on a pipe it's plain EOF, after which
    // output keeps flowing.
    let stdin_is_tty = unsafe { libc::isatty(stdin) } == 1;

    // Framed packets (data + WINCH, in order) waiting for the master.
    let mut to_master = PendingWrites::new(PENDING_WRITE_CAP_BYTES);
    // Raw shell output waiting for Warp's PTY.
    let mut to_stdout = PendingWrites::new(PENDING_WRITE_CAP_BYTES);
    let mut stdin_open = true;
    let mut buf = [0u8; READ_CHUNK];

    loop {
        let master_backlogged = to_master.is_full();
        let mut fds = [
            interest(stdin, stdin_open && !master_backlogged, false),
            interest(stdout, false, !to_stdout.is_empty()),
            interest(sock_fd, !to_stdout.is_full(), !to_master.is_empty()),
            // A pending resize stays in the self-pipe and is coalesced into a
            // single WINCH once the master catches up.
            interest(winch_fd, !master_backlogged, false),
        ];
        if poll_fds(&mut fds, -1).is_err() {
            return 1;
        }

        // master -> stdout (raw).
        if readable(&fds[2]) {
            match read_fd(sock_fd, &mut buf) {
                Ok(0) => return finish(&mut to_stdout, stdout, 0), // master/shell gone
                Ok(len) => to_stdout.push(&buf[..len]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return finish(&mut to_stdout, stdout, 0),
            }
        }

        // stdin -> master (framed data packet).
        if readable(&fds[0]) {
            match read_fd(stdin, &mut buf) {
                Ok(0) if stdin_is_tty => return 1, // Warp's PTY went away
                Ok(0) => stdin_open = false,
                Ok(len) => to_master.push_frame(TAG_DATA, &buf[..len]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return 1, // Warp's PTY went away (EIO)
            }
        }

        // SIGWINCH -> forward new size, queued in order with the data packets.
        if readable(&fds[3]) {
            // The bytes are only a wake-up signal; the size is read fresh below,
            // so a failed/short drain just means another (harmless) wake-up.
            let mut drain = [0u8; 16];
            let _ = read_fd(winch_fd, &mut drain);
            let (rows, cols) = get_winsize(stdin);
            to_master.push_frame(TAG_WINCH, &encode_winsize(rows, cols));
        }

        // Flush both directions as far as they go without blocking.
        if !to_master.is_empty() && to_master.flush_to(sock_fd).is_err() {
            return finish(&mut to_stdout, stdout, 0); // master hung up
        }
        if !to_stdout.is_empty() && to_stdout.flush_to(stdout).is_err() {
            return 1; // Warp's PTY went away
        }
    }
}

/// The master is gone: give output already received a bounded chance to reach
/// Warp (e.g. the shell's last words before `exit`), then report `status`.
fn finish(to_stdout: &mut PendingWrites, stdout: RawFd, status: i32) -> i32 {
    let _ = to_stdout.flush_until(stdout, Some(Instant::now() + FINAL_FLUSH_TIMEOUT));
    status
}

/// Read the `[len:u32][bytes]` replay block and write the bytes to stdout.
fn drain_replay(stream: &mut UnixStream, stdout: RawFd) -> std::io::Result<()> {
    let mut len_buf = [0u8; REPLAY_LEN_BYTES];
    stream.read_exact(&mut len_buf)?;
    let mut remaining = u32::from_ne_bytes(len_buf) as usize;
    let mut buf = [0u8; READ_CHUNK];
    while remaining > 0 {
        let want = remaining.min(buf.len());
        let got = stream.read(&mut buf[..want])?;
        if got == 0 {
            break;
        }
        // No deadline: waiting on Warp's reader can't deadlock (see `relay`),
        // and giving up would lose the repaint. EAGAIN-safe in case the PTY
        // was handed to us non-blocking.
        write_all_until(stdout, &buf[..got], None)?;
        remaining -= got;
    }
    Ok(())
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

// --- misc ----------------------------------------------------------------

fn nanos_since_epoch() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
#[path = "attach_tests.rs"]
mod tests;
