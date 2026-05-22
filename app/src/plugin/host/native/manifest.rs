//! oh-my-warp: plugin manifest (`plugin.json`) parsing, the capability/permission model, and
//! `engines.warp` compatibility checking. Parsed in the plugin host **without executing plugin
//! code**, so the host can gate the `warp.*` surface and skip incompatible plugins before loading.
//! See PLUGIN_SPEC.md (M4).

use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::js_api::PLUGIN_API_VERSION;

/// The manifest file a plugin directory may contain alongside `main.js`.
pub(super) const PLUGIN_MANIFEST_FILE_NAME: &str = "plugin.json";

/// A parsed `plugin.json`. Every field is optional (via `#[serde(default)]`) so a minimal manifest
/// — or none at all — still yields a usable value. A directory with only `main.js` (no manifest)
/// is represented by [`Manifest::legacy`]: back-compat with the pre-M4 surface (all namespaces, any
/// API version).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Manifest {
    pub id: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    pub engines: Engines,
    pub description: Option<String>,
    /// Capability grants (see [`Manifest::permits`]). Absent ⇒ empty ⇒ only the always-on base
    /// surface (`version`/`log`/`plugin`) is exposed.
    pub permissions: Vec<String>,
    #[serde(rename = "activationEvents")]
    pub activation_events: Vec<String>,
    pub contributes: Contributes,

    /// `true` when there is no `plugin.json` at all (bare `main.js`). Not deserialized; set by
    /// [`Manifest::legacy`]. Legacy plugins keep the full pre-M4 surface for back-compat.
    #[serde(skip)]
    pub legacy: bool,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Engines {
    /// The `warp.*` API semver range the plugin targets, e.g. `"^1.0"`.
    pub warp: Option<String>,
}

/// Declarative contributions parsed from the manifest without running plugin code.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(super) struct Contributes {
    /// Declared commands. Parsed for forward-compatibility, but palette-listing of declared
    /// (handler-less) commands plus lazy activation is a follow-up; today a plugin's commands are
    /// surfaced by the imperative `warp.commands.register`. See PLUGIN_SPEC.md §6.
    #[allow(dead_code)]
    pub commands: Vec<CommandContribution>,
    pub keybindings: Vec<KeybindingContribution>,
}

#[allow(dead_code)] // see `Contributes::commands`
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CommandContribution {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct KeybindingContribution {
    pub command: String,
    pub key: String,
}

impl Manifest {
    /// Loads `<dir>/plugin.json`. A missing file yields a [`legacy`](Self::legacy) manifest
    /// (back-compat); a malformed file is logged and also treated as legacy so a typo never
    /// silently strips a plugin's capabilities without explanation.
    pub(super) fn load(dir: &Path) -> Self {
        let path = dir.join(PLUGIN_MANIFEST_FILE_NAME);
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Manifest>(&text) {
                Ok(manifest) => manifest,
                Err(e) => {
                    log::warn!("Invalid plugin manifest {path:?}: {e}; treating as legacy plugin.");
                    Self::legacy()
                }
            },
            Err(_) => Self::legacy(),
        }
    }

    /// A manifest for a bare `main.js` plugin (or a built-in): full pre-M4 surface, any API
    /// version.
    pub(super) fn legacy() -> Self {
        Self {
            legacy: true,
            ..Default::default()
        }
    }

    /// The plugin's effective id: explicit `id`, else the directory name, else `"plugin"`.
    pub(super) fn effective_id(&self, dir: &Path) -> String {
        self.id.clone().unwrap_or_else(|| {
            dir.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("plugin")
                .to_string()
        })
    }

    /// Whether the plugin was granted `permission`. Legacy (no-manifest) plugins are granted
    /// everything for back-compat; manifest plugins get exactly what they declare.
    pub(super) fn permits(&self, permission: &str) -> bool {
        self.legacy || self.permissions.iter().any(|p| p == permission)
    }

    /// Checks the plugin's `engines.warp` range against [`PLUGIN_API_VERSION`].
    ///
    /// Returns `Err(reason)` if the plugin targets an API version this host does not provide, so
    /// the caller can skip it with a logged, user-facing reason rather than load it half-broken.
    /// Legacy plugins and a missing range default to `"*"` (always compatible).
    pub(super) fn check_engine_compatibility(&self) -> Result<(), String> {
        let range = self.engines.warp.clone().unwrap_or_else(|| "*".to_string());
        let req = semver::VersionReq::parse(&range)
            .map_err(|e| format!("invalid engines.warp range {range:?}: {e}"))?;
        let api_version = semver::Version::parse(PLUGIN_API_VERSION)
            .expect("PLUGIN_API_VERSION must be valid semver");
        if req.matches(&api_version) {
            Ok(())
        } else {
            Err(format!(
                "plugin targets warp API {range}, but this host provides {PLUGIN_API_VERSION}"
            ))
        }
    }
}
