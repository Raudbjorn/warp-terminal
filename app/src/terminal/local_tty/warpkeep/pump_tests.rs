//! Unit tests for the warpkeep pending-write buffer and poll helpers.

use std::io::{ErrorKind, Read};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use instant::Instant;

use super::super::master::{ClientPacket, parse_client_packets};
use super::super::{TAG_DATA, TAG_WINCH};
use super::{NonblockingGuard, PendingWrites, interest, write_all_until};

/// Small kernel buffer so a single flush can only write part of the queue.
const SMALL_SOCKET_BUFFER_BYTES: libc::c_int = 4096;

/// A cap large enough that backpressure never kicks in during a test that is
/// about something else.
const UNBOUNDED_CAP_BYTES: usize = usize::MAX;

/// A socket pair whose writer end is non-blocking with a tiny send buffer.
fn small_nonblocking_pair() -> (UnixStream, UnixStream) {
    let (writer, reader) = UnixStream::pair().expect("socketpair");
    for (stream, opt) in [(&writer, libc::SO_SNDBUF), (&reader, libc::SO_RCVBUF)] {
        let value = SMALL_SOCKET_BUFFER_BYTES;
        let rc = unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                opt,
                &value as *const _ as *const libc::c_void,
                std::mem::size_of_val(&value) as libc::socklen_t,
            )
        };
        assert_eq!(rc, 0, "setsockopt");
    }
    writer.set_nonblocking(true).expect("nonblocking");
    reader
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    (writer, reader)
}

fn bytes(len: usize) -> Vec<u8> {
    (0..len).map(|i| (i % 251) as u8).collect()
}

/// Flush until empty, draining the reader in between, and return what the
/// reader received — the order the bytes actually hit the wire.
fn pump_through(
    pending: &mut PendingWrites,
    writer: &UnixStream,
    reader: &mut UnixStream,
) -> Vec<u8> {
    let mut received = Vec::new();
    let mut chunk = [0u8; 8192];
    while !pending.is_empty() {
        pending.flush_to(writer.as_raw_fd()).expect("flush");
        let n = reader.read(&mut chunk).expect("read");
        received.extend_from_slice(&chunk[..n]);
    }
    // Anything still in flight in the kernel buffer.
    reader
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("read timeout");
    while let Ok(n) = reader.read(&mut chunk) {
        if n == 0 {
            break;
        }
        received.extend_from_slice(&chunk[..n]);
    }
    received
}

#[test]
fn warpkeep_pending_partial_write_keeps_the_rest_queued() {
    let (writer, mut reader) = small_nonblocking_pair();
    let data = bytes(256 * 1024);
    let mut pending = PendingWrites::new(UNBOUNDED_CAP_BYTES);
    pending.push(&data);

    let written = pending.flush_to(writer.as_raw_fd()).expect("flush");
    assert!(written > 0, "nothing written to an empty socket");
    assert!(written < data.len(), "small socket accepted everything");
    assert_eq!(
        pending.len(),
        data.len() - written,
        "lost or duplicated bytes"
    );

    // EAGAIN is not an error: flushing a full socket again writes nothing.
    assert_eq!(pending.flush_to(writer.as_raw_fd()).expect("flush"), 0);

    let mut received = vec![0u8; written];
    reader.read_exact(&mut received).expect("read");
    received.extend(pump_through(&mut pending, &writer, &mut reader));
    assert!(
        received == data,
        "bytes reordered or corrupted across partial writes"
    );
}

#[test]
fn warpkeep_pending_full_at_cap_and_clears_after_flush() {
    let (writer, mut reader) = small_nonblocking_pair();
    let cap = 100;
    let mut pending = PendingWrites::new(cap);
    pending.push(&bytes(cap - 1));
    assert!(!pending.is_full(), "full below cap");
    pending.push(&[0]);
    assert!(pending.is_full(), "not full at cap");
    // Push past the cap is still accepted: bytes are never dropped.
    pending.push(&bytes(10));
    assert_eq!(pending.len(), cap + 10);

    pump_through(&mut pending, &writer, &mut reader);
    assert!(pending.is_empty() && !pending.is_full());
}

#[test]
fn warpkeep_pending_frames_stay_ordered_through_partial_writes() {
    let (writer, mut reader) = small_nonblocking_pair();
    let mut pending = PendingWrites::new(UNBOUNDED_CAP_BYTES);
    let big = bytes(20 * 1024);
    let mut winch = [0u8; 4];
    winch[..2].copy_from_slice(&50u16.to_ne_bytes());
    winch[2..].copy_from_slice(&132u16.to_ne_bytes());

    pending.push_frame(TAG_DATA, &big);
    pending.push_frame(TAG_WINCH, &winch);
    pending.push_frame(TAG_DATA, b"tail");

    let mut wire = pump_through(&mut pending, &writer, &mut reader);
    let mut seen = Vec::new();
    parse_client_packets(&mut wire, |packet| {
        seen.push(match packet {
            ClientPacket::Data(data) => (TAG_DATA, data.to_vec()),
            ClientPacket::Winch { rows, cols } => {
                (TAG_WINCH, format!("{rows}x{cols}").into_bytes())
            }
        });
    })
    .expect("well-formed stream");
    assert!(wire.is_empty(), "trailing partial packet");
    assert_eq!(
        seen,
        vec![
            (TAG_DATA, big),
            (TAG_WINCH, b"50x132".to_vec()),
            (TAG_DATA, b"tail".to_vec()),
        ]
    );
}

#[test]
fn warpkeep_pending_flush_reports_hangup_as_error() {
    let (writer, reader) = small_nonblocking_pair();
    drop(reader);
    let mut pending = PendingWrites::new(UNBOUNDED_CAP_BYTES);
    pending.push(b"nobody is listening");
    let err = pending
        .flush_to(writer.as_raw_fd())
        .expect_err("write to a closed peer must fail, not vanish");
    assert_eq!(err.kind(), ErrorKind::BrokenPipe);
}

#[test]
fn warpkeep_pending_flush_until_times_out_when_peer_stalls() {
    let (writer, _reader) = small_nonblocking_pair();
    let mut pending = PendingWrites::new(UNBOUNDED_CAP_BYTES);
    pending.push(&bytes(256 * 1024));
    let start = Instant::now();
    let err = pending
        .flush_until(
            writer.as_raw_fd(),
            Some(Instant::now() + Duration::from_millis(100)),
        )
        .expect_err("stalled peer must time out");
    assert_eq!(err.kind(), ErrorKind::TimedOut);
    assert!(start.elapsed() < Duration::from_secs(5), "deadline ignored");
    assert!(!pending.is_empty(), "unwritten bytes must stay queued");
}

#[test]
fn warpkeep_write_all_until_waits_for_slow_reader() {
    let (writer, mut reader) = small_nonblocking_pair();
    let data = bytes(64 * 1024);
    let expected = data.clone();
    let consumer = std::thread::spawn(move || {
        let mut received = vec![0u8; expected.len()];
        reader.read_exact(&mut received).expect("read");
        received == expected
    });
    write_all_until(
        writer.as_raw_fd(),
        &data,
        Some(Instant::now() + Duration::from_secs(10)),
    )
    .expect("write_all_until");
    assert!(consumer.join().expect("consumer"), "bytes differ");
}

#[test]
fn warpkeep_interest_disables_fd_without_interest() {
    assert_eq!(interest(7, false, false).fd, -1);
    assert_eq!(interest(-1, true, true).fd, -1);
    let both = interest(7, true, true);
    assert_eq!(both.fd, 7);
    assert_eq!(both.events, libc::POLLIN | libc::POLLOUT);
}

#[test]
fn warpkeep_nonblocking_guard_restores_original_flags() {
    let mut fds = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
    let (read_end, write_end) =
        unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    let flags = |fd: &OwnedFd| unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL, 0) };
    let before = flags(&write_end);
    assert_eq!(before & libc::O_NONBLOCK, 0, "pipe starts blocking");
    {
        let _guard = NonblockingGuard::set(&[read_end.as_raw_fd(), write_end.as_raw_fd(), -1])
            .expect("set nonblocking");
        assert_ne!(flags(&write_end) & libc::O_NONBLOCK, 0);
        assert_ne!(flags(&read_end) & libc::O_NONBLOCK, 0);
    }
    assert_eq!(flags(&write_end), before);
    assert_eq!(flags(&read_end) & libc::O_NONBLOCK, 0);
}
