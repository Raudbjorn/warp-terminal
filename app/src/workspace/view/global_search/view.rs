use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_channel::Sender;
use instant::Instant;
use pathfinder_geometry::vector::vec2f;
use project_replace::engine::SearchSpec;
use project_replace::scope::{ScopeGlobs, folder_pattern};
use remote_server::HostId;
use string_offset::{ByteOffset, CharCounter};
use warp_core::r#async::debounce;
use warp_core::send_telemetry_from_ctx;
use warp_core::ui::Icon;
use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::color::internal_colors;
use warp_core::ui::theme::{AnsiColorIdentifier, Fill as ThemeFill};
use warp_editor::editor::NavigationKey;
use warp_ripgrep::search::Submatch;
use warp_util::local_or_remote_path::LocalOrRemotePath;
use warp_util::remote_path::RemotePath;
use warp_util::standardized_path::StandardizedPath;
use warpui::elements::{
    Border, ChildAnchor, ChildView, Clipped, ConstrainedBox, Container, CornerRadius,
    CrossAxisAlignment, DispatchEventResult, Empty, EventHandler, Fill, Flex, FormattedTextElement,
    Highlight, Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning,
    Padding, ParentAnchor, ParentElement, ParentOffsetBounds, Radius, ScrollStateHandle,
    Scrollable, ScrollableElement, ScrollbarWidth, Shrinkable, Stack, Text, UniformList,
    UniformListState,
};
use warpui::fonts::{Properties, Weight};
use warpui::keymap::FixedBinding;
use warpui::platform::Cursor;
use warpui::text_layout::{ClipConfig, ClipDirection, ClipStyle, TextAlignment, TextStyle};
use warpui::ui_components::components::{UiComponent as _, UiComponentStyles};
use warpui::ui_components::text::Span;
use warpui::{
    AppContext, Element, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle,
};

use crate::TelemetryEvent;
use crate::code::{default_file_icon, icon_from_file_path, icon_from_folder_path};
use crate::coding_panel_enablement_state::CodingPanelEnablementState;
use crate::editor::{
    EditorOptions, EditorView, Event as EditorEvent, InteractionState,
    PropagateAndNoOpNavigationKeys, PropagateHorizontalNavigationKeys, SingleLineEditorOptions,
    TextOptions,
};
use crate::search::ItemHighlightState as SearchHighlightState;
use crate::ui_components::blended_colors;
use crate::ui_components::icons::Icon as UiIcon;
use crate::ui_components::item_highlight::ItemHighlightState;
use crate::ui_components::render_file_search_row::{FileSearchRowOptions, render_file_search_row};
use crate::util::path::{display_name_with_host, display_path_with_host};
use crate::view_components::action_button::{ActionButton, ButtonSize, NakedTheme};
use crate::workspace::view::global_search::model::GlobalSearch;
use crate::workspace::view::global_search::name_groups::{
    NameGroup, NameGroupRow, NameMatchEntry, NameMatchLabel, SectionCounts, SectionRow,
    name_match_summary,
};
use crate::workspace::view::global_search::view::replace::ReplaceUi;
use crate::workspace::view::global_search::{
    GlobalSearchMatch, NameMatchKind, NameSearchResults, SearchConfig, SearchOptions,
};

mod replace;

const BORDER_RADIUS: f32 = 6.;
const BORDER_WIDTH: f32 = 1.;
const DO_NOT_TRUNCATE_CHAR_COUNT: usize = 40;
const DO_TRUNCATE_END_CHAR_COUNT: usize = 200;
const PRE_MATCH_CHARS: usize = 15;
const MAX_MATCH_COUNT: usize = 20000;

const QUERY_EDITOR_MAX_LINES: usize = 6;

const QUERY_DEBOUNCE_PERIOD: Duration = Duration::from_millis(300);

const ROW_ICON_SIZE: f32 = 16.;

/// Left padding that lines a name group's rows up with the label of its header.
const NAME_GROUP_ROW_INDENT: f32 = 24.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalSearchEntryFocus {
    QueryEditor,
    Results,
}

enum FocusMode {
    QueryEditor,
    ResultsList,
}

#[derive(Debug, Clone)]
pub enum GlobalSearchAction {
    SelectRow {
        directory_path: LocalOrRemotePath,
        file_path: LocalOrRemotePath,
        match_index: Option<usize>,
    },
    ToggleFileCollapsed {
        directory_path: LocalOrRemotePath,
        file_path: LocalOrRemotePath,
    },
    ToggleDirectoryCollapsed {
        directory_path: LocalOrRemotePath,
    },
    OpenMatch {
        location: LocalOrRemotePath,
        line_number: u32,
        column_num: Option<usize>,
    },
    ClickNameGroupRow {
        kind: NameMatchKind,
        row: NameGroupRow,
    },
    ClickTextMatchesHeader,
    ResultsUp,
    ResultsDown,
    ResultsLeft,
    ResultsRight,
    ResultsEnter,
    FocusQueryEditor,
    FocusResultsList,
    ToggleRegexSearch,
    ToggleCaseSensitivity,
    ToggleWholeWord,
    /// Shows or hides the fields that limit the search to some files.
    ToggleScopeOpen,
    /// Shows or hides the replacement field.
    ToggleReplaceOpen,
    TogglePreserveCase,
    /// Sets a text match aside from replacing, or takes it back in.
    ToggleMatchDismissed {
        location: LocalOrRemotePath,
        line_number: u32,
        column: usize,
    },
    /// Sets every text match of a file aside from replacing, or takes them back in.
    ToggleFileDismissed {
        location: LocalOrRemotePath,
    },
    /// Replaces every text match that was not set aside, after asking.
    ReplaceAll,
    /// Replaces the text matches of one file that were not set aside.
    ReplaceInFile {
        location: LocalOrRemotePath,
    },
    ReplaceMatch {
        location: LocalOrRemotePath,
        line_number: u32,
        column: usize,
    },
    /// Stops the replace or the undo that is running before its next file.
    CancelReplace,
    UndoLastReplace,
    DismissReplaceSummary,
}

#[cfg_attr(target_family = "wasm", allow(dead_code))]
pub enum GlobalSearchEvent {
    Started {
        search_id: u32,
        remote_host_count: usize,
        /// Whether file and folder names are being matched too, in which case `NameMatches`
        /// follows once that is done.
        name_search_started: bool,
    },
    NameMatches {
        search_id: u32,
        results: NameSearchResults,
    },
    Progress {
        search_id: u32,
        result: GlobalSearchMatch,
    },
    ProgressBatch {
        search_id: u32,
        items: Vec<GlobalSearchMatch>,
    },
    Completed {
        search_id: u32,
        total_match_count: usize,
        /// True when a remote source hit the server-side match cap.
        capped: bool,
        /// Whether the local search source failed while another source
        /// completed. Results from the surviving sources remain valid.
        local_source_failed: bool,
        /// Number of remote host search sources that failed while another
        /// source completed. Results from the surviving sources remain valid.
        remote_source_failures: usize,
    },
    Failed {
        search_id: u32,
        error: String,
    },
}

#[cfg_attr(not(feature = "local_fs"), allow(dead_code))]
pub enum Event {
    OpenMatch {
        location: LocalOrRemotePath,
        line_number: u32,
        column_num: Option<usize>,
    },
    /// Open a file that matched by name.
    OpenFile { location: LocalOrRemotePath },
    /// Show a folder that matched by name in the file tree.
    RevealFolder { location: LocalOrRemotePath },
}

enum SelectionDirection {
    Up,
    Down,
}

/// Row index among the flattened text match rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GlobalIndex(usize);

/// A row of the results list, identified by its place in the section it belongs to rather than by
/// its index, so that it stays valid while results arrive around it.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ResultRow {
    NameGroup {
        kind: NameMatchKind,
        row: NameGroupRow,
    },
    TextMatchesHeader,
    TextMatch(RowIndex),
}

/// Hierarchical index that identifies a specific text match row by its position in the
/// directory/file/match hierarchy.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RowIndex {
    directory_index: usize,
    index_type: RowIndexType,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum RowIndexType {
    DirectoryHeader,
    FileHeader {
        path_index: usize,
    },
    Match {
        path_index: usize,
        match_index: usize,
    },
}

/// A root directory containing matched files.
struct DirectoryEntry {
    path: LocalOrRemotePath,
    is_collapsed: bool,
    mouse_state: MouseStateHandle,
    matched_paths: MatchedPaths,
}

impl DirectoryEntry {
    fn new(path: LocalOrRemotePath) -> Self {
        Self {
            path,
            is_collapsed: false,
            mouse_state: MouseStateHandle::default(),
            matched_paths: MatchedPaths::new(),
        }
    }

    /// Returns the number of visible rows for this directory entry.
    /// If collapsed, only the directory header is visible (1 row).
    /// Otherwise, includes directory header + all visible file rows.
    fn visible_count(&self) -> usize {
        if self.is_collapsed {
            1
        } else {
            1 + self.matched_paths.visible_count()
        }
    }

    /// Returns the total number of matches across all files in this directory.
    fn total_match_count(&self) -> usize {
        self.matched_paths
            .paths
            .iter()
            .map(|p| p.matches.len())
            .sum()
    }
}

/// Collection of matched files within a directory.
struct MatchedPaths {
    paths: Vec<MatchedPath>,
    index_by_path: HashMap<LocalOrRemotePath, usize>,
}

impl MatchedPaths {
    fn new() -> Self {
        Self {
            paths: Vec::new(),
            index_by_path: HashMap::new(),
        }
    }

    /// Returns the total number of visible rows across all files.
    fn visible_count(&self) -> usize {
        self.paths.iter().map(|p| p.visible_count()).sum()
    }

    /// Gets or creates a MatchedPath entry for the given file location.
    /// Returns a mutable reference to the entry and its index.
    fn get_or_create(&mut self, path: &LocalOrRemotePath) -> (&mut MatchedPath, usize) {
        if let Some(&index) = self.index_by_path.get(path) {
            (&mut self.paths[index], index)
        } else {
            let index = self.paths.len();
            self.paths.push(MatchedPath::new(path.clone()));
            self.index_by_path.insert(path.clone(), index);
            (&mut self.paths[index], index)
        }
    }

    /// Gets a mutable MatchedPath entry by file location.
    fn get_mut(&mut self, path: &LocalOrRemotePath) -> Option<&mut MatchedPath> {
        self.index_by_path
            .get(path)
            .copied()
            .map(|idx| &mut self.paths[idx])
    }
}

/// A file containing matches.
struct MatchedPath {
    path: LocalOrRemotePath,
    is_collapsed: bool,
    mouse_state: MouseStateHandle,
    replace_mouse_state: MouseStateHandle,
    dismiss_mouse_state: MouseStateHandle,
    matches: Vec<Match>,
}

impl MatchedPath {
    fn new(path: LocalOrRemotePath) -> Self {
        Self {
            path,
            is_collapsed: false,
            mouse_state: MouseStateHandle::default(),
            replace_mouse_state: MouseStateHandle::default(),
            dismiss_mouse_state: MouseStateHandle::default(),
            matches: Vec::new(),
        }
    }

    /// Returns the number of visible rows for this file.
    /// If collapsed, only the file header is visible (1 row).
    /// Otherwise, includes file header + all matches.
    fn visible_count(&self) -> usize {
        if self.is_collapsed {
            1
        } else {
            1 + self.matches.len()
        }
    }
}

/// A single match within a file.
struct Match {
    line_text: String,
    line_number: u32,
    column_num: Option<usize>,
    submatches: Vec<Submatch>,
    mouse_state: MouseStateHandle,
    replace_mouse_state: MouseStateHandle,
    dismiss_mouse_state: MouseStateHandle,
}

impl Match {
    fn new(
        line_text: String,
        line_number: u32,
        column_num: Option<usize>,
        submatches: Vec<Submatch>,
    ) -> Self {
        Self {
            line_text,
            line_number,
            column_num,
            submatches,
            mouse_state: MouseStateHandle::default(),
            replace_mouse_state: MouseStateHandle::default(),
            dismiss_mouse_state: MouseStateHandle::default(),
        }
    }
}

pub struct GlobalSearchView {
    find_model: ModelHandle<GlobalSearch>,
    query_editor: ViewHandle<EditorView>,
    query_change_tx: Sender<()>,
    /// All terminal working directories for display grouping (preserved as-is)
    root_directories: Vec<LocalOrRemotePath>,
    /// Deduplicated roots for search (excludes nested subdirectories)
    search_roots: Vec<LocalOrRemotePath>,
    last_searched_pattern: Option<String>,
    directory_entries: Vec<DirectoryEntry>,
    directory_path_to_directory_index_entry: HashMap<LocalOrRemotePath, usize>,
    folder_matches: NameGroup,
    file_matches: NameGroup,
    /// The search whose name matches are still awaited. Tracked apart from `current_search_id`
    /// because name matching outlives a text search that is cut short.
    pending_name_search_id: Option<u32>,
    is_text_matches_collapsed: bool,
    text_matches_header_mouse_state: MouseStateHandle,
    selected_row: Option<ResultRow>,
    total_match_count: usize,
    is_search_in_progress: bool,
    capped_matches: bool,
    last_error: Option<String>,
    /// When the current search started, for completion telemetry.
    search_started_at: Option<Instant>,
    active_search_remote_host_count: usize,
    scroll_state: ScrollStateHandle,
    uniform_list_state: UniformListState,
    handle: WeakViewHandle<GlobalSearchView>,
    focus_mode: FocusMode,
    enablement: CodingPanelEnablementState,
    current_search_id: Option<u32>,
    regex_search_enabled: bool,
    regex_button: ViewHandle<ActionButton>,
    case_sensitivity_enabled: bool,
    case_sensitivity_button: ViewHandle<ActionButton>,
    whole_word_enabled: bool,
    whole_word_button: ViewHandle<ActionButton>,
    /// The patterns of the files to search, which narrow the search even while hidden.
    include_editor: ViewHandle<EditorView>,
    exclude_editor: ViewHandle<EditorView>,
    is_scope_open: bool,
    scope_button: ViewHandle<ActionButton>,
    /// What narrowed the last search, to search again once it changes.
    last_searched_options: Option<SearchOptions>,
    /// What the search that found the current results looked for.
    searched_spec: Option<SearchSpec>,
    replace: ReplaceUi,
}

impl Entity for GlobalSearchView {
    type Event = Event;
}

impl TypedActionView for GlobalSearchView {
    type Action = GlobalSearchAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            GlobalSearchAction::SelectRow {
                directory_path,
                file_path,
                match_index,
            } => {
                if let Some(row_index) =
                    self.path_to_row_index(directory_path, file_path, *match_index)
                {
                    self.set_selected_row(ResultRow::TextMatch(row_index), ctx);
                    self.enter_results_mode(ctx);
                }
            }
            GlobalSearchAction::ToggleFileCollapsed {
                directory_path,
                file_path,
            } => {
                self.toggle_file_collapsed(directory_path, file_path, ctx);
            }
            GlobalSearchAction::ToggleDirectoryCollapsed { directory_path } => {
                self.toggle_directory_collapsed(directory_path, ctx);
            }
            GlobalSearchAction::OpenMatch {
                location,
                line_number,
                column_num,
            } => {
                ctx.emit(Event::OpenMatch {
                    location: location.clone(),
                    line_number: *line_number,
                    column_num: *column_num,
                });
                self.enter_query_mode(ctx);
            }
            GlobalSearchAction::ClickNameGroupRow { kind, row } => {
                self.set_selected_row(
                    ResultRow::NameGroup {
                        kind: *kind,
                        row: *row,
                    },
                    ctx,
                );
                self.enter_results_mode(ctx);
                self.activate_selected_row(ctx);
                match row {
                    // Hand focus back to the query, as opening a text match does.
                    NameGroupRow::Match(_) => self.enter_query_mode(ctx),
                    NameGroupRow::Header | NameGroupRow::HiddenMatches => {}
                }
            }
            GlobalSearchAction::ClickTextMatchesHeader => {
                self.set_selected_row(ResultRow::TextMatchesHeader, ctx);
                self.enter_results_mode(ctx);
                self.toggle_text_matches_collapsed(ctx);
            }
            GlobalSearchAction::ResultsDown => {
                if !matches!(self.focus_mode, FocusMode::ResultsList) {
                    return;
                }
                if !self.has_results() {
                    return;
                }
                self.move_selection(SelectionDirection::Down, ctx);
            }
            GlobalSearchAction::ResultsUp => {
                if !matches!(self.focus_mode, FocusMode::ResultsList) {
                    return;
                }
                if !self.has_results() {
                    return;
                }
                if self.is_first_row_selected() {
                    self.enter_query_mode(ctx);
                } else {
                    self.move_selection(SelectionDirection::Up, ctx);
                }
            }
            GlobalSearchAction::ResultsLeft => {
                if !matches!(self.focus_mode, FocusMode::ResultsList) {
                    return;
                }
                self.set_selected_row_collapsed(true, ctx);
            }
            GlobalSearchAction::ResultsRight => {
                if !matches!(self.focus_mode, FocusMode::ResultsList) {
                    return;
                }
                self.set_selected_row_collapsed(false, ctx);
            }
            GlobalSearchAction::ResultsEnter => {
                if !matches!(self.focus_mode, FocusMode::ResultsList) {
                    return;
                }
                self.ensure_selection(ctx);
                if self.selected_row.is_some() {
                    self.activate_selected_row(ctx);
                }
            }
            GlobalSearchAction::FocusQueryEditor => {
                self.enter_query_mode(ctx);
            }
            GlobalSearchAction::FocusResultsList => {
                self.enter_results_mode(ctx);
            }
            GlobalSearchAction::ToggleRegexSearch => {
                self.regex_search_enabled = !self.regex_search_enabled;
                self.regex_button.update(ctx, |button, ctx| {
                    button.set_active(self.regex_search_enabled, ctx);
                });
                self.rerun_search_from_query(ctx, true);
            }
            GlobalSearchAction::ToggleCaseSensitivity => {
                self.case_sensitivity_enabled = !self.case_sensitivity_enabled;
                self.case_sensitivity_button.update(ctx, |button, ctx| {
                    button.set_active(self.case_sensitivity_enabled, ctx);
                });
                self.rerun_search_from_query(ctx, true);
            }
            GlobalSearchAction::ToggleWholeWord => {
                self.whole_word_enabled = !self.whole_word_enabled;
                self.whole_word_button.update(ctx, |button, ctx| {
                    button.set_active(self.whole_word_enabled, ctx);
                });
                self.rerun_search_from_query(ctx, true);
            }
            GlobalSearchAction::ToggleScopeOpen => {
                self.is_scope_open = !self.is_scope_open;
                self.update_scope_button(ctx);
                ctx.notify();
            }
            GlobalSearchAction::ToggleReplaceOpen => self.toggle_replace_open(ctx),
            GlobalSearchAction::TogglePreserveCase => self.toggle_preserve_case(ctx),
            GlobalSearchAction::ToggleMatchDismissed {
                location,
                line_number,
                column,
            } => self.toggle_match_dismissed(location, *line_number, *column, ctx),
            GlobalSearchAction::ToggleFileDismissed { location } => {
                self.toggle_file_dismissed(location, ctx);
            }
            GlobalSearchAction::ReplaceAll => self.confirm_replace_all(ctx),
            GlobalSearchAction::ReplaceInFile { location } => self.replace_in_file(location, ctx),
            GlobalSearchAction::ReplaceMatch {
                location,
                line_number,
                column,
            } => self.replace_match(location, *line_number, *column, ctx),
            GlobalSearchAction::CancelReplace => self.cancel_replace(ctx),
            GlobalSearchAction::UndoLastReplace => self.undo_last_replace(ctx),
            GlobalSearchAction::DismissReplaceSummary => self.dismiss_run_summary(ctx),
        }
    }
}

impl GlobalSearchView {
    /// Collapses or expands the section, directory or file whose header row is selected. Does
    /// nothing when the header is already in that state or another kind of row is selected.
    fn set_selected_row_collapsed(&mut self, collapsed: bool, ctx: &mut ViewContext<Self>) {
        let Some(selected) = self.selected_row.clone() else {
            return;
        };

        match selected {
            ResultRow::NameGroup { kind, row } => match row {
                NameGroupRow::Header => {
                    if self.name_group(kind).is_collapsed() != collapsed {
                        self.toggle_name_group_collapsed(kind, ctx);
                    }
                }
                NameGroupRow::Match(_) | NameGroupRow::HiddenMatches => {}
            },
            ResultRow::TextMatchesHeader => {
                if self.is_text_matches_collapsed != collapsed {
                    self.toggle_text_matches_collapsed(ctx);
                }
            }
            ResultRow::TextMatch(selected) => match &selected.index_type {
                RowIndexType::DirectoryHeader => {
                    if self.is_directory_collapsed(&selected) != collapsed
                        && let Some(dir_path) =
                            self.directory_path_for_row_index(&selected).cloned()
                    {
                        self.toggle_directory_collapsed(&dir_path, ctx);
                    }
                }
                RowIndexType::FileHeader { .. } => {
                    if self.is_file_collapsed(&selected) != collapsed
                        && let (Some(dir_path), Some(file_path)) = (
                            self.directory_path_for_row_index(&selected).cloned(),
                            self.file_path_for_row_index(&selected).cloned(),
                        )
                    {
                        self.toggle_file_collapsed(&dir_path, &file_path, ctx);
                    }
                }
                RowIndexType::Match { .. } => {}
            },
        }
    }

    /// Convert submatch byte ranges into character indices for highlighting.
    fn highlight_indices_from_submatches(line_text: &str, submatches: &[Submatch]) -> Vec<usize> {
        if line_text.is_empty() || submatches.is_empty() {
            return Vec::new();
        }

        let max_byte = ByteOffset::from(line_text.len());
        let total_chars = line_text.chars().count();
        let mut indices = Vec::new();

        let mut char_counter = CharCounter::new(line_text);
        for submatch in submatches {
            let start_b = submatch.byte_start.min(max_byte);
            let end_b = submatch.byte_end.min(max_byte);
            if start_b >= end_b {
                continue;
            }

            // NOTE: Ripgrep submatch offsets are byte-based (and `end_b` is exclusive). If a match
            // ends at the end of the string, `end_b == line_text.len()` and there is no character
            // starting at that byte offset; `CharCounter::char_offset(end_b)` would return `None`.
            // Handle that case explicitly.
            let Some(start_char) = char_counter.char_offset(start_b) else {
                continue;
            };

            let end_char = if end_b == max_byte {
                total_chars
            } else {
                let Some(end_char) = char_counter.char_offset(end_b) else {
                    continue;
                };
                end_char.as_usize()
            };

            let start_char = start_char.as_usize();
            if start_char >= end_char {
                continue;
            }

            indices.extend(start_char..end_char);
        }

        indices.sort_unstable();
        indices
    }

    fn elide_match_text(
        line_text: &str,
        mut highlight_indices: Vec<usize>,
    ) -> (String, Vec<usize>) {
        if line_text.is_empty() || highlight_indices.is_empty() {
            return (line_text.to_owned(), highlight_indices);
        }

        let total_chars = line_text.chars().count();
        if total_chars <= DO_NOT_TRUNCATE_CHAR_COUNT {
            return (line_text.to_owned(), highlight_indices);
        }

        highlight_indices.sort_unstable();
        let first_highlight = highlight_indices[0];

        let snippet_start_char = first_highlight.saturating_sub(PRE_MATCH_CHARS);
        let snippet_end_char = (snippet_start_char + DO_TRUNCATE_END_CHAR_COUNT).min(total_chars);

        let prefix_ellipsis = snippet_start_char > 0;
        let suffix_ellipsis = snippet_end_char < total_chars;

        let snippet_core: String = line_text
            .chars()
            .skip(snippet_start_char)
            .take(snippet_end_char.saturating_sub(snippet_start_char))
            .collect();

        let mut snippet = String::new();
        if prefix_ellipsis {
            snippet.push('…');
        }
        snippet.push_str(&snippet_core);
        if suffix_ellipsis {
            snippet.push('…');
        }

        let prefix_offset = if prefix_ellipsis { 1 } else { 0 };
        highlight_indices.retain(|&idx| idx >= snippet_start_char && idx < snippet_end_char);
        for idx in &mut highlight_indices {
            *idx = idx.saturating_sub(snippet_start_char) + prefix_offset;
        }

        (snippet, highlight_indices)
    }

    pub fn init(app: &mut AppContext) {
        use warpui::keymap::macros::*;

        app.register_fixed_bindings([
            FixedBinding::new(
                "up",
                GlobalSearchAction::ResultsUp,
                id!(GlobalSearchView::ui_name()),
            ),
            FixedBinding::new(
                "down",
                GlobalSearchAction::ResultsDown,
                id!(GlobalSearchView::ui_name()),
            ),
            FixedBinding::new(
                "left",
                GlobalSearchAction::ResultsLeft,
                id!(GlobalSearchView::ui_name()),
            ),
            FixedBinding::new(
                "right",
                GlobalSearchAction::ResultsRight,
                id!(GlobalSearchView::ui_name()),
            ),
            FixedBinding::new(
                "enter",
                GlobalSearchAction::ResultsEnter,
                id!(GlobalSearchView::ui_name()),
            ),
        ]);
    }

    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let find_model = ctx.add_model(|_| GlobalSearch::new());

        let query_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = EditorOptions {
                text: TextOptions::ui_text(Some(14.), appearance),
                select_all_on_focus: true,
                clear_selections_on_blur: true,
                propagate_and_no_op_vertical_navigation_keys:
                    PropagateAndNoOpNavigationKeys::AtBoundary,
                propagate_horizontal_navigation_keys: PropagateHorizontalNavigationKeys::Always,
                // Preserve newlines when pasting so users can search for multi-line patterns.
                convert_newline_to_space: false,
                single_line: false,
                autogrow: true,
                // Prefer explicit newlines (from paste or shift-enter) over soft wrapping.
                soft_wrap: false,
                ..Default::default()
            };

            let mut editor = EditorView::new(options, ctx);
            editor.set_placeholder_text("Search text, files and folders", ctx);
            editor
        });

        let (query_change_tx, query_change_rx) = async_channel::unbounded();
        ctx.spawn_stream_local(
            debounce(QUERY_DEBOUNCE_PERIOD, query_change_rx),
            Self::handle_debounced_query_change,
            |_, _| {},
        );

        let case_sensitivity_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new_with_boxed_theme(String::new(), Arc::new(NakedTheme))
                .with_icon(UiIcon::CaseSensitivity)
                .with_tooltip("Toggle Case Sensitivity")
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::ToggleCaseSensitivity);
                })
        });

        let regex_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new_with_boxed_theme(String::new(), Arc::new(NakedTheme))
                .with_icon(UiIcon::Regex)
                .with_tooltip("Toggle Regex")
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::ToggleRegexSearch);
                })
        });

        let whole_word_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new_with_boxed_theme(String::new(), Arc::new(NakedTheme))
                .with_icon(UiIcon::WholeWord)
                .with_tooltip("Toggle Whole Word")
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::ToggleWholeWord);
                })
        });

        let scope_button = ctx.add_typed_action_view(|_ctx| {
            ActionButton::new_with_boxed_theme(String::new(), Arc::new(NakedTheme))
                .with_icon(UiIcon::FilterFunnel)
                .with_tooltip("Toggle Files to Include and Exclude")
                .with_size(ButtonSize::Small)
                .on_click(|ctx| {
                    ctx.dispatch_typed_action(GlobalSearchAction::ToggleScopeOpen);
                })
        });

        let include_editor = Self::add_field_editor("Files to include, e.g. src/**/*.ts", ctx);
        let exclude_editor = Self::add_field_editor("Files to exclude, e.g. **/dist/**", ctx);
        for scope_editor in [&include_editor, &exclude_editor] {
            ctx.subscribe_to_view(scope_editor, |me, _handle, event, ctx| {
                me.handle_scope_editor_event(event, ctx);
            });
        }

        ctx.subscribe_to_view(&query_editor, |me, _handle, event, ctx| {
            me.handle_query_editor_event(event, ctx);
        });

        ctx.subscribe_to_model(&find_model, |me, _handle, event, ctx| {
            me.handle_find_model_event(event, ctx);
        });
        let replace = ReplaceUi::new(ctx);
        let handle = ctx.handle();

        GlobalSearchView {
            find_model,
            query_editor,
            query_change_tx,
            root_directories: Vec::new(),
            search_roots: Vec::new(),
            last_searched_pattern: None,
            directory_entries: Vec::new(),
            directory_path_to_directory_index_entry: HashMap::new(),
            folder_matches: NameGroup::empty(NameMatchKind::Folder),
            file_matches: NameGroup::empty(NameMatchKind::File),
            pending_name_search_id: None,
            is_text_matches_collapsed: false,
            text_matches_header_mouse_state: MouseStateHandle::default(),
            selected_row: None,
            total_match_count: 0,
            is_search_in_progress: false,
            capped_matches: false,
            last_error: None,
            search_started_at: None,
            active_search_remote_host_count: 0,
            scroll_state: ScrollStateHandle::default(),
            uniform_list_state: UniformListState::new(),
            handle,
            focus_mode: FocusMode::QueryEditor,
            enablement: CodingPanelEnablementState::Enabled,
            current_search_id: None,
            regex_search_enabled: false,
            regex_button,
            case_sensitivity_enabled: false,
            case_sensitivity_button,
            whole_word_enabled: false,
            whole_word_button,
            include_editor,
            exclude_editor,
            is_scope_open: false,
            scope_button,
            last_searched_options: None,
            searched_spec: None,
            replace,
        }
    }

    /// Adds a single-line editor for one of the fields below the query.
    fn add_field_editor(
        placeholder: &'static str,
        ctx: &mut ViewContext<Self>,
    ) -> ViewHandle<EditorView> {
        ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(Some(14.), appearance),
                    select_all_on_focus: true,
                    clear_selections_on_blur: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text(placeholder, ctx);
            editor
        })
    }

    pub fn on_left_panel_focused(
        &mut self,
        entry_focus: GlobalSearchEntryFocus,
        ctx: &mut ViewContext<Self>,
    ) {
        match entry_focus {
            GlobalSearchEntryFocus::QueryEditor => {
                self.enter_query_mode(ctx);
            }
            GlobalSearchEntryFocus::Results => {
                if !self.has_results() {
                    self.enter_query_mode(ctx);
                } else {
                    self.enter_results_mode(ctx);
                }
            }
        }
    }

    fn set_query_mode_state(&mut self, ctx: &mut ViewContext<Self>) {
        self.focus_mode = FocusMode::QueryEditor;
        self.query_editor.update(ctx, |editor, ctx| {
            editor.set_interaction_state(InteractionState::Editable, ctx);
        });
        self.selected_row = None;
        ctx.notify();
    }

    /// Returns an iterator over all directory locations where the given file should appear.
    /// A file matches a directory if the file location starts with that directory
    /// (remote files only match directories on the same host).
    fn find_matching_directories<'a>(
        &'a self,
        location: &'a LocalOrRemotePath,
    ) -> impl Iterator<Item = &'a LocalOrRemotePath> {
        self.root_directories
            .iter()
            .filter(move |root| location.starts_with(root))
    }

    fn apply_progress_item(&mut self, result: GlobalSearchMatch, ctx: &mut ViewContext<Self>) {
        if self.total_match_count >= MAX_MATCH_COUNT {
            return;
        }

        let location = result.location.clone();

        // Find all directories that this file belongs to
        let mut matching_directories = self.find_matching_directories(&location).peekable();
        if matching_directories.peek().is_none() {
            // File doesn't match any root directory, skip it
            let file_path_name = location.file_name().unwrap_or("<unknown>");
            log::warn!("[Global search] file {file_path_name} was not found in directories");
            return;
        }
        let matching_directories: Vec<_> = matching_directories.cloned().collect();

        // Populate hierarchical data model (directory_entries)
        let (directory_entries, directory_path_to_directory_index_entry) = (
            &mut self.directory_entries,
            &mut self.directory_path_to_directory_index_entry,
        );

        for directory_path in &matching_directories {
            // Get or create the directory entry
            let dir_index = *directory_path_to_directory_index_entry
                .entry(directory_path.clone())
                .or_insert_with(|| {
                    let idx = directory_entries.len();
                    directory_entries.push(DirectoryEntry::new(directory_path.clone()));
                    idx
                });

            // Get or create the matched path entry within this directory
            let dir_entry = &mut directory_entries[dir_index];
            let (matched_path, _path_index) = dir_entry.matched_paths.get_or_create(&location);

            // Add the match
            matched_path.matches.push(Match::new(
                result.line_text.clone(),
                result.line_number,
                result.column_num,
                result.submatches.clone(),
            ));
        }

        self.total_match_count += 1;

        if self.total_match_count == MAX_MATCH_COUNT {
            self.replace.truncated_file = location.to_local_path().map(Path::to_path_buf);
            self.abort_search(ctx);
        }
    }
    /// Stops collecting text matches once there are too many of them. Name matching for the same
    /// search carries on.
    fn abort_search(&mut self, ctx: &mut ViewContext<Self>) {
        self.capped_matches = true;
        self.stop_tracking_text_search();

        self.find_model.update(ctx, |model, model_ctx| {
            model.abort_content_search(model_ctx);
        });
    }

    fn stop_tracking_text_search(&mut self) {
        self.is_search_in_progress = false;
        self.current_search_id = None;
        self.search_started_at = None;
    }

    fn cancel_search(&mut self, ctx: &mut ViewContext<Self>) {
        self.stop_tracking_text_search();
        self.pending_name_search_id = None;

        self.find_model.update(ctx, |model, model_ctx| {
            model.abort_search(model_ctx);
        });
    }
    fn handle_debounced_query_change(&mut self, _event: (), ctx: &mut ViewContext<Self>) {
        self.rerun_search_from_query(ctx, false);
    }

    fn handle_query_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Navigate(NavigationKey::Down) => {
                if !matches!(self.focus_mode, FocusMode::QueryEditor) {
                    return;
                }
                if !self.has_results() {
                    return;
                }
                self.enter_results_mode(ctx);
            }
            EditorEvent::Edited(_)
            | EditorEvent::BufferReplaced
            | EditorEvent::BufferReinitialized => {
                self.replace.scroll_top_to_restore = None;
                self.replace.keep_dismissals_once = false;
                let query_text = self.query_editor.as_ref(ctx).buffer_text(ctx);
                if query_text.is_empty() {
                    self.cancel_search(ctx);
                    self.reset_search_state(true);
                    self.refresh_replace_controls(ctx);
                    return;
                }

                self.notify_query_changed();
                self.refresh_replace_controls(ctx);
            }
            EditorEvent::Enter => {
                self.rerun_search_from_query(ctx, true);
            }
            _ => {}
        }
    }

    fn handle_scope_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Focused => self.on_field_editor_focused(ctx),
            EditorEvent::Edited(_)
            | EditorEvent::BufferReplaced
            | EditorEvent::BufferReinitialized => {
                self.update_scope_button(ctx);
                self.notify_query_changed();
                self.refresh_replace_controls(ctx);
            }
            EditorEvent::Enter => {
                self.rerun_search_from_query(ctx, true);
            }
            _ => {}
        }
    }

    /// Keeps the results list from acting on keys typed into a field below the query, which
    /// takes the focus without going through the query editor.
    fn on_field_editor_focused(&mut self, ctx: &mut ViewContext<Self>) {
        if !matches!(self.focus_mode, FocusMode::QueryEditor) {
            self.set_query_mode_state(ctx);
        }
    }

    /// The scope narrows the search even while its fields are hidden, so the button that shows
    /// them stays lit while they hold anything.
    fn update_scope_button(&mut self, ctx: &mut ViewContext<Self>) {
        let is_active = self.is_scope_open || !self.search_scope(ctx).is_empty();
        self.scope_button.update(ctx, |button, ctx| {
            button.set_active(is_active, ctx);
        });
    }

    fn search_scope(&self, app: &AppContext) -> ScopeGlobs {
        ScopeGlobs {
            include: self.include_editor.as_ref(app).buffer_text(app),
            exclude: self.exclude_editor.as_ref(app).buffer_text(app),
        }
    }

    fn search_options(&self, app: &AppContext) -> SearchOptions {
        SearchOptions {
            whole_word: self.whole_word_enabled,
            scope: self.search_scope(app),
        }
    }

    /// Limits the search to the folder at `folder`, by naming it in the files to include.
    pub fn search_in_folder(&mut self, folder: &Path, ctx: &mut ViewContext<Self>) {
        // A pattern relative to the search root reads best, but with several roots it would
        // stand for the folder of that name in each of them.
        let root = match self.search_roots.as_slice() {
            [root] => root.to_local_path(),
            _ => None,
        };
        let pattern = folder_pattern(root, folder);
        self.include_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&pattern, ctx);
        });
        self.is_scope_open = true;
        self.update_scope_button(ctx);
        self.rerun_search_from_query(ctx, true);
        ctx.notify();
    }

    fn notify_query_changed(&self) {
        let _ = self.query_change_tx.try_send(());
    }

    fn clear_text_matches(&mut self) {
        self.directory_entries.clear();
        self.directory_path_to_directory_index_entry.clear();
        self.total_match_count = 0;
        self.capped_matches = false;
        self.replace.truncated_file = None;
        self.replace.search_incomplete = false;
    }

    fn reset_search_state(&mut self, clear_last_pattern: bool) {
        self.clear_text_matches();
        self.folder_matches = NameGroup::empty(NameMatchKind::Folder);
        self.file_matches = NameGroup::empty(NameMatchKind::File);
        self.is_text_matches_collapsed = false;
        self.selected_row = None;
        self.last_error = None;
        self.uniform_list_state = UniformListState::new();
        let keep_dismissals = std::mem::take(&mut self.replace.keep_dismissals_once);
        if clear_last_pattern || !keep_dismissals {
            self.replace.dismissals.clear();
        }

        if clear_last_pattern {
            self.last_searched_pattern = None;
            self.last_searched_options = None;
            self.searched_spec = None;
            self.replace.previewer = None;
        }
    }

    fn rerun_search_from_query(&mut self, ctx: &mut ViewContext<Self>, force: bool) {
        if !matches!(self.focus_mode, FocusMode::QueryEditor) && !force {
            return;
        }

        let pattern = self.query_editor.as_ref(ctx).buffer_text(ctx);
        if pattern.is_empty() {
            self.cancel_search(ctx);
            self.reset_search_state(true);
            ctx.notify();
            return;
        }

        if self.search_roots.is_empty() {
            log::warn!("GlobalSearch: no search roots; skipping search");
            return;
        }

        let options = self.search_options(ctx);
        let should_run_search = force
            || self.last_searched_pattern.as_deref() != Some(pattern.as_str())
            || self.last_searched_options.as_ref() != Some(&options);

        if !should_run_search {
            return;
        }

        self.last_searched_pattern = Some(pattern.clone());
        self.last_searched_options = Some(options.clone());
        self.searched_spec = Some(SearchSpec {
            pattern: pattern.clone(),
            use_regex: self.regex_search_enabled,
            case_sensitive: self.case_sensitivity_enabled,
            whole_word: options.whole_word,
        });

        let roots = self.search_roots.clone();
        self.find_model.update(ctx, |model, model_ctx| {
            model.run_search(
                pattern.clone(),
                roots,
                SearchConfig {
                    use_regex: self.regex_search_enabled,
                    use_case_sensitivity: self.case_sensitivity_enabled,
                },
                options,
                model_ctx,
            );
        });
    }

    fn handle_find_model_event(&mut self, event: &GlobalSearchEvent, ctx: &mut ViewContext<Self>) {
        match event {
            GlobalSearchEvent::Started {
                search_id,
                remote_host_count,
                name_search_started,
            } => {
                send_telemetry_from_ctx!(TelemetryEvent::GlobalSearchQueryStarted, ctx);

                self.current_search_id = Some(*search_id);
                self.pending_name_search_id = name_search_started.then_some(*search_id);
                self.search_started_at = Some(Instant::now());
                self.active_search_remote_host_count = *remote_host_count;

                self.is_search_in_progress = true;
                self.reset_search_state(false);
                self.refresh_replace_preview(ctx);
            }
            GlobalSearchEvent::NameMatches { search_id, results } => {
                if Some(*search_id) != self.pending_name_search_id {
                    return;
                }

                self.pending_name_search_id = None;
                self.folder_matches =
                    NameGroup::new(NameMatchKind::Folder, results.folders.clone());
                self.file_matches = NameGroup::new(NameMatchKind::File, results.files.clone());

                // The groups are inserted above the text matches. A list that was already scrolled
                // moves along with them so the rows on screen stay put; otherwise only the
                // selected row has to be kept in view.
                let inserted_rows =
                    self.folder_matches.visible_count() + self.file_matches.visible_count();
                if self.uniform_list_state.scroll_top().as_f64() > 0. {
                    self.uniform_list_state.add_scroll_top(inserted_rows as f32);
                } else if let Some(index) = self.selected_row_index() {
                    self.uniform_list_state.scroll_to(index);
                }
                ctx.notify();
            }
            GlobalSearchEvent::Progress { search_id, result } => {
                if Some(*search_id) != self.current_search_id {
                    return;
                }

                self.apply_progress_item(result.clone(), ctx);
                if self.capped_matches {
                    self.refresh_replace_controls(ctx);
                }
                ctx.notify();
            }
            GlobalSearchEvent::ProgressBatch { search_id, items } => {
                if Some(*search_id) != self.current_search_id {
                    return;
                }

                for item in items {
                    self.apply_progress_item(item.clone(), ctx);

                    if self.capped_matches {
                        break;
                    }
                }
                if self.capped_matches {
                    self.refresh_replace_controls(ctx);
                }
                ctx.notify();
            }
            GlobalSearchEvent::Completed {
                search_id,
                total_match_count,
                capped,
                local_source_failed,
                remote_source_failures,
            } => {
                if Some(*search_id) != self.current_search_id {
                    return;
                }

                self.is_search_in_progress = false;
                self.total_match_count = *total_match_count;
                self.capped_matches |= capped;
                self.replace.search_incomplete = *local_source_failed;
                if let Some(scroll_top) = self.replace.scroll_top_to_restore.take() {
                    self.uniform_list_state.add_scroll_top(scroll_top);
                }

                if let Some(started_at) = self.search_started_at.take() {
                    send_telemetry_from_ctx!(
                        TelemetryEvent::GlobalSearchQueryCompleted {
                            duration_ms: started_at.elapsed().as_millis() as u64,
                            remote_host_count: self.active_search_remote_host_count,
                            total_match_count: *total_match_count,
                            capped: self.capped_matches,
                            local_source_failed: *local_source_failed,
                            remote_source_failures: *remote_source_failures,
                        },
                        ctx
                    );
                }
                self.refresh_replace_controls(ctx);
            }
            GlobalSearchEvent::Failed { search_id, error } => {
                if Some(*search_id) != self.current_search_id {
                    return;
                }

                self.is_search_in_progress = false;
                self.search_started_at = None;
                // Name matches come from a separate source and stay valid.
                self.clear_text_matches();
                if matches!(
                    self.selected_row,
                    Some(ResultRow::TextMatchesHeader | ResultRow::TextMatch(_))
                ) {
                    self.selected_row = None;
                }
                self.last_error = Some(error.clone());
                self.refresh_replace_controls(ctx);
            }
        }
    }

    pub fn set_root_directories(
        &mut self,
        roots: Vec<LocalOrRemotePath>,
        _ctx: &mut ViewContext<Self>,
    ) {
        // Ancestor-dedup search roots so we don't search the same file twice
        // when terminal directories are nested (e.g. `~/code` + `~/code/a`).
        // Local and remote roots share `group_roots_by_common_ancestor` with
        // `FileTreeView` for consistency; remote roots are grouped per host
        // and deduped within each host independently.
        let local_roots: Vec<PathBuf> = roots
            .iter()
            .filter_map(|root| root.to_local_path().map(Path::to_path_buf))
            .collect();
        let deduped_local = warp_util::path::group_roots_by_common_ancestor(&local_roots).roots;

        let mut remote_roots_by_host: Vec<(HostId, Vec<StandardizedPath>)> = Vec::new();
        for root in &roots {
            let LocalOrRemotePath::Remote(remote) = root else {
                continue;
            };
            match remote_roots_by_host
                .iter_mut()
                .find(|(host_id, _)| host_id == &remote.host_id)
            {
                Some((_, paths)) => paths.push(remote.path.clone()),
                None => {
                    remote_roots_by_host.push((remote.host_id.clone(), vec![remote.path.clone()]))
                }
            }
        }
        let deduped_remote = remote_roots_by_host
            .into_iter()
            .flat_map(|(host_id, paths)| {
                warp_util::path::group_roots_by_common_ancestor(&paths)
                    .roots
                    .into_iter()
                    .map(move |path| {
                        LocalOrRemotePath::Remote(RemotePath::new(host_id.clone(), path))
                    })
            });

        self.search_roots = deduped_local
            .into_iter()
            .map(LocalOrRemotePath::Local)
            .chain(deduped_remote)
            .collect();
        self.root_directories = roots;
    }

    /// Pre-populates the search query with the given text.
    /// Selects all text so the user can easily overwrite or keep it, then triggers a search.
    pub fn set_initial_query(&mut self, text: String, ctx: &mut ViewContext<Self>) {
        self.query_editor.update(ctx, |editor, ctx| {
            editor.set_buffer_text(&text, ctx);
            editor.select_all(ctx);
        });

        // Trigger the debounced search
        self.notify_query_changed();
        ctx.notify();
    }

    pub(crate) fn set_enablement_state(
        &mut self,
        enablement: CodingPanelEnablementState,
        ctx: &mut ViewContext<Self>,
    ) {
        if self.enablement == enablement {
            return;
        }
        self.enablement = enablement;
        ctx.notify();
    }

    fn render_row_at_index(&self, index: usize, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let row_index = match self.result_row_at(index) {
            Some(ResultRow::NameGroup { kind, row }) => {
                return self.render_name_group_row(index, kind, row, appearance, app);
            }
            Some(ResultRow::TextMatchesHeader) => {
                return self.render_group_header(
                    index,
                    "Text matches",
                    self.total_match_count,
                    self.is_text_matches_collapsed,
                    self.text_matches_header_mouse_state.clone(),
                    GlobalSearchAction::ClickTextMatchesHeader,
                    appearance,
                );
            }
            Some(ResultRow::TextMatch(row_index)) => row_index,
            None => return Empty::new().finish(),
        };

        // Get the directory entry
        let Some(dir_entry) = self.directory_entries.get(row_index.directory_index) else {
            return Empty::new().finish();
        };

        match &row_index.index_type {
            RowIndexType::DirectoryHeader => {
                self.render_directory_header_from_entry(index, dir_entry, appearance, theme, app)
            }
            RowIndexType::FileHeader { path_index } => {
                let Some(matched_path) = dir_entry.matched_paths.paths.get(*path_index) else {
                    return Empty::new().finish();
                };
                self.render_file_header(
                    index,
                    &dir_entry.path,
                    matched_path,
                    appearance,
                    theme,
                    app,
                )
            }
            RowIndexType::Match {
                path_index,
                match_index,
            } => {
                let Some(matched_path) = dir_entry.matched_paths.paths.get(*path_index) else {
                    return Empty::new().finish();
                };
                let Some(matched) = matched_path.matches.get(*match_index) else {
                    return Empty::new().finish();
                };
                self.render_match_row(
                    index,
                    &dir_entry.path,
                    matched_path,
                    matched,
                    *match_index,
                    appearance,
                    theme,
                )
            }
        }
    }

    fn render_file_header(
        &self,
        index: usize,
        directory_path: &LocalOrRemotePath,
        matched_path: &MatchedPath,
        appearance: &Appearance,
        theme: &warp_core::ui::theme::WarpTheme,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let is_collapsed = matched_path.is_collapsed;
        let file_mouse_state = matched_path.mouse_state.clone();
        let file_path = matched_path.path.clone();
        let match_count = matched_path.matches.len();
        let is_dismissed = self.is_file_dismissed(matched_path);
        let row_buttons = self.file_row_buttons(matched_path);

        let directory_path_for_select = directory_path.clone();
        let file_path_clone = file_path.clone();
        let directory_path_for_toggle = directory_path.clone();

        let display_path = directory_path
            .strip_repo_prefix(&file_path)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(file_path.display_path()));

        Hoverable::new(file_mouse_state, move |mouse_state| {
            let item_highlight_state = ItemHighlightState::new(is_selected, mouse_state);
            let search_highlight_state = SearchHighlightState::new(is_selected, mouse_state);
            let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);

            let chevron_icon_enum = if is_collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            };
            let icon_size = 16.0;
            let chevron_color = item_highlight_state.text_and_icon_color(appearance);
            let chevron_icon = chevron_icon_enum
                .to_warpui_icon(ThemeFill::from(chevron_color))
                .finish();
            let chevron_icon = ConstrainedBox::new(chevron_icon)
                .with_width(icon_size)
                .with_height(icon_size)
                .finish();
            let chevron_container = Container::new(chevron_icon).with_margin_right(8.).finish();

            let tooltip_text = file_path.display_path();

            let header_text_fill = match list_highlight_state {
                ItemHighlightState::None | ItemHighlightState::Hovered if is_dismissed => {
                    ThemeFill::Solid(blended_colors::text_disabled(theme, theme.background()))
                }
                ItemHighlightState::None => {
                    ThemeFill::Solid(blended_colors::text_sub(theme, theme.background()))
                }
                ItemHighlightState::Hovered => {
                    ThemeFill::Solid(blended_colors::text_main(theme, theme.background()))
                }
                ItemHighlightState::Selected => ThemeFill::Solid(theme.foreground().into()),
            };

            let left = render_file_search_row(
                &display_path,
                FileSearchRowOptions {
                    item_font_size: Some(14.),
                    path_font_size: Some(12.),
                    highlight_state: search_highlight_state,
                    text_color_override: Some(header_text_fill),
                    max_combined_length: None,
                    ..Default::default()
                },
                app,
            );

            let count_text =
                Text::new_inline(match_count.to_string(), appearance.ui_font_family(), 12.0)
                    .with_color(appearance.theme().background().into())
                    .finish();

            let count_badge = Container::new(count_text)
                .with_horizontal_padding(4.)
                .with_vertical_padding(1.)
                .with_background_color(theme.terminal_colors().normal.yellow.into())
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .finish();

            let file_icon = icon_from_file_path(&file_path.display_path(), appearance)
                .unwrap_or_else(default_file_icon);

            let file_icon_element = Container::new(
                ConstrainedBox::new(file_icon)
                    .with_width(icon_size)
                    .with_height(icon_size)
                    .finish(),
            )
            .with_padding_right(8.)
            .finish();

            let left_container = Container::new(left).with_margin_right(8.).finish();

            let left_row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(chevron_container)
                .with_child(file_icon_element)
                .with_child(Shrinkable::new(1.0, left_container).finish())
                .finish();

            let mut trailing = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(count_badge);
            if !row_buttons.is_empty() && (mouse_state.is_hovered() || is_selected) {
                trailing.add_child(Self::render_row_buttons(row_buttons, appearance));
            }

            let header_row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1.0, left_row).finish())
                .with_child(trailing.finish())
                .finish();

            let mut row_with_padding =
                Container::new(header_row).with_padding(Padding::uniform(4.).with_left(8.)); // 8px indent under directory

            if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                let corner_radius = list_highlight_state
                    .corner_radius()
                    .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                row_with_padding = row_with_padding
                    .with_background(background_fill)
                    .with_corner_radius(corner_radius);
            }

            let base_row = row_with_padding.finish();

            // Add a tooltip on hover that shows the full (untruncated) file path.
            if mouse_state.is_hovered() {
                let tooltip = appearance
                    .ui_builder()
                    .tool_tip(tooltip_text)
                    .build()
                    .finish();

                let mut stack = Stack::new().with_child(base_row);
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
            } else {
                base_row
            }
        })
        .with_cursor(Cursor::PointingHand)
        .with_defer_events_to_children()
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::SelectRow {
                directory_path: directory_path_for_select.clone(),
                file_path: file_path_clone.clone(),
                match_index: None,
            });
            ctx.dispatch_typed_action(GlobalSearchAction::ToggleFileCollapsed {
                directory_path: directory_path_for_toggle.clone(),
                file_path: file_path_clone.clone(),
            });
        })
        .finish()
    }

    #[allow(clippy::too_many_arguments)]
    fn render_match_row(
        &self,
        index: usize,
        directory_path: &LocalOrRemotePath,
        matched_path: &MatchedPath,
        matched: &Match,
        match_index: usize,
        appearance: &Appearance,
        theme: &warp_core::ui::theme::WarpTheme,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let line_number = matched.line_number;
        let column_num = matched.column_num;
        let line_text = matched.line_text.clone();
        let submatches = matched.submatches.clone();
        let mouse_state = matched.mouse_state.clone();
        let preview = self.replace_preview(matched_path, matched);
        let is_dismissed = self.is_match_dismissed(matched_path, matched);
        let row_buttons = self.match_row_buttons(matched_path, matched);

        let directory_path_for_select = directory_path.clone();
        let file_path_for_select = matched_path.path.clone();
        let location_for_click = matched_path.path.clone();
        let location_for_double_click = matched_path.path.clone();

        Hoverable::new(mouse_state, move |mouse_state| {
            let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);

            let text_color = match list_highlight_state {
                ItemHighlightState::None | ItemHighlightState::Hovered if is_dismissed => {
                    blended_colors::text_disabled(theme, theme.background())
                }
                ItemHighlightState::None => blended_colors::text_sub(theme, theme.background()),
                ItemHighlightState::Hovered => blended_colors::text_main(theme, theme.background()),
                ItemHighlightState::Selected => theme.foreground().into(),
            };
            let font_size = appearance.ui_font_size() + 2.;
            let overlay = |color: AnsiColorIdentifier| {
                theme.ansi_overlay_2(color.to_ansi_color(&theme.terminal_colors().normal))
            };

            let match_text = match preview {
                // The matched text is struck out and followed by what replaces it.
                Some(preview) => {
                    let removed = Highlight::new().with_text_style(
                        TextStyle::new()
                            .with_background_color(overlay(AnsiColorIdentifier::Red))
                            .with_show_strikethrough(true),
                    );
                    let inserted = Highlight::new().with_text_style(
                        TextStyle::new().with_background_color(overlay(AnsiColorIdentifier::Green)),
                    );
                    let mut text =
                        Text::new_inline(preview.text, appearance.ui_font_family(), font_size)
                            .with_color(text_color);
                    if !preview.removed.is_empty() {
                        text = text.with_single_highlight(removed, preview.removed.collect());
                    }
                    if !preview.inserted.is_empty() {
                        text = text.with_single_highlight(inserted, preview.inserted.collect());
                    }
                    text
                }
                None => {
                    let highlight_indices = GlobalSearchView::highlight_indices_from_submatches(
                        &line_text,
                        &submatches,
                    );
                    let (display_text, display_highlight_indices) =
                        GlobalSearchView::elide_match_text(&line_text, highlight_indices);

                    let mut text =
                        Text::new_inline(display_text, appearance.ui_font_family(), font_size)
                            .with_color(text_color);
                    if !display_highlight_indices.is_empty() {
                        let highlight = Highlight::new().with_text_style(
                            TextStyle::new()
                                .with_background_color(overlay(AnsiColorIdentifier::Yellow)),
                        );
                        text = text.with_single_highlight(highlight, display_highlight_indices);
                    }
                    text
                }
            };

            let content = if !row_buttons.is_empty() && (mouse_state.is_hovered() || is_selected) {
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1.0, match_text.finish()).finish())
                    .with_child(Self::render_row_buttons(row_buttons, appearance))
                    .finish()
            } else {
                match_text.finish()
            };

            let mut row = Container::new(content)
                .with_vertical_padding(4.)
                .with_padding_left(32.) // 8px file indent + 24px (chevron + margin) to align with file icon
                .with_padding_right(12.);

            if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                let corner_radius = list_highlight_state
                    .corner_radius()
                    .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                row = row
                    .with_background(background_fill)
                    .with_corner_radius(corner_radius);
            }

            row.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .with_defer_events_to_children()
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::SelectRow {
                directory_path: directory_path_for_select.clone(),
                file_path: file_path_for_select.clone(),
                match_index: Some(match_index),
            });
            ctx.dispatch_typed_action(GlobalSearchAction::OpenMatch {
                location: location_for_click.clone(),
                line_number,
                column_num,
            });
        })
        // The first click of the pair has already opened the match as a preview.
        .on_double_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::OpenMatch {
                location: location_for_double_click.clone(),
                line_number,
                column_num,
            });
        })
        .finish()
    }

    fn items_count(&self) -> usize {
        self.section_counts().total()
    }

    fn has_results(&self) -> bool {
        self.items_count() > 0
    }

    fn section_counts(&self) -> SectionCounts {
        SectionCounts {
            folders: self.folder_matches.visible_count(),
            files: self.file_matches.visible_count(),
            text_matches: (!self.directory_entries.is_empty()).then(|| {
                if self.is_text_matches_collapsed {
                    0
                } else {
                    self.items_count_from_entries()
                }
            }),
        }
    }

    fn name_group(&self, kind: NameMatchKind) -> &NameGroup {
        match kind {
            NameMatchKind::Folder => &self.folder_matches,
            NameMatchKind::File => &self.file_matches,
        }
    }

    fn name_group_mut(&mut self, kind: NameMatchKind) -> &mut NameGroup {
        match kind {
            NameMatchKind::Folder => &mut self.folder_matches,
            NameMatchKind::File => &mut self.file_matches,
        }
    }

    /// The row at `index` in the flattened results list.
    fn result_row_at(&self, index: usize) -> Option<ResultRow> {
        let name_group_row = |kind| move |row| ResultRow::NameGroup { kind, row };
        match self.section_counts().row_at(index)? {
            SectionRow::Folders(offset) => self
                .folder_matches
                .row_at(offset)
                .map(name_group_row(NameMatchKind::Folder)),
            SectionRow::Files(offset) => self
                .file_matches
                .row_at(offset)
                .map(name_group_row(NameMatchKind::File)),
            SectionRow::TextMatchesHeader => Some(ResultRow::TextMatchesHeader),
            SectionRow::TextMatch(offset) => self
                .to_row_index(GlobalIndex(offset))
                .map(ResultRow::TextMatch),
        }
    }

    /// The index of `row` in the flattened results list, if the row exists and is visible.
    fn result_row_index(&self, row: &ResultRow) -> Option<usize> {
        let section_row = match row {
            ResultRow::NameGroup { kind, row } => {
                let offset = self.name_group(*kind).offset_of(*row)?;
                match kind {
                    NameMatchKind::Folder => SectionRow::Folders(offset),
                    NameMatchKind::File => SectionRow::Files(offset),
                }
            }
            ResultRow::TextMatchesHeader => SectionRow::TextMatchesHeader,
            ResultRow::TextMatch(row_index) => {
                // Validate the row still exists in the data model
                let dir_entry = self.directory_entries.get(row_index.directory_index)?;
                match &row_index.index_type {
                    RowIndexType::DirectoryHeader => {}
                    RowIndexType::FileHeader { path_index }
                    | RowIndexType::Match { path_index, .. } => {
                        // Check path exists
                        dir_entry.matched_paths.paths.get(*path_index)?;
                    }
                }
                SectionRow::TextMatch(self.to_global_index(row_index))
            }
        };
        self.section_counts().index_of(section_row)
    }

    /// Converts a flat index among the text match rows to a hierarchical RowIndex.
    /// Returns None if the index is out of bounds.
    fn to_row_index(&self, global: GlobalIndex) -> Option<RowIndex> {
        let mut running_count = 0;
        for (dir_idx, dir_entry) in self.directory_entries.iter().enumerate() {
            let dir_visible = dir_entry.visible_count();
            if running_count + dir_visible > global.0 {
                // The target row is within this directory
                let offset_within_dir = global.0 - running_count;
                return self.find_row_within_directory(dir_idx, offset_within_dir);
            }
            running_count += dir_visible;
        }
        None
    }

    /// Helper to find a specific row within a directory given an offset.
    /// offset=0 means the directory header itself.
    fn find_row_within_directory(&self, dir_idx: usize, offset: usize) -> Option<RowIndex> {
        let dir_entry = self.directory_entries.get(dir_idx)?;

        // offset 0 is the directory header
        if offset == 0 {
            return Some(RowIndex {
                directory_index: dir_idx,
                index_type: RowIndexType::DirectoryHeader,
            });
        }

        // If directory is collapsed, only the header is visible
        if dir_entry.is_collapsed {
            return None;
        }

        // Skip the directory header
        let mut remaining = offset - 1;

        for (path_idx, matched_path) in dir_entry.matched_paths.paths.iter().enumerate() {
            let path_visible = matched_path.visible_count();
            if remaining < path_visible {
                // The target row is within this file
                if remaining == 0 {
                    // It's the file header
                    return Some(RowIndex {
                        directory_index: dir_idx,
                        index_type: RowIndexType::FileHeader {
                            path_index: path_idx,
                        },
                    });
                }
                // It's a match row (remaining - 1 because we skip the file header)
                let match_index = remaining - 1;
                if match_index < matched_path.matches.len() {
                    return Some(RowIndex {
                        directory_index: dir_idx,
                        index_type: RowIndexType::Match {
                            path_index: path_idx,
                            match_index,
                        },
                    });
                }
                return None;
            }
            remaining -= path_visible;
        }

        None
    }

    /// Converts a hierarchical RowIndex to a flat index among the text match rows.
    fn to_global_index(&self, row: &RowIndex) -> usize {
        let mut running_count = 0;

        // Sum up all visible rows from directories before the target
        for dir_entry in self.directory_entries.iter().take(row.directory_index) {
            running_count += dir_entry.visible_count();
        }

        // Add the offset within the target directory
        running_count + self.offset_within_directory(row)
    }

    /// Calculates the offset of a RowIndex within its directory.
    fn offset_within_directory(&self, row: &RowIndex) -> usize {
        match &row.index_type {
            RowIndexType::DirectoryHeader => 0,
            RowIndexType::FileHeader { path_index } => {
                let Some(dir_entry) = self.directory_entries.get(row.directory_index) else {
                    debug_assert!(
                        false,
                        "RowIndex.directory_index {} out of bounds",
                        row.directory_index
                    );
                    return 0;
                };
                // 1 for directory header + sum of visible counts before this file
                1 + dir_entry
                    .matched_paths
                    .paths
                    .iter()
                    .take(*path_index)
                    .map(|p| p.visible_count())
                    .sum::<usize>()
            }
            RowIndexType::Match {
                path_index,
                match_index,
            } => {
                let Some(dir_entry) = self.directory_entries.get(row.directory_index) else {
                    debug_assert!(
                        false,
                        "RowIndex.directory_index {} out of bounds",
                        row.directory_index
                    );
                    return 0;
                };
                // 1 for directory header + sum of visible counts before this file
                // + 1 for file header + match_index
                1 + dir_entry
                    .matched_paths
                    .paths
                    .iter()
                    .take(*path_index)
                    .map(|p| p.visible_count())
                    .sum::<usize>()
                    + 1
                    + match_index
            }
        }
    }

    /// Returns the total number of visible text match rows
    fn items_count_from_entries(&self) -> usize {
        self.directory_entries
            .iter()
            .map(|d| d.visible_count())
            .sum()
    }

    /// Returns the total number of unique matches across all directories.
    fn unique_match_count(&self) -> usize {
        self.directory_entries
            .iter()
            .map(|d| d.matched_paths.paths.len())
            .sum()
    }

    /// Gets or creates a DirectoryEntry for the given location.
    /// Returns a mutable reference to the entry and its index.
    #[allow(dead_code)] // Will be used in later PRs
    fn get_or_create_directory_entry(
        &mut self,
        path: &LocalOrRemotePath,
    ) -> (&mut DirectoryEntry, usize) {
        if let Some(&index) = self.directory_path_to_directory_index_entry.get(path) {
            (&mut self.directory_entries[index], index)
        } else {
            let index = self.directory_entries.len();
            self.directory_entries
                .push(DirectoryEntry::new(path.clone()));
            self.directory_path_to_directory_index_entry
                .insert(path.clone(), index);
            (&mut self.directory_entries[index], index)
        }
    }

    /// Converts directory_path + file_path + optional match_index to a RowIndex.
    /// Returns None if the paths are not found
    fn path_to_row_index(
        &self,
        directory_path: &LocalOrRemotePath,
        file_path: &LocalOrRemotePath,
        match_index: Option<usize>,
    ) -> Option<RowIndex> {
        let &directory_index = self
            .directory_path_to_directory_index_entry
            .get(directory_path)?;
        let dir_entry = self.directory_entries.get(directory_index)?;
        let &path_index = dir_entry.matched_paths.index_by_path.get(file_path)?;

        let index_type = match match_index {
            None => RowIndexType::FileHeader { path_index },
            Some(match_idx) => RowIndexType::Match {
                path_index,
                match_index: match_idx,
            },
        };

        Some(RowIndex {
            directory_index,
            index_type,
        })
    }

    /// Gets the directory location for a given RowIndex.
    fn directory_path_for_row_index(&self, row: &RowIndex) -> Option<&LocalOrRemotePath> {
        self.directory_entries
            .get(row.directory_index)
            .map(|e| &e.path)
    }

    /// Gets the file location for a given RowIndex (if it refers to a file or match).
    fn file_path_for_row_index(&self, row: &RowIndex) -> Option<&LocalOrRemotePath> {
        let dir_entry = self.directory_entries.get(row.directory_index)?;
        match &row.index_type {
            RowIndexType::DirectoryHeader => None,
            RowIndexType::FileHeader { path_index } | RowIndexType::Match { path_index, .. } => {
                dir_entry
                    .matched_paths
                    .paths
                    .get(*path_index)
                    .map(|p| &p.path)
            }
        }
    }

    fn is_file_collapsed(&self, row: &RowIndex) -> bool {
        let Some(dir_entry) = self.directory_entries.get(row.directory_index) else {
            return false;
        };
        match &row.index_type {
            RowIndexType::DirectoryHeader => false,
            RowIndexType::FileHeader { path_index } | RowIndexType::Match { path_index, .. } => {
                dir_entry
                    .matched_paths
                    .paths
                    .get(*path_index)
                    .map(|p| p.is_collapsed)
                    .unwrap_or(false)
            }
        }
    }

    /// Checks if a directory is collapsed.
    fn is_directory_collapsed(&self, row: &RowIndex) -> bool {
        self.directory_entries
            .get(row.directory_index)
            .map(|d| d.is_collapsed)
            .unwrap_or(false)
    }

    fn render_results(&self, app: &AppContext) -> Option<Box<dyn Element>> {
        if !self.has_results() {
            return None;
        }

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let handle = self.handle.clone();

        let item_count = self.items_count();

        let build_items = move |range: Range<usize>, app: &AppContext| {
            {
                let view_handle = handle
                    .upgrade(app)
                    .expect("GlobalSearchView handle should be upgradeable");
                let view = view_handle.as_ref(app);

                (range.start..range.end).map(|idx| view.render_row_at_index(idx, app))
            }
            .collect::<Vec<_>>()
            .into_iter()
        };

        let list = UniformList::new(self.uniform_list_state.clone(), item_count, build_items);

        let scrollable = Scrollable::vertical(
            self.scroll_state.clone(),
            list.finish_scrollable(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish();

        Some(scrollable)
    }

    /// Returns the index of the currently selected row in the flattened results list.
    fn selected_row_index(&self) -> Option<usize> {
        self.result_row_index(self.selected_row.as_ref()?)
    }

    fn set_selected_row(&mut self, selected_row: ResultRow, ctx: &mut ViewContext<Self>) {
        if let Some(index) = self.result_row_index(&selected_row) {
            self.uniform_list_state.scroll_to(index);
        }
        self.selected_row = Some(selected_row);
        ctx.notify();
    }

    fn set_selected_row_by_index(&mut self, index: usize, ctx: &mut ViewContext<Self>) {
        let Some(row) = self.result_row_at(index) else {
            return;
        };
        self.selected_row = Some(row);
        self.uniform_list_state.scroll_to(index);
        ctx.notify();
    }

    fn select_first_row(&mut self, ctx: &mut ViewContext<Self>) {
        self.set_selected_row_by_index(0, ctx);
    }

    fn ensure_selection(&mut self, ctx: &mut ViewContext<Self>) {
        // Validate current selection still exists
        if self.selected_row.is_some() && self.selected_row_index().is_none() {
            self.selected_row = None;
        }

        if self.selected_row.is_none() {
            self.select_first_row(ctx);
        }
    }

    fn move_selection(&mut self, direction: SelectionDirection, ctx: &mut ViewContext<Self>) {
        let total_items = self.items_count();
        if total_items == 0 {
            return;
        }

        self.ensure_selection(ctx);

        let current_index = self
            .selected_row_index()
            .unwrap_or_else(|| total_items.saturating_sub(1));

        let next_index = match direction {
            SelectionDirection::Up => current_index.saturating_sub(1),
            SelectionDirection::Down => current_index + 1,
        };

        let max_index = total_items.saturating_sub(1);
        let clamped_next = next_index.min(max_index);

        self.set_selected_row_by_index(clamped_next, ctx);
    }

    /// Opens, expands or collapses the selected row.
    fn activate_selected_row(&mut self, ctx: &mut ViewContext<Self>) {
        let selected_row = match self.selected_row.clone() {
            Some(ResultRow::NameGroup { kind, row }) => {
                self.activate_name_group_row(kind, row, ctx);
                return;
            }
            Some(ResultRow::TextMatchesHeader) => {
                self.toggle_text_matches_collapsed(ctx);
                return;
            }
            Some(ResultRow::TextMatch(selected_row)) => selected_row,
            None => return,
        };

        match &selected_row.index_type {
            RowIndexType::FileHeader { .. } => {
                if let (Some(dir_path), Some(file_path)) = (
                    self.directory_path_for_row_index(&selected_row).cloned(),
                    self.file_path_for_row_index(&selected_row).cloned(),
                ) {
                    self.toggle_file_collapsed(&dir_path, &file_path, ctx);
                }
            }
            RowIndexType::Match {
                path_index,
                match_index,
            } => {
                let Some(dir_entry) = self.directory_entries.get(selected_row.directory_index)
                else {
                    return;
                };
                let Some(matched_path) = dir_entry.matched_paths.paths.get(*path_index) else {
                    return;
                };
                let Some(matched) = matched_path.matches.get(*match_index) else {
                    return;
                };
                ctx.emit(Event::OpenMatch {
                    location: matched_path.path.clone(),
                    line_number: matched.line_number,
                    column_num: matched.column_num,
                });
            }
            RowIndexType::DirectoryHeader => {
                if let Some(dir_path) = self.directory_path_for_row_index(&selected_row).cloned() {
                    self.toggle_directory_collapsed(&dir_path, ctx);
                }
            }
        }
    }

    fn activate_name_group_row(
        &mut self,
        kind: NameMatchKind,
        row: NameGroupRow,
        ctx: &mut ViewContext<Self>,
    ) {
        match row {
            NameGroupRow::Header => self.toggle_name_group_collapsed(kind, ctx),
            NameGroupRow::Match(index) => {
                let Some(entry) = self.name_group(kind).entry(index) else {
                    return;
                };
                let location = entry.name_match.location.clone();
                ctx.emit(match kind {
                    NameMatchKind::Folder => Event::RevealFolder { location },
                    NameMatchKind::File => Event::OpenFile { location },
                });
            }
            NameGroupRow::HiddenMatches => {
                if let Some(first_revealed) = self.name_group_mut(kind).show_more() {
                    self.set_selected_row(
                        ResultRow::NameGroup {
                            kind,
                            row: NameGroupRow::Match(first_revealed),
                        },
                        ctx,
                    );
                }
            }
        }
    }

    fn toggle_name_group_collapsed(&mut self, kind: NameMatchKind, ctx: &mut ViewContext<Self>) {
        let group = self.name_group_mut(kind);
        group.toggle_collapsed();
        let is_collapsed = group.is_collapsed();

        // If collapsing and selection was inside this group, move to the group header
        if is_collapsed
            && let Some(ResultRow::NameGroup {
                kind: selected_kind,
                ..
            }) = self.selected_row
            && selected_kind == kind
        {
            self.selected_row = Some(ResultRow::NameGroup {
                kind,
                row: NameGroupRow::Header,
            });
        }

        self.ensure_selection(ctx);
        ctx.notify();
    }

    fn toggle_text_matches_collapsed(&mut self, ctx: &mut ViewContext<Self>) {
        self.is_text_matches_collapsed = !self.is_text_matches_collapsed;

        // If collapsing and selection was on a text match row, move to the section header
        if self.is_text_matches_collapsed
            && matches!(self.selected_row, Some(ResultRow::TextMatch(_)))
        {
            self.selected_row = Some(ResultRow::TextMatchesHeader);
        }

        self.ensure_selection(ctx);
        ctx.notify();
    }

    fn toggle_directory_collapsed(
        &mut self,
        directory_path: &LocalOrRemotePath,
        ctx: &mut ViewContext<Self>,
    ) {
        let Some(&dir_idx) = self
            .directory_path_to_directory_index_entry
            .get(directory_path)
        else {
            return;
        };
        let Some(dir_entry) = self.directory_entries.get_mut(dir_idx) else {
            return;
        };

        let was_collapsed = dir_entry.is_collapsed;
        dir_entry.is_collapsed = !was_collapsed;

        // If collapsing and selection was inside this directory, move to directory header
        if !was_collapsed
            && let Some(ResultRow::TextMatch(selected_row)) = self.selected_row.as_ref()
            && selected_row.directory_index == dir_idx
            && !matches!(selected_row.index_type, RowIndexType::DirectoryHeader)
        {
            self.selected_row = Some(ResultRow::TextMatch(RowIndex {
                directory_index: dir_idx,
                index_type: RowIndexType::DirectoryHeader,
            }));
        }

        self.ensure_selection(ctx);
        ctx.notify();
    }

    fn toggle_file_collapsed(
        &mut self,
        directory_path: &LocalOrRemotePath,
        file_path: &LocalOrRemotePath,
        ctx: &mut ViewContext<Self>,
    ) {
        // Get directory index
        let Some(&dir_idx) = self
            .directory_path_to_directory_index_entry
            .get(directory_path)
        else {
            return;
        };
        let Some(dir_entry) = self.directory_entries.get_mut(dir_idx) else {
            return;
        };
        let Some(matched_path) = dir_entry.matched_paths.get_mut(file_path) else {
            return;
        };

        let was_collapsed = matched_path.is_collapsed;
        matched_path.is_collapsed = !was_collapsed;

        // If collapsing and selection was on a match in this file, move to file header
        if !was_collapsed
            && let Some(ResultRow::TextMatch(selected_row)) = self.selected_row.as_ref()
            && let RowIndexType::Match { path_index, .. } = &selected_row.index_type
        {
            // Check if the selection is in this file
            if let Some(dir_path) = self.directory_path_for_row_index(selected_row)
                && let Some(sel_file_path) = self.file_path_for_row_index(selected_row)
                && dir_path == directory_path
                && sel_file_path == file_path
            {
                self.selected_row = Some(ResultRow::TextMatch(RowIndex {
                    directory_index: selected_row.directory_index,
                    index_type: RowIndexType::FileHeader {
                        path_index: *path_index,
                    },
                }));
            }
        }

        self.ensure_selection(ctx);
        ctx.notify();
    }

    fn enter_query_mode(&mut self, ctx: &mut ViewContext<Self>) {
        self.set_query_mode_state(ctx);
        ctx.focus(&self.query_editor);
    }

    fn enter_results_mode(&mut self, ctx: &mut ViewContext<Self>) {
        if !self.has_results() {
            return;
        }
        self.focus_mode = FocusMode::ResultsList;
        self.query_editor.update(ctx, |editor, ctx| {
            editor.clear_selections(ctx);
            editor.set_interaction_state(InteractionState::Disabled, ctx);
        });
        self.ensure_selection(ctx);
        ctx.focus_self();
        ctx.notify();
    }

    fn is_first_row_selected(&self) -> bool {
        matches!(self.selected_row_index(), Some(0))
    }

    /// Checks if the row at the given global index is currently selected.
    fn is_row_at_index_selected(&self, global_index: usize) -> bool {
        self.selected_row_index() == Some(global_index)
    }

    fn render_name_group_row(
        &self,
        index: usize,
        kind: NameMatchKind,
        row: NameGroupRow,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let group = self.name_group(kind);
        match row {
            NameGroupRow::Header => {
                let label = match kind {
                    NameMatchKind::Folder => "Folders",
                    NameMatchKind::File => "Files",
                };
                self.render_group_header(
                    index,
                    label,
                    group.total(),
                    group.is_collapsed(),
                    group.header_mouse_state.clone(),
                    GlobalSearchAction::ClickNameGroupRow { kind, row },
                    appearance,
                )
            }
            NameGroupRow::Match(match_index) => {
                let Some(entry) = group.entry(match_index) else {
                    return Empty::new().finish();
                };
                self.render_name_match_row(index, kind, match_index, entry, appearance, app)
            }
            NameGroupRow::HiddenMatches => self.render_hidden_matches_row(index, group, appearance),
        }
    }

    /// Renders the collapsible header of one of the sections the results are grouped into.
    #[allow(clippy::too_many_arguments)]
    fn render_group_header(
        &self,
        index: usize,
        label: &'static str,
        count: usize,
        is_collapsed: bool,
        mouse_state: MouseStateHandle,
        click_action: GlobalSearchAction,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let theme = appearance.theme();

        Hoverable::new(mouse_state, move |mouse_state| {
            let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);
            let color = list_highlight_state.text_and_icon_color(appearance);

            let chevron_icon_enum = if is_collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            };
            let chevron_icon = ConstrainedBox::new(
                chevron_icon_enum
                    .to_warpui_icon(ThemeFill::from(color))
                    .finish(),
            )
            .with_width(ROW_ICON_SIZE)
            .with_height(ROW_ICON_SIZE)
            .finish();
            let chevron_container = Container::new(chevron_icon).with_margin_right(8.).finish();

            let label_text = Text::new_inline(label, appearance.ui_font_family(), 14.)
                .with_color(color)
                .with_style(Properties::default().weight(Weight::Semibold))
                .finish();

            let count_text = Text::new_inline(count.to_string(), appearance.ui_font_family(), 12.0)
                .with_color(theme.background().into())
                .finish();
            let count_badge = Container::new(count_text)
                .with_horizontal_padding(4.)
                .with_vertical_padding(1.)
                .with_background_color(theme.terminal_colors().normal.yellow.into())
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .finish();

            let left_row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(chevron_container)
                .with_child(Shrinkable::new(1.0, label_text).finish())
                .finish();

            let header_row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1.0, left_row).finish())
                .with_child(count_badge)
                .finish();

            let mut row =
                Container::new(header_row).with_padding(Padding::uniform(4.).with_left(0.));

            if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                let corner_radius = list_highlight_state
                    .corner_radius()
                    .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                row = row
                    .with_background(background_fill)
                    .with_corner_radius(corner_radius);
            }

            row.finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(click_action.clone());
        })
        .finish()
    }

    fn render_name_match_row(
        &self,
        index: usize,
        kind: NameMatchKind,
        match_index: usize,
        entry: &NameMatchEntry,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let theme = appearance.theme();
        let label = NameMatchLabel::new(&entry.name_match);
        let location_path = entry.name_match.location.display_path();
        let tooltip_text = display_path_with_host(&entry.name_match.location, false, app);

        Hoverable::new(entry.mouse_state.clone(), move |mouse_state| {
            let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);

            let name_color = match list_highlight_state {
                ItemHighlightState::None => blended_colors::text_main(theme, theme.background()),
                ItemHighlightState::Hovered | ItemHighlightState::Selected => {
                    theme.foreground().into()
                }
            };
            let highlight = Highlight::new().with_text_style(
                TextStyle::new().with_background_color(theme.ansi_overlay_2(
                    AnsiColorIdentifier::Yellow.to_ansi_color(&theme.terminal_colors().normal),
                )),
            );

            let icon = match kind {
                NameMatchKind::Folder => icon_from_folder_path(&location_path, false),
                NameMatchKind::File => icon_from_file_path(&location_path, appearance)
                    .unwrap_or_else(default_file_icon),
            };
            let icon = Container::new(
                ConstrainedBox::new(icon)
                    .with_width(ROW_ICON_SIZE)
                    .with_height(ROW_ICON_SIZE)
                    .finish(),
            )
            .with_padding_right(8.)
            .finish();

            let mut name_text =
                Text::new_inline(label.name.to_string(), appearance.ui_font_family(), 14.)
                    .with_color(name_color);
            if !label.name_highlights.is_empty() {
                name_text = name_text.with_single_highlight(highlight, label.name_highlights);
            }

            // The name must stay readable in a narrow panel, so the parent path gives way first.
            let mut content = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(icon)
                .with_child(Shrinkable::new(20.0, name_text.finish()).finish());

            if !label.parent.is_empty() {
                let mut parent_text =
                    Text::new_inline(label.parent.to_string(), appearance.ui_font_family(), 12.)
                        .with_color(blended_colors::text_sub(theme, theme.background()))
                        .with_clip(ClipConfig {
                            direction: ClipDirection::Start,
                            style: ClipStyle::Ellipsis,
                        });
                if !label.parent_highlights.is_empty() {
                    parent_text =
                        parent_text.with_single_highlight(highlight, label.parent_highlights);
                }
                content.add_child(
                    Shrinkable::new(
                        1.0,
                        Container::new(parent_text.finish())
                            .with_padding_left(6.)
                            .finish(),
                    )
                    .finish(),
                );
            }

            let mut row = Container::new(content.finish())
                .with_padding(Padding::uniform(4.).with_left(NAME_GROUP_ROW_INDENT));

            if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                let corner_radius = list_highlight_state
                    .corner_radius()
                    .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                row = row
                    .with_background(background_fill)
                    .with_corner_radius(corner_radius);
            }

            let base_row = row.finish();

            // Add a tooltip on hover that shows the full (untruncated) path.
            if mouse_state.is_hovered() {
                let tooltip = appearance
                    .ui_builder()
                    .tool_tip(tooltip_text)
                    .build()
                    .finish();

                let mut stack = Stack::new().with_child(base_row);
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
            } else {
                base_row
            }
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::ClickNameGroupRow {
                kind,
                row: NameGroupRow::Match(match_index),
            });
        })
        // The first click of the pair has already opened the file as a preview.
        .on_double_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::ClickNameGroupRow {
                kind,
                row: NameGroupRow::Match(match_index),
            });
        })
        .finish()
    }

    fn render_hidden_matches_row(
        &self,
        index: usize,
        group: &NameGroup,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let theme = appearance.theme();
        let kind = group.kind();
        let hidden_count = group.hidden_count();
        let can_show_more = group.can_show_more();
        let label = if can_show_more {
            format!("Show more ({hidden_count} hidden)")
        } else {
            format!("{hidden_count} more not shown. Narrow your search to see them.")
        };

        let row = Hoverable::new(
            group.hidden_matches_mouse_state.clone(),
            move |mouse_state| {
                let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);

                let text_color = match list_highlight_state {
                    ItemHighlightState::None => blended_colors::text_sub(theme, theme.background()),
                    ItemHighlightState::Hovered => {
                        blended_colors::text_main(theme, theme.background())
                    }
                    ItemHighlightState::Selected => theme.foreground().into(),
                };
                let text = Text::new_inline(label, appearance.ui_font_family(), 14.)
                    .with_color(text_color)
                    .finish();

                let mut row = Container::new(text)
                    .with_padding(Padding::uniform(4.).with_left(NAME_GROUP_ROW_INDENT));

                if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                    let corner_radius = list_highlight_state
                        .corner_radius()
                        .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                    row = row
                        .with_background(background_fill)
                        .with_corner_radius(corner_radius);
                }

                row.finish()
            },
        )
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::ClickNameGroupRow {
                kind,
                row: NameGroupRow::HiddenMatches,
            });
        });

        if can_show_more {
            row.with_cursor(Cursor::PointingHand).finish()
        } else {
            row.finish()
        }
    }

    fn render_directory_header_from_entry(
        &self,
        index: usize,
        dir_entry: &DirectoryEntry,
        appearance: &Appearance,
        theme: &warp_core::ui::theme::WarpTheme,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_selected = self.is_row_at_index_selected(index);
        let mouse_state = dir_entry.mouse_state.clone();
        let match_count = dir_entry.total_match_count();
        let is_collapsed = dir_entry.is_collapsed;
        let directory_path = &dir_entry.path;

        let display_name = if directory_path.display_name().is_empty() {
            display_path_with_host(directory_path, false, app)
        } else {
            display_name_with_host(directory_path, app)
        };
        let directory_path_for_click = directory_path.clone();
        let folder_path = directory_path.display_path();
        let tooltip_text = display_path_with_host(directory_path, false, app);

        Hoverable::new(mouse_state, move |mouse_state| {
            let list_highlight_state = ItemHighlightState::new(is_selected, mouse_state);

            // Chevron - dynamic based on collapse state
            let chevron_icon_enum = if is_collapsed {
                Icon::ChevronRight
            } else {
                Icon::ChevronDown
            };
            let icon_size = 16.0;
            let chevron_color = list_highlight_state.text_and_icon_color(appearance);
            let chevron_icon = chevron_icon_enum
                .to_warpui_icon(ThemeFill::from(chevron_color))
                .finish();
            let chevron_icon = ConstrainedBox::new(chevron_icon)
                .with_width(icon_size)
                .with_height(icon_size)
                .finish();
            let chevron_container = Container::new(chevron_icon).with_margin_right(8.).finish();

            // Folder icon
            let folder_icon = icon_from_folder_path(&folder_path, !is_collapsed);
            let folder_icon_element = Container::new(
                ConstrainedBox::new(folder_icon)
                    .with_width(icon_size)
                    .with_height(icon_size)
                    .finish(),
            )
            .with_padding_right(8.)
            .finish();

            // Directory name
            let text_color = list_highlight_state.text_and_icon_color(appearance);
            let directory_text =
                Text::new_inline(display_name.clone(), appearance.ui_font_family(), 14.)
                    .with_color(text_color)
                    .with_style(Properties::default())
                    .finish();

            // Match count badge
            let count_text =
                Text::new_inline(match_count.to_string(), appearance.ui_font_family(), 12.0)
                    .with_color(appearance.theme().background().into())
                    .finish();

            let count_badge = Container::new(count_text)
                .with_horizontal_padding(4.)
                .with_vertical_padding(1.)
                .with_background_color(theme.terminal_colors().normal.yellow.into())
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .finish();

            let left_row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(chevron_container)
                .with_child(folder_icon_element)
                .with_child(Shrinkable::new(1.0, directory_text).finish())
                .finish();

            let header_row = Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1.0, left_row).finish())
                .with_child(count_badge)
                .finish();

            let mut row_with_padding =
                Container::new(header_row).with_padding(Padding::uniform(4.).with_left(0.)); // Align chevron with header text

            if let Some(background_fill) = list_highlight_state.background_color(appearance) {
                let corner_radius = list_highlight_state
                    .corner_radius()
                    .unwrap_or_else(|| CornerRadius::with_all(Radius::Pixels(4.)));
                row_with_padding = row_with_padding
                    .with_background(background_fill)
                    .with_corner_radius(corner_radius);
            }

            let base_row = row_with_padding.finish();

            // Add tooltip on hover
            if mouse_state.is_hovered() {
                let tooltip = appearance
                    .ui_builder()
                    .tool_tip(tooltip_text.clone())
                    .build()
                    .finish();

                let mut stack = Stack::new().with_child(base_row);
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
            } else {
                base_row
            }
        })
        .with_cursor(Cursor::PointingHand)
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(GlobalSearchAction::ToggleDirectoryCollapsed {
                directory_path: directory_path_for_click.clone(),
            });
        })
        .finish()
    }
}

impl View for GlobalSearchView {
    fn ui_name() -> &'static str {
        "GlobalSearchView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        match self.enablement {
            CodingPanelEnablementState::PendingRemoteSession => {
                return self.render_remote_loading_state(app);
            }
            CodingPanelEnablementState::RemoteSession { has_remote_server } => {
                // Remote-server sessions can search via the daemon; sessions
                // without one (tmux / subshell SSH) stay unavailable.
                if !has_remote_server {
                    return self.render_remote_state(app);
                }
            }
            CodingPanelEnablementState::UnsupportedSession => {
                return self.render_unsupported_session_state(app);
            }
            CodingPanelEnablementState::Disabled => {
                return self.render_unavailable_state(app);
            }
            CodingPanelEnablementState::Enabled => {}
        }

        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let search_label = Text::new_inline("Search", appearance.ui_font_family(), 14.)
            .with_color(blended_colors::text_sub(theme, theme.background()))
            .finish();

        let editor_line_height = self
            .query_editor
            .as_ref(app)
            .line_height(app.font_cache(), appearance);

        let max_editor_height = QUERY_EDITOR_MAX_LINES as f32 * editor_line_height;

        let case_sensitivity_button =
            Container::new(ChildView::new(&self.case_sensitivity_button).finish()).finish();

        let whole_word_button =
            Container::new(ChildView::new(&self.whole_word_button).finish()).finish();

        let regex_button = Container::new(ChildView::new(&self.regex_button).finish()).finish();

        let scope_button = Container::new(ChildView::new(&self.scope_button).finish()).finish();

        let mut editor_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Shrinkable::new(
                    1.0,
                    ConstrainedBox::new(
                        Clipped::new(ChildView::new(&self.query_editor).finish()).finish(),
                    )
                    .with_max_height(max_editor_height)
                    .finish(),
                )
                .finish(),
            );
        editor_row.add_child(case_sensitivity_button);
        editor_row.add_child(whole_word_button);
        editor_row.add_child(regex_button);
        editor_row.add_child(scope_button);
        let editor_row = editor_row.finish();

        let query_row_container = Container::new(editor_row)
            .with_padding(Padding::uniform(6.))
            .with_border(Border::all(BORDER_WIDTH).with_border_fill(theme.surface_3()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BORDER_RADIUS)))
            .with_margin_top(4.)
            .with_margin_bottom(4.)
            .finish();

        let query_row = EventHandler::new(query_row_container)
            .on_left_mouse_down(|ctx, _, _| {
                ctx.dispatch_typed_action(GlobalSearchAction::FocusQueryEditor);
                DispatchEventResult::StopPropagation
            })
            .finish();

        let label_row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(self.render_replace_toggle(appearance))
            .with_child(search_label)
            .finish();

        let mut header_column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(label_row)
            .with_child(query_row);

        if self.replace.is_open {
            header_column.add_child(self.render_replace_row(app));
        }
        if self.is_scope_open {
            header_column.add_child(self.render_field_row(&self.include_editor, None, app));
            header_column.add_child(self.render_field_row(&self.exclude_editor, None, app));
        }
        if self.replace.is_open || self.is_replace_running(app) {
            header_column.add_child(self.render_replace_actions(app));
        }
        if let Some(run_summary) = self.render_run_summary(app) {
            header_column.add_child(run_summary);
        }

        let files = self.unique_match_count();
        let file_word = if files == 1 { "file" } else { "files" };

        let name_summary =
            name_match_summary(self.folder_matches.total(), self.file_matches.total());
        let is_searching = self.is_search_in_progress || self.pending_name_search_id.is_some();

        let message = if let Some(error) = &self.last_error {
            error.clone()
        } else if self.total_match_count == 0 {
            match name_summary {
                Some(name_summary) => name_summary,
                None if is_searching => "Searching…".to_string(),
                None => "No results found. Review your gitignore files.".to_string(),
            }
        } else {
            let text_summary = match self.total_match_count {
                1 => format!("1 result in {files} {file_word}"),
                n => format!("{n} results in {files} {file_word}"),
            };
            match name_summary {
                Some(name_summary) => format!("{text_summary} · {name_summary}"),
                None => text_summary,
            }
        };

        let match_text_styles = UiComponentStyles {
            font_family_id: Some(appearance.ui_font_family()),
            font_size: Some(12.),
            font_color: Some(blended_colors::text_disabled(theme, theme.background())),
            ..Default::default()
        };

        let match_text = Span::new(message, match_text_styles)
            .with_soft_wrap()
            .build()
            .finish();

        let capped_text_styles = UiComponentStyles {
            font_family_id: Some(appearance.ui_font_family()),
            font_size: Some(12.),
            font_color: Some(blended_colors::text_sub(theme, theme.background())),
            ..Default::default()
        };
        let capped_message = "The result set only contains a subset of all matches. Be more specific in your search to narrow down results.".to_string();
        let capped_text = Span::new(capped_message, capped_text_styles)
            .with_soft_wrap()
            .build()
            .finish();

        if self.last_searched_pattern.is_some() {
            header_column = header_column.with_child(match_text);
        }

        if self.capped_matches {
            let alert_icon = ConstrainedBox::new(
                Icon::AlertTriangle
                    .to_warpui_icon(ThemeFill::Solid(
                        theme.terminal_colors().normal.yellow.into(),
                    ))
                    .finish(),
            )
            .with_width(appearance.ui_font_size())
            .with_height(appearance.ui_font_size())
            .finish();

            let alert_icon = Container::new(alert_icon).with_margin_right(4.).finish();

            let capped_row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Start)
                .with_child(alert_icon)
                .with_child(Shrinkable::new(1.0, capped_text).finish())
                .finish();

            header_column =
                header_column.with_child(Container::new(capped_row).with_margin_top(4.0).finish());
        }

        let header_section = Container::new(header_column.finish())
            .with_padding(Padding::uniform(12.).with_top(4.).with_bottom(8.))
            .finish();

        let mut body = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_child(header_section);

        let should_render_pre_search_zero_state =
            self.last_searched_pattern.is_none() && self.query_editor.as_ref(app).is_empty(app);

        if should_render_pre_search_zero_state {
            body =
                body.with_child(Shrinkable::new(1.0, self.render_pre_search_state(app)).finish());
        } else if let Some(results) = self.render_results(app) {
            let results_section = Container::new(results)
                .with_horizontal_padding(12.)
                .finish();
            body = body.with_child(Shrinkable::new(1.0, results_section).finish());
        }

        let has_results = self.has_results();

        EventHandler::new(body.finish())
            .on_left_mouse_down(move |ctx, _, _| {
                if has_results {
                    ctx.dispatch_typed_action(GlobalSearchAction::FocusResultsList);
                } else {
                    ctx.dispatch_typed_action(GlobalSearchAction::FocusQueryEditor);
                }
                DispatchEventResult::StopPropagation
            })
            .finish()
    }
}

impl GlobalSearchView {
    /// Renders one of the single-line fields below the query, with `trailing` after its text.
    fn render_field_row(
        &self,
        editor: &ViewHandle<EditorView>,
        trailing: Option<Box<dyn Element>>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let line_height = editor.as_ref(app).line_height(app.font_cache(), appearance);

        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Shrinkable::new(
                    1.0,
                    ConstrainedBox::new(Clipped::new(ChildView::new(editor).finish()).finish())
                        .with_height(line_height)
                        .finish(),
                )
                .finish(),
            );
        if let Some(trailing) = trailing {
            row.add_child(trailing);
        }

        Container::new(row.finish())
            .with_padding(Padding::uniform(6.))
            .with_border(Border::all(BORDER_WIDTH).with_border_fill(theme.surface_3()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(BORDER_RADIUS)))
            .with_margin_bottom(4.)
            .finish()
    }

    fn render_zero_state(
        &self,
        icon: Icon,
        title: impl Into<Cow<'static, str>>,
        body: impl Into<Cow<'static, str>>,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let main_column = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Center)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_child(
                Container::new(
                    ConstrainedBox::new(
                        icon.to_warpui_icon(ThemeFill::Solid(internal_colors::neutral_6(theme)))
                            .finish(),
                    )
                    .with_width(24.)
                    .with_height(24.)
                    .finish(),
                )
                .with_margin_bottom(12.)
                .finish(),
            )
            .with_child(
                Text::new(
                    title,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() + 2.,
                )
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            )
            .with_child(
                ConstrainedBox::new(
                    Shrinkable::new(
                        1.,
                        Container::new(
                            Shrinkable::new(
                                1.,
                                FormattedTextElement::from_str(
                                    body,
                                    appearance.ui_font_family(),
                                    appearance.ui_font_size() + 2.,
                                )
                                .with_alignment(TextAlignment::Center)
                                .with_color(theme.disabled_text_color(theme.background()).into())
                                .finish(),
                            )
                            .finish(),
                        )
                        .finish(),
                    )
                    .finish(),
                )
                .with_max_width(425.)
                .finish(),
            )
            .finish();

        Container::new(
            Flex::row()
                .with_main_axis_size(MainAxisSize::Max)
                .with_main_axis_alignment(MainAxisAlignment::Center)
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_child(Shrinkable::new(1., main_column).finish())
                .finish(),
        )
        .with_horizontal_margin(16.)
        .finish()
    }

    fn render_pre_search_state(&self, app: &AppContext) -> Box<dyn Element> {
        self.render_zero_state(
            Icon::Search,
            "Global search",
            "Search file contents, file names and folder names across your current directories.",
            app,
        )
    }

    fn render_unavailable_state(&self, app: &AppContext) -> Box<dyn Element> {
        self.render_zero_state(
            Icon::AlertTriangle,
            "Global search unavailable",
            "Global search requires access to your local workspace. Open a new session or navigate to an active session to view.",
            app,
        )
    }

    fn render_remote_state(&self, app: &AppContext) -> Box<dyn Element> {
        self.render_zero_state(
            Icon::AlertTriangle,
            "Global search unavailable",
            "Global search isn't available for this remote session.",
            app,
        )
    }
    fn render_remote_loading_state(&self, app: &AppContext) -> Box<dyn Element> {
        self.render_zero_state(
            Icon::Loading,
            "Connecting to remote session",
            "Global search will be available once the connection is ready.",
            app,
        )
    }

    fn render_unsupported_session_state(&self, app: &AppContext) -> Box<dyn Element> {
        self.render_zero_state(
            Icon::AlertTriangle,
            "Global search unavailable",
            "Global search doesn't currently work in Git Bash or WSL.",
            app,
        )
    }
}
