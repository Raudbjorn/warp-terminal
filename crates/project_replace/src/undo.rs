//! Taking back the last replace, file by file, without ever discarding work done since.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::{fs, io};

use crate::apply::{OpenBuffer, Progress};
use crate::disk::{read_rewritable, write_atomically};
use crate::journal::{Entry, EntryKind, Journal, Transaction, TransactionDescription, sha256_hex};
use crate::report::{RefusalReason, RefusedFile, SkipReason};

/// Text to put back in an editor, for the editor to apply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BufferRestore {
    /// The path of the [`OpenBuffer`] to restore.
    pub buffer_path: PathBuf,
    /// The text the editor had when the undo was planned. The restore only applies while the
    /// editor still holds it.
    pub current_text: String,
    /// The text the editor had before the replace.
    pub restored_text: String,
    /// Whether restoring this buffer must complete a disk save before forgetting the entry.
    pub save: bool,
    entry_index: usize,
    entry_path: PathBuf,
}

/// What an undo did to the files on disk and what is left for the editors to do.
#[derive(Debug)]
pub struct UndoOutcome {
    pub description: TransactionDescription,
    /// Files put back on disk, or found to hold their original content already.
    pub restored: Vec<PathBuf>,
    pub refused: Vec<RefusedFile>,
    /// Restores still to be applied by the editors holding those files. Report how each went
    /// with [`UndoOutcome::finish`].
    pub buffer_restores: Vec<BufferRestore>,
    /// Number of files the undo did not get to because it was cancelled.
    pub not_attempted: usize,
    transaction: Transaction,
    undone: Vec<usize>,
}

/// The whole of a finished undo.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UndoReport {
    pub restored: Vec<PathBuf>,
    pub refused: Vec<RefusedFile>,
    pub not_attempted: usize,
}

impl UndoReport {
    pub fn was_cancelled(&self) -> bool {
        self.not_attempted > 0
    }
}

impl UndoOutcome {
    pub fn transaction_directory(&self) -> PathBuf {
        self.transaction.directory().to_path_buf()
    }

    /// Records what the editors did with [`UndoOutcome::buffer_restores`] and drops what was
    /// undone from the journal. Files that were refused stay in it, so that undoing can be tried
    /// again. `restored` and `refused` name restores by their [`BufferRestore::buffer_path`].
    pub fn finish(mut self, restored: &[PathBuf], refused: Vec<RefusedFile>) -> UndoReport {
        for restore in &self.buffer_restores {
            if restored.contains(&restore.buffer_path) {
                self.undone.push(restore.entry_index);
                self.restored.push(restore.entry_path.clone());
            }
        }
        self.refused.extend(refused);

        if let Err(err) = self.transaction.forget(&self.undone) {
            log::warn!("could not update the replace journal after undoing: {err}");
        }
        UndoReport {
            restored: self.restored,
            refused: self.refused,
            not_attempted: self.not_attempted,
        }
    }
}

/// Undoes the most recent replace that still has something to undo. Returns `None` when there is
/// none.
///
/// A file is only restored while it holds exactly what the replace left in it. Anything else
/// means it was edited since, and it is left as it is.
pub fn undo_latest(
    journal: &Journal,
    open_buffers: &[OpenBuffer],
    progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> io::Result<Option<UndoOutcome>> {
    let Some(transaction) = journal.latest()? else {
        return Ok(None);
    };
    let open_buffers: HashMap<PathBuf, &OpenBuffer> = open_buffers
        .iter()
        .map(|buffer| (canonical(&buffer.path), buffer))
        .collect();

    let mut restored = Vec::new();
    let mut refused = Vec::new();
    let mut buffer_restores = Vec::new();
    let mut undone = Vec::new();
    let mut not_attempted = 0;
    let total = transaction.entries.len();

    // Most recent change first, in case a file was changed more than once.
    for (done, entry) in transaction.entries.iter().rev().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            not_attempted = total - done;
            break;
        }

        let canonical_path = canonical(&entry.path);
        if file_write_guard::is_quarantined(&canonical_path) {
            refused.push(RefusedFile {
                path: entry.path.clone(),
                reason: RefusalReason::WriteFailed(
                    "a previous save for this file is still pending".to_owned(),
                ),
            });
            continue;
        }
        let open_buffer = open_buffers.get(&canonical_path).copied();
        match undo_entry(&transaction, entry, open_buffer) {
            Ok(Undone::Restored) => {
                undone.push(entry.index);
                restored.push(entry.path.clone());
            }
            Ok(Undone::InBuffer(restore)) => buffer_restores.push(restore),
            Err(reason) => refused.push(RefusedFile {
                path: entry.path.clone(),
                reason,
            }),
        }
        progress(Progress {
            done: done + 1,
            total,
        });
    }

    Ok(Some(UndoOutcome {
        description: transaction.description.clone(),
        restored,
        refused,
        buffer_restores,
        not_attempted,
        transaction,
        undone,
    }))
}

enum Undone {
    /// The file, or the editor holding it, has its original content again.
    Restored,
    InBuffer(BufferRestore),
}

fn undo_entry(
    transaction: &Transaction,
    entry: &Entry,
    open_buffer: Option<&OpenBuffer>,
) -> Result<Undone, RefusalReason> {
    let pre_image = transaction
        .pre_image(entry)
        .map_err(|err| RefusalReason::JournalDamaged(err.to_string()))?;

    match (entry.kind, open_buffer) {
        (EntryKind::Buffer | EntryKind::SavedBuffer, Some(buffer)) => {
            let save = entry.kind == EntryKind::SavedBuffer || !buffer.has_unsaved_changes;
            let current = sha256_hex(buffer.text.as_bytes());
            let needs_save = entry.kind == EntryKind::SavedBuffer && buffer.has_unsaved_changes;
            if current == entry.pre_sha256 && !needs_save {
                return Ok(Undone::Restored);
            }
            if current != entry.post_sha256 && current != entry.pre_sha256 {
                return Err(RefusalReason::EditedSinceReplace);
            }
            if save {
                read_rewritable(
                    &entry.path,
                    crate::journal::JournalLimits::default().max_transaction_bytes,
                )
                .map_err(|reason| RefusalReason::WriteFailed(reason.describe()))?;
            }
            let restored_text = String::from_utf8(pre_image)
                .map_err(|err| RefusalReason::JournalDamaged(err.to_string()))?;
            Ok(Undone::InBuffer(BufferRestore {
                buffer_path: buffer.path.clone(),
                current_text: buffer.text.clone(),
                restored_text,
                save,
                entry_index: entry.index,
                entry_path: entry.path.clone(),
            }))
        }
        // Writing the file would put it at odds with the unsaved text of the editor.
        (EntryKind::Disk, Some(buffer)) if buffer.has_unsaved_changes => {
            Err(RefusalReason::UnsavedChanges)
        }
        // The editor has nothing unsaved and picks the restored file up from disk.
        (EntryKind::Disk, Some(_) | None) => restore_on_disk(&entry.path, entry, &pre_image, false),
        // The editor that was edited is gone. The file holds its text if it was saved, though
        // possibly with other line endings than the editor's `\n`.
        (EntryKind::Buffer | EntryKind::SavedBuffer, None) => {
            restore_on_disk(&entry.path, entry, &pre_image, true)
        }
    }
}

fn has_crlf(bytes: &[u8]) -> bool {
    bytes.windows(2).any(|pair| pair == b"\r\n")
}

/// Writes `pre_image` over the file at `path` if the file holds what the replace left in it.
/// Buffer entries compare normalized line endings and restore the file's line-ending style.
fn restore_on_disk(
    path: &Path,
    entry: &Entry,
    pre_image: &[u8],
    normalize_line_endings: bool,
) -> Result<Undone, RefusalReason> {
    // shortcut: undo reads at most one transaction's budget, raise with journal limits if needed.
    let max_bytes = crate::journal::JournalLimits::default().max_transaction_bytes;
    let (bytes, stamp) = read_rewritable(path, max_bytes).map_err(|reason| match reason {
        SkipReason::Missing => RefusalReason::Missing,
        SkipReason::ReadFailed(error) => RefusalReason::ReadFailed(error),
        SkipReason::ChangedSinceSearch
        | SkipReason::ChangedDuringReplace
        | SkipReason::Binary
        | SkipReason::UnsupportedEncoding
        | SkipReason::Symlink
        | SkipReason::NotAFile
        | SkipReason::TooLarge { .. }
        | SkipReason::ReadOnly
        | SkipReason::HardLinked
        | SkipReason::OutOfScope
        | SkipReason::EditedWhileReplacing
        | SkipReason::JournalFull
        | SkipReason::SearchFailed(_)
        | SkipReason::WriteFailed(_) => RefusalReason::WriteFailed(reason.describe()),
    })?;

    let comparable = if normalize_line_endings {
        std::borrow::Cow::Owned(
            String::from_utf8_lossy(&bytes)
                .replace("\r\n", "\n")
                .into_bytes(),
        )
    } else {
        std::borrow::Cow::Borrowed(bytes.as_slice())
    };
    let current = sha256_hex(&comparable);
    if current == entry.pre_sha256 {
        return Ok(Undone::Restored);
    }
    if current != entry.post_sha256 {
        return Err(RefusalReason::EditedSinceReplace);
    }
    if !stamp.still_matches(path) {
        return Err(RefusalReason::EditedSinceReplace);
    }
    let pre_image = if normalize_line_endings && has_crlf(&bytes) {
        std::borrow::Cow::Owned(
            String::from_utf8_lossy(pre_image)
                .replace('\n', "\r\n")
                .into_bytes(),
        )
    } else {
        std::borrow::Cow::Borrowed(pre_image)
    };
    write_atomically(path, &pre_image, &stamp).map_err(|err| {
        if err.kind() == io::ErrorKind::WouldBlock {
            RefusalReason::EditedSinceReplace
        } else {
            RefusalReason::WriteFailed(err.to_string())
        }
    })?;
    Ok(Undone::Restored)
}

fn canonical(path: &Path) -> PathBuf {
    fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

#[cfg(test)]
#[path = "undo_tests.rs"]
mod tests;
