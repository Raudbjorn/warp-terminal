use super::*;

fn item(id: &str) -> ReasoningItem {
    ReasoningItem {
        id: id.to_owned(),
        encrypted_content: Some(format!("enc-{id}")),
    }
}

#[test]
fn replace_keeps_only_the_latest_responses_items() {
    let mut cache = ReasoningCache::default();
    cache.replace("c", vec![item("rs_1")]);
    cache.replace("c", vec![item("rs_2"), item("rs_3")]);
    assert_eq!(cache.items("c"), vec![item("rs_2"), item("rs_3")]);
    assert_eq!(cache.conversation_count(), 1);
}

#[test]
fn an_empty_response_ends_the_carryover() {
    let mut cache = ReasoningCache::default();
    cache.replace("c", vec![item("rs_1")]);
    cache.replace("c", Vec::new());
    assert!(cache.items("c").is_empty());
    assert_eq!(cache.conversation_count(), 0);
}

#[test]
fn unknown_conversations_have_no_items() {
    let mut cache = ReasoningCache::default();
    assert!(cache.items("missing").is_empty());
    assert_eq!(cache.conversation_count(), 0);
}

#[test]
fn clear_drops_one_conversation_only() {
    let mut cache = ReasoningCache::default();
    cache.replace("a", vec![item("rs_a")]);
    cache.replace("b", vec![item("rs_b")]);
    cache.clear("a");
    assert!(cache.items("a").is_empty());
    assert_eq!(cache.items("b"), vec![item("rs_b")]);
    // Clearing again, or clearing an unknown id, is a no-op.
    cache.clear("a");
    cache.clear("never-existed");
    assert_eq!(cache.conversation_count(), 1);
}

#[test]
fn items_per_conversation_are_capped_keeping_the_newest() {
    let mut cache = ReasoningCache::default();
    let many: Vec<_> = (0..MAX_ITEMS_PER_CONVERSATION + 5)
        .map(|n| item(&format!("rs_{n}")))
        .collect();
    cache.replace("c", many);
    let kept = cache.items("c");
    assert_eq!(kept.len(), MAX_ITEMS_PER_CONVERSATION);
    assert_eq!(kept.first().unwrap().id, "rs_5");
    assert_eq!(
        kept.last().unwrap().id,
        format!("rs_{}", MAX_ITEMS_PER_CONVERSATION + 4)
    );
}

#[test]
fn tracked_conversations_are_capped_evicting_the_least_recently_used() {
    let mut cache = ReasoningCache::default();
    for n in 0..MAX_TRACKED_CONVERSATIONS {
        cache.replace(&format!("c{n}"), vec![item(&format!("rs_{n}"))]);
    }
    // Using c0 makes c1 the least recently used.
    assert_eq!(cache.items("c0"), vec![item("rs_0")]);

    cache.replace("newcomer", vec![item("rs_new")]);

    assert_eq!(cache.conversation_count(), MAX_TRACKED_CONVERSATIONS);
    assert!(cache.items("c1").is_empty(), "c1 should have been evicted");
    assert_eq!(cache.items("c0"), vec![item("rs_0")]);
    assert_eq!(cache.items("newcomer"), vec![item("rs_new")]);
}

#[test]
fn memory_stays_bounded_over_many_conversations() {
    let mut cache = ReasoningCache::default();
    for n in 0..10 * MAX_TRACKED_CONVERSATIONS {
        cache.replace(&format!("c{n}"), vec![item("rs")]);
    }
    assert_eq!(cache.conversation_count(), MAX_TRACKED_CONVERSATIONS);
    assert_eq!(cache.order.len(), MAX_TRACKED_CONVERSATIONS);
}
