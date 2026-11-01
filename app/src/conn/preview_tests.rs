use super::{head, tail};

#[test]
fn a_tail_cut_at_a_multibyte_space_does_not_panic() {
    // The cut lands four characters in, so the first break in the kept suffix
    // is the three-byte ideographic space rather than an ASCII one.
    let text = format!("{}\u{3000}{}", "a".repeat(10), "b".repeat(30));

    assert_eq!(tail(&text, 35), format!("…{}", "b".repeat(30)));
}

#[test]
fn a_tail_cut_at_a_no_break_space_does_not_panic() {
    let text = format!("{}\u{a0}{}", "a".repeat(10), "b".repeat(30));

    assert_eq!(tail(&text, 35), format!("…{}", "b".repeat(30)));
}

#[test]
fn a_tail_with_no_break_keeps_the_whole_suffix() {
    let text = "x".repeat(50);

    assert_eq!(tail(&text, 20), format!("…{}", "x".repeat(20)));
}

#[test]
fn a_short_tail_is_returned_whole() {
    assert_eq!(tail("short answer", 100), "short answer");
}

#[test]
fn a_head_cut_at_a_multibyte_space_keeps_whole_words() {
    let text = format!("{}\u{3000}{}", "a".repeat(10), "b".repeat(30));

    assert_eq!(head(&text, 15), format!("{}…", "a".repeat(10)));
}
