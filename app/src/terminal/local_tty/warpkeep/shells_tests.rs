use std::fs::{self, File};
use std::os::fd::AsRawFd;

use nix::fcntl::{FlockArg, flock};

use super::*;

/// A pid that no process has: above Linux's pid_max.
const NO_SUCH_PID: u32 = 0x7fff_fff0;

/// A process that is alive and is not this one, standing in for a session's shell.
fn some_shell() -> u32 {
    std::os::unix::process::parent_id()
}

/// A session in `dir`. `lock_pid` is written into its lock file, which is also flocked by this
/// process when `held` (the file is returned: the lock ends when it is dropped).
fn session(
    dir: &Path,
    name: &str,
    lock_pid: Option<String>,
    shell: Option<String>,
    held: bool,
) -> Option<File> {
    let socket = dir.join(format!("{name}.sock"));
    if let Some(shell) = shell {
        fs::write(shell_path(&socket), shell).unwrap();
    }
    let lock_pid = lock_pid?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(socket.with_extension("lock"))
        .unwrap();
    fs::write(socket.with_extension("lock"), lock_pid).unwrap();
    if held {
        flock(lock.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
    }
    Some(lock)
}

fn me() -> String {
    std::process::id().to_string()
}

/// A tab's pty reports the attach worker; the tmux sync needs the shell. Fails if the map is not
/// keyed by the attach pid or does not give the shell's.
#[test]
fn an_attach_pid_finds_the_shell_of_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let _held = session(
        dir.path(),
        "wk-1-1",
        Some(me()),
        Some(some_shell().to_string()),
        true,
    );
    let shells = shells_by_attach(dir.path());
    assert_eq!(shells.get(&std::process::id()), Some(&some_shell()));
    assert_eq!(shells.len(), 1);
}

/// An attach killed without clearing its pid leaves the pid in the lock, and the pid can go to
/// another process; only a lock that process still holds counts. Fails if a recorded pid is
/// trusted without the lock.
#[test]
fn a_pid_in_a_lock_nobody_holds_is_not_an_attach() {
    let dir = tempfile::tempdir().unwrap();
    let shell = some_shell().to_string();
    // The attach died: the pid (here a live one, as if reused) is left, the lock is free.
    let _stale = session(dir.path(), "wk-1-1", Some(me()), Some(shell.clone()), false);
    assert!(shells_by_attach(dir.path()).is_empty());

    // A lock held by this process does not make another pid its attach.
    let _held = session(dir.path(), "wk-2-2", Some("4242".into()), Some(shell), true);
    assert!(shells_by_attach(dir.path()).is_empty());
}

#[test]
fn sessions_that_cannot_be_matched_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let dead = NO_SUCH_PID.to_string();
    let shell = some_shell().to_string();
    // All of them held, so only what they say decides.
    let _keep = [
        // The shell is gone.
        session(dir.path(), "wk-1-1", Some(me()), Some(dead), true),
        // No attach holds the session (the pid was cleared).
        session(
            dir.path(),
            "wk-2-2",
            Some(String::new()),
            Some(shell.clone()),
            true,
        ),
        // The master has not written its pid yet.
        session(dir.path(), "wk-3-3", Some(me()), None, true),
        // Not a pid.
        session(dir.path(), "wk-4-4", Some(me()), Some("shell".into()), true),
        session(dir.path(), "wk-5-5", Some("0".into()), Some(shell), true),
        // A socket with neither file.
        session(dir.path(), "wk-6-6", None, None, true),
    ];
    assert!(shells_by_attach(dir.path()).is_empty());
}

#[test]
fn a_missing_directory_gives_no_shells() {
    let dir = tempfile::tempdir().unwrap();
    assert!(shells_by_attach(&dir.path().join("absent")).is_empty());
}

/// The master and attach sides write what the reader parses, and the attach's pid goes away with
/// the attach.
#[test]
fn the_pids_the_workers_write_are_the_pids_read() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("wk-7-7.sock");
    write_shell_pid(&socket, some_shell()).unwrap();
    // A rewrite (a master restarted under the same name) replaces the number whole.
    write_shell_pid(&socket, some_shell()).unwrap();
    assert!(!shell_path(&socket).with_extension("shell.tmp").exists());

    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(socket.with_extension("lock"))
        .unwrap();
    flock(lock.as_raw_fd(), FlockArg::LockExclusiveNonblock).unwrap();
    // A longer number first, then the real one: nothing of the old one may stay.
    record_attach_pid(&lock, 987654).unwrap();
    record_attach_pid(&lock, std::process::id()).unwrap();
    assert_eq!(
        shells_by_attach(dir.path()).get(&std::process::id()),
        Some(&some_shell())
    );
    assert!(!shells_by_attach(dir.path()).contains_key(&987654));

    clear_attach_pid(&lock);
    assert!(shells_by_attach(dir.path()).is_empty());
}

/// `/proc/locks` as the kernel prints it.
#[test]
fn a_flock_line_names_its_owner_and_file() {
    let table = "\
1: POSIX  ADVISORY  WRITE 812 00:1a:77 0 EOF
2: FLOCK  ADVISORY  WRITE 4242 08:11:1683464 0 EOF
3: FLOCK  ADVISORY  READ 4243 00:36:99 0 EOF
";
    assert!(lock_table_has(table, 0x08, 0x11, 1683464, 4242));
    // The right file held by another pid, another file held by the pid, a POSIX lock.
    assert!(!lock_table_has(table, 0x08, 0x11, 1683464, 4243));
    assert!(!lock_table_has(table, 0x08, 0x11, 1683465, 4242));
    assert!(!lock_table_has(table, 0x00, 0x1a, 77, 812));
    assert!(!lock_table_has("", 0x08, 0x11, 1683464, 4242));
}
