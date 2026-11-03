use super::*;

#[test]
fn detects_the_dominant_terminator() {
    assert_eq!(LineEnding::detect("a\nb\n"), LineEnding::Lf);
    assert_eq!(LineEnding::detect("a\r\nb\r\n"), LineEnding::CrLf);
    assert_eq!(LineEnding::detect("no terminator"), LineEnding::Lf);
    assert_eq!(LineEnding::detect("a\r\nb\nc\n"), LineEnding::Lf);
    assert_eq!(LineEnding::detect("a\r\nb\r\nc\n"), LineEnding::CrLf);
}

#[test]
fn converts_to_crlf_without_doubling_existing_ones() {
    assert_eq!(
        LineEnding::CrLf.apply("a\nb\r\nc".to_owned()),
        "a\r\nb\r\nc"
    );
}

#[test]
fn converts_to_lf() {
    assert_eq!(
        LineEnding::Lf.apply("a\r\nb\nc\r\n".to_owned()),
        "a\nb\nc\n"
    );
    assert_eq!(LineEnding::Lf.apply("a\nb\n".to_owned()), "a\nb\n");
}
