//! Tests for the warpkeep attach relay core (`pump`).

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use instant::Instant;

use super::super::{TAG_DATA, TAG_WINCH};
use super::pump;

/// Size of the single burst typed into the relay's stdin.
const BURST_BYTES: usize = 256 * 1024;

/// How long the burst may take to come back before the test fails.
const RELAY_DEADLINE: Duration = Duration::from_secs(20);

/// How long teardown waits for the relay thread once the peer hangs up.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Socket buffer size forced on both ends of the relay socket so the
/// deadlock (if any) shows up after a few KiB instead of a few hundred.
const SMALL_SOCKET_BUFFER_BYTES: libc::c_int = 4096;

fn pipe() -> (OwnedFd, OwnedFd) {
    let mut fds = [0 as RawFd; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe");
    unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) }
}

fn shrink_socket_buffers(stream: &UnixStream) {
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
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
}

fn payload(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x2545_F491;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// A deliberately naive master: single-threaded, blocking, echoes every data
/// payload straight back. It blocks in `write` whenever the relay stops
/// reading — exactly the peer behaviour that exposed the attach deadlock.
/// Records the sequence of packets it saw.
fn naive_echo_master(
    mut stream: UnixStream,
    events: mpsc::Sender<(u8, usize)>,
) -> Vec<(u8, usize)> {
    let mut seen = Vec::new();
    loop {
        let mut header = [0u8; 5];
        if stream.read_exact(&mut header).is_err() {
            return seen;
        }
        let len = u32::from_ne_bytes([header[1], header[2], header[3], header[4]]) as usize;
        let mut body = vec![0u8; len];
        if stream.read_exact(&mut body).is_err() {
            return seen;
        }
        seen.push((header[0], len));
        let _ = events.send((header[0], len));
        if header[0] == TAG_DATA && stream.write_all(&body).is_err() {
            return seen;
        }
    }
}

/// Read from a pipe until `want` bytes arrived or `deadline` passed.
fn read_pipe_until(fd: &OwnedFd, want: usize, deadline: Instant) -> Vec<u8> {
    let mut received = Vec::with_capacity(want);
    let mut chunk = [0u8; 65536];
    while received.len() < want {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as libc::c_int;
        if unsafe { libc::poll(&mut pfd, 1, timeout_ms) } <= 0 {
            continue;
        }
        let n = unsafe {
            libc::read(
                fd.as_raw_fd(),
                chunk.as_mut_ptr() as *mut libc::c_void,
                chunk.len(),
            )
        };
        if n <= 0 {
            break;
        }
        received.extend_from_slice(&chunk[..n as usize]);
    }
    received
}

/// Regression test: a 256 KiB burst on stdin, relayed to a master that echoes
/// it back with blocking writes, must arrive on stdout in full. The old relay
/// blocked writing the burst to the socket while the master blocked writing
/// the echo back, and neither ever read again.
#[test]
fn warpkeep_attach_relays_large_burst_to_echoing_master_without_deadlock() {
    let (relay_sock, peer_sock) = UnixStream::pair().expect("socketpair");
    shrink_socket_buffers(&relay_sock);
    shrink_socket_buffers(&peer_sock);
    let peer_control = peer_sock.try_clone().expect("clone peer");
    let (events_tx, _events_rx) = mpsc::channel();
    let peer = thread::spawn(move || naive_echo_master(peer_sock, events_tx));

    let (stdin_r, stdin_w) = pipe();
    let (stdout_r, stdout_w) = pipe();

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut relay_sock = relay_sock;
        let status = pump(
            &mut relay_sock,
            stdin_r.as_raw_fd(),
            stdout_w.as_raw_fd(),
            -1,
        );
        drop(stdin_r);
        drop(stdout_w);
        let _ = done_tx.send(status);
    });

    let sent = payload(BURST_BYTES);
    let to_send = sent.clone();
    let writer = thread::spawn(move || {
        let mut stdin = std::fs::File::from(stdin_w);
        let _ = stdin.write_all(&to_send);
        // Keep stdin open (as Warp's PTY would be) until the test is done.
        stdin
    });

    let received = read_pipe_until(&stdout_r, BURST_BYTES, Instant::now() + RELAY_DEADLINE);

    // Teardown: the master hangs up; the relay must notice and exit.
    let _ = peer_control.shutdown(Shutdown::Both);
    let status = done_rx.recv_timeout(TEARDOWN_TIMEOUT);
    let _ = peer.join();
    drop(writer.join());

    assert_eq!(
        received.len(),
        BURST_BYTES,
        "relay stalled after {} of {BURST_BYTES} echoed bytes (deadlock)",
        received.len()
    );
    assert!(received == sent, "echoed bytes differ from input");
    assert_eq!(status, Ok(0), "relay did not exit cleanly on master hangup");
}

/// A window-size change is framed as a WINCH packet and forwarded to the
/// master, interleaved in order with the data packets around it.
#[test]
fn warpkeep_attach_forwards_winch_in_order_with_data() {
    let (relay_sock, peer_sock) = UnixStream::pair().expect("socketpair");
    let peer_control = peer_sock.try_clone().expect("clone peer");
    let (events_tx, events_rx) = mpsc::channel();
    let peer = thread::spawn(move || naive_echo_master(peer_sock, events_tx));

    let (stdin_r, stdin_w) = pipe();
    let (stdout_r, stdout_w) = pipe();
    let (winch_r, winch_w) = pipe();

    let (done_tx, done_rx) = mpsc::channel();
    thread::spawn(move || {
        let mut relay_sock = relay_sock;
        let status = pump(
            &mut relay_sock,
            stdin_r.as_raw_fd(),
            stdout_w.as_raw_fd(),
            winch_r.as_raw_fd(),
        );
        drop((stdin_r, stdout_w, winch_r));
        let _ = done_tx.send(status);
    });

    let mut stdin = std::fs::File::from(stdin_w);
    let mut winch = std::fs::File::from(winch_w);
    let deadline = Instant::now() + RELAY_DEADLINE;

    stdin.write_all(b"before").expect("stdin");
    assert_eq!(read_pipe_until(&stdout_r, 6, deadline), b"before");
    assert_eq!(events_rx.recv_timeout(RELAY_DEADLINE), Ok((TAG_DATA, 6)));
    winch.write_all(&[1]).expect("winch");
    // Wait for the WINCH to reach the master before typing more, so the
    // expected packet order is deterministic.
    assert_eq!(events_rx.recv_timeout(RELAY_DEADLINE), Ok((TAG_WINCH, 4)));
    stdin.write_all(b"after").expect("stdin");
    assert_eq!(read_pipe_until(&stdout_r, 5, deadline), b"after");

    let _ = peer_control.shutdown(Shutdown::Both);
    let status = done_rx.recv_timeout(TEARDOWN_TIMEOUT);
    let seen = peer.join().expect("peer");
    assert_eq!(seen, vec![(TAG_DATA, 6), (TAG_WINCH, 4), (TAG_DATA, 5)]);
    assert_eq!(status, Ok(0));
}
