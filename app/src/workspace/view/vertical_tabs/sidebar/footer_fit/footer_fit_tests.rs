use super::*;

/// Named row 232 wide, bare 96 (two rings): the names stay at a 272 sidebar (inner
/// 232), the bare rings below it, the last row when nothing fits.
#[test]
fn footer_fit_keeps_the_first_row_that_fits() {
    let rows = [232., 96.];
    assert_eq!(choose(rows, 340.), 0);
    assert_eq!(choose(rows, 232.), 0);
    assert_eq!(choose(rows, 160.), 1);
    assert_eq!(choose(rows, 50.), 1);
}

/// Flicker at the edge: a row exactly as wide as the room fits, every time.
#[test]
fn footer_fit_a_row_as_wide_as_the_room_fits() {
    assert_eq!(choose([232., 96.], 232.), 0);
    assert_eq!(choose([232., 96.], 232.), 0);
    assert_eq!(choose([232.5, 96.], 232.), 1);
}

/// Lazy: a row after the first fit is never laid out.
#[test]
fn footer_fit_stops_at_the_first_fit() {
    let mut asked = 0;
    let widths = [100., 90.].into_iter().inspect(|_| asked += 1);
    assert_eq!(choose(widths, 150.), 0);
    assert_eq!(asked, 1);
}

/// The sidebar widening: whatever the child measures, the footer reports at most the
/// max width it got (its height stays).
#[test]
fn footer_fit_never_reports_wider_than_it_was_given() {
    assert_eq!(reported(vec2f(328., 20.), 232.), vec2f(232., 20.));
    assert_eq!(reported(vec2f(96., 20.), 160.), vec2f(96., 20.));
}
