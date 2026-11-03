//! Non-blocking relay plumbing shared by the warpkeep master and attach loops.
//!
//! Both relays are single-threaded `poll` loops moving bytes in two directions
//! between peers that *echo* (a shell echoes what it reads). If such a loop
//! ever blocks in `write()` towards one peer, it stops draining that peer's
//! output; the peer then blocks writing its echo, stops reading its input, and
//! the `write()` never completes — a deadlock. That is exactly what happened
//! when Warp typed its ~100 KB bootstrap script through warpkeep.
//!
//! So every destination fd is non-blocking and fed from a [`PendingWrites`]
//! buffer: bytes are queued (never dropped), flushed as far as the fd accepts,
//! and the rest waits for `POLLOUT` while the loop keeps reading the other
//! direction. Memory stays bounded by backpressure: once a buffer reaches its
//! cap, the loop stops polling the *source* that feeds it, so the stall
//! propagates upstream through the kernel buffers instead of into our heap.

use instant::Instant;
use std::collections::VecDeque;
use std::io;
use std::os::fd::RawFd;

use super::FRAME_HEADER_BYTES;

/// An ordered queue of bytes waiting to be written to one destination fd.
///
/// The cap is a backpressure threshold, not a hard limit: [`push`](Self::push)
/// always accepts (dropping bytes would corrupt the stream), and the owner
/// stops reading the feeding source while [`is_full`](Self::is_full). Since a
/// single read is at most `READ_CHUNK`, the queue never exceeds
/// `cap + READ_CHUNK + FRAME_HEADER_BYTES`.
pub(crate) struct PendingWrites {
    queue: VecDeque<u8>,
    cap_bytes: usize,
}

impl PendingWrites {
    pub(crate) fn new(cap_bytes: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            cap_bytes,
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.queue.len()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    /// At or over the cap: stop reading the source that feeds this buffer.
    pub(crate) fn is_full(&self) -> bool {
        self.queue.len() >= self.cap_bytes
    }

    pub(crate) fn push(&mut self, data: &[u8]) {
        self.queue.extend(data);
    }

    /// Queue one `[tag][len:u32][payload]` packet. The whole frame is appended
    /// contiguously, so frames can never interleave on the wire even when the
    /// socket accepts them in arbitrary partial writes.
    pub(crate) fn push_frame(&mut self, tag: u8, payload: &[u8]) {
        let mut header = [0u8; FRAME_HEADER_BYTES];
        header[0] = tag;
        header[1..].copy_from_slice(&(payload.len() as u32).to_ne_bytes());
        self.queue.extend(header);
        self.queue.extend(payload);
    }

    /// Write as much as `fd` accepts right now without blocking. Returns the
    /// number of bytes written; `Ok` with bytes still queued means the fd is
    /// full (`EAGAIN`). Any other write error is returned as-is.
    pub(crate) fn flush_to(&mut self, fd: RawFd) -> io::Result<usize> {
        let mut total = 0;
        while !self.queue.is_empty() {
            let (front, _) = self.queue.as_slices();
            match write_once(fd, front)? {
                Some(written) => {
                    self.queue.drain(..written);
                    total += written;
                }
                None => break,
            }
        }
        Ok(total)
    }

    /// Flush everything, waiting for `POLLOUT` as needed, until `deadline`
    /// (`None` = no deadline). Used on shutdown paths where nothing else needs
    /// servicing, so waiting cannot deadlock the relay.
    pub(crate) fn flush_until(&mut self, fd: RawFd, deadline: Option<Instant>) -> io::Result<()> {
        loop {
            self.flush_to(fd)?;
            if self.queue.is_empty() {
                return Ok(());
            }
            wait_writable(fd, deadline)?;
        }
    }
}

/// One `write()`: `Some(n)` wrote `n > 0` bytes, `None` means it would block.
/// Retries `EINTR`; a zero-length write of non-empty data is an error rather
/// than a silent spin.
fn write_once(fd: RawFd, data: &[u8]) -> io::Result<Option<usize>> {
    loop {
        let n = unsafe { libc::write(fd, data.as_ptr() as *const libc::c_void, data.len()) };
        if n > 0 {
            return Ok(Some(n as usize));
        }
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return Ok(None),
            _ => return Err(err),
        }
    }
}

/// Block until `fd` is writable (or reports an error/hangup, which the next
/// write will surface), or fail with `TimedOut` once `deadline` passes.
fn wait_writable(fd: RawFd, deadline: Option<Instant>) -> io::Result<()> {
    loop {
        let timeout_ms = match deadline {
            None => -1,
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(io::ErrorKind::TimedOut.into());
                }
                // Round up so a sub-millisecond remainder doesn't busy-poll.
                remaining
                    .as_millis()
                    .saturating_add(1)
                    .min(libc::c_int::MAX as u128) as libc::c_int
            }
        };
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if n > 0 {
            return Ok(());
        }
        if n < 0 {
            let err = io::Error::last_os_error();
            if err.kind() != io::ErrorKind::Interrupted {
                return Err(err);
            }
        }
    }
}

/// Write all of `data`, waiting for `POLLOUT` whenever `fd` is full, until
/// `deadline` (`None` = no deadline). Works on blocking and non-blocking fds.
pub(crate) fn write_all_until(fd: RawFd, data: &[u8], deadline: Option<Instant>) -> io::Result<()> {
    let mut written = 0;
    while written < data.len() {
        match write_once(fd, &data[written..])? {
            Some(n) => written += n,
            None => wait_writable(fd, deadline)?,
        }
    }
    Ok(())
}

/// `read()` that retries `EINTR`. `WouldBlock` is returned to the caller.
pub(crate) fn read_fd(fd: RawFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Build a `pollfd` for the directions we currently care about. With no
/// interest the fd is disabled (`-1`) so a hangup on a gated source can't make
/// `poll` return immediately forever while we wait for the other side.
pub(crate) fn interest(fd: RawFd, read: bool, write: bool) -> libc::pollfd {
    let mut events = 0;
    if read {
        events |= libc::POLLIN;
    }
    if write {
        events |= libc::POLLOUT;
    }
    libc::pollfd {
        fd: if fd >= 0 && events != 0 { fd } else { -1 },
        events,
        revents: 0,
    }
}

/// Whether a `pollfd` we asked to read from has something for `read()`: data,
/// EOF/hangup, or an error (which `read()` then reports).
pub(crate) fn readable(pfd: &libc::pollfd) -> bool {
    pfd.fd >= 0
        && pfd.events & libc::POLLIN != 0
        && pfd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0
}

/// `poll()` the set. `EINTR` is reported as "nothing ready" (all `revents`
/// cleared) so the caller simply goes round its loop again.
pub(crate) fn poll_fds(fds: &mut [libc::pollfd], timeout_ms: libc::c_int) -> io::Result<()> {
    let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
    if n >= 0 {
        return Ok(());
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::Interrupted {
        fds.iter_mut().for_each(|pfd| pfd.revents = 0);
        return Ok(());
    }
    Err(err)
}

/// Puts fds into `O_NONBLOCK` and restores their original flags on drop, so a
/// tty we share with other processes isn't left non-blocking after we exit.
pub(crate) struct NonblockingGuard {
    saved: Vec<(RawFd, libc::c_int)>,
}

impl NonblockingGuard {
    /// Fails if any fd can't be made non-blocking: relaying through a blocking
    /// fd would silently reintroduce the deadlock this module exists to avoid.
    /// Negative fds (meaning "none") are skipped.
    pub(crate) fn set(fds: &[RawFd]) -> io::Result<Self> {
        // Read every original flag set before changing any: fds that share an
        // open file description (stdin/stdout on a tty) must all restore to
        // the true original, not to a value we already modified.
        let mut guard = Self { saved: Vec::new() };
        let mut originals = Vec::with_capacity(fds.len());
        for &fd in fds.iter().filter(|fd| **fd >= 0) {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
            if flags < 0 {
                return Err(io::Error::last_os_error());
            }
            originals.push((fd, flags));
        }
        for (fd, flags) in originals {
            if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error()); // drop restores those already set
            }
            guard.saved.push((fd, flags));
        }
        Ok(guard)
    }
}

impl Drop for NonblockingGuard {
    fn drop(&mut self) {
        for &(fd, flags) in self.saved.iter().rev() {
            unsafe { libc::fcntl(fd, libc::F_SETFL, flags) };
        }
    }
}

#[cfg(test)]
#[path = "pump_tests.rs"]
mod tests;
