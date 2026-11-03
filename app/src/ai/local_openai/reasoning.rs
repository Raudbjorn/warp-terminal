//! Per-conversation carryover of Responses API reasoning items.
//!
//! No flow in the app is multi-turn through [`super::LocalOpenAIClient`] yet, so this is only
//! reached through `chat_completion_responses`, which has no caller outside tests. It is kept,
//! bounded, for the first caller that is.

use std::collections::{HashMap, VecDeque};

use serde::{Deserialize, Serialize};

/// Most conversations tracked at once. Past this, the least recently used one is dropped.
pub(super) const MAX_TRACKED_CONVERSATIONS: usize = 64;

/// Most reasoning items kept for one conversation. A response that carries more keeps its newest.
pub(super) const MAX_ITEMS_PER_CONVERSATION: usize = 32;

/// A reasoning item returned by a Responses API provider, round-tripped on the next turn.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub(crate) struct ReasoningItem {
    pub id: String,
    /// Opaque to us: forwarded verbatim on the next turn. `None` is allowed for providers that
    /// surface only the reasoning identifier.
    pub encrypted_content: Option<String>,
}

/// Reasoning items by conversation id, bounded in both conversations and items per conversation.
///
/// Only the items of the **latest** response are kept for a conversation. The provider issues
/// fresh reasoning items for each turn, so an item from an older turn is stale and sending the
/// whole accumulated chain back would make every request larger than the last.
#[derive(Debug, Default)]
pub(super) struct ReasoningCache {
    entries: HashMap<String, Vec<ReasoningItem>>,
    /// Conversation ids, least recently used first.
    order: VecDeque<String>,
}

impl ReasoningCache {
    /// The items to send with the next request of `conversation_id`. Counts as a use.
    pub(super) fn items(&mut self, conversation_id: &str) -> Vec<ReasoningItem> {
        let Some(items) = self.entries.get(conversation_id) else {
            return Vec::new();
        };
        let items = items.clone();
        self.touch(conversation_id);
        items
    }

    /// Stores the items of the latest response, replacing what was there. An empty response ends
    /// the carryover for the conversation.
    pub(super) fn replace(&mut self, conversation_id: &str, mut items: Vec<ReasoningItem>) {
        if items.is_empty() {
            self.clear(conversation_id);
            return;
        }
        if items.len() > MAX_ITEMS_PER_CONVERSATION {
            items.drain(..items.len() - MAX_ITEMS_PER_CONVERSATION);
        }
        self.entries.insert(conversation_id.to_owned(), items);
        self.touch(conversation_id);
        while self.order.len() > MAX_TRACKED_CONVERSATIONS {
            if let Some(evicted) = self.order.pop_front() {
                self.entries.remove(&evicted);
            }
        }
    }

    /// Drops everything held for a conversation that ended or was reset.
    pub(super) fn clear(&mut self, conversation_id: &str) {
        if self.entries.remove(conversation_id).is_some() {
            self.order.retain(|id| id != conversation_id);
        }
    }

    #[cfg(test)]
    pub(super) fn conversation_count(&self) -> usize {
        self.entries.len()
    }

    fn touch(&mut self, conversation_id: &str) {
        self.order.retain(|id| id != conversation_id);
        self.order.push_back(conversation_id.to_owned());
    }
}

#[cfg(test)]
#[path = "reasoning_tests.rs"]
mod tests;
