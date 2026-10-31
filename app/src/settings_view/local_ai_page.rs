use std::cell::RefCell;
use std::collections::HashMap;

use settings::{Setting, ToggleableSetting};
use warp_core::channel::ChannelState;
use warpui::elements::{
    ChildView, Container, CrossAxisAlignment, Element, Flex, MouseStateHandle, ParentElement, Text,
};
use warpui::fonts::{Properties, Weight};
use warpui::ui_components::components::{Coords, UiComponent, UiComponentStyles};
use warpui::ui_components::switch::SwitchStateHandle;
use warpui::{AppContext, Entity, SingletonEntity, TypedActionView, View, ViewContext, ViewHandle};

use super::SettingsSection;
use super::settings_page::{
    CONTENT_FONT_SIZE, HEADER_PADDING, LocalOnlyIconState, MatchData, PageTitle, PageType,
    SettingsPageMeta, SettingsPageViewHandle, SettingsWidget, ToggleState, build_toggle_element,
    render_body_item, render_body_item_label,
};
use crate::appearance::Appearance;
use crate::editor::{
    EditorView, Event as EditorEvent, SingleLineEditorOptions, TextColors, TextOptions,
};
use crate::report_if_error;
use crate::settings::{AISettings, LocalOpenAIEnabled, LocalOpenAIProviderKind, LocalProviderKind};
use crate::view_components::{Dropdown, DropdownItem};

#[derive(Debug, Clone, PartialEq)]
pub enum LocalAISettingsPageAction {
    ToggleLocalOpenAIProvider,
    SetProviderKind(LocalProviderKind),
}

pub struct LocalAISettingsPageView {
    page: PageType<Self>,
    local_only_icon_tooltip_states: RefCell<HashMap<String, MouseStateHandle>>,
    provider_kind_dropdown: ViewHandle<Dropdown<LocalAISettingsPageAction>>,
}

impl LocalAISettingsPageView {
    pub fn new(ctx: &mut ViewContext<Self>) -> Self {
        let provider_kind_dropdown = ctx.add_typed_action_view(|ctx| {
            let mut dropdown = Dropdown::new(ctx);
            dropdown.set_top_bar_max_width(360.);
            dropdown
        });
        Self::update_provider_kind_dropdown(provider_kind_dropdown.clone(), ctx);
        // Keep the selection in sync when the setting changes elsewhere
        // (settings file, another window), and re-render so the fields shown
        // match the provider.
        ctx.subscribe_to_model(&AISettings::handle(ctx), |view, _, _, ctx| {
            Self::update_provider_kind_dropdown(view.provider_kind_dropdown.clone(), ctx);
            ctx.notify();
        });

        Self {
            page: PageType::new_uncategorized(
                vec![Box::new(LocalAIProviderWidget::new(ctx))],
                Some(PageTitle::new("Local AI")),
            ),
            local_only_icon_tooltip_states: Default::default(),
            provider_kind_dropdown,
        }
    }

    fn update_provider_kind_dropdown(
        dropdown: ViewHandle<Dropdown<LocalAISettingsPageAction>>,
        ctx: &mut ViewContext<Self>,
    ) {
        let current = AISettings::as_ref(ctx).local_provider_kind();
        dropdown.update(ctx, |dropdown, ctx| {
            dropdown.set_items(
                LocalProviderKind::ALL
                    .into_iter()
                    .map(|kind| {
                        DropdownItem::new(
                            kind.label(),
                            LocalAISettingsPageAction::SetProviderKind(kind),
                        )
                    })
                    .collect(),
                ctx,
            );
            dropdown
                .set_selected_by_action(LocalAISettingsPageAction::SetProviderKind(current), ctx);
        });
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
            LocalAISettingsPageAction::SetProviderKind(kind) => {
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(
                        settings
                            .local_openai_provider_kind
                            .set_value(kind.setting_value().to_string(), ctx)
                    );
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
        // Local AI (openai-compatible endpoint, model selection) only works in
        // the local-only channel — in Dev/Preview/Stable the local provider
        // is not configured, so showing this page would just confuse users.
        ChannelState::is_local_only()
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
                // Quote with `shell_words::join` so the text round-trips through
                // the `shell_words::split` save handler below: arguments with
                // spaces, quotes, or backslashes survive an unchanged save.
                shell_words::join(ai_settings.local_openai_opencode_args.value()),
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
                // immediately) and clamp the upper bound to 1 hour to prevent
                // integer-overflow panics inside `tokio`/`reqwest` timer wheels when
                // the duration is added to `Instant::now()`. On any other invalid
                // input, reset the editor text to the saved value.
                let value = match buffer_text.trim().parse::<u64>() {
                    // Clamp overly-large (but otherwise valid) inputs to the 1-hour
                    // cap rather than reverting them; reject non-positive/invalid input.
                    Ok(value) if value > 0 => {
                        let clamped =
                            value.min(crate::ai::local_openai::MAX_LOCAL_OPENAI_TIMEOUT_MS);
                        // When we clamped the input, the persisted value no longer matches
                        // what the user typed. Sync the editor buffer to the clamped value
                        // so the UI does not mislead users into thinking the larger value
                        // was applied.
                        if clamped != value {
                            editor.update(ctx, |editor, ctx| {
                                editor.set_buffer_text(&clamped.to_string(), ctx);
                            });
                        }
                        clamped
                    }
                    _ => {
                        log::warn!("Invalid local OpenAI timeout: {:?}", buffer_text.trim());
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
        let opencode_command_editor = Self::create_editor(ctx, opencode_command, "opencode", false);
        ctx.subscribe_to_view(&opencode_command_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let value = editor.as_ref(ctx).buffer_text(ctx).trim().to_string();
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_opencode_command.set_value(value, ctx));
                });
            }
        });

        let opencode_args_editor = Self::create_editor(ctx, opencode_args, "serve --port 0", false);
        ctx.subscribe_to_view(&opencode_args_editor, |_, editor, event, ctx| {
            if matches!(event, EditorEvent::Blurred | EditorEvent::Enter) {
                let buffer_text = editor.as_ref(ctx).buffer_text(ctx);
                // Shell-style split so quoted arguments containing spaces
                // (`--model "my model"`) stay one token. Unbalanced quotes
                // are rejected and the saved value restored, matching the
                // timeout editor above, rather than saving a mangled argv.
                let Ok(value) = shell_words::split(&buffer_text) else {
                    log::warn!(
                        "Invalid OpenCode args (unbalanced quote): {:?}",
                        buffer_text.trim()
                    );
                    let current = shell_words::join(
                        AISettings::as_ref(ctx).local_openai_opencode_args.value(),
                    );
                    editor.update(ctx, |editor, ctx| {
                        editor.set_buffer_text(&current, ctx);
                    });
                    return;
                };
                AISettings::handle(ctx).update(ctx, |settings, ctx| {
                    report_if_error!(settings.local_openai_opencode_args.set_value(value, ctx));
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

    fn render_provider_selector(
        view: &LocalAISettingsPageView,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        render_body_item::<LocalAISettingsPageAction>(
            "Provider".into(),
            None,
            LocalOnlyIconState::for_setting(
                LocalOpenAIProviderKind::storage_key(),
                LocalOpenAIProviderKind::sync_to_cloud(),
                &mut view.local_only_icon_tooltip_states.borrow_mut(),
                app,
            ),
            ToggleState::Enabled,
            appearance,
            ChildView::new(&view.provider_kind_dropdown).finish(),
            Some("Where local AI requests are sent.".to_owned()),
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
        "local ai local openai openai compatible provider endpoint base url api key model command prediction timeout ollama lm studio opencode sidecar"
    }

    fn render(
        &self,
        view: &Self::View,
        appearance: &Appearance,
        app: &AppContext,
    ) -> Box<dyn Element> {
        let provider_kind = AISettings::as_ref(app).local_provider_kind();
        let mut column = Flex::column()
            .with_cross_axis_alignment(CrossAxisAlignment::Stretch)
            .with_spacing(16.)
            .with_child(Self::render_description(
                appearance,
                "Use a local or private AI provider for command generation, workflow metadata, and command prediction. Local AI does not use Warp credits, account state, or workspace policy.",
            ))
            .with_child(self.render_toggle(view, appearance, app))
            .with_child(Self::render_provider_selector(view, appearance, app));

        column = match provider_kind {
            LocalProviderKind::OpenAICompatible => column
                .with_child(Self::render_input(
                    appearance,
                    "Base URL",
                    self.base_url_editor.clone(),
                ))
                .with_child(Self::render_input(
                    appearance,
                    "API Key",
                    self.api_key_editor.clone(),
                )),
            LocalProviderKind::OpenCode => column
                .with_child(Self::render_description(
                    appearance,
                    "OpenCode sidecar: Warp starts `opencode serve` in its own working directory on a random loopback port, protected by a per-process password. Each request runs in a short-lived session with every tool permission denied. Set `OpenCode Command` to the binary name or an absolute path; the default `opencode` resolves from `$PATH`. Arguments are split shell-style.",
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
                .with_child(Self::render_description(
                    appearance,
                    "With OpenCode, write models as `providerID/modelID`, for example `ollama/qwen2.5-coder:7b`. The provider must already be configured in OpenCode. A hosted provider sends your prompts off this machine.",
                )),
        };

        Container::new(
            column
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
