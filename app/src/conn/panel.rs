//! The Conn panel: what you read when you come back to a pane.
//!
//! Tells the focused pane's session as a story rather than a log. Each turn is
//! a chapter — what was asked, what the agent did in the words it used at the
//! time, and what came of it — read from the transcript by
//! [`conn::story`](::conn::story). The turn still in flight has no transcript
//! account yet, so it falls back to the live hook events.
//!
//! Read-only; it reflects state and never drives a session.

use std::cell::Cell;

use ::conn::story::ConnTurn;
use ::conn::{ConnEntry, ConnEntryKind, ConnSession};
use chrono::{DateTime, Datelike, Local, Utc};
use warp_errors::report_error;
use warpui::elements::new_scrollable::{NewScrollableElement, SingleAxisConfig};
use warpui::elements::{
    Align, ConstrainedBox, Container, CrossAxisAlignment, DragBarSide, Element, Empty, Fill, Flex,
    List, ListState, MainAxisSize, NewScrollable, ParentElement, Rect, Resizable,
    ResizableStateHandle, ScrollStateHandle, Shrinkable, Text, resizable_state_handle,
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

/// Truncation point for a prompt or a closing message. Long enough to
/// recognise what you asked for, short enough that the chapters stay
/// scannable.
const PREVIEW_CHARS: usize = 240;

/// How far a step is indented under the prompt it belongs to.
const STEP_INDENT: f32 = 22.;

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
    /// The pane, turn count and row count the list was last reconciled to.
    ///
    /// `ListState` owns its item count as mutable state, so it has to be told
    /// when the story grows. Reconciling happens during render, and so needs a
    /// `Cell`, because focus can move between panes of one group without
    /// emitting anything the panel can subscribe to — and a panel showing
    /// another pane's story is worse than one that is briefly behind.
    synced: Cell<Option<Sync>>,
}

/// What the list was last built for. A change in any field means the rows
/// moved, not just grew.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sync {
    pane: EntityId,
    turns: usize,
    rows: usize,
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

        let weak_self = ctx.handle();
        // The rows are addressed into the model rather than cached, so this
        // closure never goes stale and is built once.
        let list_state = ListState::new(move |index, _scroll_offset, app| {
            let Some(view) = weak_self.upgrade(app) else {
                return Empty::new().finish();
            };
            view.as_ref(app).render_row_at(index, app)
        });

        // The story grows while the panel is open, so it has to follow the
        // model rather than only redraw on focus changes.
        ctx.observe(&ConnModel::handle(ctx), |_, _, ctx| {
            ctx.notify();
        });

        Self {
            active_pane_group: None,
            list_state,
            scroll_state: ScrollStateHandle::default(),
            resizable_state_handle,
            synced: Cell::new(None),
        }
    }

    pub fn set_active_pane_group(
        &mut self,
        pane_group: ViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.active_pane_group = Some(pane_group.downgrade());
        ctx.notify();
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

    fn with_session<T>(&self, app: &AppContext, read: impl FnOnce(&ConnSession) -> T) -> Option<T> {
        let id = self.focused_terminal_id(app)?;
        ConnModel::as_ref(app).session(id).map(read)
    }

    /// Brings the list's item count in line with the story, and follows the
    /// tail when it has grown. Returns the number of rows.
    ///
    /// Heights are cached per index, so anything that could have moved a row
    /// to a different index rebuilds rather than extends. A story whose text
    /// changed without changing any count is the one case this misses; it
    /// corrects itself on the next event.
    fn reconcile(&self, app: &AppContext) -> usize {
        let Some(pane) = self.focused_terminal_id(app) else {
            self.clear_list();
            self.synced.set(None);
            return 0;
        };
        let (turns, rows) = self
            .with_session(app, |session| {
                (
                    session.story().map_or(0, |story| story.turns.len()),
                    row_count(session),
                )
            })
            .unwrap_or((0, 0));
        let target = Sync { pane, turns, rows };

        match self.synced.get() {
            Some(synced) if synced == target => return rows,
            // Same pane and the same story, with more of it: extend and follow
            // the tail, which is where "what has it done since?" lives.
            Some(synced) if synced.pane == pane && synced.turns == turns && rows > synced.rows => {
                for _ in synced.rows..rows {
                    self.list_state.add_item();
                }
            }
            // A different pane, or the story was re-read and the rows moved.
            _ => {
                self.clear_list();
                for _ in 0..rows {
                    self.list_state.add_item();
                }
            }
        }

        if rows > 0 {
            self.list_state.scroll_to(rows - 1);
        }
        self.synced.set(Some(target));
        rows
    }

    fn clear_list(&self) {
        let Some(synced) = self.synced.get() else {
            return;
        };
        for index in (0..synced.rows).rev() {
            self.list_state.remove(index);
        }
    }

    fn render_row_at(&self, index: usize, app: &AppContext) -> Box<dyn Element> {
        self.with_session(app, |session| match row_at(session, index) {
            Some(row) => render_row(&row, app),
            None => Empty::new().finish(),
        })
        .unwrap_or_else(|| Empty::new().finish())
    }

    /// The project, and the session's own generated title when it has one.
    /// Between them they answer "which of my sessions is this?" before any
    /// scrolling.
    fn render_header(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let project = self
            .with_session(app, |session| session.project.clone())
            .flatten();
        let title = self
            .with_session(app, |session| {
                session.story().and_then(|story| story.title.clone())
            })
            .flatten();

        let heading = match project {
            Some(project) => format!("Conn — {project}"),
            None => "Conn".to_owned(),
        };

        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_spacing(2.)
            .with_child(
                Text::new_inline(
                    heading,
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_style(Properties::default().weight(Weight::Semibold))
                .with_color(theme.active_ui_text_color().into())
                .finish(),
            );

        if let Some(title) = title {
            column = column.with_child(
                Text::new_inline(
                    title,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.nonactive_ui_text_color().into())
                .finish(),
            );
        }

        Container::new(column.finish())
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

/// One line of the story.
///
/// Borrowed from the model rather than built into a cached list, so a row can
/// never describe a pane the panel is no longer looking at.
enum ConnRow<'a> {
    /// A turn's heading: when it started and what was asked for.
    Turn { at: DateTime<Utc>, prompt: &'a str },
    /// Something the agent did, in the words it used at the time.
    Step { description: &'a str, runs: usize },
    /// The agent's closing message for a turn.
    Outcome(&'a str),
    /// A live hook event from the turn in flight. Thinner than a step — the
    /// plugin sends a tool's name and nothing about what it did — which is
    /// why the transcript is read at all.
    Live(&'a ConnEntry),
}

/// Rows one turn occupies: its prompt, its steps, and its closing message if
/// it has one yet.
fn turn_rows(turn: &ConnTurn) -> usize {
    1 + turn.steps.len() + usize::from(turn.outcome.is_some())
}

fn row_count(session: &ConnSession) -> usize {
    let told = session
        .story()
        .map_or(0, |story| story.turns.iter().map(turn_rows).sum());
    told + session.in_flight().len()
}

/// Walks the story to the row at `index`.
///
/// Linear in the number of rows, which costs nothing because the list only
/// asks for the rows in view.
fn row_at(session: &ConnSession, index: usize) -> Option<ConnRow<'_>> {
    let mut remaining = index;
    if let Some(story) = session.story() {
        for turn in &story.turns {
            let rows = turn_rows(turn);
            if remaining >= rows {
                remaining -= rows;
                continue;
            }
            if remaining == 0 {
                return Some(ConnRow::Turn {
                    at: turn.started_at,
                    prompt: &turn.prompt,
                });
            }
            if let Some(step) = turn.steps.get(remaining - 1) {
                return Some(ConnRow::Step {
                    description: &step.description,
                    runs: step.runs,
                });
            }
            return turn.outcome.as_deref().map(ConnRow::Outcome);
        }
    }
    session.in_flight().get(remaining).map(ConnRow::Live)
}

fn render_row(row: &ConnRow, app: &AppContext) -> Box<dyn Element> {
    match row {
        ConnRow::Turn { at, prompt } => render_turn(*at, prompt, app),
        ConnRow::Step { description, runs } => render_step(description, *runs, app),
        ConnRow::Outcome(text) => render_labelled("said", truncate(text), false, app),
        // A new instruction opens a chapter whether the transcript has caught
        // up with it or not.
        ConnRow::Live(entry) => match &entry.kind {
            ConnEntryKind::Prompt { text } => render_turn(entry.at, text, app),
            kind => {
                let (label, body, emphasised) = describe_live(kind);
                render_labelled(label, body, emphasised, app)
            }
        },
    }
}

/// A chapter heading: when the turn started, then what was asked for.
fn render_turn(at: DateTime<Utc>, prompt: &str, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();

    let rule = || {
        Shrinkable::new(
            1.,
            ConstrainedBox::new(Rect::new().with_background(theme.surface_2()).finish())
                .with_height(1.)
                .finish(),
        )
        .finish()
    };

    let divider = Flex::row()
        .with_cross_axis_alignment(CrossAxisAlignment::Center)
        .with_main_axis_size(MainAxisSize::Max)
        .with_child(
            Container::new(
                Text::new_inline(
                    format_when(at),
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 2.,
                )
                .with_color(theme.nonactive_ui_text_color().into())
                .finish(),
            )
            .with_padding_right(8.)
            .finish(),
        )
        .with_child(rule())
        .finish();

    let column = Flex::column()
        .with_cross_axis_alignment(CrossAxisAlignment::Start)
        .with_main_axis_size(MainAxisSize::Min)
        .with_spacing(6.)
        .with_child(divider)
        .with_child(labelled_body("you", truncate(prompt), true, app))
        .finish();

    Container::new(column)
        .with_padding_left(10.)
        .with_padding_right(10.)
        .with_padding_top(14.)
        .with_padding_bottom(6.)
        .finish()
}

/// A step under the prompt it belongs to. No label: the words are the model's
/// own account, and a repeated label beside fifteen of them is noise.
fn render_step(description: &str, runs: usize, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let body = if runs > 1 {
        format!("{} ×{runs}", truncate(description))
    } else {
        truncate(description)
    };

    Container::new(
        Text::new(
            body,
            appearance.ui_font_family(),
            appearance.ui_font_size() - 1.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish(),
    )
    .with_padding_left(10. + STEP_INDENT)
    .with_padding_right(10.)
    .with_padding_top(3.)
    .with_padding_bottom(3.)
    .finish()
}

fn render_labelled(
    label: &'static str,
    body: String,
    emphasised: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    Container::new(labelled_body(label, body, emphasised, app))
        .with_padding_left(10.)
        .with_padding_right(10.)
        .with_padding_top(6.)
        .with_padding_bottom(6.)
        .finish()
}

/// A small label over its text. Plain: the value here is the content and its
/// order, not the presentation.
fn labelled_body(
    label: &'static str,
    body: String,
    emphasised: bool,
    app: &AppContext,
) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    let label_color = if emphasised {
        theme.active_ui_text_color()
    } else {
        theme.nonactive_ui_text_color()
    };

    Flex::column()
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
        .finish()
}

/// A live hook event as a label and a line.
fn describe_live(kind: &ConnEntryKind) -> (&'static str, String, bool) {
    match kind {
        // Handled as a chapter heading before this is reached.
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
    }
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

/// The time a turn started, with the date once it is no longer today. A
/// session left overnight is exactly the case this panel exists for, and a
/// bare clock time would read as recent.
fn format_when(at: DateTime<Utc>) -> String {
    let at = at.with_timezone(&Local);
    let now = Local::now();
    let time = at.format("%l:%M%P").to_string().trim().to_owned();
    if at.year() == now.year() && at.ordinal() == now.ordinal() {
        return time;
    }
    format!("{} {}, {time}", at.format("%b"), at.day())
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
        let rows = self.reconcile(app);

        let content: Box<dyn Element> = if rows > 0 {
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

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
