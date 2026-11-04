//! Which shell a warpkeep tab runs. The pty Warp spawns for a tab runs the attach worker, so its
//! pid (`ShellProcessInfo`) is the worker's, while the shell is a child of the detached master:
//! no process-tree link joins the two, and anything that finds a tab's shell by pid (the tmux
//! sync matches a tmux client to the shell it was started from) misses it.
//!
//! The two ends each leave their pid where the app can read it, next to the session socket
//! (`wk-<id>.sock`):
//! - the master writes the shell's pid to `wk-<id>.shell` before it binds the socket;
//! - the attach holding the session's `wk-<id>.lock` writes its own pid into that file and
//!   clears it when it ends.
//!
//! An attach that is killed leaves its pid behind while the shell lives on, and that pid can go to
//! another process, so a pid in a lock counts only while that process still holds the lock
//! (`/proc/locks`, read without taking the lock: a probe could make a recovering attach skip the
//! session, or fail to lock a new one).

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// The file with the shell's pid, for the session whose socket is `socket`.
pub(super) fn shell_path(socket: &Path) -> PathBuf {
    socket.with_extension("shell")
}

/// Master side: records `pid` (the shell) for `socket`'s session. Written whole under another
/// name and renamed, so a reader never sees half a number.
pub(super) fn write_shell_pid(socket: &Path, pid: u32) -> io::Result<()> {
    let path = shell_path(socket);
    let temp = path.with_extension("shell.tmp");
    std::fs::write(&temp, pid.to_string())?;
    std::fs::rename(&temp, &path)
}

/// Attach side: puts `pid` (the attach worker) into the lock file it holds.
pub(super) fn record_attach_pid(lock: &File, pid: u32) -> io::Result<()> {
    lock.set_len(0)?;
    // Truncating leaves the offset where the last write ended.
    (&*lock).seek(SeekFrom::Start(0))?;
    (&*lock).write_all(pid.to_string().as_bytes())
}

/// Attach side: the session is no longer attached to this worker.
pub(super) fn clear_attach_pid(lock: &File) {
    let _ = lock.set_len(0);
}

fn parse_pid(text: &str) -> Option<u32> {
    text.trim().parse().ok().filter(|&pid| pid > 0)
}

fn process_alive(pid: u32) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    // Signal 0 only checks that the process exists; EPERM means it does, under another user.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Whether `table` (the text of `/proc/locks`) lists a flock held by `pid` on the file with this
/// device and inode. A line reads `<n>: FLOCK ADVISORY WRITE <pid> <major>:<minor>:<inode> ...`,
/// the device in hex.
fn lock_table_has(table: &str, major: u32, minor: u32, inode: u64, pid: u32) -> bool {
    let file = format!("{major:02x}:{minor:02x}:{inode}");
    table.lines().any(|line| {
        let mut fields = line.split_whitespace().skip(1);
        fields.next() == Some("FLOCK")
            && fields.nth(2).and_then(|owner| owner.parse::<u32>().ok()) == Some(pid)
            && fields.next() == Some(file.as_str())
    })
}

/// Whether process `pid` holds the flock on `lock`, or `None` where the kernel does not list
/// locks.
#[cfg(target_os = "linux")]
fn lock_held_by(lock: &Path, pid: u32) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let table = std::fs::read_to_string("/proc/locks").ok()?;
    let meta = std::fs::metadata(lock).ok()?;
    let dev = meta.dev();
    Some(lock_table_has(
        &table,
        libc::major(dev),
        libc::minor(dev),
        meta.ino(),
        pid,
    ))
}

#[cfg(not(target_os = "linux"))]
fn lock_held_by(_lock: &Path, _pid: u32) -> Option<bool> {
    None
}

/// Every attached session in `dir`: the attach worker's pid (what a tab's pty reports) mapped to
/// the pid of the shell inside. A session whose shell is gone, or whose lock its recorded attach
/// no longer holds, is left out. Reads a few small files; a missing or unreadable directory gives
/// an empty map. Without `/proc/locks` the attach only has to be alive, a weaker check.
pub fn shells_by_attach(dir: &Path) -> HashMap<u32, u32> {
    let mut shells = HashMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return shells;
    };
    for entry in entries.flatten() {
        let lock = entry.path();
        if lock.extension().and_then(|ext| ext.to_str()) != Some("lock") {
            continue;
        }
        let Some(attach) = std::fs::read_to_string(&lock)
            .ok()
            .and_then(|text| parse_pid(&text))
        else {
            continue;
        };
        let attached = lock_held_by(&lock, attach).unwrap_or_else(|| process_alive(attach));
        if !attached {
            continue;
        }
        let Some(shell) = std::fs::read_to_string(shell_path(&lock))
            .ok()
            .and_then(|text| parse_pid(&text))
            .filter(|&pid| process_alive(pid))
        else {
            continue;
        };
        shells.insert(attach, shell);
    }
    shells
}

#[cfg(test)]
#[path = "shells_tests.rs"]
mod tests;
