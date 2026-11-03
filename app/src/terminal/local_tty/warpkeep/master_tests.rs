//! Tests for the warpkeep master relay core (`serve`).
//!
//! The relay is driven against a real inner PTY whose slave runs `cat` in raw
//! mode: everything typed into the PTY comes straight back out of it, so the
//! peer echoes every byte — the same shape as bash echoing Warp's bootstrap
//! script, which is what deadlocked the old blocking relay.

use std::io::{Read, Write};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{Child, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use instant::Instant;

use super::super::{RING_CAPACITY, TAG_DATA, TAG_WINCH};
use super::{ClientPacket, parse_client_packets, serve};

/// Size of the single input burst pushed through the relay. Well above any
/// kernel PTY/socket buffer so a relay that blocks on one direction while the
/// other fills up deadlocks deterministically.
const BURST_BYTES: usize = 256 * 1024;

/// How long the whole burst may take to come back before the test fails.
/// Seconds of slack for a loaded CI box; a healthy relay needs milliseconds.
const RELAY_DEADLINE: Duration = Duration::from_secs(20);

/// How long teardown waits for the relay thread after the peer is killed.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Payload bytes per framed client packet (mirrors a typical PTY read).
const PACKET_PAYLOAD_BYTES: usize = 4096;

/// Deterministic pseudo-random payload covering all 256 byte values.
pub(super) fn test_payload(len: usize) -> Vec<u8> {
    let mut state: u32 = 0x9E37_79B9;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state >> 24) as u8
        })
        .collect()
}

/// An inner PTY whose slave side runs `cat` in raw mode (no echo, no line
/// discipline translation), so output is byte-identical to input.
struct RawCat {
    master_fd: RawFd,
    child: Child,
}

impl RawCat {
    fn spawn() -> Self {
        let ends = nix::pty::openpty(None, None).expect("openpty");
        let (master_fd, slave_fd) = (ends.master, ends.slave);
        unsafe {
            let mut termios: libc::termios = std::mem::zeroed();
            assert_eq!(libc::tcgetattr(slave_fd, &mut termios), 0, "tcgetattr");
            libc::cfmakeraw(&mut termios);
            assert_eq!(
                libc::tcsetattr(slave_fd, libc::TCSANOW, &termios),
                0,
                "tcsetattr"
            );
        }
        let dup = |fd: RawFd| unsafe { OwnedFd::from_raw_fd(libc::dup(fd)) };
        let child = command::blocking::Command::new("cat")
            .stdin(Stdio::from(dup(slave_fd)))
            .stdout(Stdio::from(dup(slave_fd)))
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cat");
        unsafe { libc::close(slave_fd) };
        Self { master_fd, child }
    }
}

/// A `serve` instance running on its own thread against a [`RawCat`].
struct Harness {
    _dir: tempfile::TempDir,
    socket: std::path::PathBuf,
    cat: RawCat,
    stop: Arc<AtomicBool>,
    done_rx: mpsc::Receiver<anyhow::Result<()>>,
}

impl Harness {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let socket = dir.path().join("wk-test.sock");
        let listener = UnixListener::bind(&socket).expect("bind");
        listener
            .set_nonblocking(true)
            .expect("nonblocking listener");
        let cat = RawCat::spawn();
        let stop = Arc::new(AtomicBool::new(false));
        let (done_tx, done_rx) = mpsc::channel();
        let master_fd = cat.master_fd;
        let stop_flag = Arc::clone(&stop);
        thread::spawn(move || {
            let result = serve(master_fd, &listener, || stop_flag.load(Ordering::SeqCst));
            let _ = done_tx.send(result);
        });
        Self {
            _dir: dir,
            socket,
            cat,
            stop,
            done_rx,
        }
    }

    /// Connect as an attach client: send the window-size header and consume
    /// the replay block, returning the stream and the replayed bytes.
    fn attach(&self) -> (UnixStream, Vec<u8>) {
        let mut stream = UnixStream::connect(&self.socket).expect("connect");
        stream
            .set_read_timeout(Some(RELAY_DEADLINE))
            .expect("read timeout");
        let mut header = Vec::new();
        header.extend_from_slice(&24u16.to_ne_bytes());
        header.extend_from_slice(&80u16.to_ne_bytes());
        stream.write_all(&header).expect("send header");
        let mut len = [0u8; 4];
        stream.read_exact(&mut len).expect("replay length");
        let mut replay = vec![0u8; u32::from_ne_bytes(len) as usize];
        stream.read_exact(&mut replay).expect("replay bytes");
        (stream, replay)
    }

    /// Stop the relay: kill the peer (which unblocks any PTY write with EIO)
    /// and wait, bounded, for `serve` to return.
    fn shutdown(mut self) -> Option<anyhow::Result<()>> {
        self.stop.store(true, Ordering::SeqCst);
        let _ = self.cat.child.kill();
        let _ = self.cat.child.wait();
        let result = self.done_rx.recv_timeout(TEARDOWN_TIMEOUT).ok();
        if result.is_some() {
            unsafe { libc::close(self.cat.master_fd) };
        }
        result
    }
}

fn frame(tag: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + payload.len());
    out.push(tag);
    out.extend_from_slice(&(payload.len() as u32).to_ne_bytes());
    out.extend_from_slice(payload);
    out
}

/// Read from `stream` until `want` bytes arrived or `deadline` passed.
fn read_until(stream: &mut UnixStream, want: usize, deadline: Instant) -> Vec<u8> {
    let mut received = Vec::with_capacity(want);
    let mut chunk = [0u8; 65536];
    while received.len() < want && Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let _ = stream.set_read_timeout(Some(remaining.max(Duration::from_millis(1))));
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => received.extend_from_slice(&chunk[..n]),
            Err(ref e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(_) => break,
        }
    }
    received
}

/// Regression test for the startup deadlock: a 256 KiB burst of input into a
/// shell that echoes it must come back in full. With blocking PTY writes the
/// master parked in `write()` while the echo filled the PTY output buffer, so
/// the peer stopped reading and nothing ever completed.
#[test]
fn warpkeep_master_relays_large_burst_to_echoing_pty_without_deadlock() {
    let harness = Harness::start();
    let (stream, replay) = harness.attach();
    assert!(replay.is_empty(), "fresh session should replay nothing");

    let sent = test_payload(BURST_BYTES);
    let mut writer_stream = stream.try_clone().expect("clone stream");
    let to_send = sent.clone();
    // Write the whole burst from a separate thread, as Warp does: the client
    // side must never be the thing that deadlocks.
    let writer = thread::spawn(move || {
        for chunk in to_send.chunks(PACKET_PAYLOAD_BYTES) {
            if writer_stream.write_all(&frame(TAG_DATA, chunk)).is_err() {
                return;
            }
        }
    });

    let mut reader_stream = stream;
    let received = read_until(
        &mut reader_stream,
        BURST_BYTES,
        Instant::now() + RELAY_DEADLINE,
    );
    let served = harness.shutdown();
    let _ = writer.join();

    assert_eq!(
        received.len(),
        BURST_BYTES,
        "relay stalled after {} of {BURST_BYTES} echoed bytes (deadlock)",
        received.len()
    );
    assert!(received == sent, "echoed bytes differ from input");
    assert!(
        matches!(served, Some(Ok(()))),
        "serve did not exit cleanly after the shell died: {served:?}"
    );
}

/// Detach/reattach keeps working: output produced for the first client is
/// recorded in the ring and replayed, byte-exact, to the next client.
#[test]
fn warpkeep_master_replays_ring_to_reattaching_client() {
    let harness = Harness::start();
    let sent = test_payload(16 * 1024);
    {
        let (mut stream, _) = harness.attach();
        for chunk in sent.chunks(PACKET_PAYLOAD_BYTES) {
            stream.write_all(&frame(TAG_DATA, chunk)).expect("send");
        }
        let received = read_until(&mut stream, sent.len(), Instant::now() + RELAY_DEADLINE);
        assert!(received == sent, "first client did not get its echo");
    } // first client detaches here

    let (_stream, replay) = harness.attach();
    assert!(replay == sent, "reattach replay differs from prior output");
    let served = harness.shutdown();
    assert!(matches!(served, Some(Ok(()))), "serve: {served:?}");
}

/// The ring keeps the most recent `RING_CAPACITY` bytes of PTY output even
/// when a burst larger than the ring flows through a client.
#[test]
fn warpkeep_master_ring_records_all_output_through_burst() {
    let harness = Harness::start();
    let sent = test_payload(RING_CAPACITY + 64 * 1024);
    {
        let (stream, _) = harness.attach();
        let mut writer_stream = stream.try_clone().expect("clone");
        let to_send = sent.clone();
        let writer = thread::spawn(move || {
            for chunk in to_send.chunks(PACKET_PAYLOAD_BYTES) {
                if writer_stream.write_all(&frame(TAG_DATA, chunk)).is_err() {
                    return;
                }
            }
        });
        let mut reader_stream = stream;
        let received = read_until(
            &mut reader_stream,
            sent.len(),
            Instant::now() + RELAY_DEADLINE,
        );
        let _ = writer.join();
        assert_eq!(received.len(), sent.len(), "burst did not complete");
    }
    let (_stream, replay) = harness.attach();
    assert!(
        replay == sent[sent.len() - RING_CAPACITY..],
        "replay is not the last RING_CAPACITY bytes of output"
    );
    let _ = harness.shutdown();
}

/// Packets are parsed in order, partial tails are kept for the next read, and
/// WINCH packets are recognised between data packets.
#[test]
fn warpkeep_master_parses_framed_packets_in_order() {
    let mut wire = Vec::new();
    wire.extend_from_slice(&frame(TAG_DATA, b"hello"));
    let mut winch = Vec::new();
    winch.extend_from_slice(&40u16.to_ne_bytes());
    winch.extend_from_slice(&120u16.to_ne_bytes());
    wire.extend_from_slice(&frame(TAG_WINCH, &winch));
    wire.extend_from_slice(&frame(TAG_DATA, b"world"));

    // Feed one byte at a time to exercise every partial-packet boundary.
    let mut buf = Vec::new();
    let mut seen = Vec::new();
    for byte in wire {
        buf.push(byte);
        let result = parse_client_packets(&mut buf, |packet| {
            seen.push(match packet {
                ClientPacket::Data(data) => format!("d:{}", String::from_utf8_lossy(data)),
                ClientPacket::Winch { rows, cols } => format!("w:{rows}x{cols}"),
            });
        });
        assert!(result.is_ok(), "valid stream rejected");
    }
    assert!(buf.is_empty(), "complete packets left bytes behind");
    assert_eq!(seen, vec!["d:hello", "w:40x120", "d:world"]);
}

/// A packet claiming an absurd length is a protocol error, not an invitation
/// to buffer gigabytes waiting for the rest of it.
#[test]
fn warpkeep_master_rejects_oversized_packet() {
    let mut buf = vec![TAG_DATA];
    buf.extend_from_slice(&u32::MAX.to_ne_bytes());
    let result = parse_client_packets(&mut buf, |_| {});
    assert!(result.is_err(), "oversized packet accepted");
}
