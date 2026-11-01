//! Bounded retry policy for the Responses engine.
//!
//! Retrying is safe only while the caller has seen nothing of the response;
//! the engine enforces that, this module decides *whether* and *how long*.

use std::time::Duration;

use crate::LocalAgentError;

/// Total attempts per turn, including the first.
const MAX_ATTEMPTS: usize = 3;
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);
const BACKOFF_MULTIPLIER: f32 = 2.0;
const MAX_BACKOFF: Duration = Duration::from_secs(8);
/// Each delay is stretched by a random fraction of up to this much, so that
/// clients that failed together do not retry together.
const JITTER_FRACTION: f32 = 0.3;

/// HTTP statuses worth retrying: request timeout, rate limit, server errors.
const TRANSIENT_STATUSES: [u16; 2] = [408, 429];

/// Error code of a 429 that is about billing, not about request rate.
const QUOTA_EXHAUSTED_MARKER: &str = "insufficient_quota";

#[derive(Debug, Clone, Copy)]
pub(super) struct RetryPolicy {
    pub(super) max_attempts: usize,
    pub(super) initial_backoff: Duration,
    pub(super) multiplier: f32,
    pub(super) max_backoff: Duration,
    pub(super) jitter_fraction: f32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: MAX_ATTEMPTS,
            initial_backoff: INITIAL_BACKOFF,
            multiplier: BACKOFF_MULTIPLIER,
            max_backoff: MAX_BACKOFF,
            jitter_fraction: JITTER_FRACTION,
        }
    }
}

impl RetryPolicy {
    /// Whether to try again after `attempt` (1-based) failed with `error`.
    pub(super) fn should_retry(&self, attempt: usize, error: &LocalAgentError) -> bool {
        attempt < self.max_attempts && is_transient(error)
    }

    /// The delay to wait after `attempt` (1-based) failed.
    pub(super) fn backoff(&self, attempt: usize) -> Duration {
        let exponent = i32::try_from(attempt.saturating_sub(1)).unwrap_or(i32::MAX);
        // `try_from` instead of `mul_f32`: the product overflows `Duration`
        // (and `mul_f32` panics) for a large enough attempt count.
        let scaled_secs = self.initial_backoff.as_secs_f32() * self.multiplier.powi(exponent);
        let base = Duration::try_from_secs_f32(scaled_secs)
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff);
        base + base.mul_f32(self.jitter_fraction * rand::random::<f32>())
    }
}

/// Failures that a later identical request can plausibly get past.
fn is_transient(error: &LocalAgentError) -> bool {
    match error {
        LocalAgentError::Transport(_) => true,
        // A spent quota stays spent: waiting out a backoff cannot fix it.
        LocalAgentError::ErrorStatus { body, .. } if body.contains(QUOTA_EXHAUSTED_MARKER) => false,
        LocalAgentError::ErrorStatus { status, .. } => {
            TRANSIENT_STATUSES.contains(status) || (500..=599).contains(status)
        }
        LocalAgentError::InvalidApiKey { .. }
        | LocalAgentError::ContextWindowExceeded { .. }
        | LocalAgentError::Parse(_)
        | LocalAgentError::Unimplemented(_) => false,
    }
}

#[cfg(test)]
#[path = "retry_tests.rs"]
mod tests;
