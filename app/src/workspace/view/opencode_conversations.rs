#![cfg(not(target_family = "wasm"))]

use std::{cell::RefCell, collections::HashMap, path::PathBuf};

use pathfinder_geometry::vector::vec2f;
use settings::Setting as _;
use warpui::{
    elements::{
        Border, ChildAnchor, ChildView, Clipped, ConstrainedBox, Container, CornerRadius,
        CrossAxisAlignment, Element, Flex, Hoverable, MainAxisAlignment, MainAxisSize,
        MouseStateHandle, OffsetPositioning, Padding, ParentAnchor, ParentElement,
        ParentOffsetBounds, Radius, Shrinkable, Stack, Text,
    },
    fonts::{Properties, Weight},
    platform::Cursor,
    ui_components::{
        button::ButtonVariant,
        components::{Coords, UiComponent, UiComponentStyles},
    },
    AppContext, Entity, ModelHandle, SingletonEntity, TypedActionView, View, ViewContext,
    ViewHandle,
};

use crate::{
    ai::active_agent_views_model::ActiveAgentViewsModel,
    appearance::Appearance,
    editor::{
        EditorView, Event as EditorEvent, PropagateAndNoOpNavigationKeys, SingleLineEditorOptions,
        TextOptions,
    },
    menu::{Event as MenuEvent, Menu, MenuItemFields},
    opencode_server::{
        opencode_session_updated_at_utc, opencode_start_command, OpenCodeServerModel,
        OpenCodeServerModelEvent, OpenCodeServerStatus, OpenCodeSessionSummary,
    },
    settings::OpenCodeServerSettings,
    ui_components::icons::Icon,
    ui_components::menu_button::{icon_button_with_context_menu, MenuDirection},
    util::time_format::format_approx_duration_from_now_utc,
    workspace::{RestoreConversationLayout, WorkspaceAction},
};

#[derive(Clone, Debug)]
pub enum OpenCodeConversationsAction {
    Refresh,
    NewConversation,
    OpenSession(String),
    ToggleSessionMenu(String),
    DeleteSession(String),
}

#[derive(Clone, Debug)]
struct OpenCodeOverflowMenuState {
    session_id: String,
}

pub struct OpenCodeConversationsView {
    model: ModelHandle<OpenCodeServerModel>,
    query_editor: ViewHandle<EditorView>,
    session_overflow_menu: ViewHandle<Menu<OpenCodeConversationsAction>>,
    overflow_menu_state: Option<OpenCodeOverflowMenuState>,
    refresh_button: MouseStateHandle,
    new_conversation_button: MouseStateHandle,
    session_buttons: RefCell<HashMap<String, MouseStateHandle>>,
    session_overflow_buttons: RefCell<HashMap<String, MouseStateHandle>>,
}

impl OpenCodeConversationsView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let model = OpenCodeServerModel::handle(ctx);
        ctx.subscribe_to_model(&model, |_, _, event, ctx| match event {
            OpenCodeServerModelEvent::OpenConversation {
                conversation_id, ..
            } => {
                if let Some(terminal_view_id) = ActiveAgentViewsModel::as_ref(ctx)
                    .get_terminal_view_id_for_conversation(*conversation_id, ctx)
                {
                    ctx.dispatch_typed_action(&WorkspaceAction::FocusTerminalViewInWorkspace {
                        terminal_view_id,
                    });
                } else {
                    ctx.dispatch_typed_action(&WorkspaceAction::RestoreOrNavigateToConversation {
                        conversation_id: *conversation_id,
                        window_id: None,
                        pane_view_locator: None,
                        terminal_view_id: None,
                        restore_layout: Some(RestoreConversationLayout::SplitPane),
                    });
                }
                ctx.notify();
            }
            OpenCodeServerModelEvent::StatusChanged
            | OpenCodeServerModelEvent::SessionsChanged
            | OpenCodeServerModelEvent::ActiveSessionChanged
            | OpenCodeServerModelEvent::PendingRequestsChanged
            | OpenCodeServerModelEvent::ModelsChanged => {
                ctx.notify();
            }
        });

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
            editor.set_placeholder_text("Search OpenCode conversations", ctx);
            editor
        });
        ctx.subscribe_to_view(&query_editor, |_, _, event, ctx| {
            if matches!(event, EditorEvent::Edited(_)) {
                ctx.notify();
            }
        });

        let session_overflow_menu = ctx.add_typed_action_view(|_| {
            Menu::new()
                .prevent_interaction_with_other_elements()
                .with_width(160.)
        });
        ctx.subscribe_to_view(&session_overflow_menu, |me, _, event, ctx| match event {
            MenuEvent::Close { .. } => {
                me.overflow_menu_state = None;
                ctx.notify();
            }
            MenuEvent::ItemSelected | MenuEvent::ItemHovered => {}
        });

        Self {
            model,
            query_editor,
            session_overflow_menu,
            overflow_menu_state: None,
            refresh_button: Default::default(),
            new_conversation_button: Default::default(),
            session_buttons: Default::default(),
            session_overflow_buttons: Default::default(),
        }
    }

    pub fn on_left_panel_focused(&mut self, ctx: &mut ViewContext<Self>) {
        ctx.focus(&self.query_editor);
    }

    fn render_status(&self, app: &AppContext, appearance: &Appearance) -> Box<dyn Element> {
        let settings = OpenCodeServerSettings::as_ref(app);
        let status = self.model.as_ref(app).status().clone();
        let status_text = match &status {
            OpenCodeServerStatus::Disconnected { message } => format!("Disconnected: {message}"),
            _ => status.label(),
        };
        let mut column = Flex::column().with_spacing(8.).with_child(
            Text::new_inline(
                status_text,
                appearance.ui_font_family(),
                appearance.ui_font_size() - 1.,
            )
            .with_color(
                appearance
                    .theme()
                    .sub_text_color(appearance.theme().background())
                    .into(),
            )
            .finish(),
        );

        if matches!(status, OpenCodeServerStatus::Disconnected { .. }) {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text(
                        format!(
                            "Start server: {}",
                            opencode_start_command(settings.server_url.value())
                        ),
                        true,
                    )
                    .with_style(UiComponentStyles {
                        font_size: Some(12.),
                        font_color: Some(
                            appearance
                                .theme()
                                .sub_text_color(appearance.theme().background())
                                .into_solid(),
                        ),
                        ..Default::default()
                    })
                    .build()
                    .finish(),
            );
        }

        column.finish()
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
            .on_click(|ctx, _, _| ctx.dispatch_typed_action(OpenCodeConversationsAction::Refresh))
            .with_cursor(Cursor::PointingHand)
            .finish()
    }

    fn render_new_conversation_button(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let theme = appearance.theme();
        let mouse_state = self.new_conversation_button.clone();

        Hoverable::new(mouse_state, move |state| {
            let label = Text::new_inline(
                "New conversation",
                appearance.ui_font_family(),
                appearance.ui_font_size() + 1.,
            )
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();

            let icon = ConstrainedBox::new(
                Icon::Plus
                    .to_warpui_icon(theme.main_text_color(theme.background()))
                    .finish(),
            )
            .with_width(appearance.ui_font_size())
            .with_height(appearance.ui_font_size())
            .finish();

            let row = Flex::row()
                .with_cross_axis_alignment(CrossAxisAlignment::Center)
                .with_spacing(8.)
                .with_child(icon)
                .with_child(label)
                .finish();

            let mut container = Container::new(row)
                .with_padding(Padding::uniform(0.).with_left(12.).with_right(12.))
                .with_border(Border::all(1.).with_border_fill(theme.outline()))
                .with_corner_radius(CornerRadius::with_all(Radius::Pixels(4.)));
            if state.is_hovered() {
                container = container.with_background(theme.surface_2());
            }

            ConstrainedBox::new(container.finish())
                .with_min_height(34.)
                .finish()
        })
        .on_click(|ctx, _, _| {
            ctx.dispatch_typed_action(OpenCodeConversationsAction::NewConversation);
        })
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn render_session(
        &self,
        session: &OpenCodeSessionSummary,
        is_active: bool,
        appearance: &Appearance,
    ) -> Box<dyn Element> {
        let mut buttons = self.session_buttons.borrow_mut();
        let mouse_state = buttons.entry(session.id.clone()).or_default().clone();
        let overflow_button_state = self
            .session_overflow_buttons
            .borrow_mut()
            .entry(session.id.clone())
            .or_default()
            .clone();
        let action = OpenCodeConversationsAction::OpenSession(session.id.clone());
        let menu_action = OpenCodeConversationsAction::ToggleSessionMenu(session.id.clone());
        let overflow_menu = self.session_overflow_menu.clone();
        let is_menu_open = self
            .overflow_menu_state
            .as_ref()
            .is_some_and(|state| state.session_id == session.id);
        let theme = appearance.theme();
        let font_family = appearance.ui_font_family();
        let font_size = appearance.ui_font_size();
        let title = if session.title.trim().is_empty() {
            session.id.as_str()
        } else {
            session.title.as_str()
        };
        let metadata = session
            .directory
            .as_ref()
            .and_then(|directory| directory.to_str())
            .map(shorten_project_path);
        let age = opencode_session_updated_at_utc(session).map(format_approx_duration_from_now_utc);

        let title = Text::new_inline(title.to_string(), font_family, font_size + 2.)
            .with_style(Properties::default().weight(Weight::Bold))
            .with_color(theme.main_text_color(theme.background()).into())
            .finish();
        let mut bottom_row = Flex::row()
            .with_main_axis_size(MainAxisSize::Max)
            .with_main_axis_alignment(MainAxisAlignment::SpaceBetween)
            .with_cross_axis_alignment(CrossAxisAlignment::End);
        if let Some(metadata) = metadata {
            bottom_row.add_child(
                Shrinkable::new(
                    1.,
                    Text::new_inline(metadata, font_family, font_size - 1.)
                        .with_color(theme.sub_text_color(theme.background()).into())
                        .finish(),
                )
                .finish(),
            );
        }
        let trailing = session.status.clone().or(age);
        if let Some(trailing) = trailing {
            bottom_row.add_child(
                Text::new_inline(trailing, font_family, font_size - 2.)
                    .with_color(theme.sub_text_color(theme.background()).into())
                    .finish(),
            );
        }

        let row = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_child(title)
            .with_child(bottom_row.finish())
            .finish();

        Hoverable::new(mouse_state, move |state| {
            let container = Container::new(row)
                .with_horizontal_padding(12.)
                .with_padding_top(8.)
                .with_padding_bottom(8.);
            let container = if is_active {
                container.with_background(theme.surface_overlay_1())
            } else {
                container
            };
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
        .on_click(move |ctx, _, _| ctx.dispatch_typed_action(action.clone()))
        .with_cursor(Cursor::PointingHand)
        .finish()
    }

    fn visible_sessions<'a>(&self, app: &'a AppContext) -> Vec<&'a OpenCodeSessionSummary> {
        let query = self
            .query_editor
            .as_ref(app)
            .buffer_text(app)
            .trim()
            .to_ascii_lowercase();
        self.model
            .as_ref(app)
            .sessions()
            .iter()
            .filter(|session| {
                query.is_empty()
                    || session.title.to_ascii_lowercase().contains(&query)
                    || session.id.to_ascii_lowercase().contains(&query)
                    || session
                        .directory
                        .as_ref()
                        .and_then(|directory| directory.to_str())
                        .is_some_and(|directory| directory.to_ascii_lowercase().contains(&query))
            })
            .collect()
    }

    fn render_sessions(&self, app: &AppContext, appearance: &Appearance) -> Box<dyn Element> {
        let active_session_id = self
            .model
            .as_ref(app)
            .opening_session_id()
            .map(ToOwned::to_owned)
            .or_else(|| {
                self.model
                    .as_ref(app)
                    .active_session()
                    .map(|session| session.summary.id.clone())
            });
        let mut column = Flex::column().with_spacing(4.);
        let sessions = self.visible_sessions(app);
        if sessions.is_empty() {
            column.add_child(
                appearance
                    .ui_builder()
                    .wrappable_text("No OpenCode conversations found for this project.", true)
                    .build()
                    .finish(),
            );
        } else {
            for session in sessions {
                column.add_child(self.render_session(
                    session,
                    active_session_id.as_deref() == Some(session.id.as_str()),
                    appearance,
                ));
            }
        }
        column.finish()
    }
}

fn shorten_project_path(path: &str) -> String {
    PathBuf::from(path)
        .file_name()
        .and_then(|name| name.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| path.to_string())
}

impl Entity for OpenCodeConversationsView {
    type Event = ();
}

impl TypedActionView for OpenCodeConversationsView {
    type Action = OpenCodeConversationsAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            OpenCodeConversationsAction::Refresh => {
                self.model.update(ctx, |model, ctx| model.refresh(ctx));
            }
            OpenCodeConversationsAction::NewConversation => {
                self.model
                    .update(ctx, |model, ctx| model.start_new_conversation(ctx));
            }
            OpenCodeConversationsAction::OpenSession(session_id) => {
                self.model.update(ctx, |model, ctx| {
                    model.open_session_as_conversation(session_id.clone(), ctx);
                });
            }
            OpenCodeConversationsAction::ToggleSessionMenu(session_id) => {
                let is_open_for_same_session = self
                    .overflow_menu_state
                    .as_ref()
                    .is_some_and(|state| state.session_id == *session_id);
                if is_open_for_same_session {
                    self.overflow_menu_state = None;
                } else {
                    self.overflow_menu_state = Some(OpenCodeOverflowMenuState {
                        session_id: session_id.clone(),
                    });
                    let delete_item = MenuItemFields::new("Delete")
                        .with_override_text_color(Appearance::as_ref(ctx).theme().ansi_fg_red())
                        .with_on_select_action(OpenCodeConversationsAction::DeleteSession(
                            session_id.clone(),
                        ))
                        .into_item();
                    self.session_overflow_menu.update(ctx, |menu, ctx| {
                        menu.set_items(vec![delete_item], ctx);
                    });
                }
                ctx.notify();
            }
            OpenCodeConversationsAction::DeleteSession(session_id) => {
                self.overflow_menu_state = None;
                self.model.update(ctx, |model, ctx| {
                    model.delete_session(session_id.clone(), ctx);
                });
                ctx.notify();
            }
        }
    }
}

impl View for OpenCodeConversationsView {
    fn ui_name() -> &'static str {
        "OpenCodeConversationsView"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let content = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(12.)
            .with_child(self.render_status(app, appearance))
            .with_child(
                Flex::row()
                    .with_spacing(8.)
                    .with_child(Shrinkable::new(1., self.render_search(appearance)).finish())
                    .with_child(self.render_refresh_button(appearance))
                    .finish(),
            )
            .with_child(self.render_new_conversation_button(app))
            .with_child(self.render_sessions(app, appearance))
            .finish();

        Container::new(content).with_uniform_padding(12.).finish()
    }
}
