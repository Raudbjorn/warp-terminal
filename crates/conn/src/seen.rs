//! What a returning reader has already seen of a pane's session.

use chrono::{DateTime, TimeDelta, Utc};

use crate::story::{ConnStory, ConnTurn};

/// How long a pane has to stay focused in the key window before its activity counts as seen. A
/// pane passed through on the way to another is not looked at.
pub const SEEN_AFTER: TimeDelta = TimeDelta::seconds(2);

/// The watermark once a pane focused in the key window since `focused_since` is checked at `now`.
/// Never moves backwards.
pub fn advance_watermark(
    watermark: Option<DateTime<Utc>>,
    latest_activity: Option<DateTime<Utc>>,
    focused_since: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Option<DateTime<Utc>> {
    if now - focused_since < SEEN_AFTER {
        return watermark;
    }
    watermark.max(latest_activity)
}

/// Turns whose answer arrived after `watermark`. With no watermark the pane was never seen, so
/// every answer is unseen. Unless `finished`, the last turn is still running: whatever it has said
/// so far is narration, not its answer.
pub fn unseen_turns(story: &ConnStory, watermark: Option<DateTime<Utc>>, finished: bool) -> usize {
    answered(story, finished)
        .filter(|turn| is_unseen(turn, watermark))
        .count()
}

/// The first turn counted by [`unseen_turns`].
pub fn first_unseen_turn(
    story: &ConnStory,
    watermark: Option<DateTime<Utc>>,
    finished: bool,
) -> Option<&ConnTurn> {
    answered(story, finished).find(|turn| is_unseen(turn, watermark))
}

fn answered(story: &ConnStory, finished: bool) -> impl Iterator<Item = &ConnTurn> {
    let running = usize::from(!finished);
    story
        .turns
        .iter()
        .take(story.turns.len().saturating_sub(running))
        .filter(|turn| turn.outcome.is_some())
}

fn is_unseen(turn: &ConnTurn, watermark: Option<DateTime<Utc>>) -> bool {
    watermark.is_none_or(|watermark| turn.last_activity() > watermark)
}

#[cfg(test)]
#[path = "seen_tests.rs"]
mod tests;
