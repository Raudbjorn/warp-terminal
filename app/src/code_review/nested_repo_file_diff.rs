//! Diff of one changed file against `HEAD`, shown beside the source control overview's list.
//!
//! The file is shown through the shared buffer of its working copy, with the committed version
//! as the diff base. Nothing here registers, watches or indexes the repository the file is in.

use std::path::{Path, PathBuf};

use warp_editor::content::buffer::InitialBufferState;
use warp_editor::render::element::VerticalExpansionBehavior;
use warp_util::file::FileId;
use warpui::r#async::SpawnedFutureHandle;
use warpui::elements::{
    ChildView, Clipped, ConstrainedBox, Container, CrossAxisAlignment, Element, Flex,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, ParentElement, Shrinkable, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::text_layout::ClipConfig;
use warpui::ui_components::components::UiComponent;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::diff_state::GitFileStatus;
use super::nested_repos::NestedRepoFile;
use super::nested_repos_view::{status_color, status_letter};
use crate::appearance::Appearance;
use crate::code::buffer_location::LocalOrRemotePath;
use crate::code::editor::view::{CodeEditorRenderOptions, CodeEditorView};
use crate::code::global_buffer_model::{GlobalBufferModel, GlobalBufferModelEvent};
use crate::editor::InteractionState;
use crate::ui_components::blended_colors;
use crate::ui_components::buttons::icon_button_with_color;
use crate::ui_components::icons::Icon;
use crate::util::git::file_content_at_head;
use crate::util::openable_file_type::is_file_content_binary;

/// Unchanged lines kept visible around each change.
const DIFF_CONTEXT_LINES: usize = 4;
const HEADER_HEIGHT: f32 = 32.;
const HEADER_HORIZONTAL_PADDING: f32 = 12.;
const FILE_NAME_MAX_WIDTH: f32 = 260.;
const SECONDARY_FONT_SIZE_RATIO: f32 = 0.9;

const BINARY_FILE_MESSAGE: &str = "Binary file. Its changes cannot be shown as text.";
const UNTRACKED_FOLDER_MESSAGE: &str =
    "Untracked folder. Its files are listed one by one once the folder is added to git.";
const DELETED_FILE_NOTE: &str = "Deleted. Showing the version at HEAD.";

/// Path, relative to the repository root, of the committed file that `file` is compared with.
/// `None` when the file has no committed counterpart.
fn base_path_at_head(file: &NestedRepoFile) -> Option<&str> {
    match &file.status {
        GitFileStatus::New | GitFileStatus::Untracked => None,
        GitFileStatus::Renamed { old_path } | GitFileStatus::Copied { old_path } => {
            Some(old_path.as_str())
        }
        GitFileStatus::Modified | GitFileStatus::Deleted | GitFileStatus::Conflicted => {
            Some(file.path.as_str())
        }
    }
}

/// Git lists a wholly untracked folder as one entry whose path ends in a slash.
fn is_untracked_folder(file: &NestedRepoFile) -> bool {
    matches!(file.status, GitFileStatus::Untracked) && file.path.ends_with('/')
}

/// What the background read of a file's committed version found.
struct LoadedBase {
    content: Option<String>,
    is_binary: bool,
}

async fn load_base(repo_root: PathBuf, file: NestedRepoFile) -> LoadedBase {
    let content = match base_path_at_head(&file) {
        Some(base_path) => file_content_at_head(&repo_root, base_path).await,
        None => None,
    };
    let is_deleted = matches!(file.status, GitFileStatus::Deleted);
    let is_binary = content
        .as_deref()
        .is_some_and(|content| content.contains('\0'))
        || (!is_deleted && is_file_content_binary(repo_root.join(&file.path)));
    LoadedBase { content, is_binary }
}

enum DiffContent {
    Loading,
    /// The working copy, with the committed version as the diff base.
    Diff {
        editor: ViewHandle<CodeEditorView>,
        file_id: FileId,
        base: String,
    },
    /// The committed version of a file that is gone from the working tree.
    Deleted {
        editor: ViewHandle<CodeEditorView>,
    },
    Message(&'static str),
}

struct ShownFile {
    repo_root: PathBuf,
    file: NestedRepoFile,
    /// Commit the diff base was read from.
    head_oid: Option<String>,
    content: DiffContent,
}

#[derive(Clone, Debug, PartialEq)]
pub enum NestedRepoFileDiffAction {
    Close,
    OpenFile,
}

#[derive(Clone, Debug)]
pub enum NestedRepoFileDiffEvent {
    Close,
    OpenFile { path: PathBuf },
}

pub struct NestedRepoFileDiffView {
    shown: Option<ShownFile>,
    load: Option<SpawnedFutureHandle>,
    close_button_mouse_state: MouseStateHandle,
    open_button_mouse_state: MouseStateHandle,
}

impl NestedRepoFileDiffView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        ctx.subscribe_to_model(&GlobalBufferModel::handle(ctx), |me, _, event, ctx| {
            me.handle_buffer_event(event, ctx);
        });
        Self {
            shown: None,
            load: None,
            close_button_mouse_state: Default::default(),
            open_button_mouse_state: Default::default(),
        }
    }

    /// Shows the diff of `file`, unless exactly that is already shown. `head_oid` is the commit
    /// the repository's `HEAD` points at, so that a moved `HEAD` reloads the diff base.
    pub fn show(
        &mut self,
        repo_root: PathBuf,
        file: NestedRepoFile,
        head_oid: Option<String>,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.shown.as_ref().is_some_and(|shown| {
            shown.repo_root == repo_root && shown.file == file && shown.head_oid == head_oid
        }) {
            return;
        }
        self.clear(ctx);

        let content = if is_untracked_folder(&file) {
            DiffContent::Message(UNTRACKED_FOLDER_MESSAGE)
        } else {
            self.load = Some(ctx.spawn(
                load_base(repo_root.clone(), file.clone()),
                Self::handle_base_loaded,
            ));
            DiffContent::Loading
        };
        self.shown = Some(ShownFile {
            repo_root,
            file,
            head_oid,
            content,
        });
        ctx.notify();
    }

    pub fn clear(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(load) = self.load.take() {
            load.abort();
        }
        if self.shown.take().is_some() {
            GlobalBufferModel::handle(ctx).update(ctx, |model, ctx| {
                model.remove_deallocated_buffers(ctx);
            });
            ctx.notify();
        }
    }

    fn handle_base_loaded(&mut self, base: LoadedBase, ctx: &mut ViewContext<Self>) {
        self.load = None;
        let Some(shown) = &self.shown else {
            return;
        };
        let content = if base.is_binary {
            DiffContent::Message(BINARY_FILE_MESSAGE)
        } else if matches!(shown.file.status, GitFileStatus::Deleted) {
            let path = shown.repo_root.join(&shown.file.path);
            let editor = Self::build_deleted_file_editor(
                &path,
                base.content.as_deref().unwrap_or_default(),
                ctx,
            );
            DiffContent::Deleted { editor }
        } else {
            let path = shown.repo_root.join(&shown.file.path);
            let base = base.content.unwrap_or_default();
            let (editor, file_id) = Self::build_diff_editor(&path, &base, ctx);
            DiffContent::Diff {
                editor,
                file_id,
                base,
            }
        };
        if let Some(shown) = &mut self.shown {
            shown.content = content;
        }
        ctx.notify();
    }

    fn build_diff_editor(
        path: &Path,
        base: &str,
        ctx: &mut ViewContext<Self>,
    ) -> (ViewHandle<CodeEditorView>, FileId) {
        let buffer_state = GlobalBufferModel::handle(ctx).update(ctx, |model, ctx| {
            model.open(LocalOrRemotePath::Local(path.to_path_buf()), ctx)
        });
        let file_id = buffer_state.file_id;
        // A bare editor over the shared buffer: the file-backed editor view would also try to
        // start a language server for the file's repository.
        let editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = CodeEditorView::new(
                None,
                Some(buffer_state.buffer),
                CodeEditorRenderOptions::new(VerticalExpansionBehavior::FillMaxHeight),
                ctx,
            )
            .with_collapsible_diffs(false)
            .disable_diff_indicator_expansion_on_hover()
            .disable_find_and_replace();
            editor.set_show_nav_bar(false);
            editor.set_language_with_local_path(path, ctx);
            editor.hide_lines_outside_of_active_diff(DIFF_CONTEXT_LINES, ctx);
            editor
        });
        let is_loaded = GlobalBufferModel::as_ref(ctx).buffer_loaded(file_id);
        editor.update(ctx, |editor, ctx| {
            editor.model.update(ctx, |model, ctx| {
                model.rebuild_layout_with_syntax_highlighting(ctx);
            });
            // An unloaded buffer gets its diff when the load completes.
            editor.set_base(base, is_loaded, ctx);
            editor.expand_diffs(ctx);
            editor.set_interaction_state(InteractionState::Selectable, ctx);
        });
        (editor, file_id)
    }

    fn build_deleted_file_editor(
        path: &Path,
        content: &str,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<CodeEditorView> {
        let editor = ctx.add_typed_action_view(|ctx| {
            let mut editor = CodeEditorView::new(
                None,
                None,
                CodeEditorRenderOptions::new(VerticalExpansionBehavior::FillMaxHeight),
                ctx,
            )
            .disable_find_and_replace();
            editor.set_show_nav_bar(false);
            editor.set_language_with_local_path(path, ctx);
            editor
        });
        editor.update(ctx, |editor, ctx| {
            editor.reset(InitialBufferState::plain_text(content), ctx);
            editor.set_interaction_state(InteractionState::Selectable, ctx);
        });
        editor
    }

    fn handle_buffer_event(&mut self, event: &GlobalBufferModelEvent, ctx: &mut ViewContext<Self>) {
        let Some(ShownFile {
            content:
                DiffContent::Diff {
                    editor,
                    file_id,
                    base,
                },
            ..
        }) = &self.shown
        else {
            return;
        };
        if event.file_id() != *file_id {
            return;
        }
        let is_content_changed = match event {
            GlobalBufferModelEvent::BufferLoaded { .. } => true,
            GlobalBufferModelEvent::BufferUpdatedFromFileEvent { success, .. } => *success,
            GlobalBufferModelEvent::FailedToLoad { .. }
            | GlobalBufferModelEvent::FileSaved { .. }
            | GlobalBufferModelEvent::FailedToSave { .. }
            | GlobalBufferModelEvent::RemoteBufferConflict { .. }
            | GlobalBufferModelEvent::ServerLocalBufferUpdated { .. } => false,
        };
        if is_content_changed {
            editor.update(ctx, |editor, ctx| {
                editor.set_base(base, true, ctx);
                editor.expand_diffs(ctx);
            });
            ctx.notify();
        }
    }

    fn render_header(&self, shown: &ShownFile, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let sub_text_color = blended_colors::text_sub(theme, theme.background());

        let path = Path::new(&shown.file.path);
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&shown.file.path)
            .to_owned();
        let directory = path
            .parent()
            .and_then(|parent| parent.to_str())
            .unwrap_or_default()
            .to_owned();

        let mut title = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Container::new(
                    Text::new_inline(status_letter(&shown.file.status), font_family, font_size)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(status_color(&shown.file.status, appearance))
                        .finish(),
                )
                .with_margin_right(8.)
                .finish(),
            )
            .with_child(
                ConstrainedBox::new(
                    Text::new_inline(file_name, font_family, font_size)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(blended_colors::text_main(theme, theme.background()))
                        .with_clip(ClipConfig::end())
                        .finish(),
                )
                .with_max_width(FILE_NAME_MAX_WIDTH)
                .finish(),
            );
        let detail = match &shown.file.status {
            GitFileStatus::Renamed { old_path } => format!("renamed from {old_path}"),
            GitFileStatus::Copied { old_path } => format!("copied from {old_path}"),
            GitFileStatus::New
            | GitFileStatus::Modified
            | GitFileStatus::Deleted
            | GitFileStatus::Untracked
            | GitFileStatus::Conflicted => directory,
        };
        if !detail.is_empty() {
            title.add_child(
                Shrinkable::new(
                    1.,
                    Container::new(
                        Text::new_inline(
                            detail,
                            font_family,
                            font_size * SECONDARY_FONT_SIZE_RATIO,
                        )
                        .with_color(sub_text_color)
                        .with_clip(ClipConfig::start())
                        .finish(),
                    )
                    .with_margin_left(8.)
                    .finish(),
                )
                .finish(),
            );
        }

        let mut buttons = Flex::row()
            .with_main_axis_size(MainAxisSize::Min)
            .with_cross_axis_alignment(CrossAxisAlignment::Center);
        let can_open = match &shown.file.status {
            GitFileStatus::Deleted => false,
            GitFileStatus::New
            | GitFileStatus::Modified
            | GitFileStatus::Renamed { .. }
            | GitFileStatus::Copied { .. }
            | GitFileStatus::Untracked
            | GitFileStatus::Conflicted => !is_untracked_folder(&shown.file),
        };
        if can_open {
            buttons.add_child(Self::render_header_button(
                Icon::LinkExternal,
                "Open file",
                &self.open_button_mouse_state,
                NestedRepoFileDiffAction::OpenFile,
                appearance,
            ));
        }
        buttons.add_child(Self::render_header_button(
            Icon::X,
            "Close diff",
            &self.close_button_mouse_state,
            NestedRepoFileDiffAction::Close,
            appearance,
        ));

        Container::new(
            ConstrainedBox::new(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., Clipped::new(title.finish()).finish()).finish())
                    .with_child(buttons.finish())
                    .finish(),
            )
            .with_height(HEADER_HEIGHT)
            .finish(),
        )
        .with_padding_left(HEADER_HORIZONTAL_PADDING)
        .with_padding_right(4.)
        .finish()
    }

    fn render_header_button(
        icon: Icon,
        tooltip: &'static str,
        mouse_state: &MouseStateHandle,
        action: NestedRepoFileDiffAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let tooltip = appearance
            .ui_builder()
            .tool_tip(tooltip.to_string())
            .build()
            .finish();
        icon_button_with_color(
            appearance,
            icon,
            false,
            mouse_state.clone(),
            blended_colors::text_sub(theme, theme.background()).into(),
        )
        .with_tooltip(move || tooltip)
        .build()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .with_cursor(Cursor::PointingHand)
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
        .with_horizontal_padding(HEADER_HORIZONTAL_PADDING)
        .with_padding_top(8.)
        .finish()
    }
}

impl Entity for NestedRepoFileDiffView {
    type Event = NestedRepoFileDiffEvent;
}

impl TypedActionView for NestedRepoFileDiffView {
    type Action = NestedRepoFileDiffAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            NestedRepoFileDiffAction::Close => ctx.emit(NestedRepoFileDiffEvent::Close),
            NestedRepoFileDiffAction::OpenFile => {
                if let Some(shown) = &self.shown {
                    ctx.emit(NestedRepoFileDiffEvent::OpenFile {
                        path: shown.repo_root.join(&shown.file.path),
                    });
                }
            }
        }
    }
}

impl View for NestedRepoFileDiffView {
    fn ui_name() -> &'static str {
        "NestedRepoFileDiffView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let Some(shown) = &self.shown else {
            return Flex::column().finish();
        };
        let body = match &shown.content {
            DiffContent::Loading => Self::render_message("Loading diff…", appearance),
            DiffContent::Diff { editor, .. } => ChildView::new(editor).finish(),
            DiffContent::Deleted { editor } => Flex::column()
                .with_child(Self::render_message(DELETED_FILE_NOTE, appearance))
                .with_child(Shrinkable::new(1., ChildView::new(editor).finish()).finish())
                .finish(),
            DiffContent::Message(message) => Self::render_message(message, appearance),
        };
        Flex::column()
            .with_child(self.render_header(shown, appearance))
            .with_child(Shrinkable::new(1., body).finish())
            .finish()
    }
}

#[cfg(test)]
#[path = "nested_repo_file_diff_tests.rs"]
mod tests;
