use std::collections::HashSet;

use warp_core::ui::theme::{Details, Fill, WarpTheme, mock_terminal_colors};
use warpui::AssetProvider;
use warpui::assets::asset_cache::Asset;
use warpui::color::ColorU;
use warpui::image_cache::ImageType;

use super::mappings::{
    DEFAULT_FILE, DEFAULT_FOLDER, EXTENSIONS, FILE_NAMES, FOLDER_NAMES, NAME_PREFIXES,
    NESTED_FOLDERS,
};
use super::{FileIcon, FolderIcon, file_icon, folder_icon};
use crate::ASSETS;

const ICON_DIR: &str = "bundled/svg/material_icon_theme/";

fn file_icon_name(path: &str) -> Option<&'static str> {
    file_icon(path).map(|icon| icon_name(icon.path))
}

fn folder_icon_name(path: &str, is_open: bool) -> &'static str {
    icon_name(folder_icon(path).path(is_open))
}

fn icon_name(asset_path: &'static str) -> &'static str {
    asset_path
        .strip_prefix(ICON_DIR)
        .and_then(|name| name.strip_suffix(".svg"))
        .unwrap_or(asset_path)
}

/// Builds a theme with solid colors given as `0xRRGGBBAA`.
fn theme_with(background: u32, foreground: u32) -> WarpTheme {
    WarpTheme::new(
        Fill::Solid(ColorU::from_u32(background)),
        ColorU::from_u32(foreground),
        Fill::Solid(ColorU::from_u32(0x2AA198FF)),
        None,
        Some(Details::Darker),
        mock_terminal_colors(),
        None,
        None,
    )
}

fn all_file_icons() -> impl Iterator<Item = FileIcon> {
    FILE_NAMES
        .iter()
        .chain(EXTENSIONS)
        .map(|(_, icon)| *icon)
        .chain(NAME_PREFIXES.iter().map(|rule| rule.icon))
}

fn all_folder_icons() -> impl Iterator<Item = FolderIcon> {
    FOLDER_NAMES
        .iter()
        .map(|(_, icon)| *icon)
        .chain(NESTED_FOLDERS.iter().map(|nested| nested.icon))
        .chain([DEFAULT_FOLDER])
}

fn all_asset_paths() -> HashSet<&'static str> {
    all_file_icons()
        .flat_map(|icon| [Some(icon.path), icon.light_path])
        .flatten()
        .chain(all_folder_icons().flat_map(|icon| [icon.closed, icon.open]))
        .chain([DEFAULT_FILE])
        .collect()
}

#[test]
fn simple_extensions_resolve() {
    for (path, expected) in [
        ("main.rs", "rust"),
        ("index.ts", "typescript"),
        ("App.tsx", "react_ts"),
        ("index.js", "javascript"),
        ("server.mjs", "javascript"),
        ("App.jsx", "react"),
        ("data.json", "json"),
        ("notes.md", "markdown"),
        ("page.mdx", "mdx"),
        ("script.py", "python"),
        ("schema.sql", "database"),
        ("index.php", "php"),
        ("style.scss", "sass"),
        ("config.yml", "yaml"),
        ("config.yaml", "yaml"),
        ("main.tf", "terraform"),
        ("prod.tfvars", "terraform"),
        ("deploy.sh", "console"),
        ("photo.webp", "image"),
        ("logo.svg", "svg"),
        ("font.woff2", "font"),
        ("clip.mp4", "video"),
        ("bundle.tar.gz", "zip"),
        ("report.docx", "word"),
        ("sheet.xlsx", "table"),
        ("paper.typ", "typst"),
        ("login.feature", "cucumber"),
        ("analysis.ipynb", "jupyter"),
        ("Cargo.lock", "lock"),
    ] {
        assert_eq!(file_icon_name(path), Some(expected), "{path}");
    }
}

#[test]
fn compound_extensions_beat_their_final_segment() {
    assert_eq!(file_icon_name("globals.d.ts"), Some("typescript-def"));
    assert_eq!(file_icon_name("welcome.blade.php"), Some("laravel"));
    assert_eq!(file_icon_name("button.test.tsx"), Some("test-jsx"));
    assert_eq!(file_icon_name("api.spec.ts"), Some("test-ts"));
    assert_eq!(file_icon_name("bundle.js.map"), Some("javascript-map"));
    assert_eq!(file_icon_name("main.tf.json"), Some("terraform"));
}

#[test]
fn unrelated_inner_dots_fall_back_to_the_final_extension() {
    assert_eq!(file_icon_name("user.service.ts"), Some("typescript"));
    assert_eq!(file_icon_name("jquery-3.7.1.min.js"), Some("javascript"));
}

#[test]
fn well_known_names_beat_extensions() {
    for (path, expected) in [
        ("package.json", "nodejs"),
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("README.md", "readme"),
        ("requirements.txt", "python-misc"),
        ("favicon.ico", "favicon"),
        ("vercel.json", "vercel"),
        ("CLAUDE.md", "claude"),
        ("AGENTS.md", "agent"),
        (".gitignore", "git"),
        (".gitattributes", "git"),
        (".npmrc", "npm"),
        (".nvmrc", "nodejs"),
        (".dockerignore", "docker"),
    ] {
        assert_eq!(file_icon_name(path), Some(expected), "{path}");
    }
}

#[test]
fn well_known_name_prefixes_cover_their_variants() {
    for (path, expected) in [
        ("Dockerfile", "docker"),
        ("Dockerfile.dev", "docker"),
        ("docker-compose.override.yml", "docker"),
        ("compose.yaml", "docker"),
        (".env", "tune"),
        (".env.local", "tune"),
        (".env.production.example", "tune"),
        ("tsconfig.json", "tsconfig"),
        ("tsconfig.app.json", "tsconfig"),
        ("Makefile", "makefile"),
        ("LICENSE", "license"),
        ("LICENSE-MIT", "license"),
        ("LICENSE.txt", "license"),
        ("README", "readme"),
        ("README-tr.md", "readme"),
        ("vite.config.ts", "vite"),
        ("next.config.mjs", "next"),
        ("tailwind.config.js", "tailwindcss"),
        (".eslintrc.json", "eslint"),
        ("wrangler.toml", "wrangler"),
    ] {
        assert_eq!(file_icon_name(path), Some(expected), "{path}");
    }
}

#[test]
fn name_prefixes_require_a_word_boundary_and_matching_extension() {
    assert_eq!(file_icon_name("composer.json"), Some("json"));
    assert_eq!(file_icon_name("license_checker.py"), Some("python"));
    assert_eq!(file_icon_name("readme_generator.ts"), Some("typescript"));
    assert_eq!(file_icon_name("vite-env.d.ts"), Some("typescript-def"));
    assert_eq!(file_icon_name("tsconfig.ts"), Some("typescript"));
    assert_eq!(file_icon_name(".environment"), None);
}

#[test]
fn file_lookup_is_case_insensitive() {
    assert_eq!(file_icon_name("MAIN.RS"), Some("rust"));
    assert_eq!(file_icon_name("Readme.MD"), Some("readme"));
    assert_eq!(file_icon_name("PACKAGE.JSON"), Some("nodejs"));
    assert_eq!(file_icon_name("DOCKERFILE.Prod"), Some("docker"));
    assert_eq!(file_icon_name("Types.D.TS"), Some("typescript-def"));
}

#[test]
fn file_lookup_only_considers_the_final_path_component() {
    assert_eq!(file_icon_name("/repo/src/index.ts"), Some("typescript"));
    assert_eq!(file_icon_name("C:\\repo\\src\\main.rs"), Some("rust"));
    assert_eq!(file_icon_name("/repo/package.json/notes"), None);
    assert_eq!(file_icon_name("/repo.rs/notes"), None);
}

#[test]
fn unknown_files_have_no_special_icon() {
    assert_eq!(file_icon_name("mystery.xyzzy"), None);
    assert_eq!(file_icon_name("no_extension"), None);
    assert_eq!(file_icon_name("trailing."), None);
    assert_eq!(file_icon_name(""), None);
}

#[test]
fn existing_cython_icon_is_preserved() {
    assert_eq!(
        file_icon("module.pyx").map(|icon| icon.path),
        Some("bundled/svg/file_type/cython.svg")
    );
}

#[test]
fn folders_have_open_and_closed_icons() {
    assert_eq!(folder_icon_name("/repo/src", false), "folder-src");
    assert_eq!(folder_icon_name("/repo/src", true), "folder-src-open");
    assert_eq!(folder_icon_name("/repo/whatever", false), "folder");
    assert_eq!(folder_icon_name("/repo/whatever", true), "folder-open");
}

#[test]
fn well_known_folder_names_resolve() {
    for (path, expected) in [
        ("src", "folder-src"),
        ("test", "folder-test"),
        ("tests", "folder-test"),
        ("node_modules", "folder-node"),
        ("public", "folder-public"),
        ("assets", "folder-resource"),
        ("components", "folder-components"),
        ("docs", "folder-docs"),
        ("scripts", "folder-scripts"),
        ("config", "folder-config"),
        ("dist", "folder-dist"),
        ("build", "folder-dist"),
        ("migrations", "folder-migrations"),
        ("supabase", "folder-supabase"),
        ("terraform", "folder-terraform"),
        ("infra", "folder-server"),
    ] {
        assert_eq!(folder_icon_name(path, false), expected, "{path}");
    }
}

#[test]
fn decorated_folder_names_resolve_like_plain_ones() {
    assert_eq!(folder_icon_name(".git", false), "folder-git");
    assert_eq!(folder_icon_name(".github", false), "folder-github");
    assert_eq!(folder_icon_name(".vscode", false), "folder-vscode");
    assert_eq!(folder_icon_name("__tests__", false), "folder-test");
    assert_eq!(folder_icon_name("__pycache__", false), "folder-python");
    assert_eq!(folder_icon_name("_tests", false), "folder-test");
    assert_eq!(folder_icon_name("@types", false), "folder-typescript");
}

#[test]
fn folder_lookup_is_case_insensitive_and_ignores_trailing_separators() {
    assert_eq!(folder_icon_name("/repo/SRC", false), "folder-src");
    assert_eq!(folder_icon_name("/repo/Docs/", true), "folder-docs-open");
    assert_eq!(folder_icon_name("C:\\repo\\Tests\\", false), "folder-test");
}

#[test]
fn nested_folders_depend_on_their_parent() {
    assert_eq!(
        folder_icon_name("/repo/.github/workflows", false),
        "folder-gh-workflows"
    );
    assert_eq!(
        folder_icon_name("/repo/.GitHub/Workflows", true),
        "folder-gh-workflows-open"
    );
    assert_eq!(folder_icon_name("/repo/src/workflows", false), "folder");
    assert_eq!(folder_icon_name("workflows", false), "folder");
}

#[test]
fn light_variant_is_only_used_on_light_backgrounds() {
    let dark_theme = theme_with(0x000000FF, 0xFFFFFFFF);
    let light_theme = theme_with(0xFFFFFFFF, 0x000000FF);

    let toml = file_icon("Cargo.toml").expect("toml has an icon");
    assert_eq!(icon_name(toml.path_for_theme(&dark_theme)), "toml");
    assert_eq!(icon_name(toml.path_for_theme(&light_theme)), "toml_light");

    let rust = file_icon("main.rs").expect("rust has an icon");
    assert_eq!(icon_name(rust.path_for_theme(&dark_theme)), "rust");
    assert_eq!(icon_name(rust.path_for_theme(&light_theme)), "rust");
}

#[test]
fn mapping_keys_are_lowercase_and_unique() {
    for (table, groups) in [("file names", FILE_NAMES), ("extensions", EXTENSIONS)] {
        let mut seen = HashSet::new();
        for key in groups.iter().flat_map(|(keys, _)| keys.iter()) {
            assert_eq!(*key, key.to_ascii_lowercase(), "{table}: {key}");
            assert!(seen.insert(*key), "{table}: duplicate key {key}");
        }
    }

    let mut seen = HashSet::new();
    for key in FOLDER_NAMES.iter().flat_map(|(keys, _)| keys.iter()) {
        assert_eq!(*key, key.to_ascii_lowercase(), "folders: {key}");
        assert!(seen.insert(*key), "folders: duplicate key {key}");
    }

    for rule in NAME_PREFIXES {
        assert_eq!(rule.prefix, rule.prefix.to_ascii_lowercase());
    }
}

#[test]
fn every_mapped_icon_is_a_bundled_svg() {
    for path in all_asset_paths() {
        let bytes = ASSETS
            .get(path)
            .unwrap_or_else(|err| panic!("{path} is not bundled: {err}"));
        match ImageType::try_from_bytes(&bytes) {
            Ok(ImageType::Svg { .. }) => {}
            Ok(
                ImageType::StaticBitmap { .. }
                | ImageType::AnimatedBitmap { .. }
                | ImageType::Unrecognized,
            ) => panic!("{path} is not an SVG"),
            Err(err) => panic!("{path} failed to parse: {err}"),
        }
    }
}

#[test]
fn every_bundled_material_icon_is_mapped() {
    let mapped = all_asset_paths();
    for path in warp_assets::Assets::iter() {
        let path: &str = &path;
        if path.starts_with(ICON_DIR) && path.ends_with(".svg") {
            assert!(mapped.contains(path), "{path} is bundled but unused");
        }
    }
}
