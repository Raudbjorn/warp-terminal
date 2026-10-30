//! oh-my-warp: picker modal body for `warp.ui.showPalette`.
//!
//! Renders a native-feeling fuzzy picker: a search input at the top, then a list of selectable
//! rows beneath. Items support optional icon (sigil/emoji), description (dimmed inline subtitle),
//! and keystroke hint, so a plugin author can produce a self-explanatory picker without hand-rolling
//! widgets. Keyboard nav (arrows, enter) is driven through editor events — the search input
//! propagates Up/Down/Enter to us (see [`PropagateAndNoOpNavigationKeys::Always`]); ESC closes via
//! the parent [`crate::modal::Modal`]'s binding.
//!
//! On accept the modal emits [`PluginPaletteEvent::Selected`] with the chosen item's command id;
//! the `Workspace` closes the modal and runs that command as a fresh `RunPluginCommand` (so we
//! never re-enter `plugin.get_mut()` from inside the calling callback). See PLUGIN_SPEC.md (M4).

use fuzzy_match::match_indices_case_insensitive;
use warpui::elements::{
    Border, ChildView, ClippedScrollStateHandle, ClippedScrollable, Clipped, ConstrainedBox,
    Container, CornerRadius, CrossAxisAlignment, Element, Fill, Flex, Hoverable,
    MainAxisAlignment, MainAxisSize, MouseStateHandle, Padding, ParentElement, Radius,
    ScrollbarWidth, Shrinkable, Text,
};
use warpui::platform::Cursor;
use warpui::{
    AppContext, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys,
    PropagateHorizontalNavigationKeys, SingleLineEditorOptions, TextOptions,
};
use crate::plugin::app_requests::PalettePluginItem;
use warp_editor::editor::NavigationKey;

const SEARCH_BOX_HORIZONTAL_PADDING: f32 = 12.;
const SEARCH_BOX_VERTICAL_PADDING: f32 = 8.;
const SEARCH_BOX_INNER_PADDING: f32 = 8.;
const SEARCH_BOX_CORNER_RADIUS: f32 = 6.;

const ROW_HORIZONTAL_PADDING: f32 = 12.;
const ROW_VERTICAL_PADDING: f32 = 8.;
const ROW_CORNER_RADIUS: f32 = 6.;
const ROW_ICON_WIDTH: f32 = 22.;
const ROW_GAP: f32 = 2.;

const LIST_HORIZONTAL_PADDING: f32 = 6.;
const LIST_VERTICAL_PADDING: f32 = 4.;

const LABEL_FONT_SIZE: f32 = 14.;
const DESCRIPTION_FONT_SIZE: f32 = 12.;
const KBD_FONT_SIZE: f32 = 11.;

/// Emitted when the user picks an item, carrying that item's command id. The `Workspace` runs the
/// command as a fresh `RunPluginCommand` (not from within the calling callback's borrow).
#[derive(Debug, PartialEq, Eq)]
pub enum PluginPaletteEvent {
    Selected(String),
}

/// Dispatched by row click / hover handlers to drive selection in the body model.
#[derive(Debug)]
pub enum PluginPaletteAction {
    /// Click on the row at the given index in the *filtered* list.
    Select(usize),
    /// Hover moved over the row at the given index in the *filtered* list.
    Hover(usize),
}

pub struct PluginPaletteModal {
    items: Vec<PalettePluginItem>,
    /// Indices into `items` for entries that match the current query, in score-desc order. When the
    /// query is empty, every index in original order.
    filtered: Vec<usize>,
    /// Selected index *into `filtered`*, or `None` when the list is empty.
    selected: Option<usize>,
    /// Per-item hover state, indexed by `filtered`'s position. Resized whenever `filtered` changes.
    row_hover_states: Vec<MouseStateHandle>,
    query_editor: ViewHandle<EditorView>,
    /// Drives the vertical scrollable wrapper so long item lists stay inside the modal
    /// frame instead of overflowing it. Same pattern as `PluginMarkdownModal`.
    scroll_state: ClippedScrollStateHandle,
}

impl PluginPaletteModal {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let query_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(Some(LABEL_FONT_SIZE), appearance),
                    select_all_on_focus: true,
                    clear_selections_on_blur: true,
                    // Up/Down/Enter should drive picker selection, not move the cursor inside the
                    // editor; the editor emits NavigationKey::Up / Down / Enter events for us to
                    // act on instead.
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::Always,
                    propagate_horizontal_navigation_keys: PropagateHorizontalNavigationKeys::Always,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Type to filter…", ctx);
            editor
        });
        ctx.subscribe_to_view(&query_editor, |me, _, event, ctx| {
            me.handle_query_editor_event(event, ctx);
        });
        Self {
            items: Vec::new(),
            filtered: Vec::new(),
            selected: None,
            row_hover_states: Vec::new(),
            query_editor,
            scroll_state: ClippedScrollStateHandle::default(),
        }
    }

    /// Replaces the picker's items and resets the search query / selection. Call before opening
    /// the modal — the workspace re-focuses us after this.
    pub fn set_items(&mut self, items: Vec<PalettePluginItem>, ctx: &mut ViewContext<Self>) {
        self.items = items;
        // Clear the search buffer so a re-shown picker starts fresh — this also fires an Edited
        // event which our handler turns into a recompute of `filtered`.
        self.query_editor.update(ctx, |editor, ctx| {
            editor.clear_buffer(ctx);
        });
        self.recompute_filtered(ctx);
    }

    /// Recomputes `filtered` from the current query and resets the selection to the top match.
    fn recompute_filtered(&mut self, ctx: &mut ViewContext<Self>) {
        let query = self
            .query_editor
            .read(ctx, |editor, ctx| editor.buffer_text(ctx))
            .trim()
            .to_string();
        if query.is_empty() {
            self.filtered = (0..self.items.len()).collect();
        } else {
            // Score each item against `query`; keep the matches, sort by score desc (then index for
            // a stable order on ties), and project to original-item indices. We match against the
            // label first; description is a tie-breaker so a plugin can put hint keywords there.
            let mut scored: Vec<(usize, i64)> = self
                .items
                .iter()
                .enumerate()
                .filter_map(|(idx, item)| {
                    let label_match = match_indices_case_insensitive(&item.label, &query);
                    let desc_match = item
                        .description
                        .as_deref()
                        .and_then(|d| match_indices_case_insensitive(d, &query));
                    let score = match (label_match, desc_match) {
                        (Some(l), Some(d)) => Some(l.score.max(d.score / 2)),
                        (Some(l), None) => Some(l.score),
                        // Half-credit a description-only hit so label matches always win.
                        (None, Some(d)) => Some(d.score / 2),
                        (None, None) => None,
                    }?;
                    Some((idx, score))
                })
                .collect();
            scored.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
            self.filtered = scored.into_iter().map(|(idx, _)| idx).collect();
        }
        // Resize the hover-state vector to match the new row count — each row owns its handle, so
        // we can't reuse handles across rebuilds without indices drifting under the user's cursor.
        self.row_hover_states = self
            .filtered
            .iter()
            .map(|_| MouseStateHandle::default())
            .collect();
        self.selected = if self.filtered.is_empty() {
            None
        } else {
            Some(0)
        };
        ctx.notify();
    }

    fn handle_query_editor_event(&mut self, event: &EditorEvent, ctx: &mut ViewContext<Self>) {
        match event {
            EditorEvent::Edited(_) => self.recompute_filtered(ctx),
            EditorEvent::Navigate(NavigationKey::Down) => self.move_selection(1, ctx),
            EditorEvent::Navigate(NavigationKey::Up) => self.move_selection(-1, ctx),
            EditorEvent::Enter => self.accept_selection(ctx),
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, ctx: &mut ViewContext<Self>) {
        let len = self.filtered.len() as isize;
        if len == 0 {
            return;
        }
        let current = self.selected.map(|s| s as isize).unwrap_or(0);
        let next = (current + delta).rem_euclid(len) as usize;
        if self.selected != Some(next) {
            self.selected = Some(next);
            ctx.notify();
        }
    }

    fn accept_selection(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(filtered_idx) = self.selected else {
            return;
        };
        let Some(item) = self
            .filtered
            .get(filtered_idx)
            .and_then(|original| self.items.get(*original))
        else {
            return;
        };
        ctx.emit(PluginPaletteEvent::Selected(item.command_id.clone()));
    }

    /// Renders a single result row. Background follows hover and selection; layout is icon column
    /// (fixed width even when absent, so labels line up across rows) + label/description column
    /// + optional keystroke hint right-aligned.
    fn render_row(
        &self,
        filtered_idx: usize,
        item: &PalettePluginItem,
        is_selected: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let theme = appearance.theme();
        let ui_font_family = appearance.ui_font_family();
        let mono_font_family = appearance.monospace_font_family();

        let icon_glyph = item.icon.clone().unwrap_or_default();
        // Reserve a fixed-width icon column so labels line up across rows even when some items have
        // no icon — ConstrainedBox is the warpui primitive for "exactly this many logical pixels".
        let icon_element = ConstrainedBox::new(
            Container::new(
                Text::new_inline(icon_glyph, ui_font_family, LABEL_FONT_SIZE)
                    .with_color(theme.main_text_color(theme.background()).into())
                    .with_selectable(false)
                    .finish(),
            )
            .finish(),
        )
        .with_width(ROW_ICON_WIDTH)
        .finish();

        let label_text = Text::new_inline(item.label.clone(), ui_font_family, LABEL_FONT_SIZE)
            .with_color(theme.main_text_color(theme.background()).into())
            .with_selectable(false)
            .finish();

        let mut label_column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_spacing(ROW_GAP)
            .with_child(label_text);

        if let Some(description) = item.description.as_deref().filter(|s| !s.is_empty()) {
            label_column.add_child(
                Text::new_inline(
                    description.to_string(),
                    ui_font_family,
                    DESCRIPTION_FONT_SIZE,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .with_selectable(false)
                .finish(),
            );
        }

        let mut row = Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::Start)
            .with_child(icon_element)
            .with_child(Shrinkable::new(1.0, label_column.finish()).finish());

        if let Some(kbd) = item.kbd.as_deref().filter(|s| !s.is_empty()) {
            // Right-edge keystroke hint in a chip — same visual treatment a native binding gets in
            // the command palette, so users learn the shortcut while they pick.
            let kbd_text = Text::new_inline(kbd.to_string(), mono_font_family, KBD_FONT_SIZE)
                .with_color(theme.sub_text_color(theme.background()).into())
                .with_selectable(false)
                .finish();
            let kbd_chip = Container::new(kbd_text)
                .with_horizontal_padding(6.)
                .with_vertical_padding(2.)
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
                .with_background(theme.surface_2())
                .with_border(Border::all(1.).with_border_fill(theme.surface_3()))
                .with_margin_left(8.)
                .finish();
            row.add_child(kbd_chip);
        }

        let hover_state = self.row_hover_states[filtered_idx].clone();
        let row_finished = row.finish();
        Hoverable::new(hover_state, move |state| {
            let background: Fill = if is_selected {
                theme.surface_overlay_1().into()
            } else if state.is_hovered() {
                theme.surface_2().into()
            } else {
                Fill::None
            };
            Container::new(row_finished)
                .with_padding(
                    Padding::uniform(0.)
                        .with_left(ROW_HORIZONTAL_PADDING)
                        .with_right(ROW_HORIZONTAL_PADDING)
                        .with_top(ROW_VERTICAL_PADDING)
                        .with_bottom(ROW_VERTICAL_PADDING),
                )
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(ROW_CORNER_RADIUS)))
                .with_background(background)
                .finish()
        })
        .with_cursor(Cursor::PointingHand)
        .on_hover(move |_, ctx, _, _| {
            ctx.dispatch_typed_action(PluginPaletteAction::Hover(filtered_idx));
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(PluginPaletteAction::Select(filtered_idx));
        })
        .finish()
    }

    /// Renders the search input at the top of the picker. Mirrors the conversation list search
    /// box's chrome so the picker looks at home next to native surfaces.
    fn render_search_box(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let editor = Shrinkable::new(
            1.0,
            Clipped::new(ChildView::new(&self.query_editor).finish()).finish(),
        )
        .finish();
        let boxed = Container::new(editor)
            .with_padding(
                Padding::uniform(0.)
                    .with_left(SEARCH_BOX_INNER_PADDING + 4.)
                    .with_right(SEARCH_BOX_INNER_PADDING + 4.)
                    .with_top(SEARCH_BOX_INNER_PADDING)
                    .with_bottom(SEARCH_BOX_INNER_PADDING),
            )
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(
                SEARCH_BOX_CORNER_RADIUS,
            )))
            .with_border(Border::all(1.).with_border_fill(theme.surface_3()))
            .with_background(theme.surface_2())
            .finish();
        Container::new(boxed)
            .with_horizontal_padding(SEARCH_BOX_HORIZONTAL_PADDING)
            .with_vertical_padding(SEARCH_BOX_VERTICAL_PADDING)
            .finish()
    }
}

impl Entity for PluginPaletteModal {
    type Event = PluginPaletteEvent;
}

impl TypedActionView for PluginPaletteModal {
    type Action = PluginPaletteAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            PluginPaletteAction::Select(index) => {
                self.selected = Some(*index);
                self.accept_selection(ctx);
            }
            PluginPaletteAction::Hover(index) => {
                if self.selected != Some(*index) {
                    self.selected = Some(*index);
                    ctx.notify();
                }
            }
        }
    }
}

impl View for PluginPaletteModal {
    fn ui_name() -> &'static str {
        "PluginPaletteModal"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        // When the modal opens (or refocuses), park focus in the query editor so the user can
        // start typing immediately, the way every other Warp picker behaves.
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.query_editor);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();

        let mut list = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min);

        if self.filtered.is_empty() {
            let message = if self.items.is_empty() {
                "No items"
            } else {
                "No matches"
            };
            list.add_child(
                Container::new(
                    Text::new_inline(message.to_string(), appearance.ui_font_family(), 13.)
                        .with_color(theme.sub_text_color(theme.background()).into())
                        .with_selectable(false)
                        .finish(),
                )
                .with_horizontal_padding(ROW_HORIZONTAL_PADDING)
                .with_vertical_padding(ROW_VERTICAL_PADDING)
                .finish(),
            );
        } else {
            for (filtered_idx, original_idx) in self.filtered.iter().enumerate() {
                let Some(item) = self.items.get(*original_idx) else {
                    continue;
                };
                let is_selected = self.selected == Some(filtered_idx);
                list.add_child(self.render_row(filtered_idx, item, is_selected, appearance));
            }
        }


        // Wrap the list in a vertical scrollable so a long picker (more items than
        // fit in the 480px-tall modal) stays accessible instead of being clipped off.
        // Same pattern as `PluginMarkdownModal`: state, body, scrollbar width, dim
        // thumb color, bright thumb color, background fill.
        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            list.finish(),
            ScrollbarWidth::Auto,
            theme.disabled_text_color(theme.background()).into(),
            theme.main_text_color(theme.background()).into(),
            Fill::None,
        )
        .finish();

        let list_container = Container::new(scrollable)
            .with_horizontal_padding(LIST_HORIZONTAL_PADDING)
            .with_vertical_padding(LIST_VERTICAL_PADDING)
            .finish();

        Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_main_axis_size(MainAxisSize::Min)
            .with_child(self.render_search_box(appearance))
            .with_child(list_container)
            .finish()
    }
}
