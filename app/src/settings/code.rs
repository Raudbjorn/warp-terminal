use std::collections::HashMap;

use settings::macros::define_settings_group;
use settings::{RespectUserSyncSetting, SupportedPlatforms, SyncToCloud};

define_settings_group!(CodeSettings, settings: [
    code_as_default_editor: CodeAsDefaultEditor {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.use_warp_as_default_editor",
        description: "Whether Warp is used as the default code editor.",
    }
    codebase_context_enabled: CodebaseContextEnabled {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "AgentModeCodebaseContext",
        toml_path: "code.indexing.agent_mode_codebase_context",
        description: "Whether codebase context is provided to the AI agent.",
    },
    auto_indexing_enabled: AutoIndexingEnabled {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::DESKTOP,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        storage_key: "AgentModeCodebaseContextAutoIndexing",
        toml_path: "code.indexing.agent_mode_codebase_context_auto_indexing",
        description: "Whether automatic codebase indexing is enabled.",
    },
    // Whether or not the user has manually dismissed the code toolbelt new feature popup.
    dismissed_code_toolbelt_new_feature_popup: DismissedCodeToolbeltNewFeaturePopup {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: true,
    },
    // Controls whether the project explorer / file tree appears in the tools panel.
    show_project_explorer: ShowProjectExplorer {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.show_project_explorer",
        description: "Whether the project explorer is shown in the tools panel.",
    },
    // Controls whether global file search appears in the tools panel.
    show_global_search: ShowGlobalSearch {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.show_global_search",
        description: "Whether global file search is shown in the tools panel.",
    },
    // Controls whether hidden files (dotfiles) are shown in the project explorer.
    show_hidden_files: ShowHiddenFiles {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.show_hidden_files",
        description: "Whether hidden files (dotfiles) are shown in the project explorer.",
    },
    // Controls whether the file is reformatted on save, by the project's formatter or the
    // language server.
    format_on_save: FormatOnSave {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.format_on_save",
        description: "Whether the file is automatically formatted on save, with the project's formatter when one is set up and otherwise with the language server. Other LSP features (hover, go-to-definition, references, diagnostics) are unaffected.",
    },
    // Controls whether formatting prefers the formatter a project is set up with over the
    // language server.
    use_project_formatters: UseProjectFormatters {
        type: bool,
        default: true,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.use_project_formatters",
        description: "Whether files are formatted with the formatter their project is set up with (Prettier, Biome, Ruff, Black, rustfmt, gofmt and others) when it is installed and the project has been explicitly approved for this application session, instead of with the language server. Use Format document to approve a project.",
    },
    // Formatter choice per language, keyed by a language identifier ("typescript"), a file
    // extension ("md") or "*". Not synced because the tools available differ per machine.
    formatter_overrides: FormatterOverrides {
        type: HashMap<String, String>,
        default: HashMap::default(),
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.formatters",
        max_table_depth: 1,
        description: "Which formatter to use per language. Keys are a language identifier such as typescript or python, a file extension such as md, or * for every file. Values are auto (the project's formatter, else the language server), lsp, off, or the name of a formatter such as prettier, biome, ruff or black.",
    },
    // How long a project formatter may run before it is stopped and the file left as it was.
    formatter_timeout_ms: FormatterTimeoutMs {
        type: u64,
        default: 5000,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Never,
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.formatter_timeout_ms",
        description: "How long, in milliseconds, to wait for shell PATH capture, project formatter discovery and execution before saving the file unformatted.",
    },
    // Controls whether the Warp text editor automatically saves file changes as the
    // user types (debounced) and when the editor loses focus. Only applies to the
    // Warp text editor, not the command line or AI input.
    auto_save: AutoSave {
        type: bool,
        default: false,
        supported_platforms: SupportedPlatforms::ALL,
        sync_to_cloud: SyncToCloud::Globally(RespectUserSyncSetting::Yes),
        surface: settings::SettingSurfaces::GUI,
        private: false,
        toml_path: "code.editor.auto_save",
        description: "Whether the Warp text editor automatically saves changes as you type and when the editor loses focus.",
    },
]);
