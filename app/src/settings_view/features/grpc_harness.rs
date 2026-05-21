//! oh-my-warp: Settings dropdown for selecting the active gRPC harness.
//!
//! Lists the harnesses configured on the selected backend (`grpc_harnesses` in
//! `agent_backends.toml`, see [`crate::util::agent_backends`]) and persists the
//! choice via [`agent_backends::set_grpc_harness`]; it takes effect on next launch.
//! The in-process bridge reads the selected backend's `grpc_harness`.

use warpui::{
    presenter::ChildView, Element, Entity, TypedActionView, View, ViewContext, ViewHandle,
};

use crate::{
    util::agent_backends,
    view_components::{Dropdown, DropdownItem},
};

/// A view wrapping a dropdown that selects the active gRPC harness.
pub struct GrpcHarnessSelectorView {
    dropdown: ViewHandle<Dropdown<GrpcHarnessAction>>,
}

#[derive(Debug, Clone)]
pub enum GrpcHarnessAction {
    /// Select this harness (empty string = nothing selectable / no gRPC backend).
    Select(String),
}

impl GrpcHarnessSelectorView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(260.);
            dropdown
        });
        Self::update_dropdown_state(dropdown.clone(), ctx);
        Self { dropdown }
    }

    /// Rebuilds the dropdown items from the selected backend's `grpc_harnesses`
    /// and highlights the current `grpc_harness`.
    fn update_dropdown_state(
        dropdown: ViewHandle<Dropdown<GrpcHarnessAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        let config = agent_backends::load();
        let (harnesses, current) = match config.selected_backend() {
            Some(b) => (b.grpc_harnesses.clone(), b.grpc_harness.clone()),
            None => (Vec::new(), String::new()),
        };
        dropdown.update(ctx, |dropdown, ctx| {
            let mut items = Vec::new();
            let mut selected_index = 0;
            if harnesses.is_empty() {
                items.push(DropdownItem::new(
                    "(no gRPC backend selected)".to_string(),
                    GrpcHarnessAction::Select(String::new()),
                ));
            } else {
                for (i, harness) in harnesses.iter().enumerate() {
                    items.push(DropdownItem::new(
                        harness.clone(),
                        GrpcHarnessAction::Select(harness.clone()),
                    ));
                    if *harness == current {
                        selected_index = i;
                    }
                }
            }
            dropdown.set_items(items, ctx);
            dropdown.set_selected_by_index(selected_index, ctx);
        });
    }
}

impl Entity for GrpcHarnessSelectorView {
    type Event = ();
}

impl View for GrpcHarnessSelectorView {
    fn ui_name() -> &'static str {
        "GrpcHarnessSelectorView"
    }

    fn render(&self, _app: &warpui::AppContext) -> Box<dyn Element> {
        ChildView::new(&self.dropdown).finish()
    }
}

impl TypedActionView for GrpcHarnessSelectorView {
    type Action = GrpcHarnessAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            GrpcHarnessAction::Select(harness) => {
                if !harness.is_empty() {
                    agent_backends::set_grpc_harness(harness);
                }
                Self::update_dropdown_state(self.dropdown.clone(), ctx);
                ctx.notify();
            }
        }
    }
}
