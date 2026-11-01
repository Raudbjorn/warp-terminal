//! Bounded quarantine for writes whose caller may stop waiting before the filesystem finishes.
//! Paths are already canonicalized by the caller. No filesystem I/O occurs while locking.
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

/// Hung writes must not create an unbounded number of completion monitors or retained journals.
pub const MAX_PENDING_WRITES: usize = 32;
const PENDING: &str = "a previous save for this file is still pending";
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static WRITES: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

struct Entry {
    id: u64,
    path: PathBuf,
    transaction: PathBuf,
    write_finished: bool,
    journal_finished: bool,
}

/// Dropping a guard deliberately keeps its quarantine: a lost observer cannot prove I/O ended.
#[derive(Clone, Debug)]
#[must_use]
pub struct WriteGuard {
    id: u64,
    path: PathBuf,
}

impl WriteGuard {
    pub fn acquire(path: PathBuf, transaction: PathBuf) -> Result<Self, &'static str> {
        let path = dunce::simplified(&path).to_path_buf();
        let transaction = dunce::simplified(&transaction).to_path_buf();
        let mut writes = WRITES
            .lock()
            .map_err(|_| "pending-save registry is unavailable")?;
        if writes.iter().any(|entry| entry.path == path) {
            return Err(PENDING);
        }
        if writes.len() >= MAX_PENDING_WRITES {
            return Err("too many saves are still pending");
        }
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        writes.push(Entry {
            id,
            path: path.clone(),
            transaction,
            write_finished: false,
            journal_finished: false,
        });
        Ok(Self { id, path })
    }

    /// Only the originating write may bypass quarantine, and only for its exact destination.
    pub fn authorizes(&self, path: &Path) -> bool {
        self.path == dunce::simplified(path)
            && WRITES.lock().is_ok_and(|writes| {
                writes
                    .iter()
                    .any(|entry| entry.id == self.id && !entry.write_finished)
            })
    }

    /// Called only after the filesystem operation returned, never on timeout or lost completion.
    pub fn write_finished(&self) {
        self.finish(true);
    }
    /// Called after the operation's journal entries have been retained or finalized.
    pub fn journal_finished(&self) {
        self.finish(false);
    }
    /// No write was dispatched, so neither late I/O nor journal completion remains to await.
    pub fn cancel_before_dispatch(self) {
        self.write_finished();
        self.journal_finished();
    }

    fn finish(&self, write: bool) {
        let Ok(mut writes) = WRITES.lock() else {
            return;
        };
        if let Some(entry) = writes.iter_mut().find(|entry| entry.id == self.id) {
            if write {
                entry.write_finished = true;
            } else {
                entry.journal_finished = true;
            }
        }
        writes.retain(|entry| !(entry.write_finished && entry.journal_finished));
    }
}

pub fn is_quarantined(canonical_path: &Path) -> bool {
    WRITES
        .lock()
        .map(|writes| {
            writes
                .iter()
                .any(|entry| entry.path == dunce::simplified(canonical_path))
        })
        .unwrap_or(true)
}

pub fn transaction_is_pinned(directory: &Path) -> bool {
    WRITES
        .lock()
        .map(|writes| {
            writes
                .iter()
                .any(|entry| entry.transaction == dunce::simplified(directory))
        })
        .unwrap_or(true)
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
