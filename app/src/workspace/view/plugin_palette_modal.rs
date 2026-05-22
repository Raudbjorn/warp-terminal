//! oh-my-warp: picker modal body for `warp.ui.showPalette`. Renders one clickable row per item;
//! selecting a row emits [`PluginPaletteEvent::Selected`] with the chosen item's command id so the
//! `Workspace` can close the modal and run that command. See PLUGIN_SPEC.md (M4).

use warpui::elements::{Container, Element, Flex, MouseStateHandle, ParentElement};
use warpui::platform::Cursor;
use warpui::ui_components::button::ButtonVariant;
use warpui::ui_components::components::{UiComponent, UiComponentStyles};
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext};

use crate::appearance::Appearance;
use crate::plugin::app_requests::PalettePluginItem;

/// Emitted when the user picks an item, carrying that item's command id. The `Workspace` runs the
/// command as a fresh `RunPluginCommand` (not from within the calling callback's borrow).
#[derive(Debug, PartialEq, Eq)]
pub enum PluginPaletteEvent {
    Selected(String),
}

/// Dispatched by a row's click handler with the index of the chosen item.
#[derive(Debug)]
pub enum PluginPaletteAction {
    Select(usize),
}

pub struct PluginPaletteModal {
    items: Vec<PalettePluginItem>,
    button_states: Vec<MouseStateHandle>,
}

impl PluginPaletteModal {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            items: Vec::new(),
            button_states: Vec::new(),
        }
    }

    /// Replaces the picker's items (one clickable row each). Call before opening the modal.
    pub fn set_items(&mut self, items: Vec<PalettePluginItem>, ctx: &mut ViewContext<Self>) {
        self.button_states = items.iter().map(|_| MouseStateHandle::default()).collect();
        self.items = items;
        ctx.notify();
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
                if let Some(item) = self.items.get(*index) {
                    ctx.emit(PluginPaletteEvent::Selected(item.command_id.clone()));
                }
            }
        }
    }
}

impl View for PluginPaletteModal {
    fn ui_name() -> &'static str {
        "PluginPaletteModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let mut column = Flex::column();
        for (index, (item, state)) in self.items.iter().zip(self.button_states.iter()).enumerate() {
            let button = appearance
                .ui_builder()
                .button(ButtonVariant::Secondary, state.clone())
                .with_style(UiComponentStyles {
                    font_size: Some(14.),
                    height: Some(36.),
                    ..Default::default()
                })
                .with_centered_text_label(item.label.clone())
                .build()
                .with_cursor(Cursor::PointingHand)
                .on_click(move |ctx, _, _| {
                    ctx.dispatch_typed_action(PluginPaletteAction::Select(index))
                })
                .finish();
            column.add_child(Container::new(button).with_margin_bottom(6.).finish());
        }
        column.finish()
    }
}
