use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::LazyLock;

use warp_core::ui::appearance::Appearance;
use warp_core::ui::theme::{ColorScheme, WarpTheme};
use warpui::Element;
use warpui::assets::asset_cache::AssetSource;
use warpui::elements::{CacheOption, Image};

use self::mappings::{
    DEFAULT_FILE, DEFAULT_FOLDER, EXTENSIONS, FILE_NAMES, FOLDER_NAMES, NAME_PREFIXES,
    NESTED_FOLDERS,
};

mod mappings;

const PATH_SEPARATORS: [char; 2] = ['/', '\\'];

/// A bundled full-color icon for a kind of file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FileIcon {
    path: &'static str,
    /// Replacement for themes with a light background, where `path` would lack contrast.
    light_path: Option<&'static str>,
}

impl FileIcon {
    fn path_for_theme(&self, theme: &WarpTheme) -> &'static str {
        let Some(light_path) = self.light_path else {
            return self.path;
        };
        match theme.inferred_color_scheme() {
            ColorScheme::LightOnDark => self.path,
            ColorScheme::DarkOnLight => light_path,
        }
    }
}

/// A bundled full-color icon for a kind of folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FolderIcon {
    closed: &'static str,
    open: &'static str,
}

impl FolderIcon {
    fn path(&self, is_open: bool) -> &'static str {
        if is_open { self.open } else { self.closed }
    }
}

static ICONS_BY_FILE_NAME: LazyLock<HashMap<&'static str, FileIcon>> =
    LazyLock::new(|| index_by_key(FILE_NAMES));

static ICONS_BY_EXTENSION: LazyLock<HashMap<&'static str, FileIcon>> =
    LazyLock::new(|| index_by_key(EXTENSIONS));

static ICONS_BY_FOLDER_NAME: LazyLock<HashMap<&'static str, FolderIcon>> =
    LazyLock::new(|| index_by_key(FOLDER_NAMES));

fn index_by_key<T: Copy>(
    groups: &'static [(&'static [&'static str], T)],
) -> HashMap<&'static str, T> {
    groups
        .iter()
        .flat_map(|(keys, icon)| keys.iter().map(move |key| (*key, *icon)))
        .collect()
}

/// Returns a special icon for the given file path, if any.
pub fn icon_from_file_path(path: &str, appearance: &Appearance) -> Option<Box<dyn Element>> {
    let icon = file_icon(path)?;
    Some(bundled_image(icon.path_for_theme(appearance.theme())))
}

/// Returns the generic icon for files that have no special icon.
pub fn default_file_icon() -> Box<dyn Element> {
    bundled_image(DEFAULT_FILE)
}

/// Returns the icon for the folder at the given path, which is generic unless the folder has a
/// well-known name.
pub fn icon_from_folder_path(path: &str, is_open: bool) -> Box<dyn Element> {
    bundled_image(folder_icon(path).path(is_open))
}

fn bundled_image(path: &'static str) -> Box<dyn Element> {
    Image::new(AssetSource::Bundled { path }, CacheOption::BySize).finish()
}

fn file_icon(path: &str) -> Option<FileIcon> {
    let name = lowercase(path.rsplit(PATH_SEPARATORS).next().unwrap_or(path));
    let name = name.as_ref();

    if let Some(icon) = ICONS_BY_FILE_NAME.get(name) {
        return Some(*icon);
    }
    if let Some(icon) = file_icon_from_name_prefix(name) {
        return Some(icon);
    }
    // Dots are visited left to right, so the longest (most specific) extension is tried first.
    name.match_indices('.')
        .find_map(|(dot, _)| ICONS_BY_EXTENSION.get(&name[dot + 1..]).copied())
}

fn file_icon_from_name_prefix(name: &str) -> Option<FileIcon> {
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    NAME_PREFIXES
        .iter()
        .find(|rule| {
            let ends_at_word_boundary = name
                .strip_prefix(rule.prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['.', '-', '_']));
            ends_at_word_boundary
                && (rule.extensions.is_empty() || rule.extensions.contains(&extension))
        })
        .map(|rule| rule.icon)
}

fn folder_icon(path: &str) -> FolderIcon {
    let mut components = path
        .trim_end_matches(PATH_SEPARATORS)
        .rsplit(PATH_SEPARATORS);
    let name = lowercase(components.next().unwrap_or_default());
    let name = undecorated_folder_name(&name);

    if let Some(parent) = components.next().map(undecorated_folder_name)
        && let Some(nested) = NESTED_FOLDERS
            .iter()
            .find(|nested| nested.name == name && nested.parent.eq_ignore_ascii_case(parent))
    {
        return nested.icon;
    }
    ICONS_BY_FOLDER_NAME
        .get(name)
        .copied()
        .unwrap_or(DEFAULT_FOLDER)
}

/// Strips the punctuation conventionally wrapped around well-known folder names, so that
/// `.github`, `_tests` and `__tests__` resolve like `github` and `tests`.
fn undecorated_folder_name(name: &str) -> &str {
    name.strip_prefix("__")
        .and_then(|name| name.strip_suffix("__"))
        .or_else(|| name.strip_prefix(['.', '_', '-']))
        .unwrap_or(name)
}

/// Lowercases ASCII letters, borrowing when the input is already lowercase since this runs for
/// every visible row on each render.
fn lowercase(name: &str) -> Cow<'_, str> {
    if name.bytes().any(|byte| byte.is_ascii_uppercase()) {
        Cow::Owned(name.to_ascii_lowercase())
    } else {
        Cow::Borrowed(name)
    }
}

#[cfg(test)]
#[path = "icon_tests.rs"]
mod tests;
