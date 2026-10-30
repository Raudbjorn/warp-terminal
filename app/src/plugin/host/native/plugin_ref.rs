use std::path::PathBuf;
use std::{fs, io};

use super::manifest::Manifest;

pub(super) const PLUGIN_ENTRYPOINT_JS_FILE_NAME: &str = "main.js";

#[derive(thiserror::Error, Debug)]
pub(super) enum PluginLoadError {
    #[error("Failed to load plugin: {0:?}")]
    File(#[from] io::Error),
}

/// References a single plugin.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) enum PluginRef {
    /// Refers to plugin source on disk.
    Path(PathBuf),
}

impl PluginRef {
    pub(super) fn plugin_bytes(&self) -> Result<Vec<u8>, PluginLoadError> {
        match self {
            PluginRef::Path(path) => {
                let entrypoint_file_path = path.join(PLUGIN_ENTRYPOINT_JS_FILE_NAME);
                fs::read(entrypoint_file_path).map_err(PluginLoadError::from)
            }
        }
    }

    /// Loads this plugin's [`Manifest`] (`plugin.json`). Bare `main.js` directories
    /// yield a legacy manifest (full pre-M4 surface, any API version).
    pub(super) fn manifest(&self) -> Manifest {
        match self {
            PluginRef::Path(path) => Manifest::load(path),
        }
    }

    /// The plugin's effective id: the manifest `id`, else the directory name.
    pub(super) fn effective_id(&self, manifest: &Manifest) -> String {
        match self {
            PluginRef::Path(path) => manifest.effective_id(path),
        }
    }

    /// A display path for the plugin's directory.
    pub(super) fn display_dir(&self) -> String {
        match self {
            PluginRef::Path(path) => path.to_string_lossy().into_owned(),
        }
    }
}
