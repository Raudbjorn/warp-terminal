use super::*;

fn edit(range: Range<usize>, new_text: &str) -> TextEdit {
    TextEdit {
        range,
        new_text: new_text.to_owned(),
    }
}

/// Checks that the edits reproduce `new`, are ordered and do not overlap, then returns them.
fn edits(old: &str, new: &str) -> Vec<TextEdit> {
    let edits = minimal_edits(old, new);
    assert_eq!(apply_edits(old, &edits), new, "{old:?} -> {new:?}");
    for pair in edits.windows(2) {
        assert!(pair[0].range.end <= pair[1].range.start, "{edits:?}");
    }
    edits
}

#[test]
fn already_formatted_text_needs_no_edits() {
    assert_eq!(edits("const a = 1;\n", "const a = 1;\n"), []);
    assert_eq!(edits("", ""), []);
}

#[test]
fn a_changed_line_is_narrowed_to_the_differing_characters() {
    assert_eq!(edits("const a=1;\n", "const a = 1;\n"), [edit(7..8, " = ")]);
}

#[test]
fn untouched_lines_between_changes_are_not_part_of_any_edit() {
    let old = "a  =  1\nkeep one\nkeep two\nb  =  2\n";
    let new = "a = 1\nkeep one\nkeep two\nb = 2\n";
    assert_eq!(edits(old, new), [edit(2..5, "="), edit(28..31, "=")]);
}

#[test]
fn adjacent_changed_lines_form_one_edit() {
    let old = "keep\nx=1\ny=2\nkeep\n";
    let new = "keep\nx = 1\ny = 2\nkeep\n";
    assert_eq!(edits(old, new), [edit(6..11, " = 1\ny = ")]);
}

#[test]
fn offsets_count_characters_not_bytes() {
    let old = "// héllo wörld 👋\nlet  x;\n";
    let new = "// héllo wörld 👋\nlet x;\n";
    assert_eq!(edits(old, new), [edit(21..22, "")]);
}

#[test]
fn a_missing_final_newline_is_one_insertion() {
    assert_eq!(edits("a\nb", "a\nb\n"), [edit(3..3, "\n")]);
}

#[test]
fn extra_trailing_blank_lines_are_one_deletion() {
    assert_eq!(edits("a\n\n\n", "a\n"), [edit(2..4, "")]);
}

#[test]
fn inserted_and_removed_lines() {
    assert_eq!(edits("a\nc\n", "a\nb\nc\n"), [edit(2..2, "b\n")]);
    assert_eq!(edits("a\nb\nc\n", "a\nc\n"), [edit(2..4, "")]);
}

#[test]
fn empty_and_fully_rewritten_texts() {
    assert_eq!(edits("", "new\n"), [edit(0..0, "new\n")]);
    assert_eq!(edits("old\n", ""), [edit(0..4, "")]);
    assert_eq!(edits("abc\n", "xyz\n"), [edit(0..3, "xyz")]);
}

#[test]
fn reindenting_every_line_round_trips() {
    let old = "fn main() {\n\tlet a = 1;\n\tif a > 0 {\n\t\tprintln!(\"{a}\");\n\t}\n}\n";
    let new = old.replace('\t', "    ");
    edits(old, &new);
}

#[test]
fn repeated_lines_round_trip() {
    edits("a\na\na\nb\na\n", "a\nb\na\na\n");
    edits("x\n\n\n\ny\n", "x\n\ny\n");
    edits("{\n}\n{\n}\n", "{}\n{}\n");
}
