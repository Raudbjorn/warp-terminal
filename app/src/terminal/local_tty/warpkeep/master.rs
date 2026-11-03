//! warpkeep master: owns the inner PTY + shell and serves attach clients over a
//! Unix socket. Detached from Warp's process/PTY (the spawner sets `setsid` and
//! redirects std fds to `/dev/null`), so it survives the GUI dying.

use instant::Instant;
use std::collections::VecDeque;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use anyhow::{Context, Result};
use command::blocking::Command;
use nix::pty::openpty;

use super::pump::{PendingWrites, interest, poll_fds, read_fd, readable};
use super::{
    FINAL_FLUSH_TIMEOUT, FRAME_HEADER_BYTES, MASTER_POLL_INTERVAL_MS, MAX_PACKET_PAYLOAD_BYTES,
    PENDING_WRITE_CAP_BYTES, READ_CHUNK, RING_CAPACITY, TAG_DATA, TAG_WINCH, WINSIZE_BYTES,
    decode_winsize, set_nonblocking, set_winsize,
};

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

    // The parent (master) keeps only the PTY master fd.
    unsafe {
        libc::close(slave_fd);
    }

    // Bind the session socket.
    let _ = std::fs::remove_file(&socket);
    let listener = UnixListener::bind(&socket).context("warpkeep-master: bind socket")?;
    listener.set_nonblocking(true)?;

    let result = serve(master_fd, &listener, || {
        matches!(child.try_wait(), Ok(Some(_)))
    });

    let _ = std::fs::remove_file(&socket);
    let _ = child.wait();
    result
}

/// The (single) attach client currently connected.
struct Client {
    stream: UnixStream,
    /// Bytes read from the client but not yet parsed: the window-size header
    /// and/or a partial packet. Bounded by one read plus one packet.
    inbox: Vec<u8>,
    /// The window-size header arrived and the replay block has been queued;
    /// from here on live PTY output is queued after it.
    attached: bool,
    /// Bytes waiting to be written to the client: the replay block, then raw
    /// PTY output.
    outbox: PendingWrites,
}

impl Client {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            inbox: Vec::new(),
            attached: false,
            outbox: PendingWrites::new(PENDING_WRITE_CAP_BYTES),
        }
    }

    /// Read what the client sent and act on it. Returns `false` when the
    /// client is gone (EOF, error) or broke the protocol and must be dropped.
    fn receive(
        &mut self,
        master_fd: RawFd,
        ring: &VecDeque<u8>,
        to_pty: &mut PendingWrites,
        buf: &mut [u8],
    ) -> bool {
        match read_fd(self.stream.as_raw_fd(), buf) {
            Ok(0) => false, // client detached
            Ok(len) => {
                self.inbox.extend_from_slice(&buf[..len]);
                self.process_inbox(master_fd, ring, to_pty)
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(_) => false,
        }
    }

    fn process_inbox(
        &mut self,
        master_fd: RawFd,
        ring: &VecDeque<u8>,
        to_pty: &mut PendingWrites,
    ) -> bool {
        if !self.attached {
            let Some((rows, cols)) = self.inbox.get(..WINSIZE_BYTES).and_then(decode_winsize)
            else {
                return true; // wait for the rest of the header
            };
            self.inbox.drain(..WINSIZE_BYTES);
            set_winsize(master_fd, rows, cols);

            // Replay block: [len:u32][bytes]. Queued instead of written inline so
            // a slow or stuck client can never wedge the master (and with it the
            // shell); the snapshot is taken now, and only output read after this
            // point is queued behind it, so nothing is duplicated or reordered.
            let (front, back) = ring.as_slices();
            self.outbox.push(&(ring.len() as u32).to_ne_bytes());
            self.outbox.push(front);
            self.outbox.push(back);
            self.attached = true;
        }

        // Window-size changes are applied immediately rather than queued behind
        // pending input: the shell hasn't necessarily read earlier input yet
        // either way, and resizing promptly is what the user expects.
        parse_client_packets(&mut self.inbox, |packet| match packet {
            ClientPacket::Data(data) => to_pty.push(data),
            ClientPacket::Winch { rows, cols } => set_winsize(master_fd, rows, cols),
        })
        .is_ok()
    }
}

/// Relay core: serve the inner PTY `master_fd` and attach clients arriving on
/// `listener` until the shell's PTY closes or `child_exited` reports true.
///
/// Never blocks on a write (see `pump`): PTY input and client output each go
/// through a [`PendingWrites`] buffer flushed on `POLLOUT`, and a full buffer
/// pauses reading from whatever feeds it.
pub(super) fn serve(
    master_fd: RawFd,
    listener: &UnixListener,
    mut child_exited: impl FnMut() -> bool,
) -> Result<()> {
    set_nonblocking(master_fd);

    let mut ring: VecDeque<u8> = VecDeque::with_capacity(RING_CAPACITY);
    let mut client: Option<Client> = None;
    // Client input waiting for the shell to read it.
    let mut to_pty = PendingWrites::new(PENDING_WRITE_CAP_BYTES);
    let mut buf = [0u8; READ_CHUNK];

    loop {
        // Backpressure: a backlogged client pauses PTY reads (the shell then
        // blocks on output, as it would on a slow terminal); a backlogged PTY
        // pauses client reads. With no client, PTY output only feeds the ring.
        let client_backlogged = client.as_ref().is_some_and(|c| c.outbox.is_full());
        let mut fds = [
            interest(master_fd, !client_backlogged, !to_pty.is_empty()),
            interest(listener.as_raw_fd(), true, false),
            // A client that hasn't sent its header yet is always read: the
            // header (plus at most one read of packets behind it) is tiny, and
            // gating it on a backlogged shell would leave a new tab waiting
            // for its replay until the shell next reads input.
            match &client {
                Some(c) => interest(
                    c.stream.as_raw_fd(),
                    !c.attached || !to_pty.is_full(),
                    !c.outbox.is_empty(),
                ),
                None => interest(-1, false, false),
            },
        ];
        if let Err(err) = poll_fds(&mut fds, MASTER_POLL_INTERVAL_MS) {
            // Still give the client the shell's last output before failing.
            finish(master_fd, &mut ring, client, &mut buf);
            return Err(err).context("warpkeep-master: poll failed");
        }

        // Inner PTY -> ring buffer + client.
        if readable(&fds[0]) {
            match read_fd(master_fd, &mut buf) {
                Ok(0) => break, // shell exited
                Ok(len) => record_output(&mut ring, client.as_mut(), &buf[..len]),
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => break, // EIO: every slave fd closed, shell is gone
            }
        }

        // New attach client. Single-attach: a new connection replaces any prior
        // one (the natural "reattach after the previous client died" semantics).
        // Output still queued for the old client is in the ring and is replayed.
        // `accept()` may or may not inherit the listener's O_NONBLOCK depending
        // on the platform; make it explicit.
        if readable(&fds[1])
            && let Ok((stream, _)) = listener.accept()
            && stream.set_nonblocking(true).is_ok()
        {
            client = Some(Client::new(stream));
        }

        // Client -> inner PTY (framed packets).
        if readable(&fds[2]) {
            let keep = client
                .as_mut()
                .is_some_and(|c| c.receive(master_fd, &ring, &mut to_pty, &mut buf));
            if !keep {
                client = None;
            }
        }

        // Flush both directions as far as they go without blocking.
        if !to_pty.is_empty() && to_pty.flush_to(master_fd).is_err() {
            break; // EIO: the shell side of the PTY is gone
        }
        if let Some(c) = client.as_mut()
            && !c.outbox.is_empty()
            && c.outbox.flush_to(c.stream.as_raw_fd()).is_err()
        {
            client = None; // EPIPE etc.: the client went away
        }

        if child_exited() {
            break;
        }
    }

    finish(master_fd, &mut ring, client, &mut buf);
    Ok(())
}

/// The shell is gone (or exited while a background job holds the PTY): collect
/// output it already wrote, then give the client a bounded chance to receive
/// everything queued for it before the master exits.
fn finish(master_fd: RawFd, ring: &mut VecDeque<u8>, mut client: Option<Client>, buf: &mut [u8]) {
    // Bounded by RING_CAPACITY so a chatty orphaned job can't keep us here.
    let mut drained = 0;
    while drained < RING_CAPACITY {
        match read_fd(master_fd, buf) {
            Ok(len) if len > 0 => {
                record_output(ring, client.as_mut(), &buf[..len]);
                drained += len;
            }
            _ => break, // EOF, EIO or EAGAIN: nothing more to collect
        }
    }
    if let Some(c) = client.as_mut() {
        let deadline = Instant::now() + FINAL_FLUSH_TIMEOUT;
        let _ = c.outbox.flush_until(c.stream.as_raw_fd(), Some(deadline));
    }
}

/// Record PTY output in the replay ring (always, so a later reattach repaints
/// everything) and queue it for the client once its replay is queued.
fn record_output(ring: &mut VecDeque<u8>, client: Option<&mut Client>, data: &[u8]) {
    push_ring(ring, data);
    if let Some(c) = client.filter(|c| c.attached) {
        c.outbox.push(data);
    }
}

/// One decoded client→master packet.
pub(super) enum ClientPacket<'a> {
    Data(&'a [u8]),
    Winch { rows: u16, cols: u16 },
}

/// The client stream violated the framing protocol.
#[derive(Debug)]
pub(super) struct ProtocolError;

/// Parse and consume as many complete `[tag][len:u32][payload]` packets as are
/// buffered, handing each to `on_packet`. Leaves any partial tail in `buf`.
/// A length field above [`MAX_PACKET_PAYLOAD_BYTES`] is a protocol error.
pub(super) fn parse_client_packets(
    buf: &mut Vec<u8>,
    mut on_packet: impl FnMut(ClientPacket<'_>),
) -> std::result::Result<(), ProtocolError> {
    let mut offset = 0;
    let mut result = Ok(());
    while buf.len() - offset >= FRAME_HEADER_BYTES {
        let tag = buf[offset];
        let len = u32::from_ne_bytes([
            buf[offset + 1],
            buf[offset + 2],
            buf[offset + 3],
            buf[offset + 4],
        ]) as usize;
        if len > MAX_PACKET_PAYLOAD_BYTES {
            result = Err(ProtocolError);
            break;
        }
        if buf.len() - offset - FRAME_HEADER_BYTES < len {
            break; // wait for the rest of the payload
        }
        let start = offset + FRAME_HEADER_BYTES;
        let payload = &buf[start..start + len];
        match tag {
            TAG_DATA => on_packet(ClientPacket::Data(payload)),
            TAG_WINCH => {
                if let Some((rows, cols)) = decode_winsize(payload) {
                    on_packet(ClientPacket::Winch { rows, cols });
                }
            }
            _ => {}
        }
        offset = start + len;
    }
    buf.drain(..offset);
    result
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

#[cfg(test)]
#[path = "master_tests.rs"]
mod tests;
