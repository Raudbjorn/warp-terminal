use std::path::PathBuf;
use std::{fs, io};

use super::manifest::Manifest;

pub(super) const PLUGIN_ENTRYPOINT_JS_FILE_NAME: &str = "main.js";

#[derive(thiserror::Error, Debug)]
pub(super) enum PluginLoadError {
    #[error("Failed to load plugin: {0:?}")]
    File(#[from] io::Error),

    #[error("Missing source for builtin plugin: {0:?}")]
    MissingBuiltin(BuiltInPluginType),
}

/// Represents "Built-in" plugins. Each variant corresponds to a plugin bundled with Warp by
/// default (e.g. Completions/Command Signatures)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum BuiltInPluginType {
    #[cfg_attr(not(feature = "completions_v2"), allow(dead_code))]
    Completions,
}

impl BuiltInPluginType {
    #[cfg(feature = "completions_v2")]
    pub(super) fn plugin_bytes(&self) -> Option<Vec<u8>> {
        match self {
            BuiltInPluginType::Completions => {
                command_signatures_v2::CommandSignaturesJs::get("main.js")
                    .map(|bytes| bytes.data.into())
            }
        }
    }

    #[cfg(not(feature = "completions_v2"))]
    pub(super) fn plugin_bytes(&self) -> Option<Vec<u8>> {
        None
    }
}

/// References a single plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum PluginRef {
    /// Refers to plugin source on disk.
    Path(PathBuf),

    /// Refers to a "built-in" plugin bundled with the Warp binary.
    #[cfg_attr(not(feature = "completions_v2"), allow(dead_code))]
    BuiltIn(BuiltInPluginType),
}

impl PluginRef {
    pub(super) fn plugin_bytes(&self) -> Result<Vec<u8>, PluginLoadError> {
        match self {
            PluginRef::Path(path) => {
                let entrypoint_file_path = path.join(PLUGIN_ENTRYPOINT_JS_FILE_NAME);
                fs::read(entrypoint_file_path).map_err(PluginLoadError::from)
            }
            PluginRef::BuiltIn(builtin_plugin_type) => match builtin_plugin_type.plugin_bytes() {
                Some(bytes) => Ok(bytes),
                None => Err(PluginLoadError::MissingBuiltin(*builtin_plugin_type)),
            },
        }
    }

    /// Loads this plugin's [`Manifest`] (`plugin.json`). Bare `main.js` directories and built-ins
    /// yield a legacy manifest (full pre-M4 surface, any API version).
    pub(super) fn manifest(&self) -> Manifest {
        match self {
            PluginRef::Path(path) => Manifest::load(path),
            PluginRef::BuiltIn(_) => Manifest::legacy(),
        }
    }

    /// The plugin's effective id: the manifest `id`, else the directory name, else a `builtin:` tag.
    pub(super) fn effective_id(&self, manifest: &Manifest) -> String {
        match self {
            PluginRef::Path(path) => manifest.effective_id(path),
            PluginRef::BuiltIn(builtin) => format!("builtin:{builtin:?}").to_lowercase(),
        }
    }

    /// A display path for the plugin's directory; empty for built-ins (no on-disk directory).
    pub(super) fn display_dir(&self) -> String {
        match self {
            PluginRef::Path(path) => path.to_string_lossy().into_owned(),
            PluginRef::BuiltIn(_) => String::new(),
        }
    }
}
