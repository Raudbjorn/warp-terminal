//! Source-control overview of every repository nested under a folder that is not itself a
//! repository: a list of repositories and their changed files, the diff of one selected file
//! beside it, and each repository's git actions.

use std::collections::HashMap;
use std::future::Future;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use pathfinder_color::ColorU;
use pathfinder_geometry::vector::vec2f;
use warp_core::ui::theme::color::internal_colors;
use warpui::r#async::Timer;
use warpui::elements::{
    Align, ChildAnchor, ChildView, Clipped, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DragBarSide, Element, Empty, Fill, Flex, Hoverable, MainAxisAlignment,
    MainAxisSize, MouseStateHandle, OffsetPositioning, ParentAnchor, ParentElement,
    ParentOffsetBounds, Radius, Rect, Resizable, ResizableStateHandle, ScrollStateHandle,
    Scrollable, ScrollableElement, ScrollbarWidth, Shrinkable, SizeConstraintCondition,
    SizeConstraintSwitch, Stack, Text, UniformList, UniformListState, resizable_state_handle,
};
use warpui::fonts::{Properties, Weight};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::{
    AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle, keymap,
};

use super::code_review_view::{CONTENT_LEFT_MARGIN, CONTENT_RIGHT_MARGIN};
use super::diff_state::{DiffStateModel, GitFileStatus};
use super::git_actions::{self, SyncOutcome};
use super::git_dialog::{GitDialog, GitDialogEvent, user_facing_git_error};
use super::nested_repo_actions::{
    MEDIUM_ROW_MIN_WIDTH, RepoAction, RowMessage, RowWidth, WIDE_ROW_MIN_WIDTH, applicable_actions,
    layout_actions, operation_in_progress_message, sync_message,
};
use super::nested_repo_file_diff::{NestedRepoFileDiffEvent, NestedRepoFileDiffView};
use super::nested_repos::{
    NestedRepoEntry, NestedRepoFile, NestedRepoKind, NestedRepoStatus, NestedReposEvent,
    NestedReposModel,
};
use crate::appearance::Appearance;
use crate::code::buffer_location::LocalOrRemotePath;
use crate::code::editor::{add_color, remove_color, replace_color};
use crate::menu::{Event as MenuEvent, Menu, MenuItem, MenuItemFields};
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button_with_color;
use crate::ui_components::dialog::{Dialog, dialog_styles};
use crate::ui_components::icons::Icon;
use crate::ui_components::item_highlight::ItemHighlightState;
use crate::util::git::{
    self, Commit, PrInfo, detect_main_branch, get_pr_for_branch, get_unpushed_commits,
    git_operation_in_progress,
};
use crate::view_components::DismissibleToast;
use crate::view_components::action_button::{ActionButton, DangerPrimaryTheme, NakedTheme};
use crate::workspace::ToastStack;

/// Every row shares one height so the list can be virtualised.
const ROW_HEIGHT: f32 = 26.;
const ROW_HORIZONTAL_PADDING: f32 = 8.;
const LIST_HORIZONTAL_PADDING: f32 = 8.;
const CHEVRON_SIZE: f32 = 16.;
const TRACKING_ICON_SIZE: f32 = 12.;
const FILE_ROW_INDENT: f32 = 20.;
const STATUS_LETTER_WIDTH: f32 = 18.;
const REPO_NAME_MAX_WIDTH: f32 = 320.;
const FILE_NAME_MAX_WIDTH: f32 = 240.;
const SECONDARY_FONT_SIZE_RATIO: f32 = 0.9;
const OVERFLOW_MENU_WIDTH: f32 = 170.;

/// Repositories with more changed files than this start collapsed, so that one busy repository
/// does not push every other one out of view.
const MAX_FILES_EXPANDED_BY_DEFAULT: usize = 30;

const DEFAULT_LIST_COLUMN_WIDTH: f32 = 300.;
const MIN_LIST_COLUMN_WIDTH: f32 = 200.;
const MAX_LIST_COLUMN_WIDTH: f32 = 520.;

/// Below this width a diff beside the list would be too narrow to read, so it goes under the
/// list instead.
pub const MIN_TWO_COLUMN_WIDTH: f32 = 520.;

/// Width at which the list and a diff fit side by side comfortably.
pub const COMFORTABLE_TWO_COLUMN_WIDTH: f32 = 780.;

/// How long a confirmation must have been on screen before it accepts a confirming click, so
/// that the second click of a double-click on a row button cannot land on it.
const DISCARD_CONFIRMATION_ARMING_DELAY: Duration = Duration::from_millis(600);

const INFO_MESSAGE_LIFETIME: Duration = Duration::from_secs(8);
const ERROR_MESSAGE_LIFETIME: Duration = Duration::from_secs(30);
const GIT_OPERATION_TIMEOUT: Duration = Duration::from_secs(120);

async fn git_operation_with_timeout<T>(
    operation: impl Future<Output = anyhow::Result<T>>,
    timeout: Duration,
) -> anyhow::Result<T> {
    match futures::future::select(
        Box::pin(operation),
        Box::pin(async_io::Timer::after(timeout)),
    )
    .await
    {
        futures::future::Either::Left((result, _)) => result,
        futures::future::Either::Right(_) => anyhow::bail!(
            "Git operation timed out after {} seconds; check repository status before retrying.",
            timeout.as_secs()
        ),
    }
}

const DISCARD_DIALOG_CONTEXT: &str = "NestedReposDiscardDialog";

pub fn init(app: &mut AppContext) {
    app.register_fixed_bindings(vec![FixedBinding::new(
        "escape",
        NestedReposViewAction::CancelDiscard,
        warpui::id!(DISCARD_DIALOG_CONTEXT),
    )]);
}

#[derive(Clone, Debug, PartialEq)]
pub enum NestedReposViewAction {
    ToggleRepo(PathBuf),
    SelectFile {
        repo_root: PathBuf,
        path: String,
    },
    Refresh,
    RunRepoAction {
        repo_root: PathBuf,
        action: RepoAction,
    },
    ToggleOverflowMenu {
        repo_root: PathBuf,
        /// Width tier of the row the menu was opened from, which decides what is in it.
        row_width: RowWidth,
    },
    ConfirmDiscard,
    CancelDiscard,
}

#[derive(Clone, Debug)]
pub enum NestedReposViewEvent {
    /// A file's diff was opened beside a list that showed none before.
    FileDiffOpened,
    /// Open a file in an editor pane.
    OpenFile { path: PathBuf },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OverviewRow {
    Repo {
        repo_index: usize,
    },
    File {
        repo_index: usize,
        file_index: usize,
    },
}

/// Lays repositories out as one flat list of rows, given each repository's changed-file count and
/// whether it is expanded.
fn flatten_rows(repos: impl IntoIterator<Item = (usize, bool)>) -> Vec<OverviewRow> {
    let mut rows = Vec::new();
    for (repo_index, (file_count, is_expanded)) in repos.into_iter().enumerate() {
        rows.push(OverviewRow::Repo { repo_index });
        if is_expanded {
            rows.extend((0..file_count).map(|file_index| OverviewRow::File {
                repo_index,
                file_index,
            }));
        }
    }
    rows
}

fn is_expanded_by_default(changed_file_count: usize) -> bool {
    changed_file_count <= MAX_FILES_EXPANDED_BY_DEFAULT
}

pub(super) fn status_letter(status: &GitFileStatus) -> &'static str {
    match status {
        GitFileStatus::New => "A",
        GitFileStatus::Modified => "M",
        GitFileStatus::Deleted => "D",
        GitFileStatus::Renamed { .. } => "R",
        GitFileStatus::Copied { .. } => "C",
        GitFileStatus::Untracked => "U",
        GitFileStatus::Conflicted => "!",
    }
}

pub(super) fn status_color(status: &GitFileStatus, appearance: &Appearance) -> ColorU {
    match status {
        GitFileStatus::New | GitFileStatus::Copied { .. } | GitFileStatus::Untracked => {
            add_color(appearance)
        }
        GitFileStatus::Modified | GitFileStatus::Renamed { .. } => replace_color(appearance),
        GitFileStatus::Deleted | GitFileStatus::Conflicted => remove_color(appearance),
    }
}

fn changed_file_count(entry: &NestedRepoEntry) -> usize {
    entry.status.as_ref().map_or(0, |status| status.files.len())
}

fn change_count_label(count: usize) -> String {
    if count == 1 {
        "1 change".to_owned()
    } else {
        format!("{count} changes")
    }
}

/// Whether `branch` is the repository's main branch, which no pull request is opened from.
fn is_main_branch(branch: &str, main_branch: &str) -> bool {
    main_branch.strip_prefix("origin/").unwrap_or(main_branch) == branch
}

/// A file whose diff is shown beside the list.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SelectedFile {
    repo_root: PathBuf,
    /// Path relative to the repository root.
    path: String,
}

/// What a repository's row reports in place of its branch.
enum RepoActivity {
    Running(&'static str),
    Finished {
        message: RowMessage,
        /// Identifies this message to the timer that clears it.
        id: usize,
    },
}

struct RepoMouseStates {
    header: MouseStateHandle,
    actions: HashMap<RepoAction, MouseStateHandle>,
    overflow_button: MouseStateHandle,
    /// One handle per changed file, in the order the files are listed.
    files: Vec<MouseStateHandle>,
}

impl Default for RepoMouseStates {
    fn default() -> Self {
        Self {
            header: Default::default(),
            actions: [
                RepoAction::Commit,
                RepoAction::Push,
                RepoAction::Publish,
                RepoAction::Sync,
                RepoAction::Discard,
                RepoAction::PullRequest,
                RepoAction::Fetch,
            ]
            .into_iter()
            .map(|action| (action, MouseStateHandle::default()))
            .collect(),
            overflow_button: Default::default(),
            files: Vec::new(),
        }
    }
}

struct DiscardConfirmation {
    repo_root: PathBuf,
    opened_at: Instant,
}

/// Which git dialog to open once the data it shows has been read.
enum DialogRequest {
    Push { publish: bool, commits: Vec<Commit> },
    CreatePr { base_branch: Option<String> },
}

pub struct NestedReposView {
    model: ModelHandle<NestedReposModel>,
    view_handle: WeakViewHandle<Self>,
    rows: Vec<OverviewRow>,
    /// Whether each repository is expanded, keyed by its root. Seeded from the default when a
    /// repository's status first loads, and changed only by the user after that.
    expanded: HashMap<PathBuf, bool>,
    mouse_states: HashMap<PathBuf, RepoMouseStates>,
    refresh_button_mouse_state: MouseStateHandle,
    list_state: UniformListState,
    scroll_state: ScrollStateHandle,

    selected_file: Option<SelectedFile>,
    file_diff: ViewHandle<NestedRepoFileDiffView>,
    list_column_state: ResizableStateHandle,

    activities: HashMap<PathBuf, RepoActivity>,
    next_message_id: usize,
    overflow_menu: ViewHandle<Menu<NestedReposViewAction>>,
    overflow_menu_repo: Option<PathBuf>,
    git_dialog: Option<ViewHandle<GitDialog>>,
    discard_confirmation: Option<DiscardConfirmation>,
    discard_confirm_button: ViewHandle<ActionButton>,
    discard_cancel_button: ViewHandle<ActionButton>,
}

impl NestedReposView {
    pub fn new(model: ModelHandle<NestedReposModel>, ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&model, |me, _, event, ctx| match event {
            NestedReposEvent::Updated => me.sync_with_model(ctx),
        });

        let file_diff = ctx.add_typed_action_view(NestedRepoFileDiffView::new);
        ctx.subscribe_to_view(&file_diff, |me, _, event, ctx| match event {
            NestedRepoFileDiffEvent::Close => me.close_file_diff(ctx),
            NestedRepoFileDiffEvent::OpenFile { path } => {
                ctx.emit(NestedReposViewEvent::OpenFile { path: path.clone() });
            }
        });

        let overflow_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_drop_shadow()
                .with_width(OVERFLOW_MENU_WIDTH)
        });
        ctx.subscribe_to_view(&overflow_menu, |me, _, event, ctx| match event {
            MenuEvent::ItemSelected | MenuEvent::Close { .. } => {
                me.overflow_menu_repo = None;
                ctx.notify();
            }
            MenuEvent::ItemHovered => {}
        });

        let discard_confirm_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Discard changes", DangerPrimaryTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(NestedReposViewAction::ConfirmDiscard);
            })
        });
        let discard_cancel_button = ctx.add_typed_action_view(|_| {
            ActionButton::new("Cancel", NakedTheme).on_click(|ctx| {
                ctx.dispatch_typed_action(NestedReposViewAction::CancelDiscard);
            })
        });

        let mut view = Self {
            model,
            view_handle: ctx.handle(),
            rows: Vec::new(),
            expanded: HashMap::new(),
            mouse_states: HashMap::new(),
            refresh_button_mouse_state: Default::default(),
            list_state: UniformListState::new(),
            scroll_state: ScrollStateHandle::default(),
            selected_file: None,
            file_diff,
            list_column_state: resizable_state_handle(DEFAULT_LIST_COLUMN_WIDTH),
            activities: HashMap::new(),
            next_message_id: 0,
            overflow_menu,
            overflow_menu_repo: None,
            git_dialog: None,
            discard_confirmation: None,
            discard_confirm_button,
            discard_cancel_button,
        };
        view.sync_with_model(ctx);
        view
    }

    fn is_expanded(&self, repo_root: &Path) -> bool {
        self.expanded.get(repo_root).copied().unwrap_or(true)
    }

    fn entry<'a>(&self, repo_root: &Path, app: &'a AppContext) -> Option<&'a NestedRepoEntry> {
        self.model
            .as_ref(app)
            .entries()
            .iter()
            .find(|entry| entry.repo.root == repo_root)
    }

    fn status<'a>(&self, repo_root: &Path, app: &'a AppContext) -> Option<&'a NestedRepoStatus> {
        self.entry(repo_root, app)?.status.as_ref()
    }

    fn sync_with_model(&mut self, ctx: &mut ViewContext<Self>) {
        let model = self.model.clone();
        let entries = model.as_ref(ctx).entries();

        self.mouse_states
            .retain(|root, _| entries.iter().any(|entry| &entry.repo.root == root));
        for entry in entries {
            let file_count = changed_file_count(entry);
            if entry.status.is_some() {
                self.expanded
                    .entry(entry.repo.root.clone())
                    .or_insert_with(|| is_expanded_by_default(file_count));
            }
            self.mouse_states
                .entry(entry.repo.root.clone())
                .or_default()
                .files
                .resize_with(file_count, Default::default);
        }
        self.rows = flatten_rows(entries.iter().map(|entry| {
            (
                changed_file_count(entry),
                self.is_expanded(&entry.repo.root),
            )
        }));

        if self
            .discard_confirmation
            .as_ref()
            .is_some_and(|confirmation| {
                !entries
                    .iter()
                    .any(|entry| entry.repo.root == confirmation.repo_root)
            })
        {
            self.discard_confirmation = None;
        }
        self.sync_file_diff(ctx);
        ctx.notify();
    }

    /// Keeps the diff beside the list in step with the selected file's current status, and
    /// closes it when the file is no longer changed.
    fn sync_file_diff(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(selected) = self.selected_file.clone() else {
            return;
        };
        let current = self.status(&selected.repo_root, ctx).and_then(|status| {
            let file = status
                .files
                .iter()
                .find(|file| file.path == selected.path)?;
            Some((file.clone(), status.head_oid.clone()))
        });
        match current {
            Some((file, head_oid)) => self.file_diff.update(ctx, |file_diff, ctx| {
                file_diff.show(selected.repo_root, file, head_oid, ctx);
            }),
            None => self.close_file_diff(ctx),
        }
    }

    fn close_file_diff(&mut self, ctx: &mut ViewContext<Self>) {
        self.selected_file = None;
        self.file_diff
            .update(ctx, |file_diff, ctx| file_diff.clear(ctx));
        ctx.notify();
    }

    fn select_file(&mut self, repo_root: &Path, path: &str, ctx: &mut ViewContext<Self>) {
        let had_selection = self.selected_file.is_some();
        self.selected_file = Some(SelectedFile {
            repo_root: repo_root.to_path_buf(),
            path: path.to_owned(),
        });
        self.sync_file_diff(ctx);
        if !had_selection && self.selected_file.is_some() {
            ctx.emit(NestedReposViewEvent::FileDiffOpened);
        }
        ctx.notify();
    }

    fn interactive_path(ctx: &mut ViewContext<Self>) -> BoxFuture<'static, Option<String>> {
        #[cfg(feature = "local_tty")]
        {
            use crate::terminal::local_shell::LocalShellState;
            LocalShellState::handle(ctx).update(ctx, |shell_state, ctx| {
                shell_state.get_interactive_path_env_var(ctx)
            })
        }
        #[cfg(not(feature = "local_tty"))]
        {
            use futures::FutureExt;
            let _ = ctx;
            futures::future::ready(None).boxed()
        }
    }

    /// Marks `repo_root` as busy with an operation. Returns `false`, having reported why on the
    /// row, when one is already running there or the repository is mid-merge or mid-rebase.
    fn start_activity(
        &mut self,
        repo_root: &Path,
        label: &'static str,
        ctx: &mut ViewContext<Self>,
    ) -> bool {
        if matches!(
            self.activities.get(repo_root),
            Some(RepoActivity::Running(_))
        ) {
            return false;
        }
        if git_operation_in_progress(repo_root) {
            self.finish_activity(repo_root, operation_in_progress_message(), ctx);
            return false;
        }
        self.activities
            .insert(repo_root.to_path_buf(), RepoActivity::Running(label));
        ctx.notify();
        true
    }

    fn clear_activity(&mut self, repo_root: &Path, ctx: &mut ViewContext<Self>) {
        self.activities.remove(repo_root);
        ctx.notify();
    }

    /// Reports the result of an operation on the repository's row, and reloads its status.
    fn finish_activity(
        &mut self,
        repo_root: &Path,
        message: RowMessage,
        ctx: &mut ViewContext<Self>,
    ) {
        let id = self.next_message_id;
        self.next_message_id += 1;
        let lifetime = if message.is_error {
            // The row can clip a long message, so failures are also shown in full.
            let window_id = ctx.window_id();
            let toast = DismissibleToast::error(message.text.clone());
            ToastStack::handle(ctx).update(ctx, |toast_stack, ctx| {
                toast_stack.add_ephemeral_toast(toast, window_id, ctx);
            });
            ERROR_MESSAGE_LIFETIME
        } else {
            INFO_MESSAGE_LIFETIME
        };
        self.activities.insert(
            repo_root.to_path_buf(),
            RepoActivity::Finished { message, id },
        );

        let expired_root = repo_root.to_path_buf();
        ctx.spawn(
            async move { Timer::after(lifetime).await },
            move |me, _, ctx| {
                let is_current = matches!(
                    me.activities.get(&expired_root),
                    Some(RepoActivity::Finished { id: current, .. }) if *current == id
                );
                if is_current {
                    me.clear_activity(&expired_root, ctx);
                }
            },
        );

        self.model
            .update(ctx, |model, ctx| model.reload_status(repo_root, ctx));
        ctx.notify();
    }

    fn run_repo_action(
        &mut self,
        repo_root: &Path,
        action: RepoAction,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(status) = self.status(repo_root, ctx).cloned() else {
            return;
        };
        // An action can be dispatched from a menu that was built before the status changed.
        if !applicable_actions(&status).contains(&action) {
            return;
        }
        match action {
            RepoAction::Commit => self.open_commit_dialog(repo_root, &status, ctx),
            RepoAction::Push => self.load_push_dialog(repo_root, &status, false, ctx),
            RepoAction::Publish => self.load_push_dialog(repo_root, &status, true, ctx),
            RepoAction::Sync => self.sync(repo_root, &status, ctx),
            RepoAction::Fetch => self.fetch(repo_root, ctx),
            RepoAction::PullRequest => self.open_pull_request(repo_root, &status, ctx),
            RepoAction::Discard => self.confirm_discard(repo_root, &status, ctx),
        }
    }

    fn sync(&mut self, repo_root: &Path, status: &NestedRepoStatus, ctx: &mut ViewContext<Self>) {
        if !self.start_activity(repo_root, "Syncing…", ctx) {
            return;
        }
        let path_env = Self::interactive_path(ctx);
        let repo = repo_root.to_path_buf();
        let branch = status.branch.clone();
        let upstream = status.upstream.clone();
        ctx.spawn(
            async move {
                let result = git_operation_with_timeout(
                    async {
                        let path_env = path_env.await;
                        git_actions::run_sync(&repo, &branch, path_env.as_deref()).await
                    },
                    GIT_OPERATION_TIMEOUT,
                )
                .await;
                (repo, result)
            },
            move |me, (repo, result): (PathBuf, anyhow::Result<SyncOutcome>), ctx| {
                let message = match result {
                    Ok(outcome) => sync_message(outcome, upstream.as_deref()),
                    Err(err) => Self::git_error_message("Sync", &repo, &err),
                };
                me.finish_activity(&repo, message, ctx);
            },
        );
    }

    fn fetch(&mut self, repo_root: &Path, ctx: &mut ViewContext<Self>) {
        if !self.start_activity(repo_root, "Fetching…", ctx) {
            return;
        }
        let path_env = Self::interactive_path(ctx);
        let repo = repo_root.to_path_buf();
        ctx.spawn(
            async move {
                let result = git_operation_with_timeout(
                    async {
                        let path_env = path_env.await;
                        git::run_fetch(&repo, path_env.as_deref()).await
                    },
                    GIT_OPERATION_TIMEOUT,
                )
                .await;
                (repo, result)
            },
            |me, (repo, result), ctx| {
                let message = match result {
                    Ok(_) => RowMessage::info("Fetched"),
                    Err(err) => Self::git_error_message("Fetch", &repo, &err),
                };
                me.finish_activity(&repo, message, ctx);
            },
        );
    }

    fn git_error_message(operation: &str, repo: &Path, err: &anyhow::Error) -> RowMessage {
        log::warn!("{operation} failed in {}: {err:#}", repo.display());
        if err.to_string().starts_with("Git operation timed out") {
            return RowMessage::error(format!(
                "{operation} timed out. Check repository status before retrying."
            ));
        }
        RowMessage::error(user_facing_git_error(&err.to_string()))
    }

    fn open_commit_dialog(
        &mut self,
        repo_root: &Path,
        status: &NestedRepoStatus,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.git_dialog.is_some() || !self.start_activity(repo_root, "Committing…", ctx) {
            return;
        }
        let repo = repo_root.to_path_buf();
        let branch = status.branch.clone();
        let has_upstream = status.upstream.is_some();
        let diff_state_model =
            ctx.add_model(|ctx| DiffStateModel::new_local_for_git_ops(repo.clone(), ctx));
        let dialog = ctx.add_typed_action_view(|ctx| {
            GitDialog::new_for_commit(
                LocalOrRemotePath::Local(repo.clone()),
                diff_state_model,
                branch,
                // Committing and opening a pull request in one step needs the pull request state,
                // which the overview does not load; the row's own pull request action covers it.
                false,
                has_upstream,
                ctx,
            )
        });
        self.show_git_dialog(repo, dialog, ctx);
    }

    /// Reads the commits a push would send, then opens the push dialog listing them.
    fn load_push_dialog(
        &mut self,
        repo_root: &Path,
        status: &NestedRepoStatus,
        publish: bool,
        ctx: &mut ViewContext<Self>,
    ) {
        let label = if publish {
            "Publishing…"
        } else {
            "Pushing…"
        };
        if self.git_dialog.is_some() || !self.start_activity(repo_root, label, ctx) {
            return;
        }
        let repo = repo_root.to_path_buf();
        let branch = status.branch.clone();
        let upstream = status.upstream.clone();
        ctx.spawn(
            async move {
                let result = git_operation_with_timeout(
                    get_unpushed_commits(&repo, Some(&branch), upstream.as_deref()),
                    GIT_OPERATION_TIMEOUT,
                )
                .await;
                (repo, result)
            },
            move |me, (repo, result), ctx| match result {
                Ok(commits) => {
                    me.open_loaded_dialog(&repo, DialogRequest::Push { publish, commits }, ctx)
                }
                Err(err) => {
                    let message = Self::git_error_message("Push commit lookup", &repo, &err);
                    me.finish_activity(&repo, message, ctx);
                }
            },
        );
    }

    /// Opens the branch's pull request in the browser, or the dialog that creates one when it
    /// has none yet.
    fn open_pull_request(
        &mut self,
        repo_root: &Path,
        status: &NestedRepoStatus,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.git_dialog.is_some()
            || !self.start_activity(repo_root, "Checking pull request…", ctx)
        {
            return;
        }
        let path_env = Self::interactive_path(ctx);
        let repo = repo_root.to_path_buf();
        let branch = status.branch.clone();
        ctx.spawn(
            async move {
                let result = git_operation_with_timeout(
                    async {
                        let path_env = path_env.await;
                        let pull_request = get_pr_for_branch(&repo, path_env.as_deref()).await?;
                        let main_branch = if pull_request.is_none() {
                            Some(detect_main_branch(&repo).await?)
                        } else {
                            None
                        };
                        Ok((pull_request, main_branch))
                    },
                    GIT_OPERATION_TIMEOUT,
                )
                .await;
                (repo, result)
            },
            move |me,
                  (repo, result): (PathBuf, anyhow::Result<(Option<PrInfo>, Option<String>)>),
                  ctx| {
                match result {
                    Ok((Some(pull_request), _)) => {
                        ctx.open_url(&pull_request.url);
                        let message = RowMessage::info(format!(
                            "Opened pull request #{}",
                            pull_request.number
                        ));
                        me.finish_activity(&repo, message, ctx);
                    }
                    Ok((None, main_branch)) => {
                        if main_branch
                            .as_deref()
                            .is_some_and(|main_branch| is_main_branch(&branch, main_branch))
                        {
                            let message = RowMessage::error(format!(
                                "{branch} is the main branch. Create a branch to open a pull \
                                 request from."
                            ));
                            me.finish_activity(&repo, message, ctx);
                        } else {
                            let request = DialogRequest::CreatePr {
                                base_branch: main_branch,
                            };
                            me.open_loaded_dialog(&repo, request, ctx);
                        }
                    }
                    Err(err) => {
                        let message = Self::git_error_message("Pull request lookup", &repo, &err);
                        me.finish_activity(&repo, message, ctx);
                    }
                }
            },
        );
    }

    fn open_loaded_dialog(
        &mut self,
        repo_root: &Path,
        request: DialogRequest,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(branch) = self
            .status(repo_root, ctx)
            .map(|status| status.branch.clone())
            .filter(|_| self.git_dialog.is_none())
        else {
            self.clear_activity(repo_root, ctx);
            return;
        };
        let repo = repo_root.to_path_buf();
        let diff_state_model =
            ctx.add_model(|ctx| DiffStateModel::new_local_for_git_ops(repo.clone(), ctx));
        let location = LocalOrRemotePath::Local(repo.clone());
        let dialog = ctx.add_typed_action_view(|ctx| match request {
            DialogRequest::Push { publish, commits } => {
                GitDialog::new_for_push(location, diff_state_model, branch, publish, commits, ctx)
            }
            DialogRequest::CreatePr { base_branch } => {
                GitDialog::new_for_pr(location, diff_state_model, branch, base_branch, ctx)
            }
        });
        self.show_git_dialog(repo, dialog, ctx);
    }

    fn show_git_dialog(
        &mut self,
        repo_root: PathBuf,
        dialog: ViewHandle<GitDialog>,
        ctx: &mut ViewContext<Self>,
    ) {
        ctx.subscribe_to_view(&dialog, move |me, _, event, ctx| {
            me.git_dialog = None;
            me.clear_activity(&repo_root, ctx);
            match event {
                GitDialogEvent::Completed => {
                    me.model
                        .update(ctx, |model, ctx| model.reload_status(&repo_root, ctx));
                }
                GitDialogEvent::Cancelled => {}
            }
            ctx.notify();
        });
        self.git_dialog = Some(dialog.clone());
        ctx.focus(&dialog);
        ctx.notify();
    }

    fn confirm_discard(
        &mut self,
        repo_root: &Path,
        status: &NestedRepoStatus,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.git_dialog.is_some() || self.discard_confirmation.is_some() {
            return;
        }
        if matches!(
            self.activities.get(repo_root),
            Some(RepoActivity::Running(_))
        ) {
            return;
        }
        let label = format!("Discard {}", change_count_label(status.files.len()));
        self.discard_confirm_button
            .update(ctx, |button, ctx| button.set_label(label, ctx));
        self.discard_confirmation = Some(DiscardConfirmation {
            repo_root: repo_root.to_path_buf(),
            opened_at: Instant::now(),
        });
        // Takes keyboard focus away from whatever could otherwise act on a stray keypress.
        ctx.focus_self();
        ctx.notify();
    }

    fn discard_confirmed(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(confirmation) = &self.discard_confirmation else {
            return;
        };
        if confirmation.opened_at.elapsed() < DISCARD_CONFIRMATION_ARMING_DELAY {
            return;
        }
        let repo = confirmation.repo_root.clone();
        self.discard_confirmation = None;
        let discarded = self
            .status(&repo, ctx)
            .map_or(0, |status| status.files.len());
        if !self.start_activity(&repo, "Discarding…", ctx) {
            return;
        }
        ctx.spawn(
            async move {
                let result = git_operation_with_timeout(
                    git::discard_all_changes(&repo),
                    GIT_OPERATION_TIMEOUT,
                )
                .await;
                (repo, result)
            },
            move |me, (repo, result), ctx| {
                let message = match result {
                    Ok(()) => {
                        RowMessage::info(format!("Discarded {}", change_count_label(discarded)))
                    }
                    Err(err) => Self::git_error_message("Discard", &repo, &err),
                };
                me.finish_activity(&repo, message, ctx);
            },
        );
    }

    fn toggle_overflow_menu(
        &mut self,
        repo_root: &Path,
        row_width: RowWidth,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.overflow_menu_repo.as_deref() == Some(repo_root) {
            self.overflow_menu_repo = None;
            ctx.notify();
            return;
        }
        let Some(status) = self.status(repo_root, ctx) else {
            return;
        };
        let items: Vec<MenuItem<NestedReposViewAction>> =
            layout_actions(&applicable_actions(status), row_width)
                .overflow
                .into_iter()
                .map(|action| {
                    MenuItemFields::new(action.label())
                        .with_icon(action.icon())
                        .with_on_select_action(NestedReposViewAction::RunRepoAction {
                            repo_root: repo_root.to_path_buf(),
                            action,
                        })
                        .into_item()
                })
                .collect();
        if items.is_empty() {
            return;
        }
        self.overflow_menu.update(ctx, |menu, ctx| {
            menu.set_items(items, ctx);
            ctx.notify();
        });
        self.overflow_menu_repo = Some(repo_root.to_path_buf());
        ctx.notify();
    }

    fn render_summary(
        &self,
        model: &NestedReposModel,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let sub_text_color = blended_colors::text_sub(theme, theme.background());

        let mut row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Text::new_inline(
                    "Source Control",
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_color(blended_colors::text_main(theme, theme.background()))
                .finish(),
            );
        if model.has_completed_scan() {
            let count = model.entries().len();
            let label = if count == 1 {
                "1 repository".to_owned()
            } else {
                format!("{count} repositories")
            };
            row.add_child(
                Container::new(
                    Text::new_inline(
                        label,
                        appearance.ui_font_family(),
                        appearance.ui_font_size(),
                    )
                    .with_color(sub_text_color)
                    .finish(),
                )
                .with_margin_left(8.)
                .finish(),
            );
        }
        row.add_child(Shrinkable::new(1., Empty::new().finish()).finish());

        let tooltip = appearance
            .ui_builder()
            .tool_tip("Refresh repositories".to_string())
            .build()
            .finish();
        row.add_child(
            icon_button_with_color(
                appearance,
                Icon::Refresh,
                false,
                self.refresh_button_mouse_state.clone(),
                sub_text_color.into(),
            )
            .with_tooltip(move || tooltip)
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(NestedReposViewAction::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish(),
        );

        Container::new(row.finish())
            .with_padding_left(CONTENT_LEFT_MARGIN)
            .with_padding_right(CONTENT_RIGHT_MARGIN)
            .with_padding_bottom(4.)
            .finish()
    }

    fn render_message(message: &'static str, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        Container::new(
            Text::new(
                message,
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_color(blended_colors::text_sub(theme, theme.background()))
            .finish(),
        )
        .with_padding_left(CONTENT_LEFT_MARGIN)
        .with_padding_right(CONTENT_LEFT_MARGIN)
        .with_padding_top(8.)
        .finish()
    }

    fn render_list(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let view_handle = self.view_handle.clone();
        let list = UniformList::new(
            self.list_state.clone(),
            self.rows.len(),
            move |range: Range<usize>, app: &AppContext| {
                let rows = match view_handle.upgrade(app) {
                    Some(view) => {
                        let view = view.as_ref(app);
                        let appearance = Appearance::as_ref(app);
                        range
                            .map(|index| view.render_row(index, appearance, app))
                            .collect()
                    }
                    None => Vec::new(),
                };
                rows.into_iter()
            },
        )
        .finish_scrollable();

        Container::new(
            Scrollable::vertical(
                self.scroll_state.clone(),
                list,
                ScrollbarWidth::Auto,
                theme.nonactive_ui_detail().into(),
                theme.active_ui_detail().into(),
                Fill::None,
            )
            .with_overlayed_scrollbar()
            .finish(),
        )
        .with_horizontal_padding(LIST_HORIZONTAL_PADDING)
        .with_padding_bottom(8.)
        .finish()
    }

    fn render_separator(appearance: &Appearance) -> Rect {
        Rect::new().with_background(appearance.theme().outline())
    }

    /// The list with the selected file's diff to its right.
    fn render_two_columns(&self, appearance: &Appearance) -> Box<dyn Element> {
        // The alignment makes the column, and with it the drag bar, as tall as the panel even
        // when the list is shorter.
        let list_column = Resizable::new(
            self.list_column_state.clone(),
            Align::new(self.render_list(appearance))
                .top_center()
                .finish(),
        )
        .with_dragbar_side(DragBarSide::Right)
        .on_resize(|ctx, _| ctx.notify())
        .with_bounds_callback(Box::new(|_| (MIN_LIST_COLUMN_WIDTH, MAX_LIST_COLUMN_WIDTH)))
        .finish();

        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(list_column)
            .with_child(
                ConstrainedBox::new(Self::render_separator(appearance).finish())
                    .with_width(1.)
                    .finish(),
            )
            .with_child(Shrinkable::new(1., ChildView::new(&self.file_diff).finish()).finish())
            .finish()
    }

    /// The list with the selected file's diff underneath, for a panel too narrow for both
    /// side by side.
    fn render_stacked(&self, appearance: &Appearance) -> Box<dyn Element> {
        Flex::column()
            .with_child(Shrinkable::new(1., self.render_list(appearance)).finish())
            .with_child(
                ConstrainedBox::new(Self::render_separator(appearance).finish())
                    .with_height(1.)
                    .finish(),
            )
            .with_child(Shrinkable::new(2., ChildView::new(&self.file_diff).finish()).finish())
            .finish()
    }

    fn render_row(
        &self,
        index: usize,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let entries = self.model.as_ref(app).entries();
        let row = match self.rows.get(index) {
            Some(OverviewRow::Repo { repo_index }) => entries.get(*repo_index).and_then(|entry| {
                let mouse_states = self.mouse_states.get(&entry.repo.root)?;
                Some(self.render_repo_row(entry, mouse_states, appearance))
            }),
            Some(OverviewRow::File {
                repo_index,
                file_index,
            }) => entries.get(*repo_index).and_then(|entry| {
                let file = entry.status.as_ref()?.files.get(*file_index)?;
                let mouse_state = self
                    .mouse_states
                    .get(&entry.repo.root)?
                    .files
                    .get(*file_index)?;
                Some(self.render_file_row(entry, file, mouse_state, appearance))
            }),
            None => None,
        };
        ConstrainedBox::new(row.unwrap_or_else(|| Empty::new().finish()))
            .with_height(ROW_HEIGHT)
            .finish()
    }

    /// A repository's header row, with as many of its action buttons as the row's width allows.
    fn render_repo_row(
        &self,
        entry: &NestedRepoEntry,
        mouse_states: &RepoMouseStates,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let row_for =
            |row_width| self.render_repo_row_for_width(entry, mouse_states, row_width, appearance);
        SizeConstraintSwitch::new(
            row_for(RowWidth::Wide),
            vec![
                (
                    SizeConstraintCondition::WidthLessThan(MEDIUM_ROW_MIN_WIDTH),
                    row_for(RowWidth::Narrow),
                ),
                (
                    SizeConstraintCondition::WidthLessThan(WIDE_ROW_MIN_WIDTH),
                    row_for(RowWidth::Medium),
                ),
            ],
        )
        .finish()
    }

    fn render_repo_row_for_width(
        &self,
        entry: &NestedRepoEntry,
        mouse_states: &RepoMouseStates,
        row_width: RowWidth,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let secondary_font_size = font_size * SECONDARY_FONT_SIZE_RATIO;
        let sub_text_color = blended_colors::text_sub(theme, theme.background());

        let repo_root = entry.repo.root.clone();
        let is_expanded = self.is_expanded(&repo_root);
        let status = entry.status.as_ref();
        let file_count = changed_file_count(entry);
        let activity = self.activities.get(&repo_root);
        let is_busy = matches!(activity, Some(RepoActivity::Running(_)));

        let mut details = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                ConstrainedBox::new(
                    Text::new_inline(entry.display_name.clone(), font_family, font_size)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(blended_colors::text_main(theme, theme.background()))
                        .with_clip(ClipConfig::start())
                        .finish(),
                )
                .with_max_width(REPO_NAME_MAX_WIDTH)
                .finish(),
            );
        match entry.repo.kind {
            NestedRepoKind::Repository => {}
            NestedRepoKind::Worktree => details.add_child(
                Container::new(
                    Text::new_inline("worktree", font_family, secondary_font_size)
                        .with_color(sub_text_color)
                        .finish(),
                )
                .with_horizontal_padding(5.)
                .with_vertical_padding(1.)
                .with_margin_left(6.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_background(internal_colors::fg_overlay_2(theme))
                .finish(),
            ),
        }
        if let Some(status) = status {
            for (count, icon) in [
                (status.ahead, Icon::ArrowUp),
                (status.behind, Icon::ArrowDown),
            ] {
                if count == 0 {
                    continue;
                }
                details.add_child(
                    Container::new(
                        ConstrainedBox::new(icon.to_warpui_icon(sub_text_color.into()).finish())
                            .with_width(TRACKING_ICON_SIZE)
                            .with_height(TRACKING_ICON_SIZE)
                            .finish(),
                    )
                    .with_margin_left(6.)
                    .finish(),
                );
                details.add_child(
                    Text::new_inline(count.to_string(), font_family, secondary_font_size)
                        .with_color(sub_text_color)
                        .finish(),
                );
            }
        }
        let (note, note_color) = match activity {
            Some(RepoActivity::Running(label)) => (Some((*label).to_owned()), sub_text_color),
            Some(RepoActivity::Finished { message, .. }) => (
                Some(message.text.clone()),
                if message.is_error {
                    remove_color(appearance)
                } else {
                    add_color(appearance)
                },
            ),
            None if entry.status_failed => (
                Some("Status unavailable; refresh to retry".to_owned()),
                remove_color(appearance),
            ),
            None => (status.map(|status| status.branch.clone()), sub_text_color),
        };
        if let Some(note) = note {
            details.add_child(
                Shrinkable::new(
                    1.,
                    Container::new(
                        Text::new_inline(note, font_family, secondary_font_size)
                            .with_color(note_color)
                            .with_clip(ClipConfig::end())
                            .finish(),
                    )
                    .with_margin_left(8.)
                    .finish(),
                )
                .finish(),
            );
        }

        let actions = status.map(applicable_actions).unwrap_or_default();
        let layout = layout_actions(&actions, row_width);
        let mut trailing = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        for action in &layout.buttons {
            let Some(mouse_state) = mouse_states.actions.get(action) else {
                continue;
            };
            trailing.add_child(Self::render_action_button(
                *action,
                &repo_root,
                mouse_state,
                layout.shows_labels,
                is_busy,
                appearance,
            ));
        }
        if !layout.overflow.is_empty() {
            trailing.add_child(self.render_overflow_button(
                &repo_root,
                row_width,
                &mouse_states.overflow_button,
                appearance,
            ));
        }
        let count_label = match status {
            Some(_) => file_count.to_string(),
            None if entry.status_failed => "!".to_owned(),
            None => "…".to_owned(),
        };
        trailing.add_child(
            Container::new(
                Text::new_inline(count_label, font_family, secondary_font_size)
                    .with_color(sub_text_color)
                    .finish(),
            )
            .with_horizontal_padding(6.)
            .with_vertical_padding(1.)
            .with_margin_left(6.)
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(8.)))
            .with_background(internal_colors::fg_overlay_2(theme))
            .finish(),
        );

        let chevron = if is_expanded {
            Icon::ChevronDown
        } else {
            Icon::ChevronRight
        };
        Hoverable::new(mouse_states.header.clone(), |mouse_state| {
            let highlight = ItemHighlightState::new(false, mouse_state);
            let row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Shrinkable::new(
                        1.,
                        Flex::row()
                            .with_cross_axis_alignment(CrossAxisAlignment::Center)
                            .with_child(
                                Container::new(
                                    ConstrainedBox::new(
                                        chevron
                                            .to_warpui_icon(
                                                highlight.text_and_icon_color(appearance).into(),
                                            )
                                            .finish(),
                                    )
                                    .with_width(CHEVRON_SIZE)
                                    .with_height(CHEVRON_SIZE)
                                    .finish(),
                                )
                                .with_margin_right(4.)
                                .finish(),
                            )
                            .with_child(
                                Shrinkable::new(1., Clipped::new(details.finish()).finish())
                                    .finish(),
                            )
                            .finish(),
                    )
                    .finish(),
                )
                .with_child(trailing.finish())
                .finish();

            Self::finish_row(row, highlight, appearance)
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(NestedReposViewAction::ToggleRepo(repo_root.clone()));
        })
        // The action buttons sit inside this row; their clicks must not also toggle it.
        .with_defer_events_to_children()
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn render_action_button(
        action: RepoAction,
        repo_root: &Path,
        mouse_state: &MouseStateHandle,
        shows_label: bool,
        is_disabled: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let color = match action {
            RepoAction::Discard => remove_color(appearance),
            RepoAction::Commit
            | RepoAction::Push
            | RepoAction::Publish
            | RepoAction::Sync
            | RepoAction::PullRequest
            | RepoAction::Fetch => blended_colors::text_main(theme, theme.background()),
        };
        let dispatched = NestedReposViewAction::RunRepoAction {
            repo_root: repo_root.to_path_buf(),
            action,
        };

        let button = if shows_label {
            let mut button = Hoverable::new(mouse_state.clone(), |mouse_state| {
                let background = if mouse_state.is_hovered() && !is_disabled {
                    internal_colors::fg_overlay_5(theme)
                } else {
                    internal_colors::fg_overlay_3(theme)
                };
                Container::new(
                    Text::new_inline(
                        action.label(),
                        appearance.ui_font_family(),
                        appearance.ui_font_size() * SECONDARY_FONT_SIZE_RATIO,
                    )
                    .with_color(if is_disabled {
                        blended_colors::text_disabled(theme, theme.background())
                    } else {
                        color
                    })
                    .finish(),
                )
                .with_horizontal_padding(6.)
                .with_vertical_padding(1.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_background(background)
                .finish()
            });
            if !is_disabled {
                button = button
                    .on_click(move |ctx, _, _| ctx.dispatch_typed_action(dispatched.clone()))
                    .with_cursor(Cursor::PointingHand);
            }
            button.finish()
        } else {
            let tooltip = appearance
                .ui_builder()
                .tool_tip(action.tooltip().to_string())
                .build()
                .finish();
            let mut button = icon_button_with_color(
                appearance,
                action.icon(),
                false,
                mouse_state.clone(),
                color.into(),
            )
            .with_tooltip(move || tooltip);
            if is_disabled {
                button = button.disabled();
            }
            button
                .build()
                .on_click(move |ctx, _, _| ctx.dispatch_typed_action(dispatched.clone()))
                .with_cursor(Cursor::PointingHand)
                .finish()
        };
        Container::new(button).with_margin_left(4.).finish()
    }

    fn render_overflow_button(
        &self,
        repo_root: &Path,
        row_width: RowWidth,
        mouse_state: &MouseStateHandle,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let is_open = self.overflow_menu_repo.as_deref() == Some(repo_root);
        let dispatched = NestedReposViewAction::ToggleOverflowMenu {
            repo_root: repo_root.to_path_buf(),
            row_width,
        };
        let button = icon_button_with_color(
            appearance,
            Icon::DotsHorizontal,
            is_open,
            mouse_state.clone(),
            blended_colors::text_sub(theme, theme.background()).into(),
        )
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(dispatched.clone()))
        .with_cursor(Cursor::PointingHand)
        .finish();

        let mut stack = Stack::new().with_child(button);
        if is_open {
            stack.add_positioned_overlay_child(
                ChildView::new(&self.overflow_menu).finish(),
                OffsetPositioning::offset_from_parent(
                    vec2f(0., 0.),
                    ParentOffsetBounds::WindowByPosition,
                    ParentAnchor::BottomRight,
                    ChildAnchor::TopRight,
                ),
            );
        }
        Container::new(stack.finish()).with_margin_left(4.).finish()
    }

    fn render_file_row(
        &self,
        entry: &NestedRepoEntry,
        file: &NestedRepoFile,
        mouse_state: &MouseStateHandle,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();

        let path = Path::new(&file.path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&file.path)
            .to_owned();
        let directory = path
            .parent()
            .and_then(|parent| parent.to_str())
            .unwrap_or_default()
            .to_owned();
        let is_selected = self.selected_file.as_ref().is_some_and(|selected| {
            selected.repo_root == entry.repo.root && selected.path == file.path
        });
        let dispatched = NestedReposViewAction::SelectFile {
            repo_root: entry.repo.root.clone(),
            path: file.path.clone(),
        };

        Hoverable::new(mouse_state.clone(), |mouse_state| {
            let highlight = ItemHighlightState::new(is_selected, mouse_state);
            let mut row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(
                    Container::new(
                        ConstrainedBox::new(
                            Text::new_inline(status_letter(&file.status), font_family, font_size)
                                .with_style(Properties::default().weight(Weight::Semibold))
                                .with_color(status_color(&file.status, appearance))
                                .finish(),
                        )
                        .with_width(STATUS_LETTER_WIDTH)
                        .finish(),
                    )
                    .with_margin_left(FILE_ROW_INDENT)
                    .finish(),
                )
                .with_child(
                    ConstrainedBox::new(
                        Text::new_inline(file_name, font_family, font_size)
                            .with_color(blended_colors::text_main(theme, theme.background()))
                            .with_clip(ClipConfig::end())
                            .finish(),
                    )
                    .with_max_width(FILE_NAME_MAX_WIDTH)
                    .finish(),
                );
            if !directory.is_empty() {
                row.add_child(
                    Shrinkable::new(
                        1.,
                        Container::new(
                            Text::new_inline(
                                directory,
                                font_family,
                                font_size * SECONDARY_FONT_SIZE_RATIO,
                            )
                            .with_color(blended_colors::text_sub(theme, theme.background()))
                            .with_clip(ClipConfig::start())
                            .finish(),
                        )
                        .with_margin_left(6.)
                        .finish(),
                    )
                    .finish(),
                );
            }
            Self::finish_row(Clipped::new(row.finish()).finish(), highlight, appearance)
        })
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(dispatched.clone()))
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    /// Wraps a row's content so it fills the row height and shows the hover highlight.
    fn finish_row(
        content: Box<dyn Element>,
        highlight: ItemHighlightState,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut container = Container::new(
            ConstrainedBox::new(content)
                .with_height(ROW_HEIGHT)
                .finish(),
        )
        .with_horizontal_padding(ROW_HORIZONTAL_PADDING);
        if let Some(background) = highlight.background_color(appearance) {
            container = container.with_background(background);
        }
        if let Some(corner_radius) = highlight.corner_radius() {
            container = container.with_corner_radius(corner_radius);
        }
        container.finish()
    }

    fn render_discard_confirmation(
        &self,
        confirmation: &DiscardConfirmation,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let entry = self.entry(&confirmation.repo_root, app);
        let repo_name = entry.map_or_else(
            || confirmation.repo_root.display().to_string(),
            |entry| entry.display_name.clone(),
        );
        let file_count = entry.map_or(0, changed_file_count);
        let description = format!(
            "This discards {} in {repo_name}: edits to tracked files are reverted and untracked \
             files are deleted. It cannot be undone.",
            change_count_label(file_count)
        );

        let dialog = Dialog::new(
            "Discard all uncommitted changes?".to_string(),
            Some(description),
            UiComponentStyles {
                width: Some(460.),
                padding: Some(Coords::uniform(24.).bottom(12.)),
                ..dialog_styles(appearance)
            },
        )
        .with_separator()
        .with_bottom_row_child(ChildView::new(&self.discard_cancel_button).finish())
        .with_bottom_row_child(
            Container::new(ChildView::new(&self.discard_confirm_button).finish())
                .with_margin_left(8.)
                .finish(),
        )
        .build()
        .finish();

        // The stack lets the dialog measure itself against the window rather than the panel.
        let mut stack = Stack::new();
        stack.add_positioned_child(
            Container::new(dialog).with_margin_top(35.).finish(),
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::Center,
                ChildAnchor::Center,
            ),
        );
        Container::new(Align::new(stack.finish()).finish())
            .with_background_color(appearance.theme().blurred_background_overlay().into())
            .with_corner_radius(app.windows().window_corner_radius())
            .finish()
    }
}

impl Entity for NestedReposView {
    type Event = NestedReposViewEvent;
}

impl TypedActionView for NestedReposView {
    type Action = NestedReposViewAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            NestedReposViewAction::ToggleRepo(repo_root) => {
                let is_expanded = self.is_expanded(repo_root);
                self.expanded.insert(repo_root.clone(), !is_expanded);
                self.sync_with_model(ctx);
            }
            NestedReposViewAction::SelectFile { repo_root, path } => {
                self.select_file(repo_root, path, ctx);
            }
            NestedReposViewAction::Refresh => {
                self.model.update(ctx, |model, ctx| model.refresh(ctx));
            }
            NestedReposViewAction::RunRepoAction { repo_root, action } => {
                self.run_repo_action(repo_root, *action, ctx);
            }
            NestedReposViewAction::ToggleOverflowMenu {
                repo_root,
                row_width,
            } => self.toggle_overflow_menu(repo_root, *row_width, ctx),
            NestedReposViewAction::ConfirmDiscard => self.discard_confirmed(ctx),
            NestedReposViewAction::CancelDiscard => {
                self.discard_confirmation = None;
                ctx.notify();
            }
        }
    }
}

impl View for NestedReposView {
    fn ui_name() -> &'static str {
        "NestedReposView"
    }

    fn keymap_context(&self, _: &AppContext) -> keymap::Context {
        let mut context = keymap::Context::default();
        context.set.insert(Self::ui_name());
        if self.discard_confirmation.is_some() {
            context.set.insert(DISCARD_DIALOG_CONTEXT);
        }
        context
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let model = self.model.as_ref(app);
        let body = if !model.has_completed_scan() {
            Self::render_message("Scanning for repositories…", appearance)
        } else if model.entries().is_empty() {
            Self::render_message("No repositories found in this folder", appearance)
        } else if self.selected_file.is_some() {
            SizeConstraintSwitch::new(
                self.render_two_columns(appearance),
                vec![(
                    SizeConstraintCondition::WidthLessThan(MIN_TWO_COLUMN_WIDTH),
                    self.render_stacked(appearance),
                )],
            )
            .finish()
        } else {
            self.render_list(appearance)
        };
        let content = Flex::column()
            .with_child(self.render_summary(model, appearance))
            .with_child(Shrinkable::new(1., body).finish())
            .finish();

        let modal = if let Some(git_dialog) = &self.git_dialog {
            Some(ChildView::new(git_dialog).finish())
        } else {
            self.discard_confirmation
                .as_ref()
                .map(|confirmation| self.render_discard_confirmation(confirmation, app))
        };
        let Some(modal) = modal else {
            return content;
        };
        let mut stack = Stack::new().with_child(content);
        stack.add_positioned_overlay_child(
            modal,
            OffsetPositioning::offset_from_parent(
                vec2f(0., 0.),
                ParentOffsetBounds::WindowByPosition,
                ParentAnchor::Center,
                ChildAnchor::Center,
            ),
        );
        stack.finish()
    }
}

#[cfg(test)]
#[path = "nested_repos_view_tests.rs"]
mod tests;
