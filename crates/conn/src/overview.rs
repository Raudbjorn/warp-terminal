//! The cross-window overview: every pane with a session, grouped by project, most urgent first.

use crate::ConnStatus;

/// One pane's session as the overview lists it. `P` identifies the pane to jump to.
pub struct OverviewItem<P> {
    pub pane: P,
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

/// The sessions of one project, most urgent first.
pub struct ProjectGroup<P> {
    pub project: String,
    pub items: Vec<OverviewItem<P>>,
}

/// Groups items by project. Projects are ordered by their most urgent session and sessions within
/// a project by urgency; ties keep project-name order and then input order.
pub fn group_by_project<P>(mut items: Vec<OverviewItem<P>>) -> Vec<ProjectGroup<P>> {
    items.sort_by_key(OverviewItem::urgency);
    let mut groups: Vec<ProjectGroup<P>> = Vec::new();
    for item in items {
        match groups
            .iter_mut()
            .find(|group| group.project == item.project)
        {
            Some(group) => group.items.push(item),
            None => groups.push(ProjectGroup {
                project: item.project.clone(),
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
