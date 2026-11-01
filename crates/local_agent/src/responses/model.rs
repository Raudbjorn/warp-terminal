//! Model-slug normalization: splits an optional reasoning-effort suffix off
//! the configured slug (`gpt-5.3-codex-high` -> `gpt-5.3-codex` + `high`).
//!
//! Accepted grammar (all lowercase; `namespace/` prefixes such as
//! `openai/gpt-5.2-low` are skipped when matching, and kept in the model):
//!
//! ```text
//! slug      = model [ "-" effort ]
//! effort    = "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
//! model     = [ namespace "/" ] base [ "-" snapshot ]
//! snapshot  = YYYY "-" MM "-" DD            (a dated release, kept in the name)
//! base      = "gpt-5" [ "." digits ] { "-" word }    (word != "chat")
//!           | "gpt-oss" { "-" word }
//!           | ( "o1" | "o3" | "o4" ) [ "-mini" | "-pro" ]
//! ```
//!
//! The `gpt-5` and `gpt-oss` forms are a prefix heuristic, not a list: any
//! variant of those families is assumed to be a reasoning model, except
//! `chat` models (`gpt-5-chat-latest`), which are not. The o-series is
//! matched exactly because its older members (`o1-preview`, `o1-mini`) take no
//! reasoning parameters. A wrong guess costs a provider error naming the
//! parameter, never a silent change in behavior.
//!
//! The effort suffix is only stripped when what remains is such a model, so
//! gateway aliases that merely end in an effort word (`my-proxy-high`) and
//! non-reasoning models pass through untouched. Dashed spellings such as
//! `gpt-5-4` are *not* rewritten to `gpt-5.4`: the slug is the provider's own
//! name for the model.
//!
//! A slug without a recognized effort suffix is sent as a plain model with no
//! `reasoning` block; the suffix is the user's explicit opt-in to reasoning
//! parameters, which non-reasoning models reject with a 400. A known
//! reasoning model additionally gets `include: reasoning.encrypted_content`
//! (`ResolvedModel::is_reasoning_model`) so its reasoning can be
//! replayed.

use serde::Serialize;

const REASONING_EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

/// The name segment that marks a GPT-5 chat model, which has no reasoning.
const NON_REASONING_SEGMENT: &str = "chat";

/// Reasoning summaries are requested so the host can render thinking blocks.
const REASONING_SUMMARY_MODE: &str = "auto";

/// The Responses API `reasoning` request parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ReasoningConfig {
    pub(super) effort: String,
    pub(super) summary: &'static str,
}

/// A configured slug resolved into what the request body needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ResolvedModel {
    /// The model name to send.
    pub(super) model: String,
    /// Set only when the slug carried an effort suffix.
    pub(super) reasoning: Option<ReasoningConfig>,
    /// Whether the model is a known reasoning model, i.e. one that accepts
    /// `include: ["reasoning.encrypted_content"]`.
    pub(super) is_reasoning_model: bool,
}

pub(super) fn resolve_model(slug: &str) -> ResolvedModel {
    let slug = slug.trim();
    let split = slug
        .rsplit_once('-')
        .filter(|(model, effort)| REASONING_EFFORTS.contains(effort) && is_reasoning_model(model));
    match split {
        Some((model, effort)) => ResolvedModel {
            model: model.to_string(),
            reasoning: Some(ReasoningConfig {
                effort: effort.to_string(),
                summary: REASONING_SUMMARY_MODE,
            }),
            is_reasoning_model: true,
        },
        None => ResolvedModel {
            model: slug.to_string(),
            reasoning: None,
            is_reasoning_model: is_reasoning_model(slug),
        },
    }
}

fn is_reasoning_model(model: &str) -> bool {
    let name = model.rsplit('/').next().unwrap_or(model);
    let name = strip_snapshot(name);
    is_gpt5_family(name) || is_gpt_oss(name) || is_o_series(name)
}

/// Drops a trailing `-YYYY-MM-DD` release date.
fn strip_snapshot(name: &str) -> &str {
    let is_digits =
        |part: &str, len: usize| part.len() == len && part.bytes().all(|b| b.is_ascii_digit());
    let mut parts = name.rsplitn(4, '-');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some(day), Some(month), Some(year), Some(base))
            if is_digits(year, 4) && is_digits(month, 2) && is_digits(day, 2) =>
        {
            base
        }
        _ => name,
    }
}

fn is_gpt5_family(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("gpt-5") else {
        return false;
    };
    let ends_name = rest.is_empty() || rest.starts_with('-');
    let is_minor_version = rest
        .strip_prefix('.')
        .and_then(|minor| minor.chars().next())
        .is_some_and(|first| first.is_ascii_digit());
    (ends_name || is_minor_version) && !has_segment(name, NON_REASONING_SEGMENT)
}

fn is_gpt_oss(name: &str) -> bool {
    name == "gpt-oss" || name.starts_with("gpt-oss-")
}

fn is_o_series(name: &str) -> bool {
    let (head, tail) = name.split_once('-').unwrap_or((name, ""));
    matches!(head, "o1" | "o3" | "o4") && matches!(tail, "" | "mini" | "pro")
}

fn has_segment(name: &str, segment: &str) -> bool {
    name.split('-').any(|part| part == segment)
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
