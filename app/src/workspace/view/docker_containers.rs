#![cfg(not(target_family = "wasm"))]

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use pathfinder_geometry::vector::vec2f;
use serde::Deserialize;
use warpui::{
    elements::{
        Border, ChildAnchor, ChildView, Clipped, ClippedScrollStateHandle, ClippedScrollable,
        ConstrainedBox, Container, CornerRadius, CrossAxisAlignment, Element, Fill, Flex,
        Hoverable, MainAxisAlignment, MainAxisSize, MouseStateHandle, OffsetPositioning, Padding,
        ParentAnchor, ParentElement, ParentOffsetBounds, Radius, ScrollbarWidth, Shrinkable, Stack,
        Text,
    },
    fonts::{Properties, Weight},
    platform::Cursor,
    r#async::FutureExt as _,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
    },
    AppContext, Entity, FocusContext, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle, WeakViewHandle,
};

use crate::{
    appearance::Appearance,
    code::buffer_location::LocalOrRemotePath,
    editor::{
        EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions,
        TextOptions,
    },
    menu::{Event as MenuEvent, Menu, MenuItemFields},
    pane_group::PaneGroup,
    remote_server::manager::RemoteServerManager,
    terminal::model::session::{ExecuteCommandOptions, Session, SessionType},
    ui_components::icons::Icon,
    ui_components::menu_button::{icon_button_with_context_menu, MenuDirection},
    util::time_format::format_approx_duration_from_now_utc,
};

const DOCKER_COMMAND_TIMEOUT: Duration = Duration::from_secs(20);
const DOCKER_LIST_COMMAND: &str = r#"sh -lc 'docker ps --format '"'"'{{json .}}'"'"''"#;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum DockerContainerSortColumn {
    Name,
    Age,
    Image,
}

impl DockerContainerSortColumn {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Age => "Age",
            Self::Image => "Image",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Asc => "asc",
            Self::Desc => "desc",
        }
    }
}

#[derive(Clone, Debug)]
pub enum DockerContainersAction {
    Refresh,
    SortBy(DockerContainerSortColumn),
    OpenLogs(String),
    ToggleContainerMenu(String),
    StopContainer(String),
}

#[derive(Clone, Debug)]
pub enum DockerContainersEvent {
    OpenLogs {
        container_id: String,
        container_name: String,
        command: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DockerContainerSummary {
    id: String,
    name: String,
    image_name: String,
    image_tag: String,
    created_at: Option<DateTime<Utc>>,
}

#[derive(Clone, Debug)]
struct DockerCommandContext {
    session: Arc<Session>,
    current_dir: Option<String>,
    environment_variables: Option<HashMap<String, String>>,
    log_target: DockerLogTarget,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum DockerLogTarget {
    Local,
    RemoteSsh {
        spawning_command: Option<String>,
        control_path: Option<PathBuf>,
        host: String,
        port: Option<String>,
    },
}

#[derive(Clone, Debug)]
struct DockerOverflowMenuState {
    container_id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DockerContainersLoadState {
    Idle,
    Loading,
    Loaded,
}

#[derive(Deserialize)]
struct DockerPsContainer {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Image")]
    image: Option<String>,
    #[serde(rename = "Names")]
    names: Option<String>,
    #[serde(rename = "CreatedAt")]
    created_at: Option<String>,
}

#[derive(Deserialize)]
struct DockerInspectContainer {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: Option<String>,
    #[serde(rename = "Created")]
    created: Option<String>,
    #[serde(rename = "Config")]
    config: Option<DockerInspectConfig>,
}

#[derive(Deserialize)]
struct DockerInspectConfig {
    #[serde(rename = "Image")]
    image: Option<String>,
}

pub struct DockerContainersView {
    active_pane_group: Option<WeakViewHandle<PaneGroup>>,
    query_editor: ViewHandle<EditorView>,
    container_overflow_menu: ViewHandle<Menu<DockerContainersAction>>,
    overflow_menu_state: Option<DockerOverflowMenuState>,
    refresh_button: MouseStateHandle,
    container_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    container_overflow_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    sort_buttons: RefCell<HashMap<DockerContainerSortColumn, MouseStateHandle>>,
    list_scroll_state: ClippedScrollStateHandle,
    containers: Vec<DockerContainerSummary>,
    stopping_containers: HashSet<String>,
    last_successful_command_context: Option<DockerCommandContext>,
    load_state: DockerContainersLoadState,
    last_error: Option<String>,
    sort_column: DockerContainerSortColumn,
    sort_direction: SortDirection,
}

impl DockerContainersView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let query_editor = ctx.add_typed_action_view(|ctx| {
            let appearance = Appearance::as_ref(ctx);
            let mut editor = EditorView::single_line(
                SingleLineEditorOptions {
                    text: TextOptions::ui_text(Some(13.), appearance),
                    propagate_and_no_op_vertical_navigation_keys:
                        PropagateAndNoOpNavigationKeys::Always,
                    select_all_on_focus: true,
                    ..Default::default()
                },
                ctx,
            );
            editor.set_placeholder_text("Search Docker containers", ctx);
            editor
        });
        ctx.subscribe_to_view(&query_editor, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                ctx.notify();
            }
        });

        let container_overflow_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(140.)
        });
        ctx.subscribe_to_view(&container_overflow_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.overflow_menu_state = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });

        Self {
            active_pane_group: None,
            query_editor,
            container_overflow_menu,
            overflow_menu_state: None,
            refresh_button: Default::default(),
            container_buttons: Default::default(),
            container_overflow_buttons: Default::default(),
            sort_buttons: Default::default(),
            list_scroll_state: Default::default(),
            containers: Vec::new(),
            stopping_containers: HashSet::new(),
            last_successful_command_context: None,
            load_state: DockerContainersLoadState::Idle,
            last_error: None,
            sort_column: DockerContainerSortColumn::Name,
            sort_direction: SortDirection::Asc,
        }
    }

    pub fn set_active_pane_group(
        &mut self,
        pane_group: WeakViewHandle<PaneGroup>,
        ctx: &mut ViewContext<Self>,
    ) {
        self.active_pane_group = Some(pane_group);
        if self.load_state == DockerContainersLoadState::Loaded {
            self.refresh(ctx);
        }
    }

    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.query_editor);
        self.refresh_if_needed(ctx);
    }

    pub fn refresh_if_needed(&mut self, ctx: &mut ViewContext<Self>) {
        if self.load_state == DockerContainersLoadState::Idle {
            self.refresh(ctx);
        }
    }

    pub fn refresh(&mut self, ctx: &mut ViewContext<Self>) {
        let command_context = match self.active_session_context(ctx) {
            Ok(command_context) => command_context,
            Err(error) => {
                self.last_error = Some(error);
                self.load_state = DockerContainersLoadState::Loaded;
                ctx.notify();
                return;
            }
        };

        self.load_state = DockerContainersLoadState::Loading;
        self.last_error = None;
        ctx.notify();

        ctx.spawn(
            async move {
                let containers = list_docker_containers(command_context.clone()).await?;
                Ok((containers, command_context))
            },
            |view, result, ctx| {
                match result {
                    Ok((containers, command_context)) => {
                        view.containers = containers;
                        view.last_successful_command_context = Some(command_context);
                        view.last_error = None;
                    }
                    Err(error) => {
                        view.last_error = Some(error);
                    }
                }
                view.load_state = DockerContainersLoadState::Loaded;
                ctx.notify();
            },
        );
    }

    fn active_session_context(&self, app: &AppContext) -> Result<DockerCommandContext, String> {
        let pane_group = self
            .active_pane_group
            .as_ref()
            .and_then(|pane_group| pane_group.upgrade(app))
            .ok_or_else(|| "No active pane group.".to_string())?;
        let terminal_view = pane_group
            .as_ref(app)
            .active_session_view(app)
            .ok_or_else(|| {
                "Open a terminal session before listing Docker containers.".to_string()
            })?;

        terminal_view.read(app, |terminal, app| {
            let session_id = terminal
                .active_block_session_id()
                .ok_or_else(|| "No active terminal session.".to_string())?;
            let session = terminal
                .sessions_model()
                .as_ref(app)
                .get(session_id)
                .ok_or_else(|| "The active terminal session is no longer available.".to_string())?;
            let current_dir = terminal.pwd_as_local_or_remote(app).map(|path| match path {
                LocalOrRemotePath::Local(path) => path.to_string_lossy().to_string(),
                LocalOrRemotePath::Remote(path) => path.path.as_str().to_string(),
            });
            let environment_variables = session
                .path()
                .as_deref()
                .map(|path| HashMap::from_iter([("PATH".to_string(), path.to_string())]));
            let log_target = docker_log_target_for_session(&session, app);

            Ok(DockerCommandContext {
                session,
                current_dir,
                environment_variables,
                log_target,
            })
        })
    }

    fn render_status(&self, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let status = match self.load_state {
            DockerContainersLoadState::Idle => "Not loaded".to_string(),
            DockerContainersLoadState::Loading => "Loading...".to_string(),
            DockerContainersLoadState::Loaded => {
                let count = self.containers.len();
                format!("{count} running")
            }
        };

        Flex::column()
            .with_spacing(2.)
            .with_child(
                Text::new_inline(
                    "Docker containers",
                    appearance.ui_font_family(),
                    appearance.ui_font_size() + 1.,
                )
                .with_style(Properties::default().weight(Weight::Bold))
                .with_color(theme.main_text_color(theme.background()).into())
                .finish(),
            )
            .with_child(
                Text::new_inline(
                    status,
                    appearance.ui_font_family(),
                    appearance.ui_font_size() - 1.,
                )
                .with_color(theme.sub_text_color(theme.background()).into())
                .finish(),
            )
            .finish()
    }

    fn render_search(&self, appearance: &Appearance) -> Box<dyn Element> {
        Container::new(
            ConstrainedBox::new(Clipped::new(ChildView::new(&self.query_editor).finish()).finish())
                .with_height(28.)
                .finish(),
        )
        .with_background_color(appearance.theme().surface_2().into())
        .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
        .with_uniform_padding(6.)
        .finish()
    }

    fn render_refresh_button(&self, appearance: &Appearance) -> Box<dyn Element> {
        appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, self.refresh_button.clone())
            .with_text_label("Refresh".to_string())
            .with_style(UiComponentStyles {
                padding: Some(Coords::default().top(6.).bottom(6.).left(12.).right(12.)),
                ..Default::default()
            })
            .build()
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(DockerContainersAction::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn render_sort_button(
        &self,
        column: DockerContainerSortColumn,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mouse_state = self
            .sort_buttons
            .borrow_mut()
            .entry(column)
            .or_default()
            .clone();
        let label = if self.sort_column == column {
            format!("{} {}", column.label(), self.sort_direction.label())
        } else {
            column.label().to_string()
        };

        appearance
            .ui_builder()
            .button(ButtonVariant::Secondary, mouse_state)
            .with_text_label(label)
            .with_style(UiComponentStyles {
                padding: Some(Coords::default().top(4.).bottom(4.).left(8.).right(8.)),
                ..Default::default()
            })
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(DockerContainersAction::SortBy(column));
            })
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn render_sort_controls(&self, appearance: &Appearance) -> Box<dyn Element> {
        Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_cross_axis_alignment(CrossAxisAlignment::Center)
            .with_spacing(6.)
            .with_child(self.render_sort_button(DockerContainerSortColumn::Name, appearance))
            .with_child(self.render_sort_button(DockerContainerSortColumn::Age, appearance))
            .with_child(self.render_sort_button(DockerContainerSortColumn::Image, appearance))
            .finish()
    }

    fn render_error(&self, appearance: &Appearance) -> Option<Box<dyn Element>> {
        let error = self.last_error.as_ref()?;
        Some(
            Container::new(
                appearance
                    .ui_builder()
                    .wrappable_text(error.clone(), true)
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(appearance.theme().ansi_fg_red()),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            )
            .with_background_color(appearance.theme().surface_2().into())
            .with_border(Border::all(1.).with_border_fill(appearance.theme().outline()))
            .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
            .with_uniform_padding(10.)
            .finish(),
        )
    }

    fn render_container(
        &self,
        container: &DockerContainerSummary,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut buttons = self.container_buttons.borrow_mut();
        let mouse_state = buttons.entry(container.id.clone()).or_default().clone();
        let overflow_button_state = self
            .container_overflow_buttons
            .borrow_mut()
            .entry(container.id.clone())
            .or_default()
            .clone();
        let open_action = DockerContainersAction::OpenLogs(container.id.clone());
        let menu_action = DockerContainersAction::ToggleContainerMenu(container.id.clone());
        let overflow_menu = self.container_overflow_menu.clone();
        let is_menu_open = self
            .overflow_menu_state
            .as_ref()
            .is_some_and(|state| state.container_id == container.id);
        let is_stopping = self.stopping_containers.contains(&container.id);
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let age = container
            .created_at
            .map(format_approx_duration_from_now_utc)
            .unwrap_or_else(|| "unknown age".to_string());
        let trailing = if is_stopping {
            "Stopping...".to_string()
        } else {
            age
        };

        let name = Text::new_inline(container.name.clone(), font_family, font_size + 1.)
            .with_style(Properties::default().weight(Weight::Bold))
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();
        let trailing = Text::new_inline(trailing, font_family, font_size - 2.)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish();
        let image = Text::new_inline(container.image_name.clone(), font_family, font_size - 1.)
            .with_color(theme.sub_text_color(theme.background()).into())
            .finish();
        let tag = Container::new(
            Text::new_inline(container.image_tag.clone(), font_family, font_size - 2.)
                .with_color(theme.main_text_color(theme.background()).into())
                .finish(),
        )
        .with_background_color(theme.surface_2().into())
        .with_border(Border::all(1.).with_border_fill(theme.outline()))
        .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)))
        .with_padding(Padding::uniform(0.).with_left(6.).with_right(6.))
        .finish();

        let row = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(4.)
            .with_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., Clipped::new(name).finish()).finish())
                    .with_child(trailing)
                    .finish(),
            )
            .with_child(
                Flex::row()
                    .with_main_axis_size(MainAxisSize::Max)
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_spacing(6.)
                    .with_child(Shrinkable::new(1., Clipped::new(image).finish()).finish())
                    .with_child(tag)
                    .finish(),
            )
            .finish();

        Hoverable::new(mouse_state, move |state| {
            let mut container = Container::new(row)
                .with_horizontal_padding(12.)
                .with_padding_top(8.)
                .with_padding_bottom(8.);
            if state.is_hovered() || is_menu_open {
                container = container.with_background(theme.surface_overlay_1());
            }

            let mut stack = Stack::new().with_child(container.finish());
            if state.is_hovered() || is_menu_open {
                let button_style = UiComponentStyles::default()
                    .set_background(theme.surface_2().into())
                    .set_border_color(theme.surface_3().into());
                let overflow_button = icon_button_with_context_menu(
                    Icon::DotsVertical,
                    {
                        let menu_action = menu_action.clone();
                        move |ctx, _, _| ctx.dispatch_typed_action(menu_action.clone())
                    },
                    overflow_button_state.clone(),
                    &overflow_menu,
                    is_menu_open,
                    MenuDirection::Left,
                    Some(Cursor::PointingHand),
                    Some(button_style),
                    appearance,
                );
                stack.add_positioned_child(
                    overflow_button.finish(),
                    OffsetPositioning::offset_from_parent(
                        vec2f(-8., 6.),
                        ParentOffsetBounds::ParentByPosition,
                        ParentAnchor::TopRight,
                        ChildAnchor::TopRight,
                    ),
                );
            }
            stack.finish()
        })
        .with_defer_events_to_children()
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(open_action.clone()))
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn visible_containers<'a>(&'a self, app: &AppContext) -> Vec<&'a DockerContainerSummary> {
        let query = self
            .query_editor
            .as_ref(app)
            .buffer_text(app)
            .trim()
            .to_ascii_lowercase();
        let mut containers = self
            .containers
            .iter()
            .filter(|container| {
                query.is_empty()
                    || container.name.to_ascii_lowercase().contains(&query)
                    || container.image_name.to_ascii_lowercase().contains(&query)
                    || container.image_tag.to_ascii_lowercase().contains(&query)
                    || container.id.to_ascii_lowercase().contains(&query)
            })
            .collect::<Vec<_>>();
        containers.sort_by(|a, b| compare_containers(a, b, self.sort_column));
        if self.sort_direction == SortDirection::Desc {
            containers.reverse();
        }
        containers
    }

    fn render_containers(&self, app: &AppContext, appearance: &Appearance) -> Box<dyn Element> {
        let theme = appearance.theme();
        let mut column = Flex::column().with_spacing(4.);
        let containers = self.visible_containers(app);
        if containers.is_empty() {
            let message = if self.containers.is_empty() {
                "No running Docker containers found."
            } else {
                "No Docker containers match the search."
            };
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text(message.to_string(), true)
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(theme.sub_text_color(theme.background()).into_solid()),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            );
        } else {
            for container in containers {
                column.add_child(self.render_container(container, appearance));
            }
        }

        ClippedScrollable::vertical(
            self.list_scroll_state.clone(),
            Container::new(column.finish()).finish(),
            ScrollbarWidth::Auto,
            theme.nonactive_ui_detail().into(),
            theme.active_ui_detail().into(),
            Fill::None,
        )
        .with_overlayed_scrollbar()
        .finish()
    }
}

impl Entity for DockerContainersView {
    type Event = DockerContainersEvent;
}

impl TypedActionView for DockerContainersView {
    type Action = DockerContainersAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            DockerContainersAction::Refresh => self.refresh(ctx),
            DockerContainersAction::SortBy(column) => {
                if self.sort_column == *column {
                    self.sort_direction = match self.sort_direction {
                        SortDirection::Asc => SortDirection::Desc,
                        SortDirection::Desc => SortDirection::Asc,
                    };
                } else {
                    self.sort_column = *column;
                    self.sort_direction = SortDirection::Asc;
                }
                ctx.notify();
            }
            DockerContainersAction::OpenLogs(container_id) => {
                if let Some(container) = self.containers.iter().find(|c| c.id == *container_id) {
                    let command_context = self
                        .last_successful_command_context
                        .clone()
                        .or_else(|| self.active_session_context(ctx).ok());
                    let command = command_context
                        .as_ref()
                        .map(|context| docker_log_tail_command(&container.id, &context.log_target))
                        .unwrap_or_else(|| docker_logs_command(&container.id));
                    ctx.emit(DockerContainersEvent::OpenLogs {
                        container_id: container.id.clone(),
                        container_name: container.name.clone(),
                        command,
                    });
                }
            }
            DockerContainersAction::ToggleContainerMenu(container_id) => {
                let is_open_for_same_container = self
                    .overflow_menu_state
                    .as_ref()
                    .is_some_and(|state| state.container_id == *container_id);
                if is_open_for_same_container {
                    self.overflow_menu_state = None;
                } else {
                    self.overflow_menu_state = Some(DockerOverflowMenuState {
                        container_id: container_id.clone(),
                    });
                    let stop_item = MenuItemFields::new("Stop")
                        .with_override_text_color(Appearance::as_ref(ctx).theme().ansi_fg_red())
                        .with_on_select_action(DockerContainersAction::StopContainer(
                            container_id.clone(),
                        ))
                        .into_item();
                    self.container_overflow_menu.update(ctx, |menu, ctx| {
                        menu.set_items(vec![stop_item], ctx);
                    });
                }
                ctx.notify();
            }
            DockerContainersAction::StopContainer(container_id) => {
                let command_context =
                    if let Some(command_context) = self.last_successful_command_context.clone() {
                        command_context
                    } else {
                        match self.active_session_context(ctx) {
                            Ok(command_context) => command_context,
                            Err(error) => {
                                self.last_error = Some(error);
                                ctx.notify();
                                return;
                            }
                        }
                    };
                let container_id = container_id.clone();
                self.overflow_menu_state = None;
                self.stopping_containers.insert(container_id.clone());
                ctx.notify();

                ctx.spawn(
                    async move {
                        stop_docker_container(command_context, container_id.clone())
                            .await
                            .map(|_| container_id)
                    },
                    |view, result, ctx| {
                        match result {
                            Ok(container_id) => {
                                view.stopping_containers.remove(&container_id);
                                view.last_error = None;
                                view.refresh(ctx);
                            }
                            Err(error) => {
                                view.stopping_containers.clear();
                                view.last_error = Some(error);
                            }
                        }
                        ctx.notify();
                    },
                );
            }
        }
    }
}

impl View for DockerContainersView {
    fn ui_name() -> &'static str {
        "DockerContainersView"
    }

    fn on_focus(&mut self, focus_ctx: &FocusContext, ctx: &mut ViewContext<Self>) {
        if focus_ctx.is_self_focused() {
            ctx.focus(&self.query_editor);
        }
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut column = Flex::column()
            .with_main_axis_size(MainAxisSize::Max)
            .with_spacing(10.)
            .with_child(
                Flex::row()
                    .with_cross_axis_alignment(CrossAxisAlignment::Center)
                    .with_child(Shrinkable::new(1., self.render_status(appearance)).finish())
                    .with_child(self.render_refresh_button(appearance))
                    .finish(),
            )
            .with_child(self.render_search(appearance))
            .with_child(self.render_sort_controls(appearance));

        if let Some(error) = self.render_error(appearance) {
            column.add_child(error);
        }
        column.add_child(Shrinkable::new(1., self.render_containers(app, appearance)).finish());

        Container::new(column.finish())
            .with_uniform_padding(10.)
            .finish()
    }
}

async fn list_docker_containers(
    command_context: DockerCommandContext,
) -> Result<Vec<DockerContainerSummary>, String> {
    let output = execute_docker_command(command_context, DOCKER_LIST_COMMAND.to_string()).await?;
    parse_docker_containers(&output)
}

async fn stop_docker_container(
    command_context: DockerCommandContext,
    container_id: String,
) -> Result<(), String> {
    let command = docker_stop_command(&container_id);
    execute_docker_command(command_context, command)
        .await
        .map(|_| ())
}

fn docker_stop_command(container_id: &str) -> String {
    format!("docker stop {}", shell_words::quote(container_id))
}

fn docker_logs_command(container_id: &str) -> String {
    format!("docker logs --tail 200 -f {}", shell_words::quote(container_id))
}

fn docker_log_tail_command(container_id: &str, log_target: &DockerLogTarget) -> String {
    let docker_command = docker_logs_command(container_id);
    match log_target {
        DockerLogTarget::Local => docker_command,
        DockerLogTarget::RemoteSsh {
            spawning_command: Some(spawning_command),
            control_path: None,
            ..
        } if reusable_non_interactive_ssh_command(spawning_command).is_some() => {
            let ssh_command = reusable_non_interactive_ssh_command(spawning_command)
                .expect("guard ensures a reusable SSH command");
            format!("{} {}", ssh_command, shell_words::quote(&docker_command))
        }
        DockerLogTarget::RemoteSsh {
            control_path: Some(control_path),
            host,
            ..
        } => format!(
            "ssh -T -S {} {} {}",
            shell_words::quote(control_path.to_string_lossy().as_ref()),
            shell_words::quote(host),
            shell_words::quote(&docker_command)
        ),
        DockerLogTarget::RemoteSsh {
            host,
            port: Some(port),
            ..
        } => format!(
            "ssh -T -p {} {} {}",
            shell_words::quote(port),
            shell_words::quote(host),
            shell_words::quote(&docker_command)
        ),
        DockerLogTarget::RemoteSsh { host, .. } => format!(
            "ssh -T {} {}",
            shell_words::quote(host),
            shell_words::quote(&docker_command)
        ),
    }
}

fn docker_log_target_for_session(session: &Session, app: &AppContext) -> DockerLogTarget {
    if !matches!(session.session_type(), SessionType::WarpifiedRemote { .. }) {
        return DockerLogTarget::Local;
    }

    let control_path = RemoteServerManager::as_ref(app).control_path_for_session(session.id());

    let ssh_info = session
        .subshell_info()
        .as_ref()
        .and_then(|info| info.ssh_connection_info.as_ref());
    let spawning_command = session
        .subshell_info()
        .as_ref()
        .map(|info| info.spawning_command.trim().to_string())
        .filter(|command| !command.is_empty());

    let host = ssh_info
        .and_then(|info| info.host.clone())
        .or_else(|| fallback_ssh_host(session))
        .unwrap_or_else(|| session.hostname().to_string());
    let port = ssh_info.and_then(|info| info.port.clone());

    DockerLogTarget::RemoteSsh {
        spawning_command,
        control_path,
        host,
        port,
    }
}

fn fallback_ssh_host(session: &Session) -> Option<String> {
    let hostname = session.hostname();
    if hostname.is_empty() {
        return None;
    }

    let user = session.user();
    if user.is_empty() {
        Some(hostname.to_string())
    } else {
        Some(format!("{user}@{hostname}"))
    }
}

fn reusable_non_interactive_ssh_command(command: &str) -> Option<String> {
    let command = command.trim_start();
    command
        .strip_prefix("ssh ")
        .map(|rest| format!("ssh -T {}", rest.trim_start()))
        .or_else(|| {
            command
                .strip_prefix("command ssh ")
                .map(|rest| format!("command ssh -T {}", rest.trim_start()))
        })
}

async fn execute_docker_command(
    command_context: DockerCommandContext,
    command: String,
) -> Result<String, String> {
    match command_context
        .session
        .execute_command(
            &command,
            command_context.current_dir.as_deref(),
            command_context.environment_variables,
            ExecuteCommandOptions::default(),
        )
        .with_timeout(DOCKER_COMMAND_TIMEOUT)
        .await
    {
        Ok(Ok(output)) if output.success() => {
            Ok(String::from_utf8_lossy(output.output()).to_string())
        }
        Ok(Ok(output)) => {
            let output = String::from_utf8_lossy(output.output()).trim().to_string();
            if output.is_empty() {
                Err("Docker command failed without output.".to_string())
            } else {
                Err(output)
            }
        }
        Ok(Err(error)) => Err(format!("Docker command failed: {error}")),
        Err(_) => Err(format!(
            "Docker command timed out after {} seconds.",
            DOCKER_COMMAND_TIMEOUT.as_secs()
        )),
    }
}

fn parse_docker_containers(output: &str) -> Result<Vec<DockerContainerSummary>, String> {
    output
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(parse_docker_container)
        .collect()
}

fn parse_docker_container(line: &str) -> Result<DockerContainerSummary, String> {
    if let Ok(container) = serde_json::from_str::<DockerPsContainer>(line) {
        return Ok(summary_from_docker_ps(container));
    }

    let container: DockerInspectContainer = serde_json::from_str(line)
        .map_err(|error| format!("Could not parse Docker output: {error}"))?;
    Ok(summary_from_docker_inspect(container))
}

fn summary_from_docker_ps(container: DockerPsContainer) -> DockerContainerSummary {
    let image = container.image.unwrap_or_else(|| "<unknown>".to_string());
    let (image_name, image_tag) = split_image_name_and_tag(&image);
    let created_at = container
        .created_at
        .as_deref()
        .and_then(parse_docker_created_at);
    let fallback_name = short_container_id(&container.id).to_string();
    let name = container
        .names
        .as_deref()
        .and_then(|names| names.split(',').next())
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(&fallback_name)
        .to_string();

    DockerContainerSummary {
        id: container.id,
        name,
        image_name,
        image_tag,
        created_at,
    }
}

fn summary_from_docker_inspect(container: DockerInspectContainer) -> DockerContainerSummary {
    let image = container
        .config
        .and_then(|config| config.image)
        .unwrap_or_else(|| "<unknown>".to_string());
    let (image_name, image_tag) = split_image_name_and_tag(&image);
    let created_at = container
        .created
        .as_deref()
        .and_then(parse_docker_created_at);
    let fallback_name = short_container_id(&container.id).to_string();
    let name = container
        .name
        .as_deref()
        .map(|name| name.trim_start_matches('/'))
        .filter(|name| !name.is_empty())
        .unwrap_or(&fallback_name)
        .to_string();

    DockerContainerSummary {
        id: container.id,
        name,
        image_name,
        image_tag,
        created_at,
    }
}

fn parse_docker_created_at(created: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(created)
        .ok()
        .or_else(|| DateTime::parse_from_str(created, "%Y-%m-%d %H:%M:%S %z %Z").ok())
        .or_else(|| {
            created
                .strip_suffix(" UTC")
                .and_then(|created| DateTime::parse_from_str(created, "%Y-%m-%d %H:%M:%S %z").ok())
        })
        .map(|created| created.with_timezone(&Utc))
}

fn split_image_name_and_tag(image: &str) -> (String, String) {
    if let Some((name, digest)) = image.split_once('@') {
        return (name.to_string(), digest.to_string());
    }

    let last_slash = image.rfind('/');
    let last_colon = image.rfind(':');
    if let Some(colon_index) = last_colon {
        if last_slash.map_or(true, |slash_index| colon_index > slash_index) {
            return (
                image[..colon_index].to_string(),
                image[colon_index + 1..].to_string(),
            );
        }
    }

    (image.to_string(), "latest".to_string())
}

fn short_container_id(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

fn compare_containers(
    left: &DockerContainerSummary,
    right: &DockerContainerSummary,
    column: DockerContainerSortColumn,
) -> Ordering {
    match column {
        DockerContainerSortColumn::Name => left
            .name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase()),
        DockerContainerSortColumn::Age => match (left.created_at, right.created_at) {
            (Some(left), Some(right)) => left.cmp(&right),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        },
        DockerContainerSortColumn::Image => {
            let left_key = format!("{}:{}", left.image_name, left.image_tag).to_ascii_lowercase();
            let right_key =
                format!("{}:{}", right.image_name, right.image_tag).to_ascii_lowercase();
            left_key.cmp(&right_key)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_image_name_and_tag_handles_registry_ports() {
        assert_eq!(
            split_image_name_and_tag("localhost:5000/team/api:1.2.3"),
            ("localhost:5000/team/api".to_string(), "1.2.3".to_string())
        );
    }

    #[test]
    fn split_image_name_and_tag_defaults_missing_tag_to_latest() {
        assert_eq!(
            split_image_name_and_tag("postgres"),
            ("postgres".to_string(), "latest".to_string())
        );
    }

    #[test]
    fn parse_docker_containers_reads_inspect_json_lines() {
        let output = r#"{"Id":"abc1234567890","Name":"/web","Created":"2026-06-05T12:34:56.123456789Z","Config":{"Image":"registry.local:5000/team/web:2.0"}}"#;

        let containers = parse_docker_containers(output).unwrap();

        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id, "abc1234567890");
        assert_eq!(containers[0].name, "web");
        assert_eq!(containers[0].image_name, "registry.local:5000/team/web");
        assert_eq!(containers[0].image_tag, "2.0");
        assert!(containers[0].created_at.is_some());
    }

    #[test]
    fn list_command_uses_single_docker_ps_call() {
        assert!(DOCKER_LIST_COMMAND.contains("docker ps --format"));
        assert!(!DOCKER_LIST_COMMAND.contains("docker inspect"));
    }

    #[test]
    fn parse_docker_containers_reads_ps_json_lines() {
        let output = r#"{"ID":"69ac6863e73d","Image":"registry.local:5000/team/web:2.0","Names":"web","CreatedAt":"2026-06-05 12:34:56 +0000 UTC"}"#;

        let containers = parse_docker_containers(output).unwrap();

        assert_eq!(containers.len(), 1);
        assert_eq!(containers[0].id, "69ac6863e73d");
        assert_eq!(containers[0].name, "web");
        assert_eq!(containers[0].image_name, "registry.local:5000/team/web");
        assert_eq!(containers[0].image_tag, "2.0");
        assert!(containers[0].created_at.is_some());
    }

    #[test]
    fn docker_log_tail_command_uses_remote_ssh_target() {
        let command = docker_log_tail_command(
            "69ac6863e73d",
            &DockerLogTarget::RemoteSsh {
                spawning_command: Some("ssh root@adm19.nt.vc".to_string()),
                control_path: None,
                host: "root@adm19.nt.vc".to_string(),
                port: None,
            },
        );

        assert_eq!(
            command,
            "ssh -T root@adm19.nt.vc 'docker logs --tail 200 -f 69ac6863e73d'"
        );
    }

    #[test]
    fn docker_log_tail_command_prefers_control_path() {
        let command = docker_log_tail_command(
            "69ac6863e73d",
            &DockerLogTarget::RemoteSsh {
                spawning_command: Some("ssh root@adm19.nt.vc".to_string()),
                control_path: Some(PathBuf::from("/tmp/warp ssh/control.sock")),
                host: "root@adm19.nt.vc".to_string(),
                port: None,
            },
        );

        assert_eq!(
            command,
            "ssh -T -S '/tmp/warp ssh/control.sock' root@adm19.nt.vc 'docker logs --tail 200 -f 69ac6863e73d'"
        );
    }

    #[test]
    fn docker_log_tail_command_preserves_ssh_port_when_no_reusable_command_exists() {
        let command = docker_log_tail_command(
            "69ac6863e73d",
            &DockerLogTarget::RemoteSsh {
                spawning_command: None,
                control_path: None,
                host: "root@adm19.nt.vc".to_string(),
                port: Some("2222".to_string()),
            },
        );

        assert_eq!(
            command,
            "ssh -T -p 2222 root@adm19.nt.vc 'docker logs --tail 200 -f 69ac6863e73d'"
        );
    }
}
