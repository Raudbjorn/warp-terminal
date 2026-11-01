use std::fs;
use std::path::Path;

use grep::regex::RegexMatcher;
use regex::RegexBuilder;
use serde_json::Value;

use super::*;
use crate::encoding::decode;

fn literal(pattern: &str) -> SearchSpec {
    SearchSpec {
        pattern: pattern.to_string(),
        case_sensitive: true,
        ..Default::default()
    }
}

fn regex(pattern: &str) -> SearchSpec {
    SearchSpec {
        use_regex: true,
        ..literal(pattern)
    }
}

fn replace_with(replacement: &str) -> ReplaceSpec {
    ReplaceSpec {
        replacement: replacement.to_string(),
        preserve_case: false,
    }
}

/// A search result as the results list keys it: line, column and matched text.
type Row = (u32, usize, String);

/// What the search worker reports for the file at `path`, as the rows the results list makes of
/// it.
fn worker_rows_at(matcher: &RegexMatcher, multiline: bool, path: &Path) -> Vec<Row> {
    let mut output = Vec::new();
    // A failed search still leaves what was found before the failure, as it does in the worker.
    let _ = search_path_json(matcher, &mut build_searcher(multiline), path, &mut output);

    let mut rows = Vec::new();
    for line in output
        .split(|byte| *byte == b'\n')
        .filter(|l| !l.is_empty())
    {
        let message: Value = serde_json::from_slice(line).unwrap();
        if message["type"] != "match" {
            continue;
        }
        let data = &message["data"];
        let Some(lines) = data["lines"]["text"].as_str() else {
            continue;
        };
        let line_number = data["line_number"].as_u64().unwrap() as u32;
        for submatch in data["submatches"].as_array().unwrap() {
            let start = submatch["start"].as_u64().unwrap() as usize;
            let end = submatch["end"].as_u64().unwrap() as usize;
            // The results list has no column for a match that starts inside a character, and
            // cannot show one that ends inside one.
            if let (Some(column), Some(matched)) = (column_of(lines, start), lines.get(start..end))
            {
                rows.push((line_number, column, matched.to_string()));
            }
        }
    }
    rows
}

fn worker_matcher(spec: &SearchSpec) -> RegexMatcher {
    build_matcher(
        &[spec.effective_pattern()],
        spec.ignore_case(),
        spec.is_multiline(),
    )
    .expect("pattern compiles")
}

fn worker_rows(spec: &SearchSpec, content: &[u8]) -> Vec<Row> {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("file.txt");
    fs::write(&path, content).unwrap();
    worker_rows_at(&worker_matcher(spec), spec.is_multiline(), &path)
}

fn rows_of(found: Vec<FoundMatch>) -> Vec<Row> {
    found
        .into_iter()
        .map(|found| (found.line, found.column, found.matched))
        .collect()
}

fn finder_rows(spec: &SearchSpec, content: &[u8]) -> Vec<Row> {
    let text = decode(content).unwrap().text;
    rows_of(MatchFinder::new(spec, None).unwrap().find(&text).unwrap())
}

const CORPUS: &[&str] = &[
    "",
    "plain foo and Foo and FOO\nfoo at the start\nends with foo\n",
    "no trailing newline foo",
    "windows foo\r\nline two foo\r\n\r\nfoo\r\n",
    "mixed foo\nendings foo\r\nhere foo",
    "\tindented foo\n    spaces foo  \n",
    "unicode: straße STRASSE ſ K \u{212a} k é É ǅ ǆ foo\n",
    "word edges: foo_bar foo-bar foobar barfoo éfoo fooé (foo) foo.\n",
    "emoji 🦀 foo 🦀🦀 foo\n日本語 foo テスト\n",
    "a\n\nb\n\n\nfoo\n",
    "foo foo foo\nfoofoo\n",
    "first foo\nsecond line\nthird foo\nfoo\nbar\nfoo\nbar\n",
    "dollars $1 ${name} $$ foo\n",
    "dots a.b a+b a*b [x] (y) {z} ^ $ | \\ foo\n",
];

const PATTERNS: &[&str] = &[
    "foo",
    "FOO",
    "foo$",
    "^foo",
    "^$",
    r"\bfoo\b",
    r"\Bfoo",
    r"foo\w*",
    r"\w+",
    r"\s+",
    r"\s+$",
    r"(?i)straSSe",
    r"k",
    r"é",
    r"[[:upper:]]+",
    r"\p{Lu}\p{Ll}*",
    r"f(o+)",
    r"(?P<first>\w)(?P<rest>\w*)",
    r"x*",
    r"foo|bar",
    r"a.b",
    r".+",
    r"\$\d",
    r"🦀+",
    r"(?-u:\w)+",
    r"(?-u:[\xC3o])",
];

const LITERALS: &[&str] = &[
    "foo", "a.b", "$1", "[x]", "(y)", "^", "\\", "🦀", "é", "ſ", "  ",
];

/// Checks, for every text of the corpus, that replacing finds exactly the rows the search worker
/// reports for `pattern`, with each combination of the case and whole word options.
fn assert_agreement_on_corpus(pattern: &str, use_regex: bool) {
    let directory = tempfile::tempdir().unwrap();
    let paths: Vec<_> = CORPUS
        .iter()
        .enumerate()
        .map(|(index, content)| {
            let path = directory.path().join(format!("{index}.txt"));
            fs::write(&path, content).unwrap();
            path
        })
        .collect();

    for case_sensitive in [true, false] {
        for whole_word in [false, true] {
            let spec = SearchSpec {
                pattern: pattern.to_string(),
                use_regex,
                case_sensitive,
                whole_word,
            };
            let finder = MatchFinder::new(&spec, None).unwrap();
            let matcher = worker_matcher(&spec);
            for (content, path) in CORPUS.iter().zip(&paths) {
                assert_eq!(
                    rows_of(finder.find(content).unwrap()),
                    worker_rows_at(&matcher, spec.is_multiline(), path),
                    "pattern {pattern:?} (regex {use_regex}, case sensitive {case_sensitive}, \
                     whole word {whole_word}) in {content:?}"
                );
            }
        }
    }
}

#[test]
fn replacing_finds_what_the_search_worker_reports() {
    for pattern in PATTERNS {
        assert_agreement_on_corpus(pattern, true);
    }
}

#[test]
fn replacing_finds_what_the_search_worker_reports_for_literals() {
    for pattern in LITERALS {
        assert_agreement_on_corpus(pattern, false);
    }
}

#[test]
fn replacing_finds_what_the_search_worker_reports_across_lines() {
    let contents = [
        "first foo\nsecond line\nthird foo\nfoo\nbar\nfoo\nbar\n",
        "foo\nbar",
        "foo\r\nbar\r\nfoo\nbar\n",
        "foo\nbar foo\nbar\n",
    ];
    let specs = [
        literal("foo\nbar"),
        literal("line\nthird"),
        regex("foo\n(\\w+)"),
        regex("o\n+b"),
        regex("(?s)foo.*?bar\n"),
    ];
    for content in contents {
        for spec in &specs {
            assert!(spec.is_multiline());
            assert_eq!(
                finder_rows(spec, content.as_bytes()),
                worker_rows(spec, content.as_bytes()),
                "pattern {:?} in {content:?}",
                spec.pattern
            );
        }
    }
}

#[test]
fn replacing_finds_what_the_search_worker_reports_in_large_files() {
    // Larger than the buffer the searcher reads a file through, so that matches fall on both
    // sides of where it refills.
    let mut content = String::new();
    for index in 0..6_000 {
        content.push_str(&format!("line {index} has foo in it and ends with foo\n"));
    }
    for spec in [regex("^line"), regex("foo$"), regex(r"\d+"), literal("foo")] {
        let rows = finder_rows(&spec, content.as_bytes());
        assert!(rows.len() >= 6_000, "pattern {:?}", spec.pattern);
        assert_eq!(
            rows,
            worker_rows(&spec, content.as_bytes()),
            "pattern {:?}",
            spec.pattern
        );
    }
}

#[test]
fn replacing_finds_what_the_search_worker_reports_behind_a_byte_order_mark() {
    let text = "foo first\nsecond foo\n";
    let utf8 = [&[0xEF, 0xBB, 0xBF][..], text.as_bytes()].concat();
    let mut utf16_le = vec![0xFF, 0xFE];
    utf16_le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
    let mut utf16_be = vec![0xFE, 0xFF];
    utf16_be.extend(text.encode_utf16().flat_map(u16::to_be_bytes));

    for content in [utf8, utf16_le, utf16_be] {
        let spec = regex("^foo|foo$");
        let rows = finder_rows(&spec, &content);
        assert_eq!(
            rows,
            vec![(1, 1, "foo".to_string()), (2, 8, "foo".to_string())]
        );
        assert_eq!(rows, worker_rows(&spec, &content));
    }
}

/// For patterns that cannot match a line break, searching line by line has to agree with running
/// the same expression over the whole text.
#[test]
fn matches_agree_with_a_plain_regex_search() {
    let patterns = [
        "foo",
        "foo$",
        "^foo",
        r"\bfoo\b",
        r"foo\w*",
        r"(?i)straSSe",
        r"k",
        r"f(o+)",
        r"\p{Lu}\p{Ll}*",
        r"🦀+",
    ];
    for content in CORPUS {
        // A plain search counts `\r` as part of the line, which is exactly where the two differ.
        if content.contains('\r') {
            continue;
        }
        for pattern in patterns {
            for case_sensitive in [true, false] {
                let spec = SearchSpec {
                    pattern: pattern.to_string(),
                    use_regex: true,
                    case_sensitive,
                    whole_word: false,
                };
                let expected: Vec<_> = RegexBuilder::new(pattern)
                    .case_insensitive(!case_sensitive)
                    .multi_line(true)
                    .build()
                    .unwrap()
                    .find_iter(content)
                    .map(|found| found.range())
                    .collect();
                let found: Vec<_> = MatchFinder::new(&spec, None)
                    .unwrap()
                    .find(content)
                    .unwrap()
                    .into_iter()
                    .map(|found| found.range)
                    .collect();
                assert_eq!(
                    found, expected,
                    "pattern {pattern:?} (case sensitive {case_sensitive}) in {content:?}"
                );
            }
        }
    }
}

#[test]
fn case_insensitive_search_folds_unicode() {
    let spec = SearchSpec {
        pattern: "STRASSE k".to_string(),
        ..Default::default()
    };
    let found = MatchFinder::new(&spec, None)
        .unwrap()
        .find("strasse \u{212a}\nStrasse K\n")
        .unwrap();
    let matched: Vec<_> = found.iter().map(|found| found.matched.as_str()).collect();
    assert_eq!(matched, ["strasse \u{212a}", "Strasse K"]);
}

#[test]
fn whole_word_respects_unicode_word_characters() {
    let spec = SearchSpec {
        whole_word: true,
        ..literal("foo")
    };
    let found = MatchFinder::new(&spec, None)
        .unwrap()
        .find("foo éfoo fooé foo_bar foo-bar (foo) foo")
        .unwrap();
    let columns: Vec<_> = found.iter().map(|found| found.column).collect();
    assert_eq!(columns, [1, 23, 32, 37]);
}

#[test]
fn whole_word_wraps_alternations_as_a_whole() {
    let spec = SearchSpec {
        whole_word: true,
        ..regex("foo|bar")
    };
    let found = MatchFinder::new(&spec, None)
        .unwrap()
        .find("foobar foo bar barfoo")
        .unwrap();
    let matched: Vec<_> = found.iter().map(|found| found.matched.as_str()).collect();
    assert_eq!(matched, ["foo", "bar"]);
}

#[test]
fn look_around_is_rejected_rather_than_misread() {
    for pattern in ["foo(?=bar)", "(?<=foo)bar", "(?!foo)bar", r"(foo)\1"] {
        let error = MatchFinder::new(&regex(pattern), None).unwrap_err();
        assert!(
            matches!(error, FindError::InvalidPattern(_)),
            "pattern {pattern:?} gave {error:?}"
        );
        assert!(
            build_matcher(&[pattern.to_string()], false, false).is_err(),
            "the search worker accepts {pattern:?}"
        );
    }
}

#[test]
fn anchors_match_on_every_line() {
    let found = MatchFinder::new(&regex("^foo$"), None)
        .unwrap()
        .find("foo\nfoo bar\nfoo\n")
        .unwrap();
    let lines: Vec<_> = found.iter().map(|found| found.line).collect();
    assert_eq!(lines, [1, 3]);
}

#[test]
fn ranges_point_at_the_matched_text() {
    let text = "héllo wörld\nwörld héllo\n";
    let found = MatchFinder::new(&literal("wörld"), None)
        .unwrap()
        .find(text)
        .unwrap();
    assert_eq!(found.len(), 2);
    for found in &found {
        assert_eq!(&text[found.range.clone()], "wörld");
    }
    assert_eq!((found[0].line, found[0].column), (1, 7));
    assert_eq!((found[1].line, found[1].column), (2, 1));
}

#[test]
fn replacement_expands_capture_groups_in_regex_mode() {
    let finder = MatchFinder::new(
        &regex(r"(?P<key>\w+)=(\d+)"),
        Some(&replace_with("${2}:${key} $$1 $2!")),
    )
    .unwrap();
    let found = finder.find("width=10 height=20\n").unwrap();
    let replacements: Vec<_> = found
        .iter()
        .map(|found| found.replacement.as_str())
        .collect();
    assert_eq!(replacements, ["10:width $1 10!", "20:height $1 20!"]);
}

#[test]
fn replacement_is_taken_literally_outside_regex_mode() {
    let finder = MatchFinder::new(&literal("a.b"), Some(&replace_with("$1 ${x} $$ \\n"))).unwrap();
    let found = finder.find("a.b axb\n").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].replacement, "$1 ${x} $$ \\n");
}

#[test]
fn replacement_expands_groups_that_need_the_rest_of_the_line() {
    let finder = MatchFinder::new(&regex(r"\b(\w+)$"), Some(&replace_with("<$1>"))).unwrap();
    let found = finder.find("one two\nthree four\n").unwrap();
    let replacements: Vec<_> = found
        .iter()
        .map(|found| found.replacement.as_str())
        .collect();
    assert_eq!(replacements, ["<two>", "<four>"]);
}

#[test]
fn replacement_can_be_empty() {
    let finder = MatchFinder::new(&literal("foo"), Some(&replace_with(""))).unwrap();
    let found = finder.find("foo bar foo\n").unwrap();
    assert_eq!(found.len(), 2);
    assert!(found.iter().all(|found| found.replacement.is_empty()));
}

#[test]
fn replacement_preserves_case_per_match() {
    let spec = SearchSpec {
        pattern: "begin".to_string(),
        ..Default::default()
    };
    let replace = ReplaceSpec {
        replacement: "end".to_string(),
        preserve_case: true,
    };
    let found = MatchFinder::new(&spec, Some(&replace))
        .unwrap()
        .find("begin Begin BEGIN\n")
        .unwrap();
    let replacements: Vec<_> = found
        .iter()
        .map(|found| found.replacement.as_str())
        .collect();
    assert_eq!(replacements, ["end", "End", "END"]);
}

#[test]
fn a_match_splitting_a_character_is_left_out() {
    let text = "é e\n";
    for pattern in [r"(?-u:\xC3)", r"(?-u:\xA9)", r"(?-u:[\xC3e])"] {
        let spec = regex(pattern);
        let found = MatchFinder::new(&spec, None).unwrap().find(text).unwrap();
        let matched: Vec<_> = found.iter().map(|found| found.matched.as_str()).collect();
        let expected: &[&str] = if pattern.contains('e') { &["e"] } else { &[] };
        assert_eq!(matched, expected, "pattern {pattern:?}");
        assert_eq!(
            finder_rows(&spec, text.as_bytes()),
            worker_rows(&spec, text.as_bytes())
        );
    }
}

#[test]
fn a_line_longer_than_the_limit_fails_the_search() {
    let text = format!("foo\n{}\n", "x".repeat(SEARCHER_LINE_HEAP_LIMIT * 2));
    let error = MatchFinder::new(&literal("foo"), None)
        .unwrap()
        .find(&text)
        .unwrap_err();
    assert!(matches!(error, FindError::Search(_)), "{error:?}");
}

#[test]
fn effective_pattern_escapes_literals_and_wraps_whole_words() {
    assert_eq!(literal("a.b").effective_pattern(), r"a\.b");
    assert_eq!(regex("a.b").effective_pattern(), "a.b");
    let whole_word = SearchSpec {
        whole_word: true,
        ..regex("a|b")
    };
    assert_eq!(
        whole_word.effective_pattern(),
        r"\b{start-half}(?:a|b)\b{end-half}"
    );
    assert!(literal("a\nb").is_multiline());
    assert!(!regex(r"a\nb").is_multiline());
}
