//! Reading the files a replace may touch and writing them back without ever leaving one half
//! written.

use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use crate::journal::sha256_hex;
use crate::report::SkipReason;

/// What was known about a file when it was read, to notice a change made behind our back.
#[derive(Clone, Debug)]
pub(crate) struct FileStamp {
    metadata: Metadata,
    modified: Option<SystemTime>,
    resolved_path: PathBuf,
    identity: (u64, u64),
    sha256: String,
}

impl FileStamp {
    pub(crate) fn resolved_path(&self) -> &Path {
        &self.resolved_path
    }

    /// Checks identity, location and bytes; timestamps alone can be restored or too coarse.
    pub(crate) fn still_matches(&self, path: &Path) -> bool {
        let unchanged = || -> io::Result<bool> {
            if fs::canonicalize(path)? != self.resolved_path {
                return Ok(false);
            }
            let current = fs::symlink_metadata(path)?;
            if !current.is_file()
                || current.len() != self.metadata.len()
                || current.modified().ok() != self.modified
            {
                return Ok(false);
            }
            let mut file = File::open(path)?;
            let opened = file.metadata()?;
            if file_identity(&file)? != self.identity
                || opened.permissions().readonly()
                || has_other_hard_links(&file, &opened)?
            {
                return Ok(false);
            }
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(self.metadata.len().saturating_add(1))
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 != self.metadata.len() || sha256_hex(&bytes) != self.sha256 {
                return Ok(false);
            }
            let current = fs::symlink_metadata(path)?;
            Ok(current.is_file()
                && current.len() == self.metadata.len()
                && current.modified().ok() == self.modified
                && fs::canonicalize(path)? == self.resolved_path
                && file_identity(&File::open(path)?)? == self.identity)
        };
        unchanged().unwrap_or(false)
    }
}

/// Reads a file that is safe to rewrite, or says why it has to be left alone.
pub(crate) fn read_rewritable(
    path: &Path,
    max_bytes: u64,
) -> Result<(Vec<u8>, FileStamp), SkipReason> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Err(SkipReason::Missing),
        Err(err) => return Err(SkipReason::ReadFailed(err.to_string())),
    };
    if metadata.file_type().is_symlink() {
        return Err(SkipReason::Symlink);
    }
    if !metadata.is_file() {
        return Err(SkipReason::NotAFile);
    }
    if metadata.len() > max_bytes {
        return Err(SkipReason::TooLarge {
            bytes: metadata.len(),
            limit: max_bytes,
        });
    }
    if metadata.permissions().readonly() {
        return Err(SkipReason::ReadOnly);
    }
    let resolved_path =
        fs::canonicalize(path).map_err(|err| SkipReason::ReadFailed(err.to_string()))?;
    let mut file = File::open(path).map_err(|err| SkipReason::ReadFailed(err.to_string()))?;
    let identity = file_identity(&file).map_err(|err| SkipReason::ReadFailed(err.to_string()))?;
    let metadata = file
        .metadata()
        .map_err(|err| SkipReason::ReadFailed(err.to_string()))?;
    if !metadata.is_file() {
        return Err(SkipReason::NotAFile);
    }
    if metadata.permissions().readonly() {
        return Err(SkipReason::ReadOnly);
    }
    if metadata.len() > max_bytes {
        return Err(SkipReason::TooLarge {
            bytes: metadata.len(),
            limit: max_bytes,
        });
    }
    if has_other_hard_links(&file, &metadata)
        .map_err(|err| SkipReason::ReadFailed(err.to_string()))?
    {
        return Err(SkipReason::HardLinked);
    }
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|err| SkipReason::ReadFailed(err.to_string()))?;
    if bytes.len() as u64 > max_bytes {
        return Err(SkipReason::TooLarge {
            bytes: bytes.len() as u64,
            limit: max_bytes,
        });
    }
    let stamp = FileStamp {
        modified: metadata.modified().ok(),
        metadata,
        resolved_path,
        identity,
        sha256: sha256_hex(&bytes),
    };
    if !stamp.still_matches(path) {
        return Err(SkipReason::ChangedDuringReplace);
    }
    Ok((bytes, stamp))
}

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // The borrowed handle and output live for the complete call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(io::Error::other)?;
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(_file: &File) -> io::Result<(u64, u64)> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cannot identify files on this platform",
    ))
}

/// Replacing a file by renaming another over it would detach it from its other names.
#[cfg(unix)]
fn has_other_hard_links(_file: &File, metadata: &Metadata) -> io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    Ok(metadata.nlink() > 1)
}

#[cfg(windows)]
fn has_other_hard_links(file: &File, _metadata: &Metadata) -> io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = BY_HANDLE_FILE_INFORMATION::default();
    // The file owns the handle and the output remains valid for the call.
    unsafe { GetFileInformationByHandle(HANDLE(file.as_raw_handle()), &mut information) }
        .map_err(io::Error::other)?;
    Ok(information.nNumberOfLinks > 1)
}

#[cfg(not(any(unix, windows)))]
fn has_other_hard_links(_file: &File, _metadata: &Metadata) -> io::Result<bool> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "cannot check hard links on this platform",
    ))
}

/// Replaces the content of the file at `path` with `bytes`, keeping its permissions. Readers see
/// either the old content or the new one, never a mix, and the old content stays if this fails.
pub(crate) fn write_atomically(path: &Path, bytes: &[u8], original: &FileStamp) -> io::Result<()> {
    let temp_path = temp_path_for(original.resolved_path())?;
    let result = write_and_rename(&temp_path, path, bytes, original, || {});
    if result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }
    result
}

fn write_and_rename(
    temp_path: &Path,
    path: &Path,
    bytes: &[u8],
    original: &FileStamp,
    before_commit: impl FnOnce(),
) -> io::Result<()> {
    let mut file = create_private(temp_path)?;
    ensure_same_owner(&file, &original.metadata)?;
    file.write_all(bytes)?;
    file.set_permissions(original.metadata.permissions())?;
    file.sync_all()?;
    drop(file);
    before_commit();
    if !original.still_matches(path) {
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "the file changed before replacement",
        ));
    }
    // shortcut: check/rename still has a narrow race; use directory-handle APIs for hostile writers.
    fs::rename(temp_path, original.resolved_path())
}

/// A sibling of `path`, so that renaming it over `path` stays within one file system.
fn temp_path_for(path: &Path) -> io::Result<PathBuf> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the path has no parent directory or no file name",
        ));
    };
    let mut temp_name = std::ffi::OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(
        ".{}-{}.replace-tmp",
        std::process::id(),
        NEXT_ID.fetch_add(1, Ordering::Relaxed)
    ));
    Ok(parent.join(temp_name))
}

/// Creates a new file with owner-only Unix permissions or inherited Windows ACLs.
pub(crate) fn create_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

/// Creates a directory with owner-only Unix permissions or inherited Windows ACLs.
pub(crate) fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        builder.mode(0o700);
        builder.create(path)?;
        // The mode only applies to directories created just now.
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    builder.create(path)
}

/// The rewritten file belongs to whoever runs the app, which would take a file away from another
/// owner.
#[cfg(unix)]
fn ensure_same_owner(temp_file: &File, original: &Metadata) -> io::Result<()> {
    use std::os::unix::fs::MetadataExt;
    if temp_file.metadata()?.uid() == original.uid() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the file belongs to another user",
        ))
    }
}

#[cfg(not(unix))]
fn ensure_same_owner(_temp_file: &File, _original: &Metadata) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
#[path = "disk_tests.rs"]
mod tests;
