use super::*;

/// The polling window is never in its own view map, so before this it never adopted;
/// with two windows each picked the other and neither adopted.
#[test]
fn the_polling_window_counts_itself() {
    let mut alone = Vec::<(u32, usize)>::new();
    count_own_window(&mut alone, 1, Some(10));
    assert_eq!(pick_adopter(&alone), Some(1));

    // Window 1 (10 tabs) polls and sees only window 2 (1 tab), and the reverse.
    let mut seen_by_1 = vec![(2u32, 1usize)];
    count_own_window(&mut seen_by_1, 1, Some(10));
    let mut seen_by_2 = vec![(1u32, 10usize)];
    count_own_window(&mut seen_by_2, 2, Some(1));
    assert_eq!(pick_adopter(&seen_by_1), Some(1));
    assert_eq!(pick_adopter(&seen_by_2), Some(1));

    // A window without the framed layout adds nothing.
    let mut none = Vec::<(u32, usize)>::new();
    count_own_window(&mut none, 1, None);
    assert_eq!(pick_adopter(&none), None);
}

fn tab(group: Option<&str>, pos: Option<u32>) -> LayoutTab {
    LayoutTab {
        pinned: false,
        group: group.map(str::to_owned),
        pos,
    }
}

#[test]
fn only_unattached_sessions_marked_for_adoption_are_candidates() {
    let output = "$1\t0\t1\twork\t2\n\
                  $2\t1\t1\twork\t0\n\
                  $3\t2\t1\t\t\n\
                  $4\t0\t\t\t\n\
                  $5\t0\t1\t\t\n\
                  bad\t0\t1\t\t\n";
    assert_eq!(
        parse_candidates(output),
        vec![
            Candidate {
                id: "$1".to_owned(),
                group: Some("work".to_owned()),
                pos: Some(2),
            },
            Candidate {
                id: "$5".to_owned(),
                group: None,
                pos: None,
            },
        ]
    );
}

#[test]
fn only_plain_session_ids_get_an_attach_command() {
    assert_eq!(
        attach_command("$12").as_deref(),
        Some("tmux attach -t '$12'")
    );
    assert_eq!(attach_command("$"), None);
    assert_eq!(attach_command("$1'; rm -rf ~; '"), None);
    assert_eq!(attach_command("work"), None);
}

#[test]
fn a_session_is_claimed_once_until_the_guard_expires() {
    let start = Instant::now();
    let mut guard = AdoptGuard::default();
    assert!(guard.claim("$1", start));
    assert!(!guard.claim("$1", start + Duration::from_secs(10)));
    assert!(!guard.claim("$1", start + Duration::from_secs(59)));
    assert!(guard.claim("$2", start + Duration::from_secs(10)));
    assert!(guard.claim("$1", start + ADOPT_GUARD));
}

#[test]
fn adopt_index_places_by_pos_within_the_group() {
    let tabs = [
        tab(Some("a"), Some(0)),
        tab(Some("b"), Some(0)),
        tab(Some("b"), Some(1)),
        tab(Some("b"), Some(3)),
        tab(None, Some(0)),
    ];
    assert_eq!(adopt_index(&tabs, Some("b"), Some(2)), 3);
    assert_eq!(adopt_index(&tabs, Some("b"), Some(0)), 1);
    // Past every position, or no position: end of the section.
    assert_eq!(adopt_index(&tabs, Some("b"), Some(9)), 4);
    assert_eq!(adopt_index(&tabs, Some("b"), None), 4);
}

#[test]
fn adopt_index_without_a_group_goes_to_other() {
    let tabs = [tab(Some("a"), Some(0)), tab(None, Some(0)), tab(None, None)];
    assert_eq!(adopt_index(&tabs, None, None), 3);
    assert_eq!(adopt_index(&tabs, None, Some(0)), 1);
}

#[test]
fn adopt_index_for_a_new_group_or_empty_section_appends() {
    let tabs = [tab(Some("a"), Some(0)), tab(None, None)];
    assert_eq!(adopt_index(&tabs, Some("new"), Some(0)), 2);
    assert_eq!(adopt_index(&[], None, Some(4)), 0);
}

#[test]
fn adopt_index_skips_pinned_tabs() {
    let mut pinned = tab(Some("a"), Some(0));
    pinned.pinned = true;
    let tabs = [pinned, tab(Some("a"), Some(1))];
    assert_eq!(adopt_index(&tabs, Some("a"), Some(0)), 1);
}

#[test]
fn the_window_with_most_tabs_adopts_and_the_frontmost_wins_ties() {
    assert_eq!(pick_adopter(&[(1, 2), (2, 5), (3, 5)]), Some(2));
    assert_eq!(pick_adopter(&[(7, 0)]), Some(7));
    assert_eq!(pick_adopter::<u32>(&[]), None);
}
