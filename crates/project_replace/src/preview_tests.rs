use super::*;

fn search(pattern: &str, use_regex: bool) -> SearchSpec {
    SearchSpec {
        pattern: pattern.to_string(),
        use_regex,
        case_sensitive: true,
        whole_word: false,
    }
}

fn replace(replacement: &str, preserve_case: bool) -> ReplaceSpec {
    ReplaceSpec {
        replacement: replacement.to_string(),
        preserve_case,
    }
}

fn highlighted(preview: &RowPreview, range: &Range<usize>) -> String {
    preview
        .text
        .chars()
        .skip(range.start)
        .take(range.len())
        .collect()
}

#[test]
fn a_literal_replacement_is_shown_as_typed() {
    let previewer = Previewer::new(&search("foo", false), &replace("$1 bar", false));
    assert_eq!(previewer.replacement_for("a foo b", 2..5), "$1 bar");
}

#[test]
fn capture_groups_are_expanded_for_a_regex_search() {
    let previewer = Previewer::new(
        &search(r"(?P<key>\w+)=(\d+)", true),
        &replace("$2:${key}", false),
    );
    let line = "width=10 height=20";
    assert_eq!(previewer.replacement_for(line, 0..8), "10:width");
    assert_eq!(previewer.replacement_for(line, 9..18), "20:height");
}

#[test]
fn the_replacement_is_left_unexpanded_when_the_match_is_not_found_again() {
    let previewer = Previewer::new(&search(r"(\w+)", true), &replace("<$1>", false));
    // The range covers only part of what the pattern matches there.
    assert_eq!(previewer.replacement_for("word", 0..2), "<$1>");
    assert_eq!(previewer.replacement_for("word", 9..12), "<$1>");

    let invalid = Previewer::new(&search("(unclosed", true), &replace("<$1>", false));
    assert_eq!(invalid.replacement_for("(unclosed", 0..9), "<$1>");
}

#[test]
fn case_is_preserved_per_match() {
    let spec = SearchSpec {
        case_sensitive: false,
        ..search("begin", false)
    };
    let previewer = Previewer::new(&spec, &replace("end", true));
    let line = "begin Begin BEGIN";
    assert_eq!(previewer.replacement_for(line, 0..5), "end");
    assert_eq!(previewer.replacement_for(line, 6..11), "End");
    assert_eq!(previewer.replacement_for(line, 12..17), "END");
}

#[test]
fn a_row_shows_the_match_then_its_replacement() {
    let preview = preview_row("let foo = 1;", 4..7, "quux");
    assert_eq!(preview.text, "let fooquux = 1;");
    assert_eq!(highlighted(&preview, &preview.removed), "foo");
    assert_eq!(highlighted(&preview, &preview.inserted), "quux");
}

#[test]
fn a_row_with_an_empty_replacement_shows_only_the_removal() {
    let preview = preview_row("let foo = 1;", 4..7, "");
    assert_eq!(preview.text, "let foo = 1;");
    assert_eq!(highlighted(&preview, &preview.removed), "foo");
    assert!(preview.inserted.is_empty());
}

#[test]
fn ranges_count_characters_not_bytes() {
    let preview = preview_row("é🦀 foo é", 7..10, "ñ");
    assert_eq!(preview.text, "é🦀 fooñ é");
    assert_eq!(preview.removed, 3..6);
    assert_eq!(preview.inserted, 6..7);
}

#[test]
fn long_context_is_cut_around_the_match() {
    let before = "b".repeat(40);
    let after = "a".repeat(300);
    let line = format!("{before}foo{after}");
    let preview = preview_row(&line, 40..43, "quux");

    assert!(preview.text.starts_with(&format!("…{}foo", "b".repeat(15))));
    assert!(preview.text.ends_with(&format!("{}…", "a".repeat(120))));
    assert_eq!(highlighted(&preview, &preview.removed), "foo");
    assert_eq!(highlighted(&preview, &preview.inserted), "quux");
    assert_eq!(preview.text.chars().count(), 1 + 15 + 3 + 4 + 120 + 1);
}

#[test]
fn long_changes_are_shortened() {
    let matched = "m".repeat(100);
    let replacement = "r".repeat(100);
    let preview = preview_row(&matched, 0..100, &replacement);
    assert_eq!(
        highlighted(&preview, &preview.removed),
        format!("{}…", "m".repeat(60))
    );
    assert_eq!(
        highlighted(&preview, &preview.inserted),
        format!("{}…", "r".repeat(60))
    );
}

#[test]
fn a_match_across_lines_stays_on_one_row() {
    let lines = "first line\nlet foo\r\nbar = 1;\nlast line\n";
    let start = lines.find("foo").unwrap();
    let end = lines.find(" = 1").unwrap();
    let preview = preview_row(lines, start..end, "a\nb");

    assert_eq!(preview.text, "let foo⏎bara⏎b = 1;");
    assert_eq!(highlighted(&preview, &preview.removed), "foo⏎bar");
    assert_eq!(highlighted(&preview, &preview.inserted), "a⏎b");
}

#[test]
fn a_range_that_does_not_fit_the_line_shows_the_line_as_it_is() {
    let preview = preview_row("short", 3..40, "x");
    assert_eq!(preview.text, "short");
    assert!(preview.removed.is_empty());
    assert!(preview.inserted.is_empty());

    // In the middle of a character.
    let preview = preview_row("é", 1..2, "x");
    assert_eq!(preview.text, "é");
}
