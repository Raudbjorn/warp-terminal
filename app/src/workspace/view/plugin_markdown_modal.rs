//! oh-my-warp: body view for the modal that renders markdown a plugin asked to show via
//! `warp.ui.showMarkdown(title, markdown)`. The `Workspace` wraps this in a [`crate::modal::Modal`]
//! and opens it when a `PluginHostEvent::ShowMarkdown` arrives. See PLUGIN_SPEC.md (M4).
//!
//! Rendering matches the chrome a native Warp markdown surface uses (agent zero-state, prompt
//! alerts): the same `FormattedTextElement` for parsed markdown, with `inline_code_properties` for
//! `` `code` `` spans, themed hyperlink color, and a vertical clipped scrollable so long bodies
//! stay inside the modal frame.

use std::collections::VecDeque;

use markdown_parser::{parse_markdown, FormattedText};
use warpui::elements::{
    ClippedScrollStateHandle, ClippedScrollable, Container, Element, Fill, FormattedTextElement,
    HighlightedHyperlink, ScrollbarWidth,
};
use warpui::{AppContext, Entity, SingletonEntity, View, ViewContext};

use crate::appearance::Appearance;

const BODY_FONT_SIZE: f32 = 14.0;
const SCROLLBAR_WIDTH: ScrollbarWidth = ScrollbarWidth::Auto;

/// Renders the most recent markdown a plugin asked to show.
pub struct PluginMarkdownModal {
    formatted: Option<FormattedText>,
    link_state: HighlightedHyperlink,
    /// Drives the vertical scrollable wrapper so long markdown stays inside the modal frame instead
    /// of overflowing it.
    scroll_state: ClippedScrollStateHandle,
}

impl PluginMarkdownModal {
    pub fn new(_ctx: &mut ViewContext<Self>) -> Self {
        Self {
            formatted: None,
            link_state: HighlightedHyperlink::default(),
            scroll_state: ClippedScrollStateHandle::default(),
        }
    }

    /// Parses and stores `markdown` for rendering. Call before opening the modal.
    pub fn set_markdown(&mut self, markdown: &str, ctx: &mut ViewContext<Self>) {
        self.formatted = parse_markdown(markdown).ok();
        // Reset the scroll position when the body changes so a re-opened modal starts at the top
        // rather than wherever the previous markdown left it.
        self.scroll_state = ClippedScrollStateHandle::default();
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
        let theme = appearance.theme();
        // The Modal sets the body background to `surface_2` (see `app/src/modal.rs` default
        // styles), so we pick text/code colors *on that surface* for legible contrast.
        let body_bg = theme.surface_2();
        let body_text_color = theme.main_text_color(body_bg).into_solid();
        let inline_code_color = theme.accent_button_color().into_solid();
        let inline_code_bg = theme.surface_3().into_solid();

        let formatted = self
            .formatted
            .clone()
            .unwrap_or_else(|| FormattedText::new(VecDeque::new()));

        let text_element = FormattedTextElement::new(
            formatted,
            BODY_FONT_SIZE,
            appearance.ui_font_family(),
            appearance.monospace_font_family(),
            body_text_color,
            self.link_state.clone(),
        )
        .with_inline_code_properties(Some(inline_code_color), Some(inline_code_bg))
        .with_hyperlink_font_color(theme.accent_button_color().into_solid())
        .finish();

        // ClippedScrollable::vertical takes (state, body, scrollbar_width, dim_color, bright_color,
        // background_fill); the colors drive the scrollbar thumb at rest vs while dragging.
        let scrollable = ClippedScrollable::vertical(
            self.scroll_state.clone(),
            text_element,
            SCROLLBAR_WIDTH,
            theme.disabled_text_color(body_bg).into(),
            theme.main_text_color(body_bg).into(),
            Fill::None,
        )
        .finish();

        Container::new(scrollable).finish()
    }
}
