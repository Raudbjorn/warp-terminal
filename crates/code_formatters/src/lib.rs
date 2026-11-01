//! Formatting buffer text with the formatters a project already uses (Prettier, Ruff, rustfmt and
//! so on).
//!
//! Nothing here installs or bundles a formatter. A tool is used only when its executable is found
//! and, for tools that format differently per project, the project opts in to it the way that
//! tool's own editor integration would expect.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{fmt, thread};

pub mod config;
pub mod discovery;
mod edits;
mod line_ending;
mod plan;
mod preference;
mod registry;
mod run;
#[cfg(all(test, unix))]
mod test_support;

pub use edits::{TextEdit, apply_edits, minimal_edits};
pub use line_ending::LineEnding;
pub use plan::{Arg, Invocation, Selection, select};
pub use preference::Preference;
pub use registry::{Language, Tool};
pub use run::{Limits, RunError, run};

/// Everything needed to format one buffer.
#[derive(Debug, Clone)]
pub struct FormatRequest {
    /// Absolute path of the file the buffer belongs to. The file itself is never read or written.
    pub file: PathBuf,
    /// The project the file belongs to, when the caller knows it.
    pub project_root: Option<PathBuf>,
    /// The buffer's current text.
    pub text: String,
    /// The user's shell `PATH`, for finding globally installed tools and for the formatter's own
    /// environment.
    pub path_env: Option<String>,
    pub preference: Preference,
    pub limits: Limits,
}

/// The result of a formatting attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormatOutcome {
    /// No project formatter applies to the file.
    NoFormatter,
    /// The user turned formatting off for the file.
    Disabled,
    /// The formatter ran and left the text as it was, which is also how tools report an ignored
    /// file.
    Unchanged {
        tool: Tool,
    },
    Formatted {
        tool: Tool,
        text: String,
        /// The edits that turn the request's text into `text`.
        edits: Vec<TextEdit>,
    },
    Failed(FormatFailure),
}

/// A formatter that could not be run, or ran and failed. The buffer must be left untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatFailure {
    /// The formatter's name.
    pub tool: String,
    /// A one-line reason, phrased to follow the tool's name.
    pub message: String,
}

impl fmt::Display for FormatFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.tool, self.message)
    }
}

/// Whether a project formatter could apply to `file`, judged from its name alone. `false` means
/// formatting is certain to be left to the language server, so nothing else needs to be gathered
/// for a [`FormatRequest`].
pub fn may_apply(file: &Path, preference: &Preference) -> bool {
    match preference {
        Preference::Lsp => false,
        Preference::Off | Preference::Tool(_) => true,
        Preference::Auto => Language::from_path(file).is_some(),
    }
}

/// Formats the request's text. This blocks on the formatter process, for up to the request's
/// timeout.
pub fn format(request: &FormatRequest) -> FormatOutcome {
    let selection = select(
        &request.file,
        request.project_root.as_deref(),
        request.path_env.as_deref(),
        &request.preference,
    );
    let invocation = match selection {
        Selection::LanguageServer => return FormatOutcome::NoFormatter,
        Selection::Off => return FormatOutcome::Disabled,
        Selection::Unavailable { tool, reason } => {
            return FormatOutcome::Failed(FormatFailure {
                tool,
                message: reason,
            });
        }
        Selection::External(invocation) => invocation,
    };
    format_with(
        &invocation,
        &request.text,
        request.path_env.as_deref(),
        &request.limits,
    )
}

/// Formats `text` with a formatter that [`select`] already chose. This blocks on the formatter
/// process, for up to the timeout in `limits`.
pub fn format_with(
    invocation: &Invocation,
    text: &str,
    path_env: Option<&str>,
    limits: &Limits,
) -> FormatOutcome {
    let tool = invocation.tool;
    match run(invocation, text, path_env, limits) {
        Ok(formatted) if formatted == text => FormatOutcome::Unchanged { tool },
        Ok(formatted) => FormatOutcome::Formatted {
            tool,
            edits: minimal_edits(text, &formatted),
            text: formatted,
        },
        Err(error) => {
            log::warn!(
                "{} failed to format {}: {error:?}",
                tool.name(),
                invocation.file.display()
            );
            FormatOutcome::Failed(FormatFailure {
                tool: tool.name().to_owned(),
                message: error.to_string(),
            })
        }
    }
}

/// Runs [`format`] on its own thread, so that neither the caller nor an async executor's worker
/// is blocked while the formatter runs.
pub fn format_in_background(
    request: FormatRequest,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    in_background(move || format(&request))
}

/// Runs [`format_with`] on its own thread, like [`format_in_background`].
pub fn format_with_in_background(
    invocation: Invocation,
    text: String,
    path_env: Option<String>,
    limits: Limits,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    in_background(move || format_with(&invocation, &text, path_env.as_deref(), &limits))
}

fn in_background(
    work: impl FnOnce() -> FormatOutcome + Send + 'static,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    let (sender, receiver) = async_channel::bounded(1);
    thread::spawn(move || {
        let _ = sender.try_send(work());
    });
    async move {
        receiver.recv().await.unwrap_or_else(|_| {
            FormatOutcome::Failed(FormatFailure {
                tool: "formatter".to_owned(),
                message: "stopped unexpectedly".to_owned(),
            })
        })
    }
}

/// Remembers which files have already had a formatting failure surfaced, so that a formatter that
/// keeps failing on save is reported once rather than on every save.
#[derive(Debug, Default)]
pub struct ReportedFailures {
    files: Mutex<HashSet<PathBuf>>,
}

impl ReportedFailures {
    /// Records a failure for `file`, returning whether it is the first one recorded for it.
    pub fn record(&self, file: &Path) -> bool {
        self.files
            .lock()
            .map(|mut files| files.insert(file.to_path_buf()))
            .unwrap_or(false)
    }
}

// The tests drive stand-in formatters written as shell scripts.
#[cfg(all(test, unix))]
#[path = "lib_tests.rs"]
mod tests;
