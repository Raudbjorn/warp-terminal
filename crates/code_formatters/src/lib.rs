//! Formatting buffer text with the formatters a project already uses (Prettier, Ruff, rustfmt and
//! so on).
//!
//! Nothing here installs or bundles a formatter. A tool is used only when its executable is found
//! and, for tools that format differently per project, the project opts in to it the way that
//! tool's own editor integration would expect.

use std::collections::HashSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
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
    /// Project code must not run until the user approves this exact canonical project root.
    NeedsAuthorization {
        project_root: PathBuf,
    },
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
/// timeout. Callers must have authorized project code; UI callers should use
/// [`format_authorized_in_background`] instead.
pub fn format(request: &FormatRequest) -> FormatOutcome {
    let selection = select(
        &request.file,
        request.project_root.as_deref(),
        request.path_env.as_deref(),
        &request.preference,
    );
    format_selected(request, selection)
}

fn format_selected(request: &FormatRequest, selection: Selection) -> FormatOutcome {
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
    in_background(move |_| format(&request))
}

/// Runs [`format_with`] on its own thread, like [`format_in_background`].
pub fn format_with_in_background(
    invocation: Invocation,
    text: String,
    path_env: Option<String>,
    limits: Limits,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    in_background(move |_| format_with(&invocation, &text, path_env.as_deref(), &limits))
}

/// Resolves the project and discovers/runs its formatter off the calling thread. Approval is
/// exact and session-scoped at the caller; a global formatter toggle is never authorization.
pub fn format_authorized_in_background(
    request: FormatRequest,
    authorized_roots: HashSet<PathBuf>,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    in_background(move |cancelled| format_authorized(request, &authorized_roots, cancelled))
}

fn format_authorized(
    mut request: FormatRequest,
    authorized_roots: &HashSet<PathBuf>,
    cancelled: &AtomicBool,
) -> FormatOutcome {
    if matches!(request.preference, Preference::Off) {
        return FormatOutcome::Disabled;
    }
    if !may_apply(&request.file, &request.preference) {
        return FormatOutcome::NoFormatter;
    }
    // Resolve aliases before deciding which project the file belongs to. A trusted directory
    // must not authorize a symlink into another checkout.
    request.file = match std::fs::canonicalize(&request.file) {
        Ok(file) => file,
        Err(error) => return background_failure(format!("could not resolve the file: {error}")),
    };
    request.project_root = request
        .project_root
        .and_then(|root| std::fs::canonicalize(root).ok());
    let scope = discovery::SearchScope::new(&request.file, request.project_root.as_deref());
    let Some(root) = scope.root() else {
        return FormatOutcome::NoFormatter;
    };
    if !authorized_roots.contains(root) {
        return FormatOutcome::NeedsAuthorization {
            project_root: root.to_path_buf(),
        };
    }
    let selection = select(
        &request.file,
        request.project_root.as_deref(),
        request.path_env.as_deref(),
        &request.preference,
    );
    // Discovery may outlive the caller on a stalled filesystem. It must not launch code later.
    if cancelled.load(Ordering::Acquire) {
        return background_failure("the formatting request was cancelled".to_owned());
    }
    format_selected(&request, selection)
}

const MAX_BACKGROUND_FORMATTERS: usize = 8;
static ACTIVE_FORMATTERS: AtomicUsize = AtomicUsize::new(0);

struct BackgroundPermit<'a>(&'a AtomicUsize);

impl<'a> BackgroundPermit<'a> {
    fn acquire(active: &'a AtomicUsize) -> Option<Self> {
        active
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < MAX_BACKGROUND_FORMATTERS).then_some(count + 1)
            })
            .ok()
            .map(|_| Self(active))
    }
}

impl Drop for BackgroundPermit<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Release);
    }
}

struct CancelOnDrop(Arc<AtomicBool>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

fn background_failure(message: String) -> FormatOutcome {
    FormatOutcome::Failed(FormatFailure {
        tool: "formatter".to_owned(),
        message,
    })
}

fn in_background(
    work: impl FnOnce(&AtomicBool) -> FormatOutcome + Send + 'static,
) -> impl Future<Output = FormatOutcome> + Send + 'static {
    async move {
        let Some(permit) = BackgroundPermit::acquire(&ACTIVE_FORMATTERS) else {
            return background_failure("too many formatters are still running".to_owned());
        };
        let (sender, receiver) = async_channel::bounded(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let _cancel_on_drop = CancelOnDrop(cancelled.clone());
        if let Err(error) = thread::Builder::new()
            .name("project-formatter".to_owned())
            .spawn(move || {
                let _permit = permit;
                let _ = sender.try_send(work(&cancelled));
            })
        {
            return background_failure(format!("could not start formatting: {error}"));
        }
        receiver
            .recv()
            .await
            .unwrap_or_else(|_| background_failure("stopped unexpectedly".to_owned()))
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
