use std::collections::BTreeMap;

use anyhow::{Result, bail};

use crate::config::{DEFAULT_MODEL, non_empty_str};

pub fn default_model_aliases() -> BTreeMap<String, String> {
    [
        ("auto", DEFAULT_MODEL),
        ("auto-efficient", DEFAULT_MODEL),
        ("auto-coding", DEFAULT_MODEL),
        ("auto-reasoning", DEFAULT_MODEL),
        ("auto-autocomplete", DEFAULT_MODEL),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_owned(), value.to_owned()))
    .collect()
}

pub fn configured_model_aliases(raw_aliases: Option<&str>) -> Result<BTreeMap<String, String>> {
    let Some(raw_aliases) = raw_aliases.and_then(non_empty_str) else {
        return Ok(BTreeMap::new());
    };
    let value: serde_json::Value = serde_json::from_str(raw_aliases)?;
    let Some(object) = value.as_object() else {
        bail!(
            "LOCAL_MODEL_ALIASES must be a JSON object mapping Warp model IDs to provider model IDs."
        );
    };
    Ok(object
        .iter()
        .filter_map(|(key, value)| {
            value
                .as_str()
                .and_then(non_empty_str)
                .map(|value| (key.clone(), value.to_owned()))
        })
        .collect())
}

pub fn resolve_provider_model(
    openai_model: Option<&str>,
    warp_model: Option<&str>,
    local_model_aliases: Option<&str>,
) -> Result<String> {
    let requested_model = warp_model.and_then(non_empty_str);
    let mut aliases = default_model_aliases();
    aliases.extend(configured_model_aliases(local_model_aliases)?);

    if let Some(requested_model) = requested_model {
        if let Some(mapped) = aliases.get(requested_model) {
            return Ok(mapped.clone());
        }
        if !requested_model.starts_with("auto") {
            return Ok(requested_model.to_owned());
        }
    }

    if let Some(openai_model) = openai_model.and_then(non_empty_str) {
        return Ok(openai_model.to_owned());
    }

    Ok(DEFAULT_MODEL.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_override_builtin_model_mappings() {
        let model = resolve_provider_model(
            None,
            Some("auto-coding"),
            Some(r#"{"auto-coding":"local/model"}"#),
        )
        .unwrap();
        assert_eq!(model, "local/model");
    }

    #[test]
    fn autocomplete_alias_resolves_to_configured_model() {
        let model = resolve_provider_model(
            None,
            Some("auto-autocomplete"),
            Some(r#"{"auto-autocomplete":"diffusiongemma-26B-A4B-it"}"#),
        )
        .unwrap();
        assert_eq!(model, "diffusiongemma-26B-A4B-it");
    }

    #[test]
    fn non_auto_requested_model_is_preserved() {
        let model = resolve_provider_model(Some("fallback"), Some("provider/model"), None).unwrap();
        assert_eq!(model, "provider/model");
    }
}
