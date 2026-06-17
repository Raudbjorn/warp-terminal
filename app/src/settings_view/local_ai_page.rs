use crate::{
    appearance::Appearance,
    editor::{EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions},
    report_if_error,
    settings::{AISettings, LocalOpenAIEnabled},
};
use settings::{Setting, ToggleableSetting};
use std::cell::RefCell;
use std::collections::HashMap;
use warpui::{
    elements::{
        Container, CrossAxisAlignment, Element, Flex, MouseStateHandle, ParentElement, Text,
    },
    fonts::{Properties, Weight},
    ui_components::{
        components::{Coords, UiComponent, UiComponentStyles},
        switch::SwitchStateHandle,
    },
    AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle,
};

use super::{
    settings_page::{
        build_toggle_element, render_body_item_label, LocalOnlyIconState, MatchData, PageType,
        SettingsPageMeta, SettingsPageViewHandle, SettingsWidget, ToggleState, CONTENT_FONT_SIZE,
        HEADER_PADDING,
    },
    SettingsSection,
};

#[derive(Debug, Clone, PartialEq)]
pub enum LocalAISettingsPageAction {
    ToggleLocalOpenAIProvider,
}

pub struct LocalAISettingsPageView {
    page: PageType<Self>,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
}

impl LocalAISettingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        Self {
            page: PageType::new_uncategorized(
                vec![Box::new(LocalAIProviderWidget::new(ctx))],
                Some("Local AI"),
            ),
            local_only_icon_tooltip_states: Default::default(),
        }
    }
}

impl View for LocalAISettingsPageView {
    fn ui_name() -> &'static str {
        "LocalAISettingsPage"
    }

    fn render(&self, app: &AppContext) -> Box<dyn Element> {
        self.page.render(self, app)
    }
}

impl Entity for LocalAISettingsPageView {
    type Event = ();
}

impl TypedActionView for LocalAISettingsPageView {
    type Action = LocalAISettingsPageAction;

    fn handle_action(&mut self, action: &Self::Action, ctx: &mut ViewContext<Self>) {
        match action {
            LocalAISettingsPageAction::ToggleLocalOpenAIProvider => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_enabled.toggle_and_save_value(ctx));
                });
                ctx.notify();
            }
        }
    }
}

impl SettingsPageMeta for LocalAISettingsPageView {
    fn section() -> SettingsSection {
        SettingsSection::LocalAI
    }

    fn should_render(&self, _ctx: &AppContext) -> bool {
        true
    }

    fn update_filter(&mut self, query: &str, ctx: &mut ViewContext<Self>) -> MatchData {
        self.page.update_filter(query, ctx)
    }

    fn scroll_to_widget(&mut self, widget_id: &'static str) {
        self.page.scroll_to_widget(widget_id)
    }

    fn clear_highlighted_widget(&mut self) {
        self.page.clear_highlighted_widget();
    }
}

impl From<ViewHandle<LocalAISettingsPageView>> for SettingsPageViewHandle {
    fn from(view_handle: ViewHandle<LocalAISettingsPageView>) -> Self {
        SettingsPageViewHandle::LocalAI(view_handle)
    }
}

struct LocalAIProviderWidget {
    enabled_toggle: SwitchStateHandle,
    base_url_editor: ViewHandle<EditorView>,
    api_key_editor: ViewHandle<EditorView>,
    command_model_editor: ViewHandle<EditorView>,
    prediction_model_editor: ViewHandle<EditorView>,
    timeout_ms_editor: ViewHandle<EditorView>,
    opencode_command_editor: ViewHandle<EditorView>,
    opencode_args_editor: ViewHandle<EditorView>,
}

impl LocalAIProviderWidget {
    fn new(ctx: &mut ViewContext<LocalAISettingsPageView>) -> Self {
        let (
            base_url,
            api_key,
            command_model,
            prediction_model,
            timeout_ms,
            opencode_command,
            opencode_args,
        ) = {
            let ai_settings = AISettings::as_ref(ctx);
            (
                ai_settings.local_openai_base_url.value().clone(),
                ai_settings.local_openai_api_key.value().clone(),
                ai_settings.local_openai_command_model.value().clone(),
                ai_settings.local_openai_prediction_model.value().clone(),
                ai_settings.local_openai_timeout_ms.value().to_string(),
                ai_settings.local_openai_opencode_command.value().clone(),
                ai_settings
                    .local_openai_opencode_args
                    .value()
                    .join(" "),
            )
        };
        let base_url_editor = Self::create_editor(ctx, base_url, "http://127.0.0.1:1234/v1", false);
        ctx.subscribe_to_view(&base_url_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_base_url.set_value(value, ctx));
                });
            }
        });

        let api_key_editor = Self::create_editor(ctx, api_key, "Optional", true);
        ctx.subscribe_to_view(&api_key_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_api_key.set_value(value, ctx));
                });
            }
        });

        let command_model_editor =
            Self::create_editor(ctx, command_model, "qwen2.5-coder:7b", false);
        ctx.subscribe_to_view(&command_model_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_command_model.set_value(value, ctx));
                });
            }
        });

        let prediction_model_editor =
            Self::create_editor(ctx, prediction_model, "qwen2.5-coder:7b", false);
        ctx.subscribe_to_view(&prediction_model_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_prediction_model.set_value(value, ctx));
                });
            }
        });

        let timeout_ms_editor = Self::create_editor(ctx, timeout_ms, "8000", false);
        ctx.subscribe_to_view(&timeout_ms_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let buffer_text = editor.as_ref(ctx).buffer_text(ctx);
                // Reject non-positive timeouts (`0ms` would make every request fail
                // immediately) as well as unparseable input, resetting to the saved value.
                let value = match buffer_text.trim().parse::<u64>() {
                    Ok(value) if value > 0 => value,
                    _ => {
                        log::warn!(
                            "Invalid local OpenAI timeout: {:?}",
                            buffer_text.trim()
                        );
                        let current = AISettings::as_ref(ctx)
                            .local_openai_timeout_ms
                            .value()
                            .to_string();
                        editor.update(ctx, |editor, ctx| {
                            editor.set_buffer_text(&current, ctx);
                        });
                        return;
                    }
                };
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_timeout_ms.set_value(value, ctx));
                });
            }
        });
        let opencode_command_editor =
            Self::create_editor(ctx, opencode_command, "opencode", false);
        ctx.subscribe_to_view(&opencode_command_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_opencode_command.set_value(value, ctx));
                });
            }
        });

        let opencode_args_editor =
            Self::create_editor(ctx, opencode_args, "serve --port 0", false);
        ctx.subscribe_to_view(&opencode_args_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let buffer_text = editor.as_ref(ctx).buffer_text(ctx);
                // Parse with `shell_words` so quoted arguments with
                // embedded spaces (e.g. `--label "my model"`) survive
                // intact. Fall back to whitespace splitting when the
                // input is not well-formed shell syntax, so a single
                // stray quote does not lock the user out of saving.
                let value: Vec<String> = shell_words::split(&buffer_text)
                    .unwrap_or_else(|_| {
                        buffer_text
                            .split_whitespace()
                            .map(str::to_string)
                            .collect()
                    });
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(
                        settings.local_openai_opencode_args.set_value(value, ctx)
                    );
                });
            }
        });

        Self {
            enabled_toggle: Default::default(),
            base_url_editor,
            api_key_editor,
            command_model_editor,
            prediction_model_editor,
            timeout_ms_editor,
            opencode_command_editor,
            opencode_args_editor,
        }
    }

    fn create_editor(
        ctx: &mut ViewContext<LocalAISettingsPageView>,
        value: String,
        placeholder: &'static str,
        is_password: bool,
    ) -> ViewHandle<EditorView> {
        ctx.add_typed_action_view(move |ctx| {
            let appearance = Appearance::as_ref(ctx);
            let options = SingleLineEditorOptions {
                is_password,
                text: TextOptions {
                    font_size_override: Some(appearance.ui_font_size()),
                    font_family_override: Some(appearance.monospace_font_family()),
                    text_colors_override: Some(TextColors {
                        default_color: appearance.theme().active_ui_text_color(),
                        disabled_color: appearance.theme().disabled_ui_text_color(),
                        hint_color: appearance.theme().disabled_ui_text_color(),
                    }),
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut editor = EditorView::single_line(options, ctx);
            editor.set_placeholder_text(placeholder, ctx);
            editor.set_buffer_text(&value, ctx);
            editor
        })
    }

    fn render_description(appearance: &Appearance, text: &'static str) -> Box<dyn Element> {
        Text::new(text, appearance.ui_font_family(), appearance.ui_font_size())
            .with_color(
                appearance
                    .theme()
                    .sub_text_color(appearance.theme().background())
                    .into(),
            )
            .soft_wrap(true)
            .finish()
    }

    fn render_toggle(
        &self,
        view: &LocalAISettingsPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let is_enabled = *AISettings::as_ref(app).local_openai_enabled.value();
        let label = render_body_item_label::<LocalAISettingsPageAction>(
            "Use Local AI provider".to_string(),
            Some(appearance.theme().active_ui_text_color().into()),
            None,
            LocalOnlyIconState::for_setting(
                LocalOpenAIEnabled::storage_key(),
                LocalOpenAIEnabled::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
        );

        let switch = appearance
            .ui_builder()
            .switch(self.enabled_toggle.clone())
            .check(is_enabled)
            .build()
            .on_click(move |ctx, _, _| {
                ctx.dispatch_typed_action(LocalAISettingsPageAction::ToggleLocalOpenAIProvider);
            })
            .finish();

        build_toggle_element(
            label,
            switch,
            appearance,
            Some(
                "When enabled, local AI requests are sent only to the endpoint configured below."
                    .to_string(),
            ),
        )
    }

    fn render_input(
        appearance: &Appearance,
        label: &'static str,
        editor: ViewHandle<EditorView>,
    ) -> Box<dyn Element> {
        let label = Text::new_inline(label, appearance.ui_font_family(), CONTENT_FONT_SIZE)
            .with_style(Properties::default().weight(Weight::Medium))
            .with_color(appearance.theme().active_ui_text_color().into())
            .finish();

        let input = appearance
            .ui_builder()
            .text_input(editor)
            .with_style(UiComponentStyles {
                padding: Some(Coords {
                    top: 10.,
                    bottom: 10.,
                    left: 16.,
                    right: 16.,
                }),
                background: Some(appearance.theme().surface_2().into()),
                ..Default::default()
            })
            .build()
            .finish();

        Flex::column()
            .with_spacing(8.)
            .with_child(label)
            .with_child(input)
            .finish()
    }
}

impl SettingsWidget for LocalAIProviderWidget {
    type View = LocalAISettingsPageView;

    fn search_terms(&self) -> &str {
        "local ai local openai openai compatible endpoint base url api key model command prediction timeout ollama lm studio opencode sidecar"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        Container::new(
            Flex::column()
                .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
                .with_spacing(16.)
                .with_child(Self::render_description(
                    appearance,
                    "Use an OpenAI-compatible local or private endpoint for command generation, workflow metadata, and command prediction. Local AI does not use Warp credits, account state, or workspace policy.",
                ))
                .with_child(self.render_toggle(view, appearance, app))
                .with_child(Self::render_input(
                    appearance,
                    "Base URL",
                    self.base_url_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "API Key",
                    self.api_key_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "Command Model",
                    self.command_model_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "Prediction Model",
                    self.prediction_model_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "Timeout (ms)",
                    self.timeout_ms_editor.clone(),
                ))
                .with_child(Self::render_description(
                    appearance,
                    "OpenCode sidecar: route requests through a locally-spawned OpenCode process bound to the working directory. The sidecar exposes an OpenAI-compatible endpoint on a random loopback port; we read the port from its startup announcement. Set `OpenCode Command` to the binary name or absolute path; the default `opencode` resolves from `$PATH`.",
                ))
                .with_child(Self::render_input(
                    appearance,
                    "OpenCode Command",
                    self.opencode_command_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "OpenCode Args",
                    self.opencode_args_editor.clone(),
                ))
                .finish(),
        )
        .with_margin_bottom(HEADER_PADDING)
        .finish()
}
}
