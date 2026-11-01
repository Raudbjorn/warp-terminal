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
use std::collections::HashMap;
use std::path::PathBuf;

use ::conn::overview::{OverviewItem, ProjectGroup, group_by_project};
use ::conn::story::ConnTurn;
use ::conn::{ConnEntry, ConnEntryKind, ConnSession, ConnStatus};
use chrono::{DateTime, Datelike, Local, Utc};
use markdown_parser::parse_markdown_with_gfm_tables;
use warp_core::ui::theme::WarpTheme;
use warp_core::ui::theme::color::internal_colors;
use warp_errors::report_error;
use warpui::color::ColorU;
use warpui::elements::new_scrollable::{NewScrollableElement, SingleAxisConfig};
use warpui::elements::{
    Align, ChildView, ClippedScrollStateHandle, ClippedScrollable, ConstrainedBox, Container,
    CrossAxisAlignment, DragBarSide, Element, Empty, Fill, Flex, FormattedTextElement,
    HeadingFontSizeMultipliers, HighlightedHyperlink, Hoverable, List, ListState, MainAxisSize,
    MouseStateHandle, NewScrollable, ParentElement, Rect, Resizable, ResizableStateHandle,
    ScrollStateHandle, ScrollbarWidth, Shrinkable, Text, resizable_state_handle,
};
use warpui::fonts::{Properties, Weight};
use warpui::platform::Cursor;
use warpui::{
    Action, AppContext, Entity, EntityId, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle,
};

use super::{ConnModel, preview};
use crate::appearance::Appearance;
use crate::drive::panel::{MAX_SIDEBAR_WIDTH_RATIO, MIN_SIDEBAR_WIDTH};
use crate::notebooks::file::FileNotebookView;
use crate::pane_group::PaneGroup;
use crate::terminal::resizable_data::{DEFAULT_RIGHT_PANEL_WIDTH, ModalType, ResizableData};
use crate::workspace::{WorkspaceAction, WorkspaceRegistry};

/// Truncation point for a prompt or a closing message. Long enough to
/// recognise what you asked for, short enough that the chapters stay
/// scannable.
const PREVIEW_CHARS: usize = 240;

/// How far a step is indented under the prompt it belongs to.
const STEP_INDENT: f32 = 22.;

/// Heading sizes for a rendered answer. The panel is a narrow sidebar, so headings stay close to
/// body size rather than taking the document-sized defaults.
const ANSWER_HEADING_SIZES: HeadingFontSizeMultipliers = HeadingFontSizeMultipliers {
    h1: 1.3,
    h2: 1.2,
    h3: 1.1,
    h4: 1.,
    h5: 1.,
    h6: 1.,
};

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
    /// Hover state for links in the latest answer, the only row that renders them.
    answer_link: HighlightedHyperlink,
    /// The plan each pane is reading in place of its story, keyed by terminal view. A pane keeps
    /// its entry while focus is elsewhere, so coming back to it shows the plan again.
    plans: HashMap<EntityId, OpenPlan>,
    show_plan_link: MouseStateHandle,
    show_story_link: MouseStateHandle,
    open_in_pane_link: MouseStateHandle,
    mode_link: MouseStateHandle,
    /// Cleared on render once focus has moved, so returning to the pane does not revive it.
    mode_choice: Cell<Option<ModeChoice>>,
    overview_scroll: ClippedScrollStateHandle,
    /// Click state for each overview row, keyed by terminal view. Kept in step with the model's
    /// panes, because a handle made during render would never register a click.
    overview_links: HashMap<EntityId, MouseStateHandle>,
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
    divider: Option<DateTime<Utc>>,
    /// When the reader's current visit to the pane began, if they are on one.
    visit: Option<DateTime<Utc>>,
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
        ctx.observe(&ConnModel::handle(ctx), |me, _, ctx| {
            me.sync_plans(ctx);
            me.sync_overview_links(ctx);
            ctx.notify();
        });

        let mut panel = Self {
            active_pane_group: None,
            list_state,
            scroll_state: ScrollStateHandle::default(),
            resizable_state_handle,
            answer_link: HighlightedHyperlink::default(),
            plans: HashMap::new(),
            show_plan_link: MouseStateHandle::default(),
            show_story_link: MouseStateHandle::default(),
            open_in_pane_link: MouseStateHandle::default(),
            mode_link: MouseStateHandle::default(),
            mode_choice: Cell::new(None),
            overview_scroll: ClippedScrollStateHandle::default(),
            overview_links: HashMap::new(),
            synced: Cell::new(None),
        };
        // A window opened after sessions exist sees no model change until the next event.
        panel.sync_overview_links(ctx);
        panel
    }

    pub fn set_active_pane_group(
        &mut self,
        pane_group: ViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.active_pane_group = Some(pane_group.downgrade());
        ctx.notify();
    }

    /// Keeps each open plan on its session's current plan file, and drops the plans of sessions
    /// that are gone or no longer have one.
    fn sync_plans(&mut self, ctx: &mut ViewContext<Self>) {
        let model = ConnModel::as_ref(ctx);
        let current: Vec<(EntityId, Option<PathBuf>)> = self
            .plans
            .keys()
            .map(|pane| {
                let plan_file = model
                    .session(*pane)
                    .and_then(|session| session.story())
                    .and_then(|story| story.plan_file.clone());
                (*pane, plan_file)
            })
            .collect();
        for (pane, plan_file) in current {
            match (plan_file, self.plans.get_mut(&pane)) {
                (Some(plan_file), Some(open)) if open.path != plan_file => {
                    open.path = plan_file.clone();
                    open.view
                        .update(ctx, |view, ctx| view.open_local(plan_file, None, ctx));
                }
                (Some(_), _) => {}
                (None, _) => {
                    self.plans.remove(&pane);
                }
            }
        }
    }

    fn sync_overview_links(&mut self, ctx: &mut ViewContext<Self>) {
        let model = ConnModel::as_ref(ctx);
        self.overview_links
            .retain(|pane, _| model.session(*pane).is_some());
        for pane in model.panes() {
            self.overview_links.entry(pane).or_default();
        }
    }

    fn mode(&self, app: &AppContext) -> PanelMode {
        let focused = self.focused_terminal_id(app);
        let choice = self
            .mode_choice
            .get()
            .filter(|choice| choice.pane == focused);
        self.mode_choice.set(choice);
        let has_session =
            focused.is_some_and(|pane| ConnModel::as_ref(app).session(pane).is_some());
        panel_mode(focused, has_session, choice)
    }

    fn toggle_mode(&mut self, ctx: &mut ViewContext<Self>) {
        let mode = match self.mode(ctx) {
            PanelMode::ThisPane => PanelMode::All,
            PanelMode::All => PanelMode::ThisPane,
        };
        self.mode_choice.set(Some(ModeChoice {
            pane: self.focused_terminal_id(ctx),
            mode,
        }));
        ctx.notify();
    }

    /// Every pane with a session in every window, as the overview lists them.
    fn overview(&self, app: &AppContext) -> Vec<ProjectGroup<EntityId>> {
        let model = ConnModel::as_ref(app);
        let items = open_terminal_panes_in_all_windows(app)
            .into_iter()
            .filter_map(|pane| {
                let session = model.session(pane)?;
                Some(OverviewItem {
                    pane,
                    project: session
                        .project
                        .clone()
                        .unwrap_or_else(|| "Unknown project".to_owned()),
                    title: session_title(session),
                    status: session.status,
                    unseen: session.unseen_turns(),
                })
            })
            .collect();
        group_by_project(items)
    }

    /// Drops a manual mode choice, for a pane picked explicitly even if it was already focused.
    pub fn follow_focused_pane(&mut self, ctx: &mut ViewContext<Self>) {
        self.mode_choice.set(None);
        ctx.notify();
    }

    fn show_plan(&mut self, ctx: &mut ViewContext<Self>) {
        let Some(pane) = self.focused_terminal_id(ctx) else {
            return;
        };
        let Some(path) = self.plan_file(ctx) else {
            return;
        };
        let view = ctx.add_typed_action_view(|ctx| {
            let mut view = FileNotebookView::new(ctx);
            view.open_local(path.clone(), None, ctx);
            view
        });
        self.plans.insert(pane, OpenPlan { path, view });
        ctx.notify();
    }

    fn show_story(&mut self, ctx: &mut ViewContext<Self>) {
        if let Some(pane) = self.focused_terminal_id(ctx) {
            self.plans.remove(&pane);
            ctx.notify();
        }
    }

    /// The plan file of the focused pane's session, when it has presented a plan.
    fn plan_file(&self, app: &AppContext) -> Option<PathBuf> {
        self.with_session(app, |session| {
            session.story().and_then(|story| story.plan_file.clone())
        })
        .flatten()
    }

    /// The plan the focused pane is reading in place of its story.
    fn open_plan(&self, app: &AppContext) -> Option<&OpenPlan> {
        self.plans.get(&self.focused_terminal_id(app)?)
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
        let model = ConnModel::as_ref(app);
        let divider = model.divider(pane);
        let (turns, rows) = self
            .with_session(app, |session| {
                (
                    session.story().map_or(0, |story| story.turns.len()),
                    row_count(session, divider),
                )
            })
            .unwrap_or((0, 0));
        let target = Sync {
            pane,
            turns,
            rows,
            divider,
            visit: model.visit_since(pane),
        };

        let arrived = match self.synced.get() {
            Some(synced) if synced == target => return rows,
            // Same pane and the same story, with more of it: extend and follow
            // the tail, which is where "what has it done since?" lives.
            Some(synced)
                if synced.pane == pane
                    && synced.turns == turns
                    && synced.divider == divider
                    && rows > synced.rows =>
            {
                for _ in synced.rows..rows {
                    self.list_state.add_item();
                }
                false
            }
            // A different pane, or the story was re-read and the rows moved.
            synced => {
                self.clear_list();
                for _ in 0..rows {
                    self.list_state.add_item();
                }
                synced.is_none_or(|synced| {
                    synced.pane != pane || (target.visit.is_some() && synced.visit != target.visit)
                })
            }
        };

        // Only arriving opens at the divider. Later changes follow the tail, so a story re-read
        // mid-turn does not pull the reader back.
        let scroll_to = if arrived {
            self.with_session(app, |session| scroll_target(session, divider))
                .flatten()
        } else {
            rows.checked_sub(1)
        };
        if let Some(index) = scroll_to {
            self.list_state.scroll_to(index);
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
        let divider = self.synced.get().and_then(|synced| synced.divider);
        self.with_session(app, |session| match row_at(session, divider, index) {
            Some(row) => render_row(&row, &self.answer_link, app),
            None => Empty::new().finish(),
        })
        .unwrap_or_else(|| Empty::new().finish())
    }

    /// The project, and the session's own generated title when it has one.
    /// Between them they answer "which of my sessions is this?" before any
    /// scrolling.
    fn render_header(&self, mode: PanelMode, app: &AppContext) -> Box<dyn Element> {
        if mode == PanelMode::All {
            return self.render_overview_header(app);
        }
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
        let status = self.with_session(app, |session| session.status);
        let heading = match project {
            Some(project) => format!("Conn — {project}"),
            None => "Conn".to_owned(),
        };

        let mut heading_row = Flex::row().with_spacing(8.).with_child(
            Text::new_inline(
                heading,
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_style(Properties::default().weight(Weight::Semibold))
            .with_color(theme.active_ui_text_color().into())
            .finish(),
        );
        if let Some(status) = status {
            heading_row = heading_row.with_child(
                Text::new_inline(
                    status.label(),
                    appearance.ui_font_family(),
                    appearance.ui_font_size(),
                )
                .with_color(status_color(status, theme))
                .finish(),
            );
        }

        heading_row = heading_row.with_child(self.render_link(
            "All sessions",
            &self.mode_link,
            ConnPanelAction::ToggleMode,
            app,
        ));

        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min)
            .with_spacing(2.)
            .with_child(heading_row.finish());

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

        if let Some(open) = self.open_plan(app) {
            let path = open.path.clone();
            column = column.with_child(
                Flex::row()
                    .with_spacing(12.)
                    .with_child(self.render_link(
                        "Back to story",
                        &self.show_story_link,
                        ConnPanelAction::ShowStory,
                        app,
                    ))
                    .with_child(self.render_link(
                        "Open in pane",
                        &self.open_in_pane_link,
                        WorkspaceAction::OpenConnPlan { path },
                        app,
                    ))
                    .finish(),
            );
        } else if self.plan_file(app).is_some() {
            column = column.with_child(self.render_link(
                "Open plan",
                &self.show_plan_link,
                ConnPanelAction::ShowPlan,
                app,
            ));
        }

        Container::new(column.finish())
            .with_uniform_padding(10.)
            .finish()
    }

    fn render_link<A: Action + Clone>(
        &self,
        label: &'static str,
        mouse_state: &MouseStateHandle,
        action: A,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size() - 1.;
        let theme = appearance.theme();
        let color = theme.accent().into_solid();
        let hovered_color = theme.active_ui_text_color().into();

        Hoverable::new(mouse_state.clone(), move |state| {
            Container::new(
                Text::new_inline(label, font_family, font_size)
                    .with_color(if state.is_hovered() {
                        hovered_color
                    } else {
                        color
                    })
                    .finish(),
            )
            .with_padding_top(4.)
            .finish()
        })
        .on_click(move |ctx, _, _| {
            ctx.dispatch_typed_action(action.clone());
        })
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn render_overview_header(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut row = Flex::row().with_spacing(8.).with_child(
            Text::new_inline(
                "Conn — all sessions",
                appearance.ui_font_family(),
                appearance.ui_font_size(),
            )
            .with_style(Properties::default().weight(Weight::Semibold))
            .with_color(appearance.theme().active_ui_text_color().into())
            .finish(),
        );
        if self.focused_terminal_id(app).is_some() {
            row = row.with_child(self.render_link(
                "This pane",
                &self.mode_link,
                ConnPanelAction::ToggleMode,
                app,
            ));
        }
        Container::new(row.finish())
            .with_uniform_padding(10.)
            .finish()
    }

    fn render_overview(&self, app: &AppContext) -> Box<dyn Element> {
        let groups = self.overview(app);
        if groups.is_empty() {
            return self.render_message("No agent sessions in any window.", app);
        }
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Start)
            .with_main_axis_size(MainAxisSize::Min);
        for group in groups {
            column = column.with_child(
                Container::new(
                    Text::new_inline(group.project, font_family, font_size - 1.)
                        .with_style(Properties::default().weight(Weight::Semibold))
                        .with_color(theme.nonactive_ui_text_color().into())
                        .finish(),
                )
                .with_padding_top(10.)
                .with_padding_bottom(2.)
                .finish(),
            );
            for item in group.items {
                let Some(mouse_state) = self.overview_links.get(&item.pane) else {
                    continue;
                };
                let title = item.title.unwrap_or_else(|| "New session".to_owned());
                let status = item.status;
                let unseen = (item.unseen > 0).then(|| format!("{} new", item.unseen));
                let pane = item.pane;
                let text_color = theme.active_ui_text_color().into();
                let hover_color = theme.surface_2().into_solid();
                let row = Hoverable::new(mouse_state.clone(), move |state| {
                    let mut row = Flex::row().with_spacing(8.).with_child(
                        Shrinkable::new(
                            1.,
                            Text::new_inline(title.clone(), font_family, font_size)
                                .with_color(text_color)
                                .finish(),
                        )
                        .finish(),
                    );
                    if let Some(unseen) = &unseen {
                        row = row.with_child(
                            Text::new_inline(unseen.clone(), font_family, font_size - 1.)
                                .with_color(text_color)
                                .finish(),
                        );
                    }
                    let mut container = Container::new(
                        row.with_child(
                            Text::new_inline(status.label(), font_family, font_size - 1.)
                                .with_color(status_color(status, theme))
                                .finish(),
                        )
                        .finish(),
                    )
                    .with_vertical_padding(4.)
                    .with_horizontal_padding(6.);
                    if state.is_hovered() {
                        container = container.with_background_color(hover_color);
                    }
                    container.finish()
                })
                .on_click(move |ctx, _, _| {
                    // Handled by the workspace rather than this view: focusing a pane in another
                    // tab updates this panel, which cannot happen inside its own update.
                    ctx.dispatch_typed_action(WorkspaceAction::FocusTerminalViewInWorkspace {
                        terminal_view_id: pane,
                    });
                })
                .with_cursor(Cursor::PointingHand)
                .finish();
                column = column.with_child(row);
            }
        }
        ClippedScrollable::vertical(
            self.overview_scroll.clone(),
            Container::new(column.finish())
                .with_horizontal_padding(10.)
                .with_padding_bottom(10.)
                .finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .finish()
    }

    /// Shown before a session exists in the focused pane. Says what the panel
    /// is waiting for rather than appearing broken.
    fn render_empty_state(&self, app: &AppContext) -> Box<dyn Element> {
        let message = match self.focused_terminal_id(app) {
            Some(_) => "No agent session in this pane yet.",
            None => "No pane focused.",
        };
        self.render_message(message, app)
    }

    fn render_message(&self, message: &'static str, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelMode {
    ThisPane,
    All,
}

/// A mode picked by hand, and the pane that was focused when it was picked.
#[derive(Debug, Clone, Copy)]
struct ModeChoice {
    pane: Option<EntityId>,
    mode: PanelMode,
}

/// The panel tells the focused pane's story when there is one and lists every session otherwise.
/// A choice made by hand wins until focus moves to another pane.
fn panel_mode(
    focused: Option<EntityId>,
    focused_has_session: bool,
    choice: Option<ModeChoice>,
) -> PanelMode {
    if let Some(choice) = choice.filter(|choice| choice.pane == focused) {
        choice.mode
    } else if focused_has_session {
        PanelMode::ThisPane
    } else {
        PanelMode::All
    }
}

/// A plan shown in the panel in place of a pane's story.
struct OpenPlan {
    path: PathBuf,
    view: ViewHandle<FileNotebookView>,
}

#[derive(Debug, Clone)]
pub enum ConnPanelAction {
    /// Show the focused pane's plan in place of its story.
    ShowPlan,
    /// Return the focused pane to its story.
    ShowStory,
    /// Switch between the focused pane's story and every session.
    ToggleMode,
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
    Outcome {
        text: &'a str,
        /// Whether this closes the story's last turn. That answer is the one being read, so it
        /// is shown whole and formatted; earlier ones stay short so the chapters remain scannable.
        latest: bool,
    },
    /// A live hook event from the turn in flight. Thinner than a step — the
    /// plugin sends a tool's name and nothing about what it did — which is
    /// why the transcript is read at all.
    Live(&'a ConnEntry),
    /// No transcript was found, so there is no account to tell and the rows
    /// below are bare tool names.
    NoTranscript,
    /// Where the reader's unseen turns begin.
    SinceYouLooked,
}

/// How much of a prompt an overview row shows when the session has no title yet.
const TITLE_CHARS: usize = 80;

/// Claude Code's own title for the session, or failing that the last thing asked of it.
fn session_title(session: &ConnSession) -> Option<String> {
    session
        .story()
        .and_then(|story| story.title.clone())
        .or_else(|| {
            session
                .prompts()
                .last()
                .map(|prompt| preview::head(prompt, TITLE_CHARS))
        })
}

/// Every open terminal pane in every window.
fn open_terminal_panes_in_all_windows(app: &AppContext) -> Vec<EntityId> {
    WorkspaceRegistry::as_ref(app)
        .all_workspaces(app)
        .into_iter()
        .flat_map(|(_, workspace)| {
            workspace
                .as_ref(app)
                .tab_views()
                .flat_map(|pane_group| pane_group.as_ref(app).open_terminal_panes(app))
                .map(|(_, terminal_view_id)| terminal_view_id)
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Matches the colors Warp gives the same states on tabs and pane headers.
fn status_color(status: ConnStatus, theme: &WarpTheme) -> ColorU {
    match status {
        ConnStatus::Working => theme.ansi_fg_magenta(),
        ConnStatus::NeedsYou => theme.ansi_fg_yellow(),
        ConnStatus::Done => theme.ansi_fg_green(),
        ConnStatus::Failed => theme.ansi_fg_red(),
        ConnStatus::Ended => internal_colors::neutral_5(theme),
    }
}

/// Whether the panel is running on hook events alone.
///
/// The hooks report a tool's name and nothing about what it did, so this is a
/// visibly worse panel. Saying so beats letting someone conclude the feature
/// is broken — most often the cause is fixable, such as a session started with
/// transcript saving turned off.
fn is_hook_only(session: &ConnSession) -> bool {
    session.story().is_none_or(|story| story.turns.is_empty()) && !session.entries().is_empty()
}

/// Live rows worth showing beside a story.
///
/// Tool activity drops out once there is a story. The transcript names each
/// call in the agent's own words within a few seconds, and the hook entry
/// coalesces every call in the turn into a single row whose time keeps moving,
/// so it sits below the chapter restating the same work as `Bash ×18`.
///
/// What stays is what the story cannot say: a permission request or a question
/// is the session waiting on *you*, which is the most important thing the
/// panel can report, and the transcript records it as just another tool call.
fn live_rows(session: &ConnSession) -> impl Iterator<Item = &ConnEntry> {
    let told = session.story().is_some_and(|story| !story.turns.is_empty());
    session
        .in_flight()
        .iter()
        .filter(move |entry| !told || !matches!(entry.kind, ConnEntryKind::ToolCompleted { .. }))
}

/// Rows one turn occupies: its prompt, its steps, and its closing message if
/// it has one yet.
fn turn_rows(turn: &ConnTurn) -> usize {
    1 + turn.steps.len() + usize::from(turn.outcome.is_some())
}

fn row_count(session: &ConnSession, divider: Option<DateTime<Utc>>) -> usize {
    let told = session
        .story()
        .map_or(0, |story| story.turns.iter().map(turn_rows).sum());
    told + live_rows(session).count()
        + usize::from(is_hook_only(session))
        + usize::from(divider_turn(session, divider).is_some())
}

/// The turn the "since you looked" divider opens: the first to start at or after `divider`.
fn divider_turn(session: &ConnSession, divider: Option<DateTime<Utc>>) -> Option<usize> {
    let divider = divider?;
    session
        .story()?
        .turns
        .iter()
        .position(|turn| turn.started_at >= divider)
}

/// The row a story opens at: the divider when there is one, otherwise the tail, where "what has it
/// done since?" lives.
fn scroll_target(session: &ConnSession, divider: Option<DateTime<Utc>>) -> Option<usize> {
    let (Some(divider_turn), Some(story)) = (divider_turn(session, divider), session.story())
    else {
        return row_count(session, divider).checked_sub(1);
    };
    Some(
        usize::from(is_hook_only(session))
            + story.turns[..divider_turn]
                .iter()
                .map(turn_rows)
                .sum::<usize>(),
    )
}

/// Walks the story to the row at `index`.
///
/// Linear in the number of rows, which costs nothing because the list only
/// asks for the rows in view.
fn row_at(
    session: &ConnSession,
    divider: Option<DateTime<Utc>>,
    index: usize,
) -> Option<ConnRow<'_>> {
    let mut remaining = index;
    if is_hook_only(session) {
        if remaining == 0 {
            return Some(ConnRow::NoTranscript);
        }
        remaining -= 1;
    }
    if let Some(story) = session.story() {
        let divider_turn = divider_turn(session, divider);
        for (turn_index, turn) in story.turns.iter().enumerate() {
            if divider_turn == Some(turn_index) {
                if remaining == 0 {
                    return Some(ConnRow::SinceYouLooked);
                }
                remaining -= 1;
            }
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
            return turn.outcome.as_deref().map(|text| ConnRow::Outcome {
                text,
                latest: turn_index + 1 == story.turns.len(),
            });
        }
    }
    live_rows(session).nth(remaining).map(ConnRow::Live)
}

fn render_row(
    row: &ConnRow,
    answer_link: &HighlightedHyperlink,
    app: &AppContext,
) -> Box<dyn Element> {
    match row {
        ConnRow::Turn { at, prompt } => render_turn(*at, prompt, app),
        ConnRow::Step { description, runs } => render_step(description, *runs, app),
        ConnRow::Outcome { text, latest: true } => render_labelled(
            "said",
            markdown_body(text, Some(answer_link), app),
            false,
            app,
        ),
        ConnRow::Outcome {
            text,
            latest: false,
        } => render_labelled("said", plain_body(tail_preview(text), app), false, app),
        ConnRow::SinceYouLooked => render_since_you_looked(app),
        // A new instruction opens a chapter whether the transcript has caught
        // up with it or not.
        ConnRow::NoTranscript => render_notice(
            "No transcript for this session, so this is tool names only. A \
             session started with transcript saving off writes none.",
            app,
        ),
        ConnRow::Live(entry) => match &entry.kind {
            ConnEntryKind::Prompt { text } => render_turn(entry.at, text, app),
            // A live answer is replaced by the story's outcome row once the transcript is
            // read, so it does not get link hover state of its own.
            ConnEntryKind::Responded { text: Some(text) } => {
                render_labelled("said", markdown_body(text, None, app), false, app)
            }
            kind => {
                let (label, body, emphasised) = describe_live(kind);
                render_labelled(label, plain_body(body, app), emphasised, app)
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
        .with_child(labelled_body(
            "you",
            plain_body(truncate(prompt), app),
            true,
            app,
        ))
        .finish();

    Container::new(column)
        .with_padding_left(10.)
        .with_padding_right(10.)
        .with_padding_top(14.)
        .with_padding_bottom(6.)
        .finish()
}

fn render_since_you_looked(app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    let theme = appearance.theme();
    Container::new(
        Flex::row()
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(8.)
            .with_child(
                Text::new_inline(
                    "since you looked",
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 2.,
                )
                .with_color(theme.accent().into_solid())
                .finish(),
            )
            .with_child(
                Shrinkable::new(
                    1.,
                    ConstrainedBox::new(Rect::new().with_background(theme.accent()).finish())
                        .with_height(1.)
                        .finish(),
                )
                .finish(),
            )
            .finish(),
    )
    .with_horizontal_padding(10.)
    .with_padding_top(14.)
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

/// A quiet line about the panel itself rather than about the session.
fn render_notice(message: &'static str, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    Container::new(
        Text::new(
            message,
            appearance.ui_font_family(),
            appearance.ui_font_size() - 1.,
        )
        .with_color(appearance.theme().nonactive_ui_text_color().into())
        .finish(),
    )
    .with_uniform_padding(10.)
    .finish()
}

fn render_labelled(
    label: &'static str,
    body: Box<dyn Element>,
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
    body: Box<dyn Element>,
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
        .with_child(body)
        .finish()
}

fn plain_body(text: String, app: &AppContext) -> Box<dyn Element> {
    let appearance = Appearance::as_ref(app);
    Text::new(text, appearance.ui_font_family(), appearance.ui_font_size())
        .with_color(appearance.theme().active_ui_text_color().into())
        .finish()
}

/// A whole message rendered as Markdown. With `link` its hyperlinks are clickable; without, the
/// text ignores the mouse. Text the parser rejects is shown as written.
fn markdown_body(
    text: &str,
    link: Option<&HighlightedHyperlink>,
    app: &AppContext,
) -> Box<dyn Element> {
    let text = text.trim();
    let Ok(parsed) = parse_markdown_with_gfm_tables(text) else {
        return plain_body(text.to_owned(), app);
    };
    let appearance = Appearance::as_ref(app);
    let element = FormattedTextElement::new(
        parsed,
        appearance.ui_font_size(),
        appearance.ui_font_family(),
        appearance.monospace_font_family(),
        appearance.theme().active_ui_text_color().into(),
        link.cloned().unwrap_or_default(),
    )
    .with_heading_to_font_size_multipliers(ANSWER_HEADING_SIZES);
    match link {
        Some(_) => element
            .register_default_click_handlers(|url, _, app| app.open_url(&url.url))
            .finish(),
        None => element.disable_mouse_interaction().finish(),
    }
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
            text.as_deref().map(tail_preview).unwrap_or_default(),
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
        return truncate(&drop_json_tail(summary));
    }
    describe_tool(tool_name, target)
}

/// Cuts a raw JSON blob off the end of a summary.
///
/// The plugin builds "Wants to run X: <preview>", where the preview is the
/// tool's `command` or `file_path` — or, for a tool with neither, its whole
/// input serialised. A wall of JSON in the panel says less than the tool's
/// name alone.
fn drop_json_tail(summary: &str) -> String {
    match summary.split_once(": ") {
        Some((head, tail)) if tail.trim_start().starts_with(['{', '[']) => head.to_owned(),
        _ => summary.to_owned(),
    }
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
/// arbitrary user and model text, and on a word boundary where there is one
/// nearby, so a cut prompt does not end mid-word.
fn truncate(text: &str) -> String {
    preview::head(text, PREVIEW_CHARS)
}

/// The end of a long message rather than its beginning. See [`preview::tail`].
fn tail_preview(text: &str) -> String {
    preview::tail(text, PREVIEW_CHARS)
}

impl View for ConnPanelView {
    fn ui_name() -> &'static str {
        "ConnPanelView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let rows = self.reconcile(app);
        let mode = self.mode(app);

        let content: Box<dyn Element> = if mode == PanelMode::All {
            Shrinkable::new(1.0, self.render_overview(app)).finish()
        } else if let Some(open) = self.open_plan(app) {
            Shrinkable::new(1.0, ChildView::new(&open.view).finish()).finish()
        } else if rows > 0 {
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
            .with_child(self.render_header(mode, app))
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

impl TypedActionView for ConnPanelView {
    type Action = ConnPanelAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            ConnPanelAction::ShowPlan => self.show_plan(ctx),
            ConnPanelAction::ShowStory => self.show_story(ctx),
            ConnPanelAction::ToggleMode => self.toggle_mode(ctx),
        }
    }
}

#[cfg(test)]
#[path = "panel_tests.rs"]
mod tests;
