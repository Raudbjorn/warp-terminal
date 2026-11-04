use super::*;

const HALF: Duration = Duration::from_millis(110);

#[test]
fn first_value_rises_in_and_hiding_forgets_it() {
    let morph = Morph::<f32>::default();
    let now = Instant::now();
    morph.sync_at(Some(40.), now);
    let (frame, animating) = morph.frame_at(now).unwrap();
    assert_eq!((frame.from, frame.to, frame.t), (40., 40., 1.));
    assert_eq!((frame.appear, frame.count), (0., 0.));
    assert!(animating);
    let (frame, animating) = morph.frame_at(now + COUNT_UP).unwrap();
    assert_eq!((frame.appear, frame.count), (1., 1.));
    assert!(!animating);
    morph.sync_at(None, now + COUNT_UP);
    assert!(morph.frame_at(now + COUNT_UP).is_none());
    morph.sync_at(Some(90.), now + COUNT_UP);
    let (frame, animating) = morph.frame_at(now + COUNT_UP).unwrap();
    assert_eq!((frame.to, frame.appear), (90., 0.));
    assert!(animating);
}

#[test]
fn change_eases_from_old_to_new_value() {
    let morph = Morph::<f32>::default();
    let now = Instant::now();
    morph.sync_at(Some(20.), now);
    morph.sync_at(Some(80.), now);
    let shown = |at| {
        let (frame, _) = morph.frame_at(at).unwrap();
        frame.from.mix(&frame.to, frame.t)
    };
    assert_eq!(shown(now), 20.);
    // Eased out: past the linear midpoint halfway through.
    let half = shown(now + HALF);
    assert!(half > 50. && half < 80., "{half}");
    assert_eq!(shown(now + MORPH), 80.);
    assert!(!morph.frame_at(now + COUNT_UP).unwrap().1);
}

#[test]
fn turning_mid_morph_starts_from_what_is_shown() {
    let morph = Morph::<f32>::default();
    let now = Instant::now();
    morph.sync_at(Some(0.), now);
    morph.sync_at(Some(100.), now);
    let (frame, _) = morph.frame_at(now + HALF).unwrap();
    let on_screen = frame.from.mix(&frame.to, frame.t);
    morph.sync_at(Some(10.), now + HALF);
    let (frame, animating) = morph.frame_at(now + HALF).unwrap();
    assert_eq!((frame.from, frame.to, frame.t), (on_screen, 10., 0.));
    assert!(animating);
}
