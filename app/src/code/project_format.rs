//! Formatting a buffer with the formatter its project is set up with (Prettier, Ruff and so on),
//! ahead of the language server.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use code_formatters::{
    FormatFailure, FormatOutcome, FormatRequest, Limits, Preference, ReportedFailures, Selection,
    TextEdit,
};
use futures::FutureExt as _;
use futures::future::{self, Either};
use repo_metadata::repositories::DetectedRepositories;
use string_offset::CharOffset;
use vec1::Vec1;
use warp_util::content_version::ContentVersion;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::r#async::Timer;
use warpui::{AppContext, SingletonEntity, ViewContext};

use super::{FormatTrigger, LocalCodeEditorView};
use crate::ai::persisted_workspace::PersistedWorkspace;
use crate::settings::CodeSettings;
use crate::terminal::local_shell::LocalShellState;

/// Files whose formatter has already been reported as failing on save during this session.
static REPORTED_SAVE_FAILURES: LazyLock<ReportedFailures> =
    LazyLock::new(ReportedFailures::default);

const FORMATTING_OFF_NOTICE: &str = "Formatting is turned off for this file.";

/// The user's choice of formatter for a file that a project formatter may handle.
struct ProjectFormatSettings {
    file: PathBuf,
    preference: Preference,
    limits: Limits,
}

impl LocalCodeEditorView {
    /// Formats the buffer with its project's formatter. Returns `false` when no project formatter
    /// applies to this file, leaving the request to the language server.
    pub(super) fn format_with_project_formatter(
        &mut self,
        trigger: FormatTrigger,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        let Some(ProjectFormatSettings {
            file,
            preference,
            limits,
        }) = self.project_format_settings(ctx)
        else {
            return false;
        };
        if matches!(preference, Preference::Off) {
            self.finish_format(trigger, Some(FORMATTING_OFF_NOTICE.to_owned()), ctx);
            return true;
        }
        let project_root = project_root(&file, ctx);

        let shell_state = LocalShellState::handle(ctx);
        let login_path = shell_state
            .as_ref(ctx)
            .local_shell_info()
            .and_then(|shell| shell.get_path_env_var().clone());
        let mut interactive_path = shell_state.update(ctx, |shell_state, ctx| {
            shell_state.get_interactive_path_env_var(ctx)
        });

        let Some(captured_path) = interactive_path.as_mut().now_or_never() else {
            // The shell's `PATH` is still being captured, and without it there is no telling
            // whether a project formatter owns this file, so the choice waits for it.
            let Some(text) = self.buffer_text(ctx) else {
                self.finish_format(trigger, None, ctx);
                return true;
            };
            let request_file = file.clone();
            let outcome = async move {
                // Capturing the `PATH` runs the user's shell startup files, which must not be
                // able to hold up a save indefinitely.
                let timeout = Timer::after(limits.timeout);
                futures::pin_mut!(timeout);
                let captured_path = match future::select(interactive_path, timeout).await {
                    Either::Left((path, _)) => path,
                    Either::Right(_) => None,
                };
                let Some(path_env) = captured_path.or(login_path) else {
                    return FormatOutcome::NoFormatter;
                };
                code_formatters::format_in_background(FormatRequest {
                    file: request_file,
                    project_root,
                    text,
                    path_env: Some(path_env),
                    preference,
                    limits,
                })
                .await
            };
            self.await_project_format(trigger, file, outcome, ctx);
            return true;
        };

        // An app launched from the Dock has a minimal `PATH` of its own, so without the shell's
        // neither a formatter nor the interpreter it runs on can be relied on.
        let Some(path_env) = captured_path.or(login_path) else {
            return false;
        };
        // Choosing the formatter only inspects the project, so it happens here: a file without a
        // project formatter then takes exactly the path it always has.
        let selection = code_formatters::select(
            &file,
            project_root.as_deref(),
            Some(path_env.as_str()),
            &preference,
        );
        match selection {
            Selection::LanguageServer => false,
            Selection::Off => {
                self.finish_format(trigger, Some(FORMATTING_OFF_NOTICE.to_owned()), ctx);
                true
            }
            Selection::Unavailable { tool, reason } => {
                let failure = FormatFailure {
                    tool,
                    message: reason,
                };
                self.report_project_format_failure(trigger, &file, &failure, ctx);
                self.finish_format(trigger, None, ctx);
                true
            }
            Selection::External(invocation) => {
                match self.buffer_text(ctx) {
                    Some(text) => {
                        let outcome = code_formatters::format_with_in_background(
                            invocation,
                            text,
                            Some(path_env),
                            limits,
                        );
                        self.await_project_format(trigger, file, outcome, ctx);
                    }
                    None => self.finish_format(trigger, None, ctx),
                }
                true
            }
        }
    }

    /// Starts capturing the user's shell `PATH` when a project formatter may handle this file, so
    /// that the first save does not have to wait for it.
    pub(super) fn warm_up_project_formatter(&self, ctx: &mut ViewContext<Self>) {
        if self.project_format_settings(ctx).is_none() {
            return;
        }
        // The shell state caches the captured `PATH`, so the future itself is not needed.
        drop(
            LocalShellState::handle(ctx).update(ctx, |shell_state, ctx| {
                shell_state.get_interactive_path_env_var(ctx)
            }),
        );
    }

    /// The settings to format this file with, or `None` when no project formatter can apply.
    fn project_format_settings(&self, ctx: &AppContext) -> Option<ProjectFormatSettings> {
        // App contexts that never run formatters, such as test harnesses, do not register these.
        if !ctx.has_singleton_model::<CodeSettings>()
            || !ctx.has_singleton_model::<LocalShellState>()
        {
            return None;
        }
        let settings = CodeSettings::as_ref(ctx);
        if !*settings.use_project_formatters {
            return None;
        }
        let file = self.file_path()?.to_path_buf();
        let preference = Preference::resolve(&settings.formatter_overrides, &file);
        if !code_formatters::may_apply(&file, &preference) {
            return None;
        }
        Some(ProjectFormatSettings {
            file,
            preference,
            limits: Limits::with_timeout(Duration::from_millis(*settings.formatter_timeout_ms)),
        })
    }

    /// The buffer's text with `\n` line endings, whatever the file is saved with, so that edits
    /// computed against it line up with the buffer's own offsets. `None` if the two do not line
    /// up, in which case a formatter's edits could not be applied without corrupting the buffer.
    fn buffer_text(&self, ctx: &AppContext) -> Option<String> {
        let buffer = self
            .editor
            .as_ref(ctx)
            .model
            .as_ref(ctx)
            .buffer()
            .as_ref(ctx);
        let text = buffer.text().into_string();
        // Buffer offsets start at 1, so the offset past the last character is one more than the
        // number of characters.
        if CharOffset::from(text.chars().count() + 1) != buffer.max_charoffset() {
            log::warn!("Not formatting: buffer text does not line up with buffer offsets");
            return None;
        }
        Some(text)
    }

    /// Applies the formatter's `outcome`, for the text the buffer holds right now, once it is
    /// ready.
    fn await_project_format(
        &mut self,
        trigger: FormatTrigger,
        file: PathBuf,
        outcome: impl Future<Output = FormatOutcome> + Send + 'static,
        ctx: &mut ViewContext<Self>,
    ) {
        let version = self.editor.as_ref(ctx).version(ctx);
        // A save that is waiting on the formatter still has to happen if the editor is closed in
        // the meantime, so the editor is kept alive until the formatter is done.
        let keep_alive = ctx.handle().upgrade(ctx);
        ctx.spawn(outcome, move |me, outcome, ctx| {
            me.finish_project_format(trigger, version, &file, outcome, ctx);
            drop(keep_alive);
        });
    }

    fn finish_project_format(
        &mut self,
        trigger: FormatTrigger,
        version: ContentVersion,
        file: &Path,
        outcome: FormatOutcome,
        ctx: &mut ViewContext<Self>,
    ) {
        match outcome {
            FormatOutcome::NoFormatter => self.format_with_language_server(trigger, ctx),
            FormatOutcome::Disabled => {
                self.finish_format(trigger, Some(FORMATTING_OFF_NOTICE.to_owned()), ctx)
            }
            FormatOutcome::Unchanged { .. } => self.finish_format(trigger, None, ctx),
            FormatOutcome::Formatted { edits, .. } => {
                // Edits computed against text the user has since changed would land in the wrong
                // places.
                if self.file_path() == Some(file)
                    && self.editor.as_ref(ctx).version_match(&version, ctx)
                {
                    self.apply_project_format_edits(edits, ctx);
                    self.finish_format(trigger, None, ctx);
                } else {
                    self.finish_format(
                        trigger,
                        Some("The file changed while it was being formatted.".to_owned()),
                        ctx,
                    );
                }
            }
            FormatOutcome::Failed(failure) => {
                self.report_project_format_failure(trigger, file, &failure, ctx);
                self.finish_format(trigger, None, ctx);
            }
        }
    }

    /// Applies the formatter's edits as a single change, so one undo restores the previous text.
    fn apply_project_format_edits(&mut self, edits: Vec<TextEdit>, ctx: &mut ViewContext<Self>) {
        // Edits are applied one after another, so they go in from the end of the buffer backwards
        // to keep the offsets of those still to come valid. Buffer offsets start at 1.
        let edits: Vec<_> = edits
            .into_iter()
            .rev()
            .map(|edit| {
                let range =
                    CharOffset::from(edit.range.start + 1)..CharOffset::from(edit.range.end + 1);
                (edit.new_text, range)
            })
            .collect();
        if let Ok(edits) = Vec1::try_from_vec(edits) {
            self.editor
                .update(ctx, |editor, ctx| editor.apply_edits(edits, ctx));
        }
    }

    /// Tells the user that the project's formatter failed. An explicit request always says so,
    /// while a save only does the first time for each file, so a formatter that keeps failing does
    /// not interrupt every save.
    fn report_project_format_failure(
        &self,
        trigger: FormatTrigger,
        file: &Path,
        failure: &FormatFailure,
        ctx: &mut ViewContext<Self>,
    ) {
        log::warn!("Could not format {}: {failure}", file.display());
        let should_notify = match trigger {
            FormatTrigger::Save(_) => REPORTED_SAVE_FAILURES.record(file),
            FormatTrigger::Command => true,
        };
        if should_notify {
            self.show_format_notice(format!("Could not format with {failure}"), ctx);
        }
    }
}

/// The project `file` belongs to, as far as the app already knows. The formatter crate works out
/// the boundary itself when this is `None`.
fn project_root(file: &Path, ctx: &AppContext) -> Option<PathBuf> {
    let workspace_root = || {
        ctx.has_singleton_model::<PersistedWorkspace>()
            .then(|| PersistedWorkspace::as_ref(ctx).root_for_workspace(file))
            .flatten()
            .map(Path::to_path_buf)
    };
    let repository_root = || {
        ctx.has_singleton_model::<DetectedRepositories>()
            .then(|| {
                DetectedRepositories::as_ref(ctx)
                    .get_root_for_path(&LocalOrRemotePath::Local(file.to_path_buf()))
            })
            .flatten()
            .and_then(|root| PathBuf::try_from(root).ok())
    };
    workspace_root().or_else(repository_root)
}
