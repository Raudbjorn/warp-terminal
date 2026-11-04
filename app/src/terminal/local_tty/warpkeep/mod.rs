//! warpkeep — oh-my-warp durable terminal sessions.
//!
//! A transparent, dtach-style session keeper bundled directly into the warp-oss
//! binary (invoked as a hidden `worker` subcommand, like the terminal server).
//! The shell runs inside a long-lived *master* process that owns an inner PTY and
//! survives the Warp GUI dying; an *attach* client relays bytes verbatim between
//! Warp's PTY and the master's socket. Because the relay is byte-transparent
//! (no screen redraw, no escape-sequence interpretation), Warp keeps doing all
//! the rendering — blocks, AI, autosuggestions — unlike a multiplexer such as
//! tmux. On reattach the master replays a ring buffer of recent output so the
//! screen repaints seamlessly.
//!
//! Two worker subcommands:
//! - `warpkeep` (attach): picks an orphaned session to recover or creates a new
//!   one, then relays. This is what the shell-spawn wrap invokes.
//! - `warpkeep-master` (hidden): the detached master process; not invoked
//!   directly by users.

#![cfg(unix)]

use std::os::fd::RawFd;
use std::time::Duration;

mod attach;
mod master;
mod pump;
mod shells;

pub use attach::run_attach;
pub use master::run_master;
pub use shells::shells_by_attach;

/// Framed-packet tags for the client→master direction. The master→client
/// direction is the raw PTY byte stream (preceded once by the replay block).
pub(crate) const TAG_DATA: u8 = b'd';
pub(crate) const TAG_WINCH: u8 = b'w';

/// Bytes in a client→master packet header: `[tag:u8][len:u32]`.
pub(crate) const FRAME_HEADER_BYTES: usize = 5;

/// Bytes in an encoded window size `[rows:u16][cols:u16]`: the attach
/// handshake header and the payload of a `TAG_WINCH` packet.
pub(crate) const WINSIZE_BYTES: usize = 4;

/// Bytes in the replay block's length prefix (`[len:u32]`).
pub(crate) const REPLAY_LEN_BYTES: usize = 4;

/// Attach side: how long a single handshake read (replay length or a chunk
/// of the replay) may wait on the master before the attach gives up, so a
/// wedged master can't hang a new tab forever before relaying starts.
pub(crate) const HANDSHAKE_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn encode_winsize(rows: u16, cols: u16) -> [u8; WINSIZE_BYTES] {
    let mut out = [0u8; WINSIZE_BYTES];
    out[..2].copy_from_slice(&rows.to_ne_bytes());
    out[2..].copy_from_slice(&cols.to_ne_bytes());
    out
}

/// Decode `[rows:u16][cols:u16]`; `None` unless exactly `WINSIZE_BYTES` long.
pub(crate) fn decode_winsize(bytes: &[u8]) -> Option<(u16, u16)> {
    let bytes: &[u8; WINSIZE_BYTES] = bytes.try_into().ok()?;
    Some((
        u16::from_ne_bytes([bytes[0], bytes[1]]),
        u16::from_ne_bytes([bytes[2], bytes[3]]),
    ))
}

/// Largest payload a well-formed client packet carries: one stdin read. A
/// larger length field means a corrupt stream; the master drops the client
/// instead of buffering up to 4 GiB waiting for the rest of the "packet".
pub(crate) const MAX_PACKET_PAYLOAD_BYTES: usize = READ_CHUNK;

/// Backpressure threshold for each relay direction's pending-write buffer.
/// While a destination's buffer holds at least this many bytes, the relay
/// stops reading the source that feeds it (see `pump`), so per-direction
/// memory stays under `PENDING_WRITE_CAP_BYTES + READ_CHUNK + FRAME_HEADER_BYTES`.
/// Four reads' worth absorbs a bootstrap-sized burst without stalling the
/// source on every chunk.
pub(crate) const PENDING_WRITE_CAP_BYTES: usize = 4 * READ_CHUNK;

/// On shutdown (shell exited / master hung up), how long to keep trying to
/// deliver already-read output before exiting anyway.
pub(crate) const FINAL_FLUSH_TIMEOUT: Duration = Duration::from_secs(2);

/// How often the master wakes with nothing to do, to notice the shell having
/// exited while a background job still holds the PTY open.
pub(crate) const MASTER_POLL_INTERVAL_MS: libc::c_int = 1000;

/// Size of the per-session replay ring buffer. Replayed to a client on attach so
/// the screen repaints (this is what fixes dtach's "press ctrl-l after reattach"
/// gap). 256 KiB comfortably covers a screenful of even very wide output.
pub(crate) const RING_CAPACITY: usize = 256 * 1024;

/// Read chunk size for PTY/socket I/O.
pub(crate) const READ_CHUNK: usize = 65536;

/// Set `O_NONBLOCK` on a raw fd. Best-effort; logs nothing (used on hot paths).
pub(crate) fn set_nonblocking(fd: RawFd) {
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL, 0);
        if flags >= 0 {
            libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        }
    }
}

/// Apply a window size to a PTY master fd.
pub(crate) fn set_winsize(fd: RawFd, rows: u16, cols: u16) {
    let ws = libc::winsize {
        ws_row: rows,
        ws_col: cols,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    unsafe {
        libc::ioctl(fd, libc::TIOCSWINSZ, &ws as *const _);
    }
}

/// Read the current window size of a terminal fd (e.g. our stdin = Warp's PTY).
pub(crate) fn get_winsize(fd: RawFd) -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    unsafe {
        libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws as *mut _);
    }
    (ws.ws_row, ws.ws_col)
}
