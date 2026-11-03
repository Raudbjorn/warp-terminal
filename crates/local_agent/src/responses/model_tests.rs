use super::*;

fn reasoning(effort: &str) -> Option<ReasoningConfig> {
    Some(ReasoningConfig {
        effort: effort.to_string(),
        summary: "auto",
    })
}

#[test]
fn splits_effort_suffix_from_codex_variants() {
    for (slug, model, effort) in [
        ("gpt-5.3-codex-high", "gpt-5.3-codex", "high"),
        ("gpt-5.2-codex-low", "gpt-5.2-codex", "low"),
        ("gpt-5.2-medium", "gpt-5.2", "medium"),
        ("gpt-5.1-codex-max-xhigh", "gpt-5.1-codex-max", "xhigh"),
        ("gpt-5-mini-minimal", "gpt-5-mini", "minimal"),
        ("gpt-5-none", "gpt-5", "none"),
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, model, "slug {slug}");
        assert_eq!(resolved.reasoning, reasoning(effort), "slug {slug}");
        assert!(resolved.is_reasoning_model, "slug {slug}");
    }
}

#[test]
fn splits_effort_suffix_from_other_reasoning_families() {
    for (slug, model) in [
        ("o3-high", "o3"),
        ("o4-mini-low", "o4-mini"),
        ("o3-mini-high", "o3-mini"),
        ("gpt-oss-120b-medium", "gpt-oss-120b"),
        ("openai/gpt-5.2-low", "openai/gpt-5.2"),
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, model, "slug {slug}");
        assert!(resolved.reasoning.is_some(), "slug {slug}");
    }
}

#[test]
fn preserves_base_names_that_merely_look_suffixed() {
    for slug in [
        "gpt-5.3-codex",
        "gpt-5.2-codex",
        "gpt-5-mini",
        "gpt-5.1-codex-max",
        "o4-mini",
        "gpt-oss-20b",
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, slug);
        assert_eq!(resolved.reasoning, None, "slug {slug}");
        assert!(resolved.is_reasoning_model, "slug {slug}");
    }
}

#[test]
fn never_invents_reasoning_for_models_it_does_not_know() {
    for slug in [
        "gpt-4.1",
        "gpt-4o-high",
        "gpt-5-chat-latest",
        "gpt-5-chat-latest-high",
        "gpt-50-high",
        "llama-3.3-70b",
        "my-proxy-high",
        "deepseek-r1-low",
        "claude-sonnet-4-5",
        "o1-preview-high",
        "high",
        "",
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, slug, "slug {slug:?}");
        assert_eq!(resolved.reasoning, None, "slug {slug:?}");
        assert!(!resolved.is_reasoning_model, "slug {slug:?}");
    }
}

#[test]
fn does_not_rewrite_dashed_versions() {
    // `gpt-5-4` is a gpt-5 variant like any other: the name is sent as typed.
    let resolved = resolve_model("gpt-5-4-low");
    assert_eq!(resolved.model, "gpt-5-4");
    assert_eq!(resolved.reasoning, reasoning("low"));
}

#[test]
fn dated_snapshots_are_reasoning_models_and_keep_their_date() {
    for (slug, model, effort) in [
        ("gpt-5-2025-08-07", "gpt-5-2025-08-07", None),
        ("gpt-5-mini-2025-08-07", "gpt-5-mini-2025-08-07", None),
        ("o3-2025-04-16", "o3-2025-04-16", None),
        ("o4-mini-2025-04-16", "o4-mini-2025-04-16", None),
        ("o3-2025-04-16-high", "o3-2025-04-16", Some("high")),
        (
            "gpt-5.2-pro-2025-12-11-xhigh",
            "gpt-5.2-pro-2025-12-11",
            Some("xhigh"),
        ),
        (
            "openai/o3-2025-04-16-low",
            "openai/o3-2025-04-16",
            Some("low"),
        ),
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, model, "slug {slug}");
        assert_eq!(
            resolved.reasoning,
            effort.and_then(reasoning),
            "slug {slug}"
        );
        assert!(resolved.is_reasoning_model, "slug {slug}");
    }
}

#[test]
fn a_date_is_not_mistaken_for_part_of_a_longer_number() {
    for slug in [
        "gpt-4o-2024-08-06",
        "gpt-4.1-2025-04-14-high",
        "o3-2025-4-16",
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, slug, "slug {slug}");
        assert!(!resolved.is_reasoning_model, "slug {slug}");
    }
}

#[test]
fn unlisted_gpt5_and_oss_variants_are_assumed_to_reason() {
    for slug in [
        "gpt-5.1-codex-spark",
        "gpt-5-turbo",
        "gpt-5.4-thinking",
        "gpt-oss-120b",
        "gpt-oss-safeguard-20b",
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, slug, "slug {slug}");
        assert!(resolved.is_reasoning_model, "slug {slug}");

        let with_effort = resolve_model(&format!("{slug}-high"));
        assert_eq!(with_effort.model, slug, "slug {slug}");
        assert_eq!(with_effort.reasoning, reasoning("high"), "slug {slug}");
    }
}

#[test]
fn chat_models_are_not_reasoning_models() {
    for slug in [
        "gpt-5-chat-latest",
        "gpt-5.1-chat-latest",
        "gpt-5-chat",
        "gpt-5-chat-2025-08-07",
        "gpt-5-chat-latest-high",
    ] {
        let resolved = resolve_model(slug);
        assert_eq!(resolved.model, slug, "slug {slug}");
        assert_eq!(resolved.reasoning, None, "slug {slug}");
        assert!(!resolved.is_reasoning_model, "slug {slug}");
    }
}

#[test]
fn trims_surrounding_whitespace() {
    let resolved = resolve_model("  gpt-5.2-high\n");
    assert_eq!(resolved.model, "gpt-5.2");
    assert_eq!(resolved.reasoning, reasoning("high"));
}
