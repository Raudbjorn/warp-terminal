//! Copies of the files a replace is about to change, kept on disk so the replace can be undone
//! even after the app restarts.
//!
//! Each replace is one transaction: a directory holding a manifest and one copy per changed file.
//! Copies include unsaved editor text. Unix permissions restrict them to the owner; Windows
//! inherits the access controls of the app's user state directory.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::disk::{create_private, create_private_dir};

const MANIFEST_FILE: &str = "manifest.jsonl";
const MANIFEST_VERSION: u32 = 1;

/// How much undo data is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct JournalLimits {
    /// Number of replaces that can be undone, most recent first.
    pub max_transactions: usize,
    /// Bytes of file copies one replace may keep. Files beyond it are not replaced.
    pub max_transaction_bytes: u64,
    /// Bytes all transactions together may keep before the oldest are dropped.
    pub max_total_bytes: u64,
}

impl Default for JournalLimits {
    fn default() -> Self {
        Self {
            max_transactions: 5,
            max_transaction_bytes: 256 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
        }
    }
}

/// What a transaction replaced, to tell the user what an undo takes back.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransactionDescription {
    pub pattern: String,
    pub replacement: String,
}

/// Where the copy of an entry came from, which decides what it is compared with on undo.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// The file was rewritten on disk. The copy holds its bytes.
    Disk,
    /// The file was edited through an editor. The copy holds the editor's text, with `\n` line
    /// endings.
    Buffer,
    /// The editor was clean before replacement, so restoring it must also persist its text.
    SavedBuffer,
}

/// One changed file of a transaction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub index: usize,
    pub path: PathBuf,
    pub kind: EntryKind,
    /// SHA-256 of the copy.
    pub pre_sha256: String,
    /// SHA-256 of what the replace left in the file or in the editor.
    pub post_sha256: String,
    pub occurrences: usize,
}

impl Entry {
    fn blob_name(index: usize) -> String {
        format!("{index}.pre")
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "snake_case")]
enum Record {
    Header {
        version: u32,
        #[serde(flatten)]
        description: TransactionDescription,
    },
    Entry(Entry),
    /// Takes back an entry whose file ended up not being changed.
    Retract {
        index: usize,
    },
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// The undo journal: the transactions kept under one directory.
#[derive(Clone, Debug)]
pub struct Journal {
    root: PathBuf,
    limits: JournalLimits,
}

impl Journal {
    pub fn new(root: PathBuf, limits: JournalLimits) -> Self {
        Self { root, limits }
    }

    /// Starts a transaction, dropping the oldest ones to stay within the limits.
    pub fn begin(&self, description: TransactionDescription) -> io::Result<TransactionWriter> {
        if self.limits.max_transactions == 0 {
            return Err(io::Error::new(
                io::ErrorKind::StorageFull,
                "the undo journal is disabled",
            ));
        }
        create_private_dir(&self.root)?;
        let header = encode_record(&Record::Header {
            version: MANIFEST_VERSION,
            description,
        })?;
        let budget = self
            .limits
            .max_total_bytes
            .checked_sub(header.len() as u64)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::StorageFull, "the undo journal is full")
            })?;
        self.prune(self.limits.max_transactions.saturating_sub(1), budget, None)?;

        let id = new_transaction_id();
        let directory = self.root.join(&id);
        create_private_dir(&directory)?;
        let mut manifest = create_private(&directory.join(MANIFEST_FILE))?;
        manifest.write_all(&header)?;
        Ok(TransactionWriter {
            id,
            directory,
            manifest,
            next_index: 0,
            live_entries: 0,
            bytes_reserved: 0,
            max_bytes: self.limits.max_transaction_bytes,
            journal: self.clone(),
        })
    }

    /// The most recent transaction that still has something to undo.
    pub fn latest(&self) -> io::Result<Option<Transaction>> {
        for id in self.transaction_ids()?.into_iter().rev() {
            let directory = self.root.join(&id);
            match Transaction::load(id, directory.clone()) {
                Ok(transaction) if !transaction.entries.is_empty() => {
                    return Ok(Some(transaction));
                }
                Ok(_) => {
                    let _ = fs::remove_dir_all(&directory);
                }
                Err(err) => {
                    log::warn!(
                        "ignoring unreadable replace journal {}: {err}",
                        directory.display()
                    );
                }
            }
        }
        Ok(None)
    }

    /// Transaction ids, oldest first. Ids start with their creation time, so sorting them orders
    /// them by age.
    fn transaction_ids(&self) -> io::Result<Vec<String>> {
        let entries = match fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };
        let mut ids = Vec::new();
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                if let Ok(name) = entry.file_name().into_string() {
                    ids.push(name);
                }
            }
        }
        ids.sort();
        Ok(ids)
    }

    /// Drops old transactions before allocating more bytes, never the active transaction.
    fn prune(&self, max_count: usize, max_bytes: u64, protected: Option<&str>) -> io::Result<()> {
        let mut ids = self.transaction_ids()?;
        let mut sizes: Vec<u64> = ids
            .iter()
            .map(|id| directory_size(&self.root.join(id)))
            .collect::<io::Result<_>>()?;
        while ids.len() > max_count || sizes.iter().sum::<u64>() > max_bytes {
            let Some(index) = ids.iter().position(|id| {
                Some(id.as_str()) != protected
                    && !file_write_guard::transaction_is_pinned(&self.root.join(id))
            }) else {
                return Err(io::Error::new(
                    io::ErrorKind::StorageFull,
                    "the undo journal is full",
                ));
            };
            fs::remove_dir_all(self.root.join(&ids[index]))?;
            ids.remove(index);
            sizes.remove(index);
        }
        Ok(())
    }
}

fn directory_size(directory: &Path) -> io::Result<u64> {
    fs::read_dir(directory)?.try_fold(0u64, |size, entry| {
        Ok(size.saturating_add(entry?.metadata()?.len()))
    })
}

fn new_transaction_id() -> String {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default();
    format!(
        "{millis:015}-{:010}-{:010}",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    )
}

fn encode_record(record: &Record) -> io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(record).map_err(io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

fn write_record(manifest: &mut File, record: &Record) -> io::Result<()> {
    manifest.write_all(&encode_record(record)?)
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("the undo journal is full")]
    Full,
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Adds the files of a replace in progress to its transaction.
#[derive(Debug)]
pub struct TransactionWriter {
    id: String,
    directory: PathBuf,
    manifest: File,
    next_index: usize,
    live_entries: usize,
    bytes_reserved: u64,
    max_bytes: u64,
    journal: Journal,
}

impl TransactionWriter {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sets room aside for a copy of `bytes` bytes, to be recorded with
    /// [`Self::record_reserved`]. Fails when the transaction has no room for it.
    pub fn reserve(&mut self, bytes: u64) -> Result<(), RecordError> {
        if self.bytes_reserved.saturating_add(bytes) > self.max_bytes {
            return Err(RecordError::Full);
        }
        self.bytes_reserved += bytes;
        Ok(())
    }

    /// Keeps `pre_image` as the copy of the file at `path`. Call this before changing the file,
    /// so that a change that is cut short can still be undone. Returns the index of the entry.
    pub fn record(
        &mut self,
        path: &Path,
        kind: EntryKind,
        pre_image: &[u8],
        post_sha256: String,
        occurrences: usize,
    ) -> Result<usize, RecordError> {
        self.reserve(pre_image.len() as u64)?;
        self.record_reserved(path, kind, pre_image, post_sha256, occurrences)
            .map_err(|err| {
                if err.kind() == io::ErrorKind::StorageFull {
                    RecordError::Full
                } else {
                    RecordError::Io(err)
                }
            })
    }

    /// Like [`Self::record`], for a copy whose room was set aside with [`Self::reserve`].
    pub fn record_reserved(
        &mut self,
        path: &Path,
        kind: EntryKind,
        pre_image: &[u8],
        post_sha256: String,
        occurrences: usize,
    ) -> io::Result<usize> {
        let index = self.next_index;
        let record = encode_record(&Record::Entry(Entry {
            index,
            path: path.to_path_buf(),
            kind,
            pre_sha256: sha256_hex(pre_image),
            post_sha256,
            occurrences,
        }))?;
        self.make_room((pre_image.len() as u64).saturating_add(record.len() as u64))?;
        let mut blob = create_private(&self.directory.join(Entry::blob_name(index)))?;
        blob.write_all(pre_image)?;
        blob.sync_all()?;
        drop(blob);
        self.manifest.write_all(&record)?;
        self.manifest.sync_all()?;
        self.next_index += 1;
        self.live_entries += 1;
        Ok(index)
    }

    fn make_room(&self, additional_bytes: u64) -> io::Result<()> {
        let budget = self
            .journal
            .limits
            .max_total_bytes
            .checked_sub(additional_bytes)
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::StorageFull, "the undo journal is full")
            })?;
        self.journal
            .prune(self.journal.limits.max_transactions, budget, Some(&self.id))
    }

    /// Takes back the entry at `index`, whose file ended up not being changed.
    pub fn retract(&mut self, index: usize) -> io::Result<()> {
        let record = encode_record(&Record::Retract { index })?;
        self.make_room(record.len() as u64)?;
        self.manifest.write_all(&record)?;
        self.live_entries = self.live_entries.saturating_sub(1);
        let _ = fs::remove_file(self.directory.join(Entry::blob_name(index)));
        Ok(())
    }

    /// Ends the transaction. Returns its id, or `None` when it changed nothing and so was
    /// dropped.
    pub fn finish(self) -> io::Result<Option<String>> {
        if self.live_entries == 0 {
            drop(self.manifest);
            fs::remove_dir_all(&self.directory)?;
            return Ok(None);
        }
        self.manifest.sync_all()?;
        self.make_room(0)?;
        Ok(Some(self.id))
    }
}

/// A transaction read back from disk.
#[derive(Clone, Debug)]
pub struct Transaction {
    pub id: String,
    pub description: TransactionDescription,
    /// The entries that still have to be undone, in the order their files were changed.
    pub entries: Vec<Entry>,
    directory: PathBuf,
}

impl Transaction {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    fn load(id: String, directory: PathBuf) -> io::Result<Self> {
        let manifest = fs::read(directory.join(MANIFEST_FILE))?;
        let mut description = None;
        let mut entries: Vec<Entry> = Vec::new();
        for line in manifest.split(|byte| *byte == b'\n') {
            if line.is_empty() {
                continue;
            }
            // A crash can cut the last line short. Its file was not changed yet, since an entry
            // is written before its file is.
            let Ok(record) = serde_json::from_slice::<Record>(line) else {
                break;
            };
            match record {
                Record::Header {
                    version,
                    description: header,
                } => {
                    if version != MANIFEST_VERSION {
                        return Err(io::Error::other(format!(
                            "unsupported journal version {version}"
                        )));
                    }
                    description = Some(header);
                }
                Record::Entry(entry) => entries.push(entry),
                Record::Retract { index } => entries.retain(|entry| entry.index != index),
            }
        }
        let description =
            description.ok_or_else(|| io::Error::other("the journal has no header"))?;
        Ok(Self {
            id,
            description,
            entries,
            directory,
        })
    }

    /// The copy kept for `entry`, checked against the hash recorded with it.
    pub fn pre_image(&self, entry: &Entry) -> io::Result<Vec<u8>> {
        let bytes = fs::read(self.directory.join(Entry::blob_name(entry.index)))?;
        if sha256_hex(&bytes) != entry.pre_sha256 {
            return Err(io::Error::other("the undo copy does not match its hash"));
        }
        Ok(bytes)
    }

    /// Forgets the entries at `indices`, which have been undone. The transaction is dropped
    /// altogether once it has no entry left.
    pub fn forget(&mut self, indices: &[usize]) -> io::Result<()> {
        self.entries.retain(|entry| !indices.contains(&entry.index));
        if self.entries.is_empty() {
            return fs::remove_dir_all(&self.directory);
        }

        // Written aside and renamed, so that a crash leaves either the old manifest or the new.
        let temp_path = self.directory.join(format!("{MANIFEST_FILE}.tmp"));
        let _ = fs::remove_file(&temp_path);
        let mut manifest = create_private(&temp_path)?;
        write_record(
            &mut manifest,
            &Record::Header {
                version: MANIFEST_VERSION,
                description: self.description.clone(),
            },
        )?;
        for entry in &self.entries {
            write_record(&mut manifest, &Record::Entry(entry.clone()))?;
        }
        manifest.sync_all()?;
        drop(manifest);
        fs::rename(&temp_path, self.directory.join(MANIFEST_FILE))?;

        for index in indices {
            let _ = fs::remove_file(self.directory.join(Entry::blob_name(*index)));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "journal_tests.rs"]
mod tests;
