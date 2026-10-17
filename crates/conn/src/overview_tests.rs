use super::{OverviewItem, group_by_project};
use crate::ConnStatus;

fn item(pane: u32, project: &str, status: ConnStatus) -> OverviewItem<u32> {
    OverviewItem {
        pane,
        project: project.to_owned(),
        title: None,
        status,
        unseen: 0,
    }
}

fn unseen(pane: u32, project: &str, status: ConnStatus, unseen: usize) -> OverviewItem<u32> {
    OverviewItem {
        unseen,
        ..item(pane, project, status)
    }
}

fn shape(items: Vec<OverviewItem<u32>>) -> Vec<(String, Vec<u32>)> {
    group_by_project(items)
        .into_iter()
        .map(|group| {
            (
                group.project,
                group.items.into_iter().map(|item| item.pane).collect(),
            )
        })
        .collect()
}

#[test]
fn the_project_with_the_most_urgent_session_comes_first() {
    let groups = shape(vec![
        item(1, "alpha", ConnStatus::Working),
        item(2, "beta", ConnStatus::Done),
        item(3, "gamma", ConnStatus::NeedsYou),
        item(4, "delta", ConnStatus::Ended),
        item(5, "epsilon", ConnStatus::Failed),
    ]);

    let order: Vec<&str> = groups.iter().map(|(project, _)| project.as_str()).collect();
    assert_eq!(order, ["gamma", "epsilon", "alpha", "beta", "delta"]);
}

#[test]
fn a_projects_sessions_sit_together_most_urgent_first() {
    let groups = shape(vec![
        item(1, "warp", ConnStatus::Working),
        item(2, "docs", ConnStatus::Done),
        item(3, "warp", ConnStatus::Ended),
        item(4, "warp", ConnStatus::NeedsYou),
    ]);

    assert_eq!(
        groups,
        [
            ("warp".to_owned(), vec![4, 1, 3]),
            ("docs".to_owned(), vec![2]),
        ]
    );
}

#[test]
fn equally_urgent_projects_are_ordered_by_name() {
    let groups = shape(vec![
        item(1, "zeta", ConnStatus::Working),
        item(2, "alpha", ConnStatus::Working),
    ]);

    let order: Vec<&str> = groups.iter().map(|(project, _)| project.as_str()).collect();
    assert_eq!(order, ["alpha", "zeta"]);
}

/// A finished session with answers nobody has read waits on the reader; one already read does not.
#[test]
fn done_with_unseen_answers_ranks_above_working() {
    let groups = shape(vec![
        item(1, "warp", ConnStatus::Working),
        item(2, "warp", ConnStatus::Done),
        unseen(3, "warp", ConnStatus::Done, 2),
        item(4, "warp", ConnStatus::Failed),
        item(5, "warp", ConnStatus::Ended),
        item(6, "warp", ConnStatus::NeedsYou),
    ]);

    assert_eq!(groups, [("warp".to_owned(), vec![6, 4, 3, 1, 2, 5])]);
}

#[test]
fn a_project_ranks_by_its_unseen_done_session() {
    let groups = shape(vec![
        item(1, "alpha", ConnStatus::Working),
        unseen(2, "beta", ConnStatus::Done, 1),
    ]);

    let order: Vec<&str> = groups.iter().map(|(project, _)| project.as_str()).collect();
    assert_eq!(order, ["beta", "alpha"]);
}
