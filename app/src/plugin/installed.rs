//! oh-my-warp: app-side enumeration of installed plugins for the Settings → Plugins list.
//!
//! Reads `~/.warp/plugins/*/plugin.json` for display only. The plugin **host** independently parses
//! the same manifests to gate capabilities and enforce `engines.warp` (see
//! `host/native/manifest.rs`); this is a read-only view for the settings UI, so it deliberately
//! parses just the display fields. See PLUGIN_SPEC.md (M4).

use std::fs;

use serde::Deserialize;

/// A plugin discovered under `~/.warp/plugins`, as shown in Settings.
#[derive(Debug, Clone)]
pub struct InstalledPlugin {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub permissions: Vec<String>,
    pub engines_warp: String,
    /// Whether the directory has a `plugin.json` (vs a bare `main.js` legacy plugin).
    pub has_manifest: bool,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct ManifestDisplay {
    id: Option<String>,
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    permissions: Vec<String>,
    engines: EnginesDisplay,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct EnginesDisplay {
    warp: Option<String>,
}

/// Scans `~/.warp/plugins` and returns the installed plugins (directories containing a `main.js`),
/// sorted by display name. A missing plugins directory yields an empty list.
pub fn installed_plugins() -> Vec<InstalledPlugin> {
    let Some(plugins_dir) = dirs::home_dir().map(|home| home.join(".warp/plugins")) else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(&plugins_dir) else {
        return Vec::new();
    };

    let mut plugins: Vec<InstalledPlugin> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() || !path.join("main.js").is_file() {
                return None;
            }
            let dir_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("plugin")
                .to_string();
            let (manifest, has_manifest) = match fs::read_to_string(path.join("plugin.json")) {
                Ok(text) => (
                    serde_json::from_str::<ManifestDisplay>(&text).unwrap_or_default(),
                    true,
                ),
                Err(_) => (ManifestDisplay::default(), false),
            };
            Some(InstalledPlugin {
                id: manifest.id.unwrap_or_else(|| dir_name.clone()),
                name: manifest.name.unwrap_or(dir_name),
                version: manifest.version.unwrap_or_else(|| "—".to_string()),
                description: manifest.description.unwrap_or_default(),
                permissions: manifest.permissions,
                engines_warp: manifest.engines.warp.unwrap_or_else(|| "*".to_string()),
                has_manifest,
            })
        })
        .collect();
    plugins.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    plugins
}
