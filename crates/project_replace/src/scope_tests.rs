use std::path::Path;

use super::*;

fn scope_of(include: &str, exclude: &str) -> Scope {
    Scope::parse(&ScopeGlobs {
        include: include.to_string(),
        exclude: exclude.to_string(),
    })
    .unwrap()
}

fn includes(scope: &Scope, relative: &str) -> bool {
    let root = Path::new("/project");
    scope.includes(root, &root.join(relative))
}

#[test]
fn an_empty_scope_covers_everything() {
    let scope = scope_of("", "  ");
    assert!(scope.is_unrestricted());
    assert!(includes(&scope, "src/main.rs"));
    assert!(Scope::unrestricted().is_unrestricted());
    assert!(ScopeGlobs::default().is_empty());
}

#[test]
fn include_patterns_with_a_slash_are_relative_to_the_root() {
    let scope = scope_of("src/**/*.ts", "");
    assert!(!scope.is_unrestricted());
    assert!(includes(&scope, "src/a.ts"));
    assert!(includes(&scope, "src/deep/er/b.ts"));
    assert!(!includes(&scope, "src/a.js"));
    assert!(!includes(&scope, "lib/src/a.ts"));
    assert!(!includes(&scope, "a.ts"));
}

#[test]
fn include_patterns_without_a_slash_match_at_any_depth() {
    let scope = scope_of("*.rs", "");
    assert!(includes(&scope, "main.rs"));
    assert!(includes(&scope, "src/deep/lib.rs"));
    assert!(!includes(&scope, "src/main.c"));
}

#[test]
fn a_folder_stands_for_everything_below_it() {
    let by_name = scope_of("docs", "");
    assert!(includes(&by_name, "docs/guide.md"));
    assert!(includes(&by_name, "packages/app/docs/api/index.md"));
    assert!(!includes(&by_name, "src/docs.rs"));

    let anchored = scope_of("./docs", "");
    assert!(includes(&anchored, "docs/guide.md"));
    assert!(!includes(&anchored, "packages/app/docs/api/index.md"));

    let nested = scope_of("packages/app", "");
    assert!(includes(&nested, "packages/app/src/index.ts"));
    assert!(!includes(&nested, "packages/web/src/index.ts"));
}

#[test]
fn exclude_patterns_win_over_include_patterns() {
    let scope = scope_of("src/**/*.ts", "**/dist/**, *.d.ts");
    assert!(includes(&scope, "src/a.ts"));
    assert!(!includes(&scope, "src/dist/a.ts"));
    assert!(!includes(&scope, "src/types.d.ts"));

    let only_exclude = scope_of("", "node_modules");
    assert!(includes(&only_exclude, "src/a.ts"));
    assert!(!includes(&only_exclude, "node_modules/pkg/index.js"));
    assert!(!includes(
        &only_exclude,
        "packages/app/node_modules/pkg/index.js"
    ));
}

#[test]
fn an_exclamation_mark_excludes_in_either_field() {
    let scope = scope_of("src/**/*.ts, !**/dist/**", "!*.spec.ts");
    assert!(includes(&scope, "src/a.ts"));
    assert!(!includes(&scope, "src/dist/a.ts"));
    assert!(!includes(&scope, "src/a.spec.ts"));
}

#[test]
fn several_patterns_are_separated_by_commas() {
    let scope = scope_of(" *.rs ,*.toml,, ", "");
    assert!(includes(&scope, "src/main.rs"));
    assert!(includes(&scope, "Cargo.toml"));
    assert!(!includes(&scope, "README.md"));
}

#[test]
fn an_absolute_path_stands_for_that_file_or_folder() {
    let scope = scope_of("/project/src/ui", "/project/src/ui/generated");
    assert!(includes(&scope, "src/ui/button.rs"));
    assert!(!includes(&scope, "src/ui/generated/icons.rs"));
    assert!(!includes(&scope, "src/uikit/button.rs"));
    assert!(!includes(&scope, "src/core/mod.rs"));

    // The same folder is out of scope when searched from another root.
    let other_root = Path::new("/other");
    assert!(!scope.includes(other_root, &other_root.join("src/ui/button.rs")));
}

#[test]
fn files_outside_the_root_only_match_absolute_paths() {
    let globbed = scope_of("*.rs", "");
    assert!(!globbed.includes(Path::new("/project"), Path::new("/elsewhere/main.rs")));
    let unrestricted = scope_of("", "*.md");
    assert!(unrestricted.includes(Path::new("/project"), Path::new("/elsewhere/main.rs")));
}

#[test]
fn patterns_starting_with_a_comment_character_are_taken_literally() {
    let scope = scope_of("#notes", "");
    assert!(includes(&scope, "#notes/today.md"));
    assert!(!includes(&scope, "notes/today.md"));
}

#[test]
fn an_invalid_pattern_is_reported() {
    let error = Scope::parse(&ScopeGlobs {
        include: "src/[z-a]".to_string(),
        exclude: String::new(),
    })
    .err()
    .expect("the pattern is invalid");
    assert_eq!(error.pattern, "src/[z-a]");
}

#[test]
fn a_backslash_makes_a_character_stand_for_itself() {
    let scope = scope_of(r"src/\[id\], a\,b.txt", "");
    assert!(includes(&scope, "src/[id]/page.tsx"));
    assert!(!includes(&scope, "src/i/page.tsx"));
    assert!(includes(&scope, "docs/a,b.txt"));
    assert!(!includes(&scope, "docs/a"));
    assert!(!includes(&scope, "b.txt"));
}

#[test]
fn a_folder_pattern_limits_the_search_to_that_folder() {
    let root = Path::new("/project");
    let folder = root.join("app/[id]/a,b {x}");
    let pattern = folder_pattern(Some(root), &folder);
    assert_eq!(pattern, r"./app/\[id\]/a\,b \{x\}");

    let scope = scope_of(&pattern, "");
    assert!(includes(&scope, "app/[id]/a,b {x}/page.tsx"));
    assert!(includes(&scope, "app/[id]/a,b {x}/deep/er/page.tsx"));
    assert!(!includes(&scope, "app/i/a,b {x}/page.tsx"));
    assert!(!includes(&scope, "lib/app/[id]/a,b {x}/page.tsx"));
    assert!(!includes(&scope, "app/page.tsx"));

    assert_eq!(folder_pattern(Some(root), &root.join("src")), "./src");
}

#[test]
fn a_folder_outside_the_root_or_the_root_itself_is_named_by_its_path() {
    let root = Path::new("/project");
    assert_eq!(folder_pattern(Some(root), root), "/project");
    assert_eq!(
        folder_pattern(Some(root), Path::new("/other/[x]")),
        r"/other/\[x\]"
    );
    assert_eq!(folder_pattern(None, Path::new("/other/src")), "/other/src");

    let scope = scope_of(&folder_pattern(None, Path::new("/other/[x]")), "");
    let other = Path::new("/other");
    assert!(scope.includes(other, &other.join("[x]/main.rs")));
    assert!(!scope.includes(other, &other.join("x/main.rs")));

    // Searching two roots while limited to one of them leaves the other out.
    let scope = scope_of(&folder_pattern(Some(root), root), "");
    assert!(scope.includes(root, &root.join("src/main.rs")));
    assert!(!scope.includes(other, &other.join("src/main.rs")));
}
