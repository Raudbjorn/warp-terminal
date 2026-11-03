use warpui_core::color::ColorU;

use super::{ColorMap, convert_capture_name_to_color};

const KEYWORD: ColorU = ColorU {
    r: 1,
    g: 0,
    b: 0,
    a: 255,
};
const FUNCTION: ColorU = ColorU {
    r: 2,
    g: 0,
    b: 0,
    a: 255,
};
const STRING: ColorU = ColorU {
    r: 3,
    g: 0,
    b: 0,
    a: 255,
};
const TYPE: ColorU = ColorU {
    r: 4,
    g: 0,
    b: 0,
    a: 255,
};
const NUMBER: ColorU = ColorU {
    r: 5,
    g: 0,
    b: 0,
    a: 255,
};
const COMMENT: ColorU = ColorU {
    r: 6,
    g: 0,
    b: 0,
    a: 255,
};
const PROPERTY: ColorU = ColorU {
    r: 7,
    g: 0,
    b: 0,
    a: 255,
};
const TAG: ColorU = ColorU {
    r: 8,
    g: 0,
    b: 0,
    a: 255,
};

fn color_map() -> ColorMap {
    ColorMap {
        keyword_color: KEYWORD,
        function_color: FUNCTION,
        string_color: STRING,
        type_color: TYPE,
        number_color: NUMBER,
        comment_color: COMMENT,
        property_color: PROPERTY,
        tag_color: TAG,
    }
}

#[test]
fn capture_names_resolve_by_their_first_segment() {
    let cases = [
        ("keyword", KEYWORD),
        ("keyword.control.repeat", KEYWORD),
        ("function.call", FUNCTION),
        ("string.special", STRING),
        ("type.builtin", TYPE),
        ("number", NUMBER),
        ("comment", COMMENT),
        ("property", PROPERTY),
        ("tag", TAG),
        ("punctuation.bracket", COMMENT),
    ];
    for (name, expected) in cases {
        assert_eq!(
            convert_capture_name_to_color(name, &color_map()),
            Some(expected),
            "{name}",
        );
    }
}

#[test]
fn older_and_markup_capture_names_are_colored() {
    let cases = [
        ("conditional", KEYWORD),
        ("repeat", KEYWORD),
        ("include", KEYWORD),
        ("exception", KEYWORD),
        ("storageclass", KEYWORD),
        ("boolean", NUMBER),
        ("float", NUMBER),
        ("constant.numeric", NUMBER),
        ("method.call", FUNCTION),
        ("field", PROPERTY),
        ("attribute", PROPERTY),
        ("markup.heading.2", KEYWORD),
        ("markup.raw.block", STRING),
        ("text.title", KEYWORD),
        ("text.literal", STRING),
        ("text.uri", FUNCTION),
        ("text.reference", PROPERTY),
    ];
    for (name, expected) in cases {
        assert_eq!(
            convert_capture_name_to_color(name, &color_map()),
            Some(expected),
            "{name}",
        );
    }
}

#[test]
fn unclassified_capture_names_are_left_uncolored() {
    for name in [
        "variable",
        "operator",
        "constant",
        "constant.builtin",
        "markup.bold",
        "none",
    ] {
        assert_eq!(
            convert_capture_name_to_color(name, &color_map()),
            None,
            "{name}"
        );
    }
}
