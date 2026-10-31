use std::borrow::Cow;

use chrono::Utc;

use super::*;

fn named_event(contains_ugc: bool) -> Event {
    Event {
        payload: EventPayload::NamedEvent {
            user_id: Some("user".into()),
            anonymous_id: "anon".into(),
            name: Cow::Borrowed("Some Event"),
            value: Some(json!({ "k": "v" })),
        },
        session_created_at: Utc::now(),
        timestamp: Utc::now(),
        contains_ugc,
    }
}

#[test]
fn event_to_json_maps_named_event() {
    let value = event_to_json(named_event(false));
    assert_eq!(value["type"], "track");
    assert_eq!(value["event"], "Some Event");
    assert_eq!(value["user_id"], "user");
    assert_eq!(value["anonymous_id"], "anon");
    assert_eq!(value["payload"]["k"], "v");
}

#[test]
fn event_to_json_maps_identify() {
    let event = Event {
        payload: EventPayload::IdentifyUser {
            user_id: "user".into(),
            anonymous_id: "anon".into(),
        },
        session_created_at: Utc::now(),
        timestamp: Utc::now(),
        contains_ugc: false,
    };
    let value = event_to_json(event);
    assert_eq!(value["type"], "identify");
    assert!(value["event"].is_null());
}
