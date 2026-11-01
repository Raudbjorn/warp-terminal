//! State of the name-match groups listed above the text matches, and the arithmetic that lays the
//! result sections out as one flat list.

use warpui::elements::MouseStateHandle;

use super::{NameMatch, NameMatchKind, RankedNameMatches};

/// Number of matches a group reveals at a time.
pub(super) const NAME_GROUP_PAGE_SIZE: usize = 50;

/// A row of a name-match group.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameGroupRow {
    Header,
    /// The match at this position in the group's ranking.
    Match(usize),
    /// Trailing row that stands in for the matches the group is not showing.
    HiddenMatches,
}

pub(super) struct NameMatchEntry {
    pub(super) name_match: NameMatch,
    pub(super) mouse_state: MouseStateHandle,
}

/// The two parts a name match's row shows, each with the char indices to highlight in it.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct NameMatchLabel<'a> {
    pub(super) name: &'a str,
    pub(super) name_highlights: Vec<usize>,
    /// Path of the containing directory, empty for an entry directly below its search root.
    pub(super) parent: &'a str,
    pub(super) parent_highlights: Vec<usize>,
}

impl<'a> NameMatchLabel<'a> {
    pub(super) fn new(name_match: &'a NameMatch) -> Self {
        let display_path = name_match.display_path.as_str();
        let (parent, name) = match display_path.rfind(['/', '\\']) {
            Some(separator) => (&display_path[..separator], &display_path[separator + 1..]),
            None => ("", display_path),
        };

        let parent_chars = parent.chars().count();
        // The separator between the two parts is shown in neither.
        let name_start = display_path.chars().count() - name.chars().count();
        let mut name_highlights = Vec::new();
        let mut parent_highlights = Vec::new();
        for &index in &name_match.highlight_indices {
            if index >= name_start {
                name_highlights.push(index - name_start);
            } else if index < parent_chars {
                parent_highlights.push(index);
            }
        }

        Self {
            name,
            name_highlights,
            parent,
            parent_highlights,
        }
    }
}

/// The matches of one kind, of which only a leading page is shown until more are asked for.
pub(super) struct NameGroup {
    kind: NameMatchKind,
    entries: Vec<NameMatchEntry>,
    total: usize,
    shown: usize,
    is_collapsed: bool,
    pub(super) header_mouse_state: MouseStateHandle,
    pub(super) hidden_matches_mouse_state: MouseStateHandle,
}

impl NameGroup {
    pub(super) fn empty(kind: NameMatchKind) -> Self {
        Self::new(kind, RankedNameMatches::default())
    }

    pub(super) fn new(kind: NameMatchKind, ranked: RankedNameMatches) -> Self {
        let entries: Vec<_> = ranked
            .matches
            .into_iter()
            .map(|name_match| NameMatchEntry {
                name_match,
                mouse_state: MouseStateHandle::default(),
            })
            .collect();
        Self {
            kind,
            shown: entries.len().min(NAME_GROUP_PAGE_SIZE),
            // The total can never be smaller than what was actually kept.
            total: ranked.total.max(entries.len()),
            entries,
            is_collapsed: false,
            header_mouse_state: MouseStateHandle::default(),
            hidden_matches_mouse_state: MouseStateHandle::default(),
        }
    }

    pub(super) fn kind(&self) -> NameMatchKind {
        self.kind
    }

    /// Number of entries that matched, shown or not.
    pub(super) fn total(&self) -> usize {
        self.total
    }

    pub(super) fn is_empty(&self) -> bool {
        self.total == 0
    }

    pub(super) fn is_collapsed(&self) -> bool {
        self.is_collapsed
    }

    pub(super) fn toggle_collapsed(&mut self) {
        self.is_collapsed = !self.is_collapsed;
    }

    /// The shown match at `index` in the group's ranking.
    pub(super) fn entry(&self, index: usize) -> Option<&NameMatchEntry> {
        self.entries[..self.shown].get(index)
    }

    /// Number of matches that are not shown.
    pub(super) fn hidden_count(&self) -> usize {
        self.total - self.shown
    }

    /// Whether some of the hidden matches can still be revealed. The rest were dropped as too
    /// weak before they reached the group.
    pub(super) fn can_show_more(&self) -> bool {
        self.shown < self.entries.len()
    }

    /// Reveals the next page of matches, returning the position of the first one revealed.
    pub(super) fn show_more(&mut self) -> Option<usize> {
        if !self.can_show_more() {
            return None;
        }
        let first_revealed = self.shown;
        self.shown = (self.shown + NAME_GROUP_PAGE_SIZE).min(self.entries.len());
        Some(first_revealed)
    }

    /// Number of rows the group occupies in the results list.
    pub(super) fn visible_count(&self) -> usize {
        if self.is_empty() {
            0
        } else if self.is_collapsed {
            1
        } else {
            1 + self.shown + usize::from(self.hidden_count() > 0)
        }
    }

    /// The row at `offset` from the group's first row.
    pub(super) fn row_at(&self, offset: usize) -> Option<NameGroupRow> {
        if offset >= self.visible_count() {
            None
        } else if offset == 0 {
            Some(NameGroupRow::Header)
        } else if offset <= self.shown {
            Some(NameGroupRow::Match(offset - 1))
        } else {
            Some(NameGroupRow::HiddenMatches)
        }
    }

    /// The offset of `row` from the group's first row, if the row is currently visible.
    pub(super) fn offset_of(&self, row: NameGroupRow) -> Option<usize> {
        let offset = match row {
            NameGroupRow::Header => 0,
            NameGroupRow::Match(index) if index < self.shown => 1 + index,
            NameGroupRow::Match(_) => return None,
            NameGroupRow::HiddenMatches => 1 + self.shown,
        };
        (self.row_at(offset) == Some(row)).then_some(offset)
    }
}

/// A row of the results list, located by its offset within the section it belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SectionRow {
    Folders(usize),
    Files(usize),
    TextMatchesHeader,
    /// Offset among the rows listed under the text matches header.
    TextMatch(usize),
}

/// Number of rows each section of the results list occupies. The sections are stacked in the
/// order folders, files, text matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SectionCounts {
    pub(super) folders: usize,
    pub(super) files: usize,
    /// Rows listed under the text matches header, or `None` when there are no text matches and
    /// the header is left out as well.
    pub(super) text_matches: Option<usize>,
}

impl SectionCounts {
    pub(super) fn total(&self) -> usize {
        self.folders + self.files + self.text_matches.map_or(0, |rows| rows + 1)
    }

    /// The row at `index` in the flat list.
    pub(super) fn row_at(&self, index: usize) -> Option<SectionRow> {
        if index < self.folders {
            return Some(SectionRow::Folders(index));
        }
        let index = index - self.folders;
        if index < self.files {
            return Some(SectionRow::Files(index));
        }
        let index = index - self.files;
        let text_matches = self.text_matches?;
        if index == 0 {
            Some(SectionRow::TextMatchesHeader)
        } else if index <= text_matches {
            Some(SectionRow::TextMatch(index - 1))
        } else {
            None
        }
    }

    /// The index of `row` in the flat list, if its section has such a row.
    pub(super) fn index_of(&self, row: SectionRow) -> Option<usize> {
        let index = match row {
            SectionRow::Folders(offset) => offset,
            SectionRow::Files(offset) => self.folders + offset,
            SectionRow::TextMatchesHeader => self.folders + self.files,
            SectionRow::TextMatch(offset) => self.folders + self.files + 1 + offset,
        };
        (self.row_at(index) == Some(row)).then_some(index)
    }
}

/// Describes how many entries matched by name, or returns `None` when none did.
pub(super) fn name_match_summary(folders: usize, files: usize) -> Option<String> {
    let counted = |count, singular: &str| match count {
        0 => None,
        1 => Some(format!("1 {singular}")),
        _ => Some(format!("{count} {singular}s")),
    };
    let parts: Vec<_> = [counted(folders, "folder"), counted(files, "file")]
        .into_iter()
        .flatten()
        .collect();
    (!parts.is_empty()).then(|| format!("{} by name", parts.join(" and ")))
}

#[cfg(test)]
#[path = "name_groups_tests.rs"]
mod tests;
