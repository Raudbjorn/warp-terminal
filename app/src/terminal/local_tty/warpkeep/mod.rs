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

mod attach;
mod master;

pub use attach::run_attach;
pub use master::run_master;

/// Framed-packet tags for the client→master direction. The master→client
/// direction is the raw PTY byte stream (preceded once by the replay block).
pub(crate) const TAG_DATA: u8 = b'd';
pub(crate) const TAG_WINCH: u8 = b'w';

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
