//! The Conn panel: what you read when you come back to a pane.
//!
//! Renders the focused pane's session history from [`ConnModel`] — the
//! instructions sent, the forks the agent took, and the last thing it said.
//! Read-only; it reflects state and never drives a session.

use ::conn::{ConnEntry, ConnEntryKind};
use warp_errors::report_error;
use warpui::elements::new_scrollable::{NewScrollableElement, SingleAxisConfig};
use warpui::elements::{
    Align, Container, CrossAxisAlignment, DragBarSide, Element, Empty, Fill, Flex, List, ListState,
    MainAxisSize, NewScrollable, ParentElement, Resizable, ResizableStateHandle, ScrollStateHandle,
    Shrinkable, Text, resizable_state_handle,
};
use warpui::fonts::{Properties, Weight};
use warpui::{
    AppContext, Entity, EntityId, SingletonEntity, View, ViewContext, ViewHandle, WeakViewHandle,
};

use super::ConnModel;
use crate::appearance::Appearance;
use crate::drive::panel::{MAX_SIDEBAR_WIDTH_RATIO, MIN_SIDEBAR_WIDTH};
use crate::pane_group::PaneGroup;
use crate::terminal::resizable_data::{DEFAULT_RIGHT_PANEL_WIDTH, ModalType, ResizableData};

/// Truncation point for a prompt or response in the list. Long enough to
/// recognise what you asked for, short enough that the spine stays scannable.
const PREVIEW_CHARS: usize = 240;

pub struct ConnPanelView {
    /// The pane group whose focused pane this panel follows. Set by the
    /// workspace.
    ///
    /// Weak deliberately. The panel only reads, so it must not be what keeps a
    /// closed tab's pane group — and through it, its terminal models — alive.
    active_pane_group: Option<WeakViewHandle<PaneGroup>>,
    list_state: ListState<()>,
    scroll_state: ScrollStateHandle,
    resizable_state_handle: ResizableStateHandle,
    /// The pane and entry count the list was last built for. The list owns its
    /// item count as mutable state, so it has to be resynced rather than
    /// derived during render.
    synced: Option<(EntityId, usize)>,
}

impl Entity for ConnPanelView {
    type Event = ();
}

impl ConnPanelView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let resizable_state_handle = match ResizableData::handle(ctx)
            .as_ref(ctx)
            .get_handle(ctx.window_id(), ModalType::RightPanelWidth)
        {
            Some(handle) => handle,
            None => {
                report_error!("Couldn't retrieve Conn panel resizable state handle.");
                resizable_state_handle(DEFAULT_RIGHT_PANEL_WIDTH)
            }
        };

        let list_state = Self::fresh_list_state(ctx.handle());

        // History accumulates while the panel is open, so the list has to
        // follow the model rather than only rebuild on focus changes.
        ctx.observe(&ConnModel::handle(ctx), |me, _, ctx| {
            me.sync_list(ctx);
        });

        Self {
            active_pane_group: None,
            list_state,
            scroll_state: ScrollStateHandle::default(),
            resizable_state_handle,
            synced: None,
        }
    }

    /// `ListState` owns its item count and cannot be cleared, so rebuilding
    /// the list means building a new one.
    fn fresh_list_state(weak_self: WeakViewHandle<Self>) -> ListState<()> {
        ListState::new(move |index, _scroll_offset, app| {
            let Some(view) = weak_self.upgrade(app) else {
                return Empty::new().finish();
            };
            view.as_ref(app).render_entry_at(index, app)
        })
    }

    pub fn set_active_pane_group(
        &mut self,
        pane_group: ViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.active_pane_group = Some(pane_group.downgrade());
        self.sync_list(ctx);
    }

    /// The terminal view the panel is currently reporting on.
    ///
    /// Follows focus rather than capturing a handle, so splitting or switching
    /// panes retargets the panel instead of stranding it on a stale pane.
    fn focused_terminal_id(&self, app: &AppContext) -> Option<EntityId> {
        let pane_group = self.active_pane_group.as_ref()?.upgrade(app)?;
        pane_group
            .read(app, |pane_group, app| pane_group.focused_session_view(app))
            .map(|terminal| terminal.id())
    }

    fn entry_count(&self, app: &AppContext) -> usize {
        self.focused_terminal_id(app)
            .and_then(|id| {
                ConnModel::as_ref(app)
                    .session(id)
                    .map(|session| session.entries().len())
            })
            .unwrap_or(0)
    }

    /// Rebuilds the list when the focused pane changes, and extends it as new
    /// entries arrive.
    fn sync_list(&mut self, ctx: &mut ViewContext<Self>) {
        let pane = self.focused_terminal_id(ctx);
        let count = self.entry_count(ctx);
        let target = pane.map(|pane| (pane, count));

        match (self.synced, target) {
            // Same pane, more history: extend and follow the tail, which is
            // where the answer to "what has it done since?" lives.
            (Some((synced_pane, synced_count)), Some((pane, count)))
                if synced_pane == pane && count >= synced_count =>
            {
                for _ in synced_count..count {
                    self.list_state.add_item();
                }
                if count > synced_count && count > 0 {
                    self.list_state.scroll_to(count - 1);
                }
            }
            // A different pane, or history that shrank because entries were
            // evicted: rebuild from scratch.
            _ => {
                self.list_state = Self::fresh_list_state(ctx.handle());
                for _ in 0..count {
                    self.list_state.add_item();
                }
            }
        }

        self.synced = target;
        ctx.notify();
    }

    fn render_entry_at(&self, index: usize, app: &AppContext) -> Box<dyn Element> {
        let Some(entry) = self
            .focused_terminal_id(app)
            .and_then(|id| ConnModel::as_ref(app).session(id))
            .and_then(|session| session.entries().get(index).cloned())
        else {
            return Empty::new().finish();
        };
        render_entry(&entry, app)
    }

    fn render_header(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let project = self
            .focused_terminal_id(app)
            .and_then(|id| ConnModel::as_ref(app).session(id))
            .and_then(|session| session.project.clone());

        let title = match project {
            Some(project) => format!("Conn — {project}"),
            None => "Conn".to_owned(),
        };

        Container::new(
            Text::new_inline(
                title,
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_style(Properties::default().weight(Weight::Semibold))
            .with_color(appearance.theme().active_ui_text_color().into())
            .finish(),
        )
        .with_uniform_padding(10.)
        .finish()
    }

    /// Shown before a session exists in the focused pane. Says what the panel
    /// is waiting for rather than appearing broken.
    fn render_empty_state(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let message = match self.focused_terminal_id(app) {
            Some(_) => "No agent session in this pane yet.",
            None => "No pane focused.",
        };

        Align::new(
            Container::new(
                Text::new_inline(
                    message,
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(appearance.theme().nonactive_ui_text_color().into())
                .finish(),
            )
            .with_uniform_padding(24.)
            .finish(),
        )
        .finish()
    }
}

/// One history entry as a labelled line. Plain text: the value here is the
/// content and its order, not the presentation.
fn render_entry(entry: &ConnEntry, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    let (label, body, emphasised) = match &entry.kind {
        ConnEntryKind::Prompt { text } => ("you", truncate(text), true),
        ConnEntryKind::PermissionRequested {
            summary,
            tool_name,
            target,
        } => (
            "asked to",
            describe_permission(summary.as_deref(), tool_name.as_deref(), target.as_deref()),
            true,
        ),
        ConnEntryKind::PermissionResolved => ("asked to", "— answered".to_owned(), false),
        ConnEntryKind::QuestionAsked { summary } => (
            "asked you",
            summary.clone().unwrap_or_else(|| "a question".to_owned()),
            true,
        ),
        ConnEntryKind::ToolCompleted {
            tool_name,
            target,
            runs,
        } => {
            let described = describe_tool(tool_name.as_deref(), target.as_deref());
            let body = if *runs > 1 {
                format!("{described} ×{runs}")
            } else {
                described
            };
            ("ran", body, false)
        }
        ConnEntryKind::Responded { text } => (
            "said",
            text.as_deref().map(truncate).unwrap_or_default(),
            false,
        ),
        ConnEntryKind::Failed {
            error_type,
            message,
        } => (
            "failed",
            describe_failure(error_type.as_deref(), message.as_deref()),
            true,
        ),
    };

    let label_color = if emphasised {
        theme.active_ui_text_color()
    } else {
        theme.nonactive_ui_text_color()
    };

    let row = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_main_axis_size(MainAxisSize::Min)
        .with_spacing(2.)
        .with_child(
            Text::new_inline(
                label,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 2.,
            )
            .with_color(label_color.into())
            .finish(),
        )
        .with_child(
            Text::new(body, appearance.ui_font_family(), appearance.ui_font_size())
                .with_color(theme.active_ui_text_color().into())
                .finish(),
        )
        .finish();

    Container::new(row)
        .with_padding_left(10.)
        .with_padding_right(10.)
        .with_padding_top(6.)
        .with_padding_bottom(6.)
        .finish()
}

fn describe_permission(
    summary: Option<&str>,
    tool_name: Option<&str>,
    target: Option<&str>,
) -> String {
    if let Some(summary) = summary.filter(|summary| !summary.trim().is_empty()) {
        return truncate(summary);
    }
    describe_tool(tool_name, target)
}

fn describe_tool(tool_name: Option<&str>, target: Option<&str>) -> String {
    match (tool_name, target) {
        (Some(tool), Some(target)) => truncate(&format!("{tool}: {target}")),
        (Some(tool), None) => tool.to_owned(),
        (None, Some(target)) => truncate(target),
        (None, None) => "a tool".to_owned(),
    }
}

fn describe_failure(error_type: Option<&str>, message: Option<&str>) -> String {
    match (error_type, message) {
        (Some(kind), Some(message)) => truncate(&format!("{kind}: {message}")),
        (Some(kind), None) => kind.to_owned(),
        (None, Some(message)) => truncate(message),
        (None, None) => "unknown error".to_owned(),
    }
}

/// Truncates on a character boundary, since prompts and responses are
/// arbitrary user and model text.
fn truncate(text: &str) -> String {
    let text = text.trim();
    if text.chars().count() <= PREVIEW_CHARS {
        return text.to_owned();
    }
    let truncated: String = text.chars().take(PREVIEW_CHARS).collect();
    format!("{truncated}…")
}

impl View for ConnPanelView {
    fn ui_name() -> &'static str {
        "ConnPanelView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let has_entries = self.entry_count(app) > 0;

        let content: Box<dyn Element> = if has_entries {
            Shrinkable::new(
                1.0,
                NewScrollable::vertical(
                    SingleAxisConfig::Manual {
                        handle: self.scroll_state.clone(),
                        child: NewScrollableElement::finish_scrollable(List::new(
                            self.list_state.clone(),
                        )),
                    },
                    Appearance::as_ref(app).theme().nonactive_ui_detail().into(),
                    Appearance::as_ref(app).theme().active_ui_detail().into(),
                    Fill::None,
                )
                .finish(),
            )
            .finish()
        } else {
            Shrinkable::new(1.0, self.render_empty_state(app)).finish()
        };

        let panel = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_child(self.render_header(app))
            .with_child(content)
            .finish();

        if warpui::platform::is_mobile_device() {
            return panel;
        }

        // Conn docks on the right, so the drag bar is on its left edge.
        Resizable::new(self.resizable_state_handle.clone(), panel)
            .with_dragbar_side(DragBarSide::Left)
            .on_resize(move |ctx, _| {
                ctx.notify();
            })
            .with_bounds_callback(Box::new(|window_size| {
                let min_width = MIN_SIDEBAR_WIDTH;
                let max_width = window_size.x() * MAX_SIDEBAR_WIDTH_RATIO;
                (min_width, max_width.max(min_width))
            }))
            .finish()
    }
}
