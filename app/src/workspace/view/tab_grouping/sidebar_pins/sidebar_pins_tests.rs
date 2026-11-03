use super::*;

#[test]
fn any_value_pins_and_only_unset_does_not() {
    assert_eq!(parse(""), None);
    assert_eq!(parse("1761134400"), Some(1761134400));
    assert_eq!(parse(" 1761134400\n"), Some(1761134400));
    assert_eq!(parse("yes"), Some(0));
    assert_eq!(parse("-3"), Some(0));
}

#[test]
fn the_last_pinned_comes_first_and_ties_go_by_name() {
    let pins = [(0, 100, "b"), (1, 300, "z"), (2, 100, "a"), (3, 0, "c")];
    assert_eq!(order(pins), vec![1, 2, 0, 3]);
}
