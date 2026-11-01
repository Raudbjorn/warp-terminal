//! Capturing block and session output to local files.
//!
//! Backs the `Save …/Stream … to file...` context-menu commands that replaced
//! the cloud-only "Share block..." / "Share session..." items.
//!
//! Snapshot commands write once and close. Streaming commands write the same
//! content and then keep the handle open, appending as the command produces
//! more output.
//!
//! ## Why streaming polls
//!
//! There is no "rendered text was appended" event anywhere in the terminal. The
//! only live signal is the raw PTY byte broadcast, which carries escape
//! sequences rather than rendered text, so a stream that wants plain text has to
//! re-render and diff. [`FileStream::update_tail`] is driven from the existing
//! terminal wakeup tick.
//!
//! ## Why the tail can rewrite rather than append
//!
//! A block's grid is not append-only: `\r`, progress bars and cursor movement
//! rewrite lines in place. When a fresh render is still an extension of what was
//! written, only the new suffix is appended. When it is not, the mutable tail's
//! section of the file is rewound and rewritten. Session tails include background
//! output after the last completed foreground command. The file therefore always
//! equals the current rendered output instead of accumulating every intermediate
//! frame of a progress bar.

use std::fs::{File, OpenOptions};
use std::io::{Error, ErrorKind, Result as IoResult, Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};

/// Separator written between blocks in session captures.
pub const BLOCK_SEPARATOR: &str = "\n";

/// Timestamp format used by the `{timestamp}` and `{finished-timestamp}` tokens.
const TIMESTAMP_FORMAT: &str = "%Y%m%d%H%M%S";

/// What a capture command covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureScope {
    /// The target block(s), output only.
    BlockOutput,
    /// The target block(s), command line plus output.
    BlockFull,
    /// Every block in the session, command lines plus output.
    Session,
}

/// One capture command: what to write, and whether the file stays open.
///
/// The six menu commands differ only along these two axes, so they all funnel
/// through a single handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureRequest {
    pub scope: CaptureScope,
    /// `true` for streaming. Block streams become plain saves when their command
    /// has finished; session streams remain open while the prompt is idle.
    pub keep_open: bool,
}

impl CaptureRequest {
    pub fn block_output(keep_open: bool) -> Self {
        Self {
            scope: CaptureScope::BlockOutput,
            keep_open,
        }
    }

    pub fn block_full(keep_open: bool) -> Self {
        Self {
            scope: CaptureScope::BlockFull,
            keep_open,
        }
    }

    pub fn session(keep_open: bool) -> Self {
        Self {
            scope: CaptureScope::Session,
            keep_open,
        }
    }

    pub fn is_session(&self) -> bool {
        matches!(self.scope, CaptureScope::Session)
    }
}

/// Values substituted into a filename pattern.
///
/// Each field is the *raw* value; [`expand_pattern`] sanitises them
/// individually, so a command containing `/` or `..` cannot escape into a path.
#[derive(Debug, Default, Clone)]
pub struct PatternTokens {
    /// The target block's command. With a multi-selection, the first block's.
    pub command: Option<String>,
    /// The target block's completion time. With a multi-selection, the last
    /// block's. `None` while nothing has finished, which is always the case for
    /// a stream started mid-command.
    pub finished: Option<DateTime<Local>>,
    /// The tab name when the user set one, otherwise `None` so the pattern falls
    /// back to the first command. Auto-generated tab names come from the working
    /// directory and make poor filenames.
    pub session_name: Option<String>,
}

/// Replaces characters that are not safe in a filename on any supported
/// platform, and trims the result to something reasonable.
///
/// Applied per token rather than to the finished string: sanitising afterwards
/// would also strip the dot in `.log`.
fn sanitize_token(raw: &str) -> String {
    const MAX_TOKEN_LEN: usize = 64;

    let cleaned: String = raw
        .trim()
        .chars()
        .map(|c| match c {
            // Windows-reserved plus path separators and control characters.
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();

    // Collapse runs of underscores and whitespace so `git log --oneline > x`
    // does not become a wall of separators.
    let mut out = String::with_capacity(cleaned.len());
    let mut last_was_sep = false;
    for c in cleaned.chars() {
        let is_sep = c == '_' || c.is_whitespace();
        if is_sep {
            if !last_was_sep {
                out.push('_');
            }
        } else {
            out.push(c);
        }
        last_was_sep = is_sep;
    }

    let trimmed = out.trim_matches(|c| c == '_' || c == '.');
    let mut result: String = trimmed.chars().take(MAX_TOKEN_LEN).collect();
    if result.is_empty() {
        result.push_str("untitled");
    }
    result
}

/// Expands a user-configured filename pattern.
///
/// Recognised tokens: `{command}`, `{timestamp}`, `{finished-timestamp}` and
/// `{session-or-command}`. Unknown tokens are left alone so a typo is visible in
/// the suggested filename rather than silently vanishing.
///
/// `{finished-timestamp}` falls back to the current time when nothing has
/// finished yet, so a stream started mid-command still produces a usable name.
pub fn expand_pattern(pattern: &str, tokens: &PatternTokens) -> String {
    let now = Local::now();
    let timestamp = now.format(TIMESTAMP_FORMAT).to_string();
    let finished = tokens
        .finished
        .unwrap_or(now)
        .format(TIMESTAMP_FORMAT)
        .to_string();

    let command = tokens.command.as_deref().unwrap_or_default();
    let session_or_command = tokens
        .session_name
        .as_deref()
        .filter(|name| !name.trim().is_empty())
        .unwrap_or(command);

    pattern
        .replace("{command}", &sanitize_token(command))
        .replace("{timestamp}", &timestamp)
        .replace("{finished-timestamp}", &finished)
        .replace("{session-or-command}", &sanitize_token(session_or_command))
}

/// A capture file, optionally still being appended to.
///
/// Dropping it closes the handle, so a stream cannot outlive the view that owns
/// it.
pub struct FileStream {
    path: PathBuf,
    file: File,
    /// Index of the block currently being tailed.
    block_index: usize,
    /// Text already written for that block.
    tail_text: String,
    /// Byte offset at which that block's section starts, so a non-append change
    /// can rewind to exactly there without disturbing earlier blocks.
    tail_offset: u64,
}

impl FileStream {
    /// Creates the file and writes `initial`.
    ///
    /// `tail` names the block that subsequent [`Self::update_tail`] calls refer
    /// to, together with the portion of `initial` that belongs to it. Pass the
    /// empty string when the block has produced nothing yet.
    pub fn create(path: &Path, initial: &str, tail: Option<(usize, String)>) -> IoResult<Self> {
        let (block_index, tail_text) = tail.unwrap_or((0, String::new()));
        let tail_offset = initial
            .strip_suffix(&tail_text)
            .ok_or_else(|| {
                Error::new(
                    ErrorKind::InvalidInput,
                    "capture tail must end the snapshot",
                )
            })?
            .len() as u64;
        let mut file = open_capture_file(path)?;
        file.write_all(initial.as_bytes())?;
        file.flush()?;

        Ok(Self {
            path: path.to_path_buf(),
            file,
            block_index,
            tail_text,
            tail_offset,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn block_index(&self) -> usize {
        self.block_index
    }

    /// Brings the file up to date with a fresh render of the tailed block.
    ///
    /// Appends when `rendered` merely extends what was written; otherwise
    /// rewinds to the block's start offset and rewrites its section. Does no I/O
    /// when nothing changed, which is the common case on a quiet wakeup tick.
    pub fn update_tail(&mut self, rendered: &str) -> IoResult<()> {
        if rendered == self.tail_text {
            return Ok(());
        }

        if let Some(added) = rendered.strip_prefix(self.tail_text.as_str()) {
            self.file.write_all(added.as_bytes())?;
        } else {
            // The block redrew itself; replace its section wholesale.
            self.file.seek(SeekFrom::Start(self.tail_offset))?;
            self.file.write_all(rendered.as_bytes())?;
            let end = self.tail_offset + rendered.len() as u64;
            self.file.set_len(end)?;
            self.file.seek(SeekFrom::Start(end))?;
        }

        self.file.flush()?;
        self.tail_text = rendered.to_owned();
        Ok(())
    }

    /// Finalises the current block and starts tailing another.
    ///
    /// Used by session streams when a block completes: `final_render` is written
    /// as that block's last state, a separator follows, and the tail moves on.
    pub fn advance_to(
        &mut self,
        final_render: &str,
        next_block_index: usize,
        separator: &str,
    ) -> IoResult<()> {
        self.update_tail(final_render)?;
        self.file.write_all(separator.as_bytes())?;
        self.file.flush()?;

        self.block_index = next_block_index;
        self.tail_text = String::new();
        self.tail_offset = self.file.stream_position()?;
        Ok(())
    }

    /// Closes the already-flushed file, returning its path for the confirmation toast.
    pub fn finish(self) -> PathBuf {
        self.path
    }
}

/// Writes `contents` to `path` and closes it, for the snapshot commands.
pub fn write_snapshot(path: &Path, contents: &str) -> IoResult<()> {
    let mut file = open_capture_file(path)?;
    file.write_all(contents.as_bytes())?;
    file.flush()
}

fn open_capture_file(path: &Path) -> IoResult<File> {
    let mut options = OpenOptions::new();
    options.create(true).write(true);
    // Captures include unredacted command output, so new files are private.
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    // Lock before truncating so another pane or a path alias cannot destroy a live capture.
    file.try_lock().map_err(Error::from)?;
    file.set_len(0)?;
    Ok(file)
}

#[cfg(test)]
#[path = "file_capture_tests.rs"]
mod tests;
