//! Shortening a story's text for a surface with less room than the story has.
//!
//! Conn shows the same prompts and answers in two places at two widths — the
//! docked panel and the tab list's hover card — so where to cut is decided
//! here rather than twice. The rules are shared; only the budget differs.

/// How far a cut will reach back for a word boundary before giving up and
/// cutting mid-word. Past this the whitespace is far enough away that honouring
/// it would cost more text than the ragged edge costs.
const WORD_BOUNDARY_SLACK: usize = 24;

/// The opening of a text, cut at a word boundary where one is near enough.
pub(crate) fn head(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut kept: String = text.chars().take(limit).collect();
    if let Some(space) = kept.rfind(char::is_whitespace)
        && kept[space..].chars().count() < WORD_BOUNDARY_SLACK
    {
        kept.truncate(space);
    }
    format!("{}…", kept.trim_end())
}

/// The end of a long message rather than its beginning.
///
/// An answer of any length opens with orientation — "here is what I found" —
/// and closes with the conclusion and whatever decision it is waiting on.
/// Coming back to a pane, the close is the part worth reading, and the opening
/// is the part you can infer from your own prompt.
pub(crate) fn tail(text: &str, limit: usize) -> String {
    let text = flatten(text);
    let length = text.chars().count();
    if length <= limit {
        return text;
    }
    let tail: String = text.chars().skip(length - limit).collect();
    // Start after the first break so the preview does not open mid-word.
    let start = tail.find(char::is_whitespace).map_or(0, |index| index + 1);
    format!("…{}", tail[start..].trim_start())
}

/// Drops Markdown heading markers so a preview reads as prose.
///
/// Both surfaces render plain text, and a stray `##` mid-line reads as noise
/// rather than as structure.
pub(crate) fn flatten(text: &str) -> String {
    text.trim()
        .lines()
        .map(|line| line.trim_start_matches('#').trim_start())
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}
