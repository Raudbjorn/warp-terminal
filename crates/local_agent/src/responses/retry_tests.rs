use std::time::Duration;

use super::*;

fn policy() -> RetryPolicy {
    RetryPolicy::default()
}

fn status(status: u16) -> LocalAgentError {
    LocalAgentError::ErrorStatus {
        status,
        body: String::new(),
    }
}

#[test]
fn retries_transport_failures_and_transient_statuses() {
    for error in [
        LocalAgentError::Transport("reset".to_string()),
        status(408),
        status(429),
        status(500),
        status(503),
        status(599),
    ] {
        assert!(policy().should_retry(1, &error), "{error:?}");
    }
}

#[test]
fn does_not_retry_permanent_failures() {
    for error in [
        status(400),
        status(404),
        status(422),
        LocalAgentError::InvalidApiKey {
            model_slug: "m".to_string(),
        },
        LocalAgentError::ContextWindowExceeded {
            message: String::new(),
        },
        LocalAgentError::Parse("bad".to_string()),
    ] {
        assert!(!policy().should_retry(1, &error), "{error:?}");
    }
}

#[test]
fn stops_after_the_last_attempt() {
    let error = status(503);

    assert!(policy().should_retry(policy().max_attempts - 1, &error));
    assert!(!policy().should_retry(policy().max_attempts, &error));
}

#[test]
fn backoff_grows_exponentially_with_bounded_jitter() {
    let policy = policy();
    for attempt in 1..=policy.max_attempts {
        let base = policy
            .initial_backoff
            .mul_f32(
                policy
                    .multiplier
                    .powi(i32::try_from(attempt - 1).unwrap_or(0)),
            )
            .min(policy.max_backoff);
        let ceiling = base + base.mul_f32(policy.jitter_fraction);
        for _ in 0..50 {
            let delay = policy.backoff(attempt);
            assert!(
                delay >= base && delay <= ceiling,
                "attempt {attempt}: {delay:?} outside [{base:?}, {ceiling:?}]"
            );
        }
    }
}

#[test]
fn backoff_is_capped() {
    let policy = RetryPolicy {
        jitter_fraction: 0.0,
        ..RetryPolicy::default()
    };

    assert_eq!(policy.backoff(100), policy.max_backoff);
    assert_eq!(policy.backoff(1), Duration::from_millis(500));
}
