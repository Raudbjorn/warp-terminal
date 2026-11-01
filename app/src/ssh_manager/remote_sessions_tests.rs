use super::*;

fn row(
    id: i64,
    tab_id: i64,
    parent: Option<i64>,
    leaf: Option<(&str, &str)>,
    horizontal: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": id,
        "tab_id": tab_id,
        "parent": parent,
        "is_leaf": i64::from(leaf.is_some()),
        "horizontal": if leaf.is_some() { None } else { Some(i64::from(horizontal)) },
        "kind": leaf.map(|(kind, _)| kind),
        "cwd": leaf.map(|(_, cwd)| cwd),
        "title": null,
        "color": null,
    })
}

fn output(rows: Vec<serde_json::Value>) -> String {
    format!(
        "/home/me/.local/state/warp-ws0/warp-oss/warp.sqlite\n{}",
        serde_json::Value::Array(rows)
    )
}

fn shape(layout: &PaneTemplateType) -> String {
    match layout {
        PaneTemplateType::PaneTemplate { .. } => "pane".to_string(),
        PaneTemplateType::PaneBranchTemplate {
            split_direction,
            panes,
        } => format!(
            "{split_direction:?}[{}]",
            panes.iter().map(shape).collect::<Vec<_>>().join(", ")
        ),
    }
}

#[test]
fn rebuilds_tabs_with_nested_splits_and_leaf_directories() {
    let mut first = row(1, 1, None, Some(("terminal", "/srv/app")), false);
    first["title"] = "api".into();
    first["color"] = serde_yaml::to_string(&SelectedTabColor::Color(AnsiColorIdentifier::Red))
        .unwrap()
        .into();
    let sessions = parse_remote_output(&output(vec![
        first,
        row(2, 2, None, None, true),
        row(3, 2, Some(2), Some(("terminal", "/srv/left")), false),
        row(4, 2, Some(2), None, false),
        row(5, 2, Some(4), Some(("terminal", "/srv/top")), false),
        row(6, 2, Some(4), Some(("terminal", "")), false),
    ]))
    .unwrap();

    assert_eq!(
        sessions.database,
        "/home/me/.local/state/warp-ws0/warp-oss/warp.sqlite"
    );
    assert_eq!(sessions.tabs.len(), 2);
    assert_eq!(sessions.tabs[0].title.as_deref(), Some("api"));
    assert_eq!(sessions.tabs[0].color, Some(AnsiColorIdentifier::Red));
    assert_eq!(
        sessions.tabs[0].directories,
        vec![Some("/srv/app".to_string())]
    );
    assert_eq!(
        shape(&sessions.tabs[1].layout),
        "Horizontal[pane, Vertical[pane, pane]]"
    );
    assert_eq!(
        sessions.tabs[1].directories,
        vec![
            Some("/srv/left".to_string()),
            Some("/srv/top".to_string()),
            None
        ]
    );
}

#[test]
fn drops_non_terminal_panes_and_collapses_their_splits() {
    let sessions = parse_remote_output(&output(vec![
        row(1, 1, None, None, true),
        row(2, 1, Some(1), Some(("terminal", "/srv")), false),
        row(3, 1, Some(1), Some(("code", "")), false),
        row(4, 2, None, Some(("notebook", "")), false),
    ]))
    .unwrap();

    assert_eq!(sessions.tabs.len(), 1);
    assert_eq!(shape(&sessions.tabs[0].layout), "pane");
    assert_eq!(sessions.tabs[0].directories, vec![Some("/srv".to_string())]);
}

#[test]
fn accepts_a_database_without_tabs() {
    let sessions = parse_remote_output("/db/warp.sqlite\n").unwrap();
    assert!(sessions.tabs.is_empty());
    assert!(parse_remote_output("").is_err());
}

#[test]
fn rejects_invalid_remote_pane_graphs() {
    for rows in [
        vec![row(1, 1, None, None, true), row(1, 1, Some(1), None, true)],
        vec![
            row(1, 1, Some(2), None, true),
            row(2, 1, Some(1), None, true),
        ],
        vec![row(1, 1, Some(2), None, true)],
        vec![row(1, 1, Some(2), None, true), row(2, 2, None, None, true)],
    ] {
        assert!(parse_remote_output(&output(rows)).is_err());
    }
}

#[test]
fn bounds_remote_session_size_and_split_depth() {
    assert!(parse_remote_output(&"x".repeat(MAX_SESSION_OUTPUT_BYTES + 1)).is_err());
    let rows = (0..=MAX_SESSION_NODES)
        .map(|id| row(id as i64, id as i64, None, Some(("terminal", "/")), false))
        .collect();
    assert!(parse_remote_output(&output(rows)).is_err());
    let rows = (0..=(MAX_SPLIT_DEPTH + 1))
        .map(|id| row(id as i64, 1, (id > 0).then_some(id as i64 - 1), None, true))
        .collect();
    assert!(parse_remote_output(&output(rows)).is_err());
}
