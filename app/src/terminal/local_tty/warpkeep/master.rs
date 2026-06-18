//! warpkeep master: owns the inner PTY + shell and serves attach clients over a
//! Unix socket. Detached from Warp's process/PTY (the spawner sets `setsid` and
//! redirects std fds to `/dev/null`), so it survives the GUI dying.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{Context, Result};
use command::blocking::Command;
use nix::pty::openpty;

use super::{set_winsize, READ_CHUNK, RING_CAPACITY, TAG_DATA, TAG_WINCH};

/// Run the master event loop: spawn the shell in an inner PTY and relay between
/// it and (at most one) connected attach client until the shell exits.
pub fn run_master(socket: PathBuf, command: Vec<String>) -> Result<()> {
    let (program, args) = command
        .split_first()
        .context("warpkeep-master: empty command")?;

    // Create the inner PTY the shell will run in.
    let initial_win = libc::winsize {
        ws_row: 24,
        ws_col: 80,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let ends = openpty(Some(&initial_win), None).context("openpty failed")?;
    let master_fd: RawFd = ends.master;
    let slave_fd: RawFd = ends.slave;

    // Spawn the shell with the PTY slave as its controlling terminal.
    let mut cmd = Command::new(program);
    cmd.args(args);
    unsafe {
        cmd.pre_exec(move || {
            libc::dup2(slave_fd, libc::STDIN_FILENO);
            libc::dup2(slave_fd, libc::STDOUT_FILENO);
            libc::dup2(slave_fd, libc::STDERR_FILENO);
            libc::setsid();
            #[allow(clippy::cast_lossless)]
            libc::ioctl(slave_fd, libc::TIOCSCTTY as _, 0);
            if slave_fd > libc::STDERR_FILENO {
                libc::close(slave_fd);
            }
            Ok(())
        });
    }
    let mut child = cmd
        .spawn()
        .context("failed to spawn shell under warpkeep")?;

    // The parent (master) keeps only the PTY master fd. We deliberately keep it
    // (and the client socket) in BLOCKING mode: reads are gated by `poll`, so
    // they never block, while writes block until fully drained. That is what
    // makes the relay lossless — a non-blocking `write_all` of a large burst
    // (e.g. Warp's multi-KB shell-bootstrap init script) would hit `WouldBlock`
    // and silently drop bytes, corrupting the bootstrap.
    unsafe {
        libc::close(slave_fd);
    }

    // Bind the session socket.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).context("warpkeep-master: bind socket")?;
    listener.set_nonblocking(true)?;

    let mut ring: VecDeque<u8> = VecDeque::with_capacity(RING_CAPACITY);
    let mut client: Option<UnixStream> = None;
    let mut client_buf: Vec<u8> = Vec::new();
    let mut buf = [0u8; READ_CHUNK];

    let result = loop {
        let mut fds = [
            libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: listener.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: client.as_ref().map_or(-1, |c| c.as_raw_fd()),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, 1000) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            break Err(err).context("warpkeep-master: poll failed");
        }

        // Inner PTY -> ring buffer + client.
        if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match read_fd(master_fd, &mut buf) {
                Ok(0) => break Ok(()), // shell exited
                Ok(len) => {
                    let data = &buf[..len];
                    push_ring(&mut ring, data);
                    if let Some(stream) = client.as_mut() {
                        if stream.write_all(data).is_err() {
                            client = None;
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break Ok(()),
            }
        }

        // New attach client. Single-attach: a new connection replaces any prior
        // one (the natural "reattach after the previous client died" semantics).
        if fds[1].revents & libc::POLLIN != 0 {
            if let Ok((stream, _)) = listener.accept() {
                match handshake_new_client(stream, master_fd, &ring) {
                    Ok(stream) => {
                        client = Some(stream);
                        client_buf.clear();
                    }
                    Err(_) => { /* drop the half-open client, keep serving */ }
                }
            }
        }

        // Client -> inner PTY (framed packets).
        if client.is_some() && fds[2].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0
        {
            let stream = client.as_mut().unwrap();
            match read_stream(stream, &mut buf) {
                Ok(0) => client = None, // client detached
                Ok(len) => {
                    client_buf.extend_from_slice(&buf[..len]);
                    parse_client_packets(&mut client_buf, master_fd);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => client = None,
            }
        }

        if matches!(child.try_wait(), Ok(Some(_))) {
            break Ok(());
        }
    };

    let _ = std::fs::remove_file(&socket);
    let _ = child.wait();
    result
}

/// Send the replay buffer to a freshly connected client and apply its initial
/// window size. The client first sends a 4-byte `[rows:u16][cols:u16]` header.
fn handshake_new_client(
    mut stream: UnixStream,
    master_fd: RawFd,
    ring: &VecDeque<u8>,
) -> Result<UnixStream> {
    // On BSD/macOS `accept()` inherits the listener's non-blocking flag, so force
    // the accepted stream back to BLOCKING. It stays blocking for the whole event
    // loop (see `run_master`): reads are gated by `poll` so they never block, and
    // writes block until fully drained — which is what keeps the relay lossless.
    stream.set_nonblocking(false)?;
    let mut header = [0u8; 4];
    stream.read_exact(&mut header)?;
    let rows = u16::from_ne_bytes([header[0], header[1]]);
    let cols = u16::from_ne_bytes([header[2], header[3]]);
    set_winsize(master_fd, rows, cols);

    // Replay block: [len:u32][bytes].
    let snapshot: Vec<u8> = ring.iter().copied().collect();
    stream.write_all(&(snapshot.len() as u32).to_ne_bytes())?;
    stream.write_all(&snapshot)?;

    Ok(stream)
}

/// Parse and consume as many complete `[tag][len:u32][payload]` packets as are
/// buffered, applying each to the PTY master. Leaves any partial tail in `buf`.
fn parse_client_packets(buf: &mut Vec<u8>, master_fd: RawFd) {
    let mut offset = 0;
    while buf.len() - offset >= 5 {
        let tag = buf[offset];
        let len = u32::from_ne_bytes([
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
            buf[offset + 4],
        ]) as usize;
        if buf.len() - offset - 5 < len {
            break; // wait for the rest of the payload
        }
        let payload = &buf[offset + 5..offset + 5 + len];
        match tag {
            TAG_DATA => {
                write_fd(master_fd, payload);
            }
            TAG_WINCH if len == 4 => {
                let rows = u16::from_ne_bytes([payload[0], payload[1]]);
                let cols = u16::from_ne_bytes([payload[2], payload[3]]);
                set_winsize(master_fd, rows, cols);
            }
            _ => {}
        }
        offset += 5 + len;
    }
    buf.drain(..offset);
}

/// Append bytes to the bounded ring buffer, evicting the oldest as needed.
fn push_ring(ring: &mut VecDeque<u8>, data: &[u8]) {
    if data.len() >= RING_CAPACITY {
        ring.clear();
        ring.extend(&data[data.len() - RING_CAPACITY..]);
        return;
    }
    let overflow = (ring.len() + data.len()).saturating_sub(RING_CAPACITY);
    if overflow > 0 {
        // `VecDeque::drain` removes the leading `overflow` bytes in one batch (O(overflow) total
        // work, with a memcpy under the hood) instead of `overflow` separate `pop_front` calls
        // (each of which is a per-element shift). The shell is the hot path here, and the savings
        // show up on long-running sessions where the ring fills repeatedly.
        ring.drain(..overflow);
    }
    ring.extend(data);
}

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

fn read_stream(stream: &mut UnixStream, buf: &mut [u8]) -> std::io::Result<usize> {
    stream.read(buf)
}
