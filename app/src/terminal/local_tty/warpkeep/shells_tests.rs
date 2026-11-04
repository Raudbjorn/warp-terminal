use std::fs;

use super::*;

/// A pid that no process has: above Linux's pid_max.
const NO_SUCH_PID: u32 = 0x7fff_fff0;

fn session(dir: &Path, name: &str, attach: Option<&str>, shell: Option<&str>) {
    let socket = dir.join(format!("{name}.sock"));
    if let Some(attach) = attach {
        fs::write(socket.with_extension("lock"), attach).unwrap();
    }
    if let Some(shell) = shell {
        fs::write(shell_path(&socket), shell).unwrap();
    }
}

/// A tab's pty reports the attach worker; the tmux sync needs the shell. Fails if the map is not
/// keyed by the attach pid or does not give the shell's.
#[test]
fn an_attach_pid_finds_the_shell_of_its_session() {
    let dir = tempfile::tempdir().unwrap();
    let alive = std::process::id().to_string();
    session(dir.path(), "wk-1-1", Some("4242"), Some(&alive));
    let shells = shells_by_attach(dir.path());
    assert_eq!(shells.get(&4242), Some(&std::process::id()));
    assert_eq!(shells.len(), 1);
}

#[test]
fn sessions_that_cannot_be_matched_are_left_out() {
    let dir = tempfile::tempdir().unwrap();
    let alive = std::process::id().to_string();
    let dead = NO_SUCH_PID.to_string();
    // The shell is gone.
    session(dir.path(), "wk-1-1", Some("1001"), Some(&dead));
    // No attach holds the session (the lock was cleared, or is empty).
    session(dir.path(), "wk-2-2", Some(""), Some(&alive));
    // The master has not written its pid yet.
    session(dir.path(), "wk-3-3", Some("1003"), None);
    // Not a pid.
    session(dir.path(), "wk-4-4", Some("1004"), Some("shell"));
    session(dir.path(), "wk-5-5", Some("0"), Some(&alive));
    // A socket with neither file.
    session(dir.path(), "wk-6-6", None, None);
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
    write_shell_pid(&socket, std::process::id()).unwrap();
    // A rewrite (a master restarted under the same name) replaces the number whole.
    write_shell_pid(&socket, std::process::id()).unwrap();
    assert!(!shell_path(&socket).with_extension("shell.tmp").exists());

    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(socket.with_extension("lock"))
        .unwrap();
    // A longer number first, then a shorter one: nothing of the old one may stay.
    record_attach_pid(&lock, 987654).unwrap();
    record_attach_pid(&lock, 77).unwrap();
    assert_eq!(
        shells_by_attach(dir.path()).get(&77),
        Some(&std::process::id())
    );
    assert!(!shells_by_attach(dir.path()).contains_key(&987654));

    clear_attach_pid(&lock);
    assert!(shells_by_attach(dir.path()).is_empty());
}
