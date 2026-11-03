//! The cross-window overview: every pane with a session, grouped by project, most urgent first.

use std::path::Path;

use crate::ConnStatus;

/// One pane's session as the overview lists it. `P` identifies the pane to jump to.
pub struct OverviewItem<P> {
    pub pane: P,
    /// A stable project grouping key. The displayed label is derived by `group_by_project`.
    pub project: String,
    pub title: Option<String>,
    pub status: ConnStatus,
    /// Answered turns the reader has not seen.
    pub unseen: usize,
}

impl<P> OverviewItem<P> {
    /// Lower is more urgent: what waits on the reader before what is still running.
    fn urgency(&self) -> u8 {
        match self.status {
            ConnStatus::NeedsYou => 0,
            ConnStatus::Failed => 1,
            ConnStatus::Done if self.unseen > 0 => 2,
            ConnStatus::Working => 3,
            ConnStatus::Done => 4,
            ConnStatus::Ended => 5,
        }
    }
}

/// A displayed project group. `project` is the basename label, while grouping
/// was performed using each item's stable key.
pub struct ProjectGroup<P> {
    pub project: String,
    pub items: Vec<OverviewItem<P>>,
}

fn project_label(project_key: &str) -> String {
    let path = Path::new(project_key);
    let name = path.file_name().and_then(|name| name.to_str()).unwrap_or(project_key);
    name.strip_suffix(".git").unwrap_or(name).to_owned()
}

/// Groups items by project. Projects are ordered by their most urgent session and sessions within
/// a project by urgency; ties keep project-name order and then input order.
pub fn group_by_project<P>(mut items: Vec<OverviewItem<P>>) -> Vec<ProjectGroup<P>> {
    items.sort_by_key(OverviewItem::urgency);
    let mut groups: Vec<ProjectGroup<P>> = Vec::new();
    for item in items {
        let label = project_label(&item.project);
        match groups.iter_mut().find(|group| {
            group
                .items
                .first()
                .is_some_and(|first| first.project == item.project)
        }) {
            Some(group) => group.items.push(item),
            None => groups.push(ProjectGroup {
                project: label,
                items: vec![item],
            }),
        }
    }
    groups.sort_by(|a, b| {
        (a.items[0].urgency(), &a.project).cmp(&(b.items[0].urgency(), &b.project))
    });
    groups
}

#[cfg(test)]
#[path = "overview_tests.rs"]
mod tests;
