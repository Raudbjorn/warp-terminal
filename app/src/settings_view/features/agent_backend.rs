//! oh-my-warp: Settings dropdown for selecting the active server/agent backend.
//!
//! Lists "Warp (Default)" plus any alternative backends defined in
//! `agent_backends.toml` (see [`crate::util::agent_backends`]). Selecting one
//! persists the choice; it takes effect on the next launch.

use warpui::{
    presenter::ChildView, Element, Entity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::{
    util::agent_backends::{self, DEFAULT_BACKEND_ID, DEFAULT_BACKEND_LABEL},
    view_components::{Dropdown, DropdownItem},
};

/// A view wrapping a dropdown that selects the active backend.
pub struct BackendSelectorView {
    dropdown: ViewHandle<Dropdown<BackendAction>>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum BackendAction {
    /// Select the backend with this id ([`DEFAULT_BACKEND_ID`] = built-in Warp).
    Select(String),
}

impl BackendSelectorView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(260.);
            dropdown
        });
        Self::update_dropdown_state(dropdown.clone(), ctx);
        Self { dropdown }
    }

    /// Rebuilds the dropdown items ("Warp (Default)" + configured backends) and
    /// highlights the currently-selected one.
    fn update_dropdown_state(
        dropdown: ViewHandle<Dropdown<BackendAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        let config = agent_backends::load();
        let selected_id = config.selected_id().to_string();
        dropdown.update(ctx, |dropdown, ctx| {
            let mut items = vec![DropdownItem::new(
                DEFAULT_BACKEND_LABEL,
                BackendAction::Select(DEFAULT_BACKEND_ID.to_string()),
            )];
            let mut selected_index = 0;
            for (i, backend) in config.backends.iter().enumerate() {
                items.push(DropdownItem::new(
                    backend.display_name().to_string(),
                    BackendAction::Select(backend.id.clone()),
                ));
                if backend.id == selected_id {
                    selected_index = i + 1;
                }
            }
            dropdown.set_items(items, ctx);
            dropdown.set_selected_by_index(selected_index, ctx);
        });
    }
}

impl Entity for BackendSelectorView {
    type Event = ();
}

impl View for BackendSelectorView {
    fn ui_name() -> &'static str {
        "BackendSelectorView"
    }

    fn render(&self, _app: &warpui::AppContext) -> Box<dyn Element> {
        ChildView::new(&self.dropdown).finish()
    }
}

impl TypedActionView for BackendSelectorView {
    type Action = BackendAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            BackendAction::Select(id) => {
                agent_backends::set_selected(id);
                Self::update_dropdown_state(self.dropdown.clone(), ctx);
                ctx.notify();
            }
        }
    }
}
