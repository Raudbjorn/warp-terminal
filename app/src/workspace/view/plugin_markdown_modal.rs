//! oh-my-warp: body view for the modal that renders markdown a plugin asked to show via
//! `warp.ui.showMarkdown(title, markdown)`. The `Workspace` wraps this in a [`crate::modal::Modal`]
//! and opens it when a `PluginHostEvent::ShowMarkdown` arrives. See PLUGIN_SPEC.md (M4).

use std::collections::VecDeque;

use markdown_parser::{parse_markdown, FormattedText};
use warpui::elements::{Container, Element, FormattedTextElement, HighlightedHyperlink};
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext};

use crate::appearance::Appearance;

/// Renders the most recent markdown a plugin asked to show.
pub struct PluginMarkdownModal {
    formatted: Option<FormattedText>,
    link_state: HighlightedHyperlink,
}

impl PluginMarkdownModal {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            formatted: None,
            link_state: HighlightedHyperlink::default(),
        }
    }

    /// Parses and stores `markdown` for rendering. Call before opening the modal.
    pub fn set_markdown(&mut self, markdown: &str, ctx: &mut ViewContext<Self>) {
        self.formatted = parse_markdown(markdown).ok();
        ctx.notify();
    }
}

impl Entity for PluginMarkdownModal {
    type Event = ();
}

impl View for PluginMarkdownModal {
    fn ui_name() -> &'static str {
        "PluginMarkdownModal"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        let appearance = Appearance::as_ref(app);
        let formatted = self
            .formatted
            .clone()
            .unwrap_or_else(|| FormattedText::new(VecDeque::new()));
        Container::new(
            FormattedTextElement::new(
                formatted,
                14.0,
                appearance.ui_font_family(),
                appearance.monospace_font_family(),
                appearance
                    .theme()
                    .main_text_color(appearance.theme().surface_2())
                    .into_solid(),
                self.link_state.clone(),
            )
            .finish(),
        )
        .finish()
    }
}
