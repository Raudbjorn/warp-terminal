//! The replace side of global search: the replacement field, what each result would become, the
//! results set aside from replacing, and the replace itself.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pathfinder_geometry::vector::vec2f;
use project_replace::apply::FileTarget;
use project_replace::engine::SearchSpec;
use project_replace::preview::{Previewer, RowPreview, preview_row};
use project_replace::replacement::ReplaceSpec;
use project_replace::report::{
    DELETION_WARNING, ReplaceCounts, confirmation_title, describe_change, replace_all_label,
    replace_summary, undo_summary,
};
use project_replace::selection::{Dismissals, Selection, TargetsBuilder, count_targets};
use warp_core::ui::Icon;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::Fill as ThemeFill;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warpui::elements::{
    Border, ChildAnchor, ChildView, ConstrainedBox, Container, CornerRadius, CrossAxisAlignment,
    Flex, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding,
    ParentAnchor, ParentElement, ParentOffsetBounds, Radius, Shrinkable, Stack, Text,
};
use warpui::modals::{AlertDialogWithCallbacks, ModalButton};
use warpui::platform::Cursor;
use warpui::ui_components::components::{UiComponent as _, UiComponentStyles};
use warpui::ui_components::text::Span;
use warpui::{AppContext, Element, ModelHandle, SingletonEntity, ViewContext, ViewHandle};

use super::{GlobalSearchAction, GlobalSearchView, Match, MatchedPath, ROW_ICON_SIZE};
use crate::editor::{EditorView, Event as EditorEvent};
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon as UiIcon;
use crate::view_components::DismissibleToast;
use crate::view_components::action_button::{
    ActionButton, ButtonSize, DisabledSecondaryTheme, NakedTheme, SecondaryTheme,
};
use crate::workspace::view::global_search::replace_model::{
    ProjectReplace, ProjectReplaceEvent, ReplaceJob,
};
use crate::workspace::{ToastStack, WorkspaceAction};

/// Most files listed in the summary of a run. The rest are counted.
const MAX_SUMMARY_DETAILS: usize = 8;

/// What the last replace or undo did.
pub(super) struct RunSummary {
    headline: String,
    /// One line per file that was left alone, with the reason.
    details: Vec<String>,
    /// Whether the run was a replace that can be taken back.
    can_undo: bool,
}

/// The state of the replace side of a global search.
pub(super) struct ReplaceUi {
    /// Whether the replacement field shows.
    pub(super) is_open: bool,
    pub(super) toggle_mouse_state: MouseStateHandle,
    pub(super) editor: ViewHandle<EditorView>,
    pub(super) preserve_case_enabled: bool,
    pub(super) preserve_case_button: ViewHandle<ActionButton>,
    /// The results a replace leaves alone.
    pub(super) dismissals: Dismissals,
    /// Works out what each result would be replaced with. Present while the replacement field
    /// shows and holds something.
    pub(super) previewer: Option<Previewer>,
    pub(super) model: ModelHandle<ProjectReplace>,
    pub(super) replace_all_button: ViewHandle<ActionButton>,
    pub(super) cancel_button: ViewHandle<ActionButton>,
    /// Whether a replace can start: the results are complete and are those of the search the
    /// fields describe, and there is a replacement.
    pub(super) is_ready: bool,
    /// What replacing everything would cover, while a replace can start.
    pub(super) counts: ReplaceCounts,
    /// The file the search was in when it stopped at its limit of matches, which may hold more
    /// of them than are listed.
    pub(super) truncated_file: Option<PathBuf>,
    /// Whether the search ended early, so that files may be missing matches or missing
    /// altogether.
    pub(super) search_incomplete: bool,
    /// Where the results were scrolled to before a replace made them search again.
    pub(super) scroll_top_to_restore: Option<f32>,
    /// Set for the search that follows a replace, which looks for the same thing and so keeps
    /// what was set aside.
    pub(super) keep_dismissals_once: bool,
    /// Shown until dismissed or until the next run.
    pub(super) summary: Option<RunSummary>,
    pub(super) undo_button: ViewHandle<ActionButton>,
    /// The undo button of the message the workspace shows, whose click goes through the
    /// workspace back to this view.
    pub(super) undo_toast_button: ViewHandle<ActionButton>,
    pub(super) dismiss_summary_mouse_state: MouseStateHandle,
}

impl ReplaceUi {
    pub(super) fn new(ctx: &mut ViewContext<GlobalSearchView>) -> Self {
        let editor = GlobalSearchView::add_field_editor("Replace", ctx);
        ctx.subscribe_to_view(&editor, |me, _handle, event, ctx| {
            me.handle_replace_editor_event(event, ctx);
        });

        let preserve_case_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new_with_boxed_theme(String::new(), Arc::new(NakedTheme))
                .with_icon(UiIcon::PreserveCase)
                .with_tooltip("Toggle Preserve Case")
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::TogglePreserveCase);
                })
        });

        let replace_all_button = ctx.add_typed_action_view(|ctx| {
            let mut button = ActionButton::new("Replace All", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .with_disabled_theme(DisabledSecondaryTheme)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::ReplaceAll);
                });
            button.set_disabled(true, ctx);
            button
        });

        let cancel_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Cancel", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::CancelReplace);
                })
        });

        let undo_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Undo", SecondaryTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::UndoLastReplace);
                })
        });

        let undo_toast_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new("Undo", NakedTheme)
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(WorkspaceAction::UndoLastProjectReplace);
                })
        });

        let model = ctx.add_model(|_| ProjectReplace::new());
        ctx.subscribe_to_model(&model, |me, _handle, event, ctx| {
            me.handle_replace_event(event, ctx);
        });

        Self {
            is_open: false,
            toggle_mouse_state: MouseStateHandle::default(),
            editor,
            preserve_case_enabled: false,
            preserve_case_button,
            dismissals: Dismissals::default(),
            previewer: None,
            model,
            replace_all_button,
            cancel_button,
            is_ready: false,
            counts: ReplaceCounts::default(),
            truncated_file: None,
            search_incomplete: false,
            scroll_top_to_restore: None,
            keep_dismissals_once: false,
            summary: None,
            undo_button,
            undo_toast_button,
            dismiss_summary_mouse_state: MouseStateHandle::default(),
        }
    }
}

/// A small button inside a result row.
pub(super) struct RowButton {
    icon: Icon,
    tooltip: &'static str,
    mouse_state: MouseStateHandle,
    action: GlobalSearchAction,
}

impl GlobalSearchView {
    fn handle_replace_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Focused => self.on_field_editor_focused(ctx),
            EditorEvent::Edited(_)
            | EditorEvent::BufferReplaced
            | EditorEvent::BufferReinitialized => self.refresh_replace_preview(ctx),
            _ => {}
        }
    }

    fn handle_replace_event(&mut self, event: &ProjectReplaceEvent, ctx: &mut ViewContext<Self>) {
        match event {
            ProjectReplaceEvent::Progress => {}
            ProjectReplaceEvent::Finished(report) => {
                let headline = replace_summary(
                    report.counts(),
                    &report.skipped,
                    report.was_cancelled().then_some(report.not_attempted),
                );
                let details = report
                    .skipped
                    .iter()
                    .map(|skipped| self.summary_line(&skipped.path, &skipped.reason.describe()))
                    .collect();
                self.show_run_summary(
                    RunSummary {
                        headline,
                        details,
                        can_undo: report.can_undo,
                    },
                    ctx,
                );
                let changed: Vec<PathBuf> = report
                    .replaced
                    .iter()
                    .map(|replaced| replaced.path.clone())
                    .collect();
                self.search_again_after_run(&changed, ctx);
            }
            ProjectReplaceEvent::UndoFinished {
                description,
                report,
            } => {
                let headline = format!(
                    "Undo of {}: {}",
                    describe_change(&description.pattern, &description.replacement),
                    undo_summary(
                        report.restored.len(),
                        &report.refused,
                        report.was_cancelled().then_some(report.not_attempted),
                    )
                );
                let details = report
                    .refused
                    .iter()
                    .map(|refused| self.summary_line(&refused.path, &refused.reason.describe()))
                    .collect();
                self.show_run_summary(
                    RunSummary {
                        headline,
                        details,
                        can_undo: false,
                    },
                    ctx,
                );
                self.search_again_after_run(&report.restored, ctx);
            }
            ProjectReplaceEvent::NothingToUndo => {
                let toast = DismissibleToast::default("There is no replace to undo.".to_string());
                self.show_replace_toast(toast, ctx);
            }
            ProjectReplaceEvent::Failed(message) => {
                self.show_replace_toast(DismissibleToast::error(message.clone()), ctx);
            }
        }
        self.refresh_replace_controls(ctx);
    }

    /// A line of a run summary: the file, relative to the folder it was searched from, and what
    /// happened to it.
    fn summary_line(&self, path: &Path, outcome: &str) -> String {
        let relative = self
            .search_roots
            .iter()
            .filter_map(|root| root.to_local_path())
            .find_map(|root| path.strip_prefix(root).ok());
        format!("{}: {outcome}", relative.unwrap_or(path).display())
    }

    /// Shows what a run did, in the panel and as a message with the same undo button.
    fn show_run_summary(&mut self, summary: RunSummary, ctx: &mut ViewContext<Self>) {
        let mut toast = DismissibleToast::default(summary.headline.clone());
        if summary.can_undo {
            toast = toast.with_action_button(self.replace.undo_toast_button.clone());
        }
        self.show_replace_toast(toast, ctx);
        self.replace.summary = Some(summary);
    }

    /// The results on screen describe the files as they were before the run changed `changed`,
    /// so searches again, to come back to the same place in the list with the same results set
    /// aside.
    fn search_again_after_run(&mut self, changed: &[PathBuf], ctx: &mut ViewContext<Self>) {
        self.replace.dismissals.files_changed(changed);
        self.replace.keep_dismissals_once = true;
        self.replace.scroll_top_to_restore =
            Some(self.uniform_list_state.scroll_top().as_f64() as f32);
        self.rerun_search_from_query(ctx, true);
    }

    fn show_replace_toast(
        &self,
        toast: DismissibleToast<WorkspaceAction>,
        ctx: &mut ViewContext<Self>,
    ) {
        let window_id = ctx.window_id();
        let toast = toast.with_object_id(format!("project_replace_{}", ctx.view_id()));
        ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
            toast_stack.add_ephemeral_toast(toast, window_id, ctx);
        });
    }

    /// Takes back the last replace made from any global search, the way it was made: file by
    /// file, leaving alone what was edited since.
    pub fn undo_last_replace(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.model.update(ctx, |model, ctx| {
            model.undo(ctx);
        });
    }

    pub(super) fn dismiss_run_summary(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.summary = None;
        ctx.notify();
    }

    /// Shows the replacement field.
    pub fn show_replace(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.is_open = true;
        self.refresh_replace_preview(ctx);
    }

    pub(super) fn toggle_replace_open(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.is_open = !self.replace.is_open;
        self.refresh_replace_preview(ctx);
    }

    pub(super) fn toggle_preserve_case(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.preserve_case_enabled = !self.replace.preserve_case_enabled;
        let is_active = self.replace.preserve_case_enabled;
        self.replace
            .preserve_case_button
            .update(ctx, |button, ctx| {
                button.set_active(is_active, ctx);
            });
        self.refresh_replace_preview(ctx);
    }

    /// The text to replace results with, as typed.
    pub(super) fn replacement_text(&self, app: &AppContext) -> String {
        self.replace.editor.as_ref(app).buffer_text(app)
    }

    /// Brings what the results would be replaced with in line with the search that found them and
    /// with the replacement field.
    pub(super) fn refresh_replace_preview(&mut self, ctx: &mut ViewContext<Self>) {
        let replacement = self.replacement_text(ctx);
        self.replace.previewer = match &self.searched_spec {
            // An empty replacement is a replacement too: it deletes the matches.
            Some(search) if self.replace.is_open => Some(Previewer::new(
                search,
                &ReplaceSpec {
                    replacement,
                    preserve_case: self.replace.preserve_case_enabled,
                },
            )),
            Some(_) | None => None,
        };
        self.refresh_replace_controls(ctx);
    }

    /// Whether a replace can start right now.
    fn is_replace_ready(&self, app: &AppContext) -> bool {
        let Some(searched) = &self.searched_spec else {
            return false;
        };
        // Results that are still coming in, or that belong to a search other than the one the
        // fields describe, are not what the user is looking at.
        let described = SearchSpec {
            pattern: self.query_editor.as_ref(app).buffer_text(app),
            use_regex: self.regex_search_enabled,
            case_sensitive: self.case_sensitivity_enabled,
            whole_word: self.whole_word_enabled,
        };
        let scope_is_searched = self
            .last_searched_options
            .as_ref()
            .is_some_and(|options| options.scope == self.search_scope(app));

        self.replace.is_open
            && self.replace.previewer.is_some()
            && !self.is_search_in_progress
            && !self.replace.search_incomplete
            && self.last_error.is_none()
            && *searched == described
            && scope_is_searched
            && !self.replace.model.as_ref(app).is_running()
    }

    /// Brings the button that replaces everything in line with what there is to replace and with
    /// whether a replace can start.
    pub(super) fn refresh_replace_controls(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.is_ready = self.is_replace_ready(ctx);
        self.replace.counts = if self.replace.is_ready {
            count_targets(&self.replace_targets(Selection::All))
        } else {
            ReplaceCounts::default()
        };

        let counts = self.replace.counts;
        let label = if counts.is_empty() {
            "Replace All".to_string()
        } else {
            replace_all_label(counts)
        };
        self.replace.replace_all_button.update(ctx, |button, ctx| {
            button.set_label(label, ctx);
            button.set_disabled(counts.is_empty(), ctx);
        });
        ctx.notify();
    }

    /// The files a replace of `selection` goes through, each with every match the search found
    /// in it.
    fn replace_targets(&self, selection: Selection<'_>) -> Vec<FileTarget> {
        let mut builder = TargetsBuilder::new(&self.replace.dismissals, selection);
        for directory in &self.directory_entries {
            for matched_path in &directory.matched_paths.paths {
                for matched in &matched_path.matches {
                    let (Some((path, column)), Some(submatch)) = (
                        Self::replaceable_key(matched_path, matched),
                        matched.submatches.first(),
                    ) else {
                        continue;
                    };
                    builder.add_row(
                        path,
                        matched.line_number,
                        column,
                        &matched.line_text,
                        submatch.byte_start.as_usize()..submatch.byte_end.as_usize(),
                    );
                }
            }
        }
        if let Some(path) = &self.replace.truncated_file {
            builder.mark_truncated(path);
        }
        builder.build()
    }

    /// Asks before replacing everything that was not set aside.
    pub(super) fn confirm_replace_all(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.is_replace_ready(ctx) {
            return;
        }
        let Some(search) = &self.searched_spec else {
            return;
        };
        let files = self.replace_targets(Selection::All);
        let counts = count_targets(&files);
        if counts.is_empty() {
            return;
        }

        let replacement = self.replacement_text(ctx);
        let title = confirmation_title(counts, &search.pattern, &replacement);
        let caveats = "Files that changed since the search are left alone. The replace can be \
                       taken back with Undo Last Replace.";
        let (details, confirm_label) = if replacement.is_empty() {
            (format!("{DELETION_WARNING} {caveats}"), "Delete")
        } else {
            (caveats.to_string(), "Replace")
        };
        ctx.show_native_platform_modal(
            AlertDialogWithCallbacks::for_view(
                title,
                details,
                vec![
                    // The first button is the one Return presses.
                    ModalButton::for_view("Cancel", |_: &mut Self, _| {}),
                    ModalButton::for_view(confirm_label, move |me: &mut Self, ctx| {
                        me.start_replace(files, ctx);
                    }),
                ],
                |_, _| {},
            )
            .asked_every_time(),
        );
    }

    pub(super) fn replace_in_file(
        &mut self,
        location: &LocalOrRemotePath,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = location.to_local_path() else {
            return;
        };
        let files = self.replace_targets(Selection::File(path));
        self.start_replace(files, ctx);
    }

    pub(super) fn replace_match(
        &mut self,
        location: &LocalOrRemotePath,
        line_number: u32,
        column: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(path) = location.to_local_path() else {
            return;
        };
        let files = self.replace_targets(Selection::Match {
            path,
            line: line_number,
            column,
        });
        self.start_replace(files, ctx);
    }

    fn start_replace(&mut self, files: Vec<FileTarget>, ctx: &mut ViewContext<Self>) {
        if files.is_empty() || !self.is_replace_ready(ctx) {
            return;
        }
        let (Some(search), Some(options)) =
            (self.searched_spec.clone(), &self.last_searched_options)
        else {
            return;
        };
        let job = ReplaceJob {
            search,
            replace: ReplaceSpec {
                replacement: self.replacement_text(ctx),
                preserve_case: self.replace.preserve_case_enabled,
            },
            roots: self
                .search_roots
                .iter()
                .filter_map(|root| root.to_local_path().map(Path::to_path_buf))
                .collect(),
            scope: options.scope.clone(),
            files,
        };
        self.replace.summary = None;
        self.replace.model.update(ctx, |model, ctx| {
            model.start(job, ctx);
        });
    }

    pub(super) fn cancel_replace(&mut self, ctx: &mut ViewContext<Self>) {
        self.replace.model.update(ctx, |model, ctx| {
            model.cancel(ctx);
        });
    }

    pub(super) fn toggle_match_dismissed(
        &mut self,
        location: &LocalOrRemotePath,
        line_number: u32,
        column: usize,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(path) = location.to_local_path() {
            self.replace
                .dismissals
                .toggle_match(path, line_number, column);
            self.refresh_replace_controls(ctx);
        }
    }

    pub(super) fn toggle_file_dismissed(
        &mut self,
        location: &LocalOrRemotePath,
        ctx: &mut ViewContext<Self>,
    ) {
        if let Some(path) = location.to_local_path() {
            self.replace.dismissals.toggle_file(path);
            self.refresh_replace_controls(ctx);
        }
    }

    /// The file and column that, with its line, identify a result for replacing. `None` for a
    /// result that cannot be replaced: one on another host, or one the search could not place in
    /// its line.
    pub(super) fn replaceable_key<'a>(
        matched_path: &'a MatchedPath,
        matched: &Match,
    ) -> Option<(&'a Path, usize)> {
        Some((matched_path.path.to_local_path()?, matched.column_num?))
    }

    pub(super) fn is_file_dismissed(&self, matched_path: &MatchedPath) -> bool {
        matched_path
            .path
            .to_local_path()
            .is_some_and(|path| self.replace.dismissals.is_file_dismissed(path))
    }

    pub(super) fn is_match_dismissed(&self, matched_path: &MatchedPath, matched: &Match) -> bool {
        Self::replaceable_key(matched_path, matched).is_some_and(|(path, column)| {
            self.replace
                .dismissals
                .is_match_dismissed(path, matched.line_number, column)
        })
    }

    /// What the result would look like once replaced, while there is a replacement to show for
    /// it.
    pub(super) fn replace_preview(
        &self,
        matched_path: &MatchedPath,
        matched: &Match,
    ) -> Option<RowPreview> {
        let previewer = self.replace.previewer.as_ref()?;
        Self::replaceable_key(matched_path, matched)?;
        if self.is_match_dismissed(matched_path, matched) {
            return None;
        }
        let submatch = matched.submatches.first()?;
        let range = submatch.byte_start.as_usize()..submatch.byte_end.as_usize();
        let replacement = previewer.replacement_for(&matched.line_text, range.clone());
        Some(preview_row(&matched.line_text, range, &replacement))
    }

    /// The buttons a text match shows while the replacement field does.
    pub(super) fn match_row_buttons(
        &self,
        matched_path: &MatchedPath,
        matched: &Match,
    ) -> Vec<RowButton> {
        let Some((_, column)) = Self::replaceable_key(matched_path, matched) else {
            return Vec::new();
        };
        if !self.replace.is_open {
            return Vec::new();
        }

        let location = &matched_path.path;
        let line_number = matched.line_number;
        let is_dismissed = self.is_match_dismissed(matched_path, matched);
        let mut buttons = Vec::new();
        if self.replace.is_ready && !is_dismissed {
            buttons.push(RowButton {
                icon: Icon::Check,
                tooltip: "Replace",
                mouse_state: matched.replace_mouse_state.clone(),
                action: GlobalSearchAction::ReplaceMatch {
                    location: location.clone(),
                    line_number,
                    column,
                },
            });
        }
        buttons.push(RowButton {
            icon: if is_dismissed { Icon::Plus } else { Icon::X },
            tooltip: if is_dismissed {
                "Include in Replace"
            } else {
                "Leave out of Replace"
            },
            mouse_state: matched.dismiss_mouse_state.clone(),
            action: GlobalSearchAction::ToggleMatchDismissed {
                location: location.clone(),
                line_number,
                column,
            },
        });
        buttons
    }

    /// The buttons the header of a file shows while the replacement field does.
    pub(super) fn file_row_buttons(&self, matched_path: &MatchedPath) -> Vec<RowButton> {
        if !self.replace.is_open || !matched_path.path.is_local() {
            return Vec::new();
        }

        let location = &matched_path.path;
        let is_dismissed = self.is_file_dismissed(matched_path);
        let mut buttons = Vec::new();
        if self.replace.is_ready && !is_dismissed {
            buttons.push(RowButton {
                icon: Icon::Check,
                tooltip: "Replace All in File",
                mouse_state: matched_path.replace_mouse_state.clone(),
                action: GlobalSearchAction::ReplaceInFile {
                    location: location.clone(),
                },
            });
        }
        buttons.push(RowButton {
            icon: if is_dismissed { Icon::Plus } else { Icon::X },
            tooltip: if is_dismissed {
                "Include in Replace"
            } else {
                "Leave out of Replace"
            },
            mouse_state: matched_path.dismiss_mouse_state.clone(),
            action: GlobalSearchAction::ToggleFileDismissed {
                location: location.clone(),
            },
        });
        buttons
    }

    /// Renders the chevron that shows and hides the replacement field.
    pub(super) fn render_replace_toggle(&self, appearance: &Appearance) -> Box<dyn Element> {
        let is_open = self.replace.is_open;
        let theme = appearance.theme();
        Hoverable::new(self.replace.toggle_mouse_state.clone(), move |state| {
            let color = if state.is_hovered() {
                theme.foreground().into()
            } else {
                blended_colors::text_sub(theme, theme.background())
            };
            let icon = if is_open {
                Icon::ChevronDown
            } else {
                Icon::ChevronRight
            };
            Container::new(
                ConstrainedBox::new(icon.to_warpui_icon(ThemeFill::Solid(color)).finish())
                    .with_width(ROW_ICON_SIZE)
                    .with_height(ROW_ICON_SIZE)
                    .finish(),
            )
            .with_margin_right(4.)
            .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::ToggleReplaceOpen);
        })
        .finish()
    }

    pub(super) fn render_replace_row(&self, app: &AppContext) -> Box<dyn Element> {
        let preserve_case_button =
            Container::new(ChildView::new(&self.replace.preserve_case_button).finish()).finish();
        self.render_field_row(&self.replace.editor, Some(preserve_case_button), app)
    }

    /// Renders the button that replaces everything, or how far a running replace got and the
    /// button that stops it.
    pub(super) fn render_replace_actions(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let model = self.replace.model.as_ref(app);

        let hint = |text: String| {
            Text::new_inline(text, appearance.ui_font_family(), 12.)
                .with_color(blended_colors::text_sub(theme, theme.background()))
                .finish()
        };
        let row = if model.is_running() {
            let activity = if model.is_undoing() {
                "Undoing"
            } else {
                "Replacing"
            };
            let progress = match model.progress() {
                Some(progress) => {
                    format!("{activity}… {} of {} files", progress.done, progress.total)
                }
                None => format!("{activity}…"),
            };
            Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(ChildView::new(&self.replace.cancel_button).finish())
                .with_child(
                    Shrinkable::new(
                        1.0,
                        Container::new(hint(progress)).with_margin_left(8.).finish(),
                    )
                    .finish(),
                )
        } else {
            let mut row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(ChildView::new(&self.replace.replace_all_button).finish());
            if let Some(text) = self.replace_hint() {
                row.add_child(
                    Shrinkable::new(
                        1.0,
                        Container::new(hint(text.to_string()))
                            .with_margin_left(8.)
                            .finish(),
                    )
                    .finish(),
                );
            }
            row
        };
        Container::new(row.finish()).with_margin_bottom(4.).finish()
    }

    /// Whether a replace or an undo is under way, which shows even with the replacement field
    /// hidden.
    pub(super) fn is_replace_running(&self, app: &AppContext) -> bool {
        self.replace.model.as_ref(app).is_running()
    }

    /// Renders what the last replace or undo did, with the files it left alone.
    pub(super) fn render_run_summary(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        let summary = self.replace.summary.as_ref()?;
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let text = |text: String, color| {
            Span::new(
                text,
                UiComponentStyles {
                    font_family_id: Some(appearance.ui_font_family()),
                    font_size: Some(12.),
                    font_color: Some(color),
                    ..Default::default()
                },
            )
            .with_soft_wrap()
            .build()
            .finish()
        };
        let main_color = blended_colors::text_main(theme, theme.background());
        let sub_color = blended_colors::text_sub(theme, theme.background());

        let dismiss = Self::render_row_button(
            RowButton {
                icon: Icon::X,
                tooltip: "Dismiss",
                mouse_state: self.replace.dismiss_summary_mouse_state.clone(),
                action: GlobalSearchAction::DismissReplaceSummary,
            },
            appearance,
        );
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Start)
                    .with_child(
                        Shrinkable::new(1.0, text(summary.headline.clone(), main_color)).finish(),
                    )
                    .with_child(dismiss)
                    .finish(),
            );
        for detail in summary.details.iter().take(MAX_SUMMARY_DETAILS) {
            column.add_child(text(detail.clone(), sub_color));
        }
        let hidden_details = summary.details.len().saturating_sub(MAX_SUMMARY_DETAILS);
        if hidden_details > 0 {
            column.add_child(text(format!("…and {hidden_details} more"), sub_color));
        }
        if summary.can_undo {
            column.add_child(
                Container::new(ChildView::new(&self.replace.undo_button).finish())
                    .with_margin_top(4.)
                    .finish(),
            );
        }

        Some(
            Container::new(column.finish())
                .with_padding(Padding::uniform(6.))
                .with_border(Border::all(1.).with_border_fill(theme.surface_3()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(6.)))
                .with_margin_bottom(4.)
                .finish(),
        )
    }

    /// Why nothing can be replaced right now, when the reason is not plain from the results.
    fn replace_hint(&self) -> Option<&'static str> {
        if self.replace.is_ready {
            return None;
        }
        if self.replace.search_incomplete {
            Some("The search ended early. Search again to replace.")
        } else if self.is_search_in_progress {
            Some("Waiting for the search to finish.")
        } else {
            None
        }
    }

    /// Renders the buttons of a result row, in order, to go at the end of the row.
    pub(super) fn render_row_buttons(
        buttons: Vec<RowButton>,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut row = Flex::row().with_cross_axis_alignment(CrossAxisAlignment::Center);
        for button in buttons {
            row.add_child(Self::render_row_button(button, appearance));
        }
        row.finish()
    }

    fn render_row_button(button: RowButton, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let RowButton {
            icon,
            tooltip,
            mouse_state,
            action,
        } = button;
        Hoverable::new(mouse_state, move |state| {
            let color = if state.is_hovered() {
                theme.foreground().into()
            } else {
                blended_colors::text_sub(theme, theme.background())
            };
            let icon = Container::new(
                ConstrainedBox::new(icon.to_warpui_icon(ThemeFill::Solid(color)).finish())
                    .with_width(ROW_ICON_SIZE)
                    .with_height(ROW_ICON_SIZE)
                    .finish(),
            )
            .with_margin_left(6.)
            .finish();
            if !state.is_hovered() {
                return icon;
            }

            let tooltip = appearance
                .ui_builder()
                .tool_tip(tooltip.to_string())
                .build()
                .finish();
            let mut stack = Stack::new().with_child(icon);
            stack.add_positioned_overlay_child(
                tooltip,
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 4.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomMiddle,
                    ChildAnchor::TopLeft,
                ),
            );
            stack.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .finish()
    }
}
