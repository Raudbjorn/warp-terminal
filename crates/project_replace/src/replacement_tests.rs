use super::*;

#[test]
fn preserve_case_follows_plain_words() {
    assert_eq!(preserve_case("begin", "End"), "end");
    assert_eq!(preserve_case("Begin", "end"), "End");
    assert_eq!(preserve_case("BEGIN", "end"), "END");
    assert_eq!(preserve_case("bEGIN", "End"), "End");
}

#[test]
fn preserve_case_follows_each_part_of_compound_names() {
    assert_eq!(preserve_case("oneTwoThree", "fourFiveSix"), "fourFiveSix");
    assert_eq!(preserve_case("OneTwoThree", "fourFiveSix"), "FourFiveSix");
    assert_eq!(preserve_case("ONE_TWO", "three_four"), "THREE_FOUR");
    assert_eq!(preserve_case("One-two", "three-four"), "Three-four");
    assert_eq!(preserve_case("HTMLParser", "xmlReader"), "XMLReader");
}

#[test]
fn preserve_case_treats_differently_delimited_text_as_a_whole() {
    assert_eq!(preserve_case("ONE_TWO", "three-four"), "Three-four");
    assert_eq!(preserve_case("One", "two_three"), "Two_three");
}

#[test]
fn preserve_case_keeps_extra_parts_and_empty_text() {
    assert_eq!(preserve_case("ONE_TWO", "a_b_c"), "A_B_c");
    assert_eq!(preserve_case("", "Anything"), "Anything");
    assert_eq!(preserve_case("Word", ""), "");
}

#[test]
fn template_escapes_dollars_outside_regex_mode() {
    let replace = ReplaceSpec {
        replacement: "$1 costs $5".to_string(),
        preserve_case: false,
    };
    let literal = SearchSpec::default();
    let regex = SearchSpec {
        use_regex: true,
        ..Default::default()
    };
    assert_eq!(replace.template(&literal), "$$1 costs $$5");
    assert_eq!(replace.template(&regex), "${1} costs ${5}");
}

#[test]
fn capture_expander_expands_numbered_and_named_groups() {
    let expander = CaptureExpander::new(r"(?P<key>\w+)=(\d+)", true, false).unwrap();
    assert_eq!(
        expander.expand(Some(' '), "width=10", Some(';'), "${2}:$key $$"),
        Some("10:width $".to_string())
    );
}

#[test]
fn a_numbered_group_can_be_followed_by_text() {
    assert_eq!(delimit_numbered_groups("$1_total"), "${1}_total");
    assert_eq!(delimit_numbered_groups("$12x $2"), "${12}x ${2}");
    // Escaped dollars, braced and named references are left as they are.
    assert_eq!(
        delimit_numbered_groups("$$1 ${1}a $name $"),
        "$$1 ${1}a $name $"
    );

    let expander = CaptureExpander::new(r"(\w+)_count", true, false).unwrap();
    assert_eq!(
        expander.expand(Some(' '), "widget_count", Some(' '), "$1_total"),
        Some("widget_total".to_string())
    );

    let spec = ReplaceSpec {
        replacement: "$1_total".to_string(),
        preserve_case: false,
    };
    let regex_search = SearchSpec {
        pattern: r"(\w+)_count".to_string(),
        use_regex: true,
        case_sensitive: true,
        whole_word: false,
    };
    assert_eq!(spec.template(&regex_search), "${1}_total");
}

#[test]
fn capture_expander_sees_the_characters_around_the_match() {
    let boundary = CaptureExpander::new(r"\b(\w+)\b", true, false).unwrap();
    assert_eq!(
        boundary.expand(Some(' '), "word", Some('.'), "<$1>"),
        Some("<word>".to_string())
    );
    // Without the characters around it, "ord" would look like a word of its own.
    assert_eq!(boundary.expand(Some('w'), "ord", Some('.'), "<$1>"), None);

    let not_boundary = CaptureExpander::new(r"\B(or)", true, false).unwrap();
    assert_eq!(
        not_boundary.expand(Some('w'), "or", Some('d'), "[$1]"),
        Some("[or]".to_string())
    );

    let line = CaptureExpander::new(r"^(\w+)$", true, false).unwrap();
    assert_eq!(
        line.expand(Some('\n'), "word", Some('\n'), "$1!"),
        Some("word!".to_string())
    );
    assert_eq!(
        line.expand(None, "word", None, "$1!"),
        Some("word!".to_string())
    );
    assert_eq!(line.expand(Some(' '), "word", None, "$1!"), None);
}

#[test]
fn capture_expander_handles_wide_characters_and_case() {
    let expander = CaptureExpander::new(r"(é+)(\w)", false, false).unwrap();
    assert_eq!(
        expander.expand(Some('🦀'), "ÉéX", Some('🦀'), "$2$1"),
        Some("XÉé".to_string())
    );
    assert!(CaptureExpander::new("(unclosed", true, false).is_none());
}

#[test]
fn in_editor_pattern_wraps_whole_words_with_ascii_boundaries() {
    assert_eq!(in_editor_pattern("a.b", false, false), r"a\.b");
    assert_eq!(in_editor_pattern("a.b", true, false), "a.b");

    let pattern = in_editor_pattern("foo|bar", true, true);
    let regex = Regex::new(&pattern).unwrap();
    let matched: Vec<_> = regex
        .find_iter("foobar foo bar barfoo foo_ (bar)")
        .map(|found| found.range())
        .collect();
    assert_eq!(matched, [7..10, 11..14, 28..31]);

    let expander = CaptureExpander::new("(fo)o", true, true).unwrap();
    assert_eq!(
        expander.expand(Some(' '), "foo", Some(' '), "${1}x"),
        Some("fox".to_string())
    );
    assert_eq!(expander.expand(Some('a'), "foo", Some(' '), "${1}x"), None);
}
