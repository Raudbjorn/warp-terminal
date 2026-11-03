use super::*;

fn file(pid: u32, status: &str, tmux: &str) -> String {
    format!(
        r#"{{"pid":{pid},"sessionId":"s{pid}","status":"{status}","waitingFor":"permission prompt","statusUpdatedAt":1000,"tmux":"{tmux}","updatedAt":1000}}"#
    )
}

#[test]
fn a_waiting_file_is_read_with_its_pane() {
    assert_eq!(
        parse_session_file(&file(7, "waiting", "home:@49.%49")),
        Some(Wait {
            pane: "%49".to_owned(),
            pid: 7,
            label: "permission prompt".to_owned(),
            updated_ms: 1000,
        })
    );
    let unlabeled = r#"{"pid":8,"status":"waiting","waitingFor":null,"tmux":"x:@1.%3"}"#;
    assert_eq!(
        parse_session_file(unlabeled).map(|wait| wait.label),
        Some("Waiting for you".to_owned())
    );
}

#[test]
fn files_of_dead_processes_and_other_states_are_ignored() {
    for status in ["idle", "busy", "shell"] {
        assert_eq!(parse_session_file(&file(7, status, "home:@49.%49")), None);
    }
    assert_eq!(parse_session_file(&file(7, "waiting", "no-pane")), None);
    assert_eq!(parse_session_file("not json"), None);
    let waits = vec![parse_session_file(&file(7, "waiting", "home:@49.%49")).unwrap()];
    let panes = parse_pane_sessions("%49 $3\n");
    assert!(waits_by_session(waits, &panes, |_| false).is_empty());
}

#[test]
fn waits_map_through_their_pane_to_its_session() {
    let panes = parse_pane_sessions("%1 $1\n%2 $1\n%3 $2\n");
    let mut first = parse_session_file(&file(10, "waiting", "a:@1.%2")).unwrap();
    first.updated_ms = 5;
    let mut newer = parse_session_file(&file(11, "waiting", "a:@1.%1")).unwrap();
    newer.updated_ms = 9;
    let elsewhere = parse_session_file(&file(12, "waiting", "b:@9.%9")).unwrap();
    let by_session = waits_by_session(vec![first, newer, elsewhere], &panes, |_| true);
    // Two panes of session $1 wait: the newer one speaks for it. $2 has none, and a pane
    // the server does not know (%9) maps nowhere.
    assert_eq!(by_session.len(), 1);
    assert_eq!(by_session["$1"].pid, 11);
}
