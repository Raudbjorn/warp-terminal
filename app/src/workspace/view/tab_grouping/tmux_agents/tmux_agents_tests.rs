use super::*;

/// A split tab whose first terminal is a plain shell and whose second one is in tmux renames the
/// session of the second, the one the sync follows. Fails if the first shell is taken whether or
/// not it has a tmux client.
#[test]
fn a_rename_follows_the_first_shell_with_a_tmux_client() {
    let sessions = HashMap::from([(30, "work"), (40, "play")]);
    assert_eq!(first_tmux_shell(&[10, 30, 40], &sessions), Some(30));
    assert_eq!(first_tmux_shell(&[40, 30], &sessions), Some(40));
}

#[test]
fn a_tab_without_a_tmux_client_has_nothing_to_rename() {
    let sessions = HashMap::from([(30, "work")]);
    assert_eq!(first_tmux_shell(&[10, 20], &sessions), None);
    assert_eq!(first_tmux_shell(&[], &sessions), None);
    assert_eq!(first_tmux_shell(&[10], &HashMap::<u32, ()>::new()), None);
}
