//! Model-slug normalization: splits an optional reasoning-effort suffix off
//! the configured slug (`gpt-5.3-codex-high` -> `gpt-5.3-codex` + `high`).
//!
//! Accepted grammar (all lowercase, matched on the last `/`-separated
//! segment so gateway-namespaced slugs such as `openai/gpt-5.2-low` work):
//!
//! ```text
//! slug     = model "-" effort
//! effort   = "none" | "minimal" | "low" | "medium" | "high" | "xhigh"
//! model    = "gpt-5" [ "." digits ] { "-" variant }
//!          | "gpt-oss-" digits "b"
//!          | ( "o1" | "o3" | "o4" ) [ "-mini" | "-pro" ]
//! variant  = "codex" | "max" | "mini" | "nano" | "pro"
//! ```
//!
//! The suffix is only stripped when what remains is a known reasoning model,
//! so gateway aliases that merely end in an effort word (`my-proxy-high`) and
//! non-reasoning models pass through untouched. Dashed spellings such as
//! `gpt-5-4` are *not* rewritten to `gpt-5.4`: the slug is the provider's own
//! name for the model.
//!
//! A slug without a recognized effort suffix is sent as a plain model with no
//! `reasoning` block; the suffix is the user's explicit opt-in to reasoning
//! parameters, which non-reasoning models reject with a 400.

use serde::Serialize;

const REASONING_EFFORTS: [&str; 6] = ["none", "minimal", "low", "medium", "high", "xhigh"];

/// GPT-5 name segments that follow the version (`gpt-5.1-codex-max`).
const GPT5_VARIANTS: [&str; 5] = ["codex", "max", "mini", "nano", "pro"];

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
    is_gpt5_family(name) || is_gpt_oss(name) || is_o_series(name)
}

fn is_gpt5_family(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("gpt-5") else {
        return false;
    };
    let rest = match rest.strip_prefix('.') {
        Some(minor) => {
            let digits = minor.chars().take_while(char::is_ascii_digit).count();
            if digits == 0 {
                return false;
            }
            &minor[digits..]
        }
        None => rest,
    };
    if rest.is_empty() {
        return true;
    }
    rest.strip_prefix('-')
        .is_some_and(|variants| variants.split('-').all(|v| GPT5_VARIANTS.contains(&v)))
}

fn is_gpt_oss(name: &str) -> bool {
    name.strip_prefix("gpt-oss-")
        .and_then(|size| size.strip_suffix('b'))
        .is_some_and(|digits| !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()))
}

fn is_o_series(name: &str) -> bool {
    let (head, tail) = name.split_once('-').unwrap_or((name, ""));
    matches!(head, "o1" | "o3" | "o4") && matches!(tail, "" | "mini" | "pro")
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
