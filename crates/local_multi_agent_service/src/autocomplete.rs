use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::provider::{ProviderChatMessage, system_message, user_text_message};

pub const AUTOCOMPLETE_MODEL_ALIAS: &str = "auto-autocomplete";
pub const AUTOCOMPLETE_MAX_TOKENS: u32 = 256;

const DEFAULT_SYSTEM_PROMPT: &str = r#"/no_think
You are Warp's local shell autocomplete model.

Complete the user's current shell command.

Rules:
- Return exactly one shell command, and nothing else.
- The command must be a single line.
- Preserve the typed prefix exactly at the start of the command.
- Prefer matching history when it starts with the typed prefix.
- Prefer IDs, names, and other values from recent command output when the typed prefix is asking for one.
- For docker container commands like docker logs, docker exec, docker stop, docker restart, docker inspect, or docker rm, complete container IDs or names from recent docker ps output.
- Prefer completing the current argument, flag, subcommand, branch, path, or filename when context supports it.
- Prefer provided file candidates for path completions.
- If the typed prefix ends with a space and file candidates are relevant, append the first relevant file candidate.
- For package-name arguments like cargo -p or cargo --package, prefer the most specific package-looking candidate; if the candidate is under crates/<name>, complete with <name>.
- Do not reason out loud.
- Do not wrap the answer in Markdown, quotes, JSON, comments, channel tokens, or explanations.
- If there is not enough context, return the typed prefix unchanged."#;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutocompleteBlockContext {
    pub command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalCommandAutocompleteRequest {
    pub prefix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_blocks: Vec<AutocompleteBlockContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub file_candidates: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub system_prompt_override: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocalCommandAutocompleteResponse {
    pub commands: Vec<String>,
    pub most_likely_action: String,
    pub raw_output: String,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub parse_status: String,
}

impl LocalCommandAutocompleteResponse {
    pub fn from_command(command: String) -> LocalCommandAutocompleteResponse {
        LocalCommandAutocompleteResponse {
            commands: vec![command.clone()],
            most_likely_action: command,
            raw_output: String::new(),
            source: "deterministic".to_string(),
            parse_status: "skipped_model".to_string(),
        }
    }

    pub fn from_raw_output(
        raw_output: String,
        request: &LocalCommandAutocompleteRequest,
    ) -> LocalCommandAutocompleteResponse {
        let parsed_command = parse_autocomplete_command(&raw_output, &request.prefix);
        let parse_status =
            parse_autocomplete_status(&raw_output, &request.prefix, parsed_command.as_deref());
        let fallback_command = parsed_command
            .is_none()
            .then(|| fallback_command_from_context(request))
            .flatten();
        let source = if parsed_command.is_some() {
            "model"
        } else if fallback_command.is_some() {
            "fallback"
        } else {
            "none"
        };
        let command = parsed_command
            .or(fallback_command)
            .unwrap_or_default();
        let commands = if command.is_empty() {
            Vec::new()
        } else {
            vec![command.clone()]
        };
        LocalCommandAutocompleteResponse {
            commands,
            most_likely_action: command,
            raw_output,
            source: source.to_string(),
            parse_status: parse_status.to_string(),
        }
    }
}

pub(crate) fn deterministic_autocomplete_command(
    request: &LocalCommandAutocompleteRequest,
) -> Option<String> {
    matching_history_command(request).or_else(|| docker_container_command_candidate(request))
}

pub(crate) fn autocomplete_provider_messages(
    request: &LocalCommandAutocompleteRequest,
) -> Vec<ProviderChatMessage> {
    let system_prompt = request
        .system_prompt_override
        .as_deref()
        .filter(|prompt| !prompt.trim().is_empty())
        .unwrap_or(DEFAULT_SYSTEM_PROMPT);

    let context = json!({
        "typed_prefix": request.prefix,
        "cwd": request.cwd,
        "shell": request.shell,
        "platform": request.platform,
        "recent_blocks": request.recent_blocks,
        "history": request.history,
        "file_candidates": request.file_candidates,
    });

    vec![
        system_message(system_prompt.to_string()),
        user_text_message(format!(
            "Autocomplete this shell command from the provided JSON context:\n{}",
            serde_json::to_string_pretty(&context).unwrap_or_else(|_| context.to_string())
        )),
    ]
}

pub fn parse_autocomplete_command(raw_output: &str, prefix: &str) -> Option<String> {
    let raw_output = strip_harmony_thought(raw_output);
    candidates_from_json(raw_output)
        .into_iter()
        .chain(candidates_from_fence(raw_output))
        .chain(std::iter::once(raw_output.to_string()))
        .find_map(|candidate| normalize_candidate(&candidate, prefix))
}

fn candidates_from_json(raw_output: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<Value>(raw_output.trim()) else {
        return Vec::new();
    };

    let mut candidates = Vec::new();
    for key in ["command", "completion", "most_likely_action"] {
        if let Some(value) = value.get(key).and_then(Value::as_str) {
            candidates.push(value.to_string());
        }
    }
    if let Some(commands) = value.get("commands").and_then(Value::as_array) {
        candidates.extend(
            commands
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string),
        );
    }
    candidates
}

fn candidates_from_fence(raw_output: &str) -> Vec<String> {
    let trimmed = raw_output.trim();
    if !trimmed.starts_with("```") || !trimmed.ends_with("```") {
        return Vec::new();
    }

    let inner = trimmed.trim_start_matches("```").trim_end_matches("```");
    let inner = inner
        .strip_prefix("sh\n")
        .or_else(|| inner.strip_prefix("shell\n"))
        .or_else(|| inner.strip_prefix("bash\n"))
        .or_else(|| inner.strip_prefix("zsh\n"))
        .unwrap_or(inner);
    vec![inner.trim().to_string()]
}

fn normalize_candidate(candidate: &str, prefix: &str) -> Option<String> {
    let candidate = candidate.trim().trim_matches('"').trim_matches('\'').trim();
    if candidate.is_empty() || candidate.contains('\n') || candidate.contains('\r') {
        return None;
    }
    if !candidate.starts_with(prefix) {
        return None;
    }
    Some(candidate.to_string())
}

fn parse_autocomplete_status(
    raw_output: &str,
    prefix: &str,
    parsed_command: Option<&str>,
) -> &'static str {
    if parsed_command.is_some() {
        return "accepted";
    }
    let trimmed = raw_output.trim();
    if trimmed.is_empty() {
        return "empty";
    }
    if has_harmony_internal_without_final(raw_output) {
        return "harmony_internal_without_final";
    }
    if trimmed.contains('\n') || trimmed.contains('\r') {
        return "multiline";
    }
    let normalized = trimmed.trim_matches('"').trim_matches('\'').trim();
    if !normalized.starts_with(prefix) {
        return "non_prefix";
    }
    "invalid"
}

fn fallback_command_from_context(request: &LocalCommandAutocompleteRequest) -> Option<String> {
    if let Some(command) = deterministic_autocomplete_command(request) {
        return Some(command);
    }

    if let Some(command) = docker_container_command_candidate(request) {
        return Some(command);
    }

    if is_package_argument_prefix(&request.prefix) {
        return package_candidate(&request.file_candidates)
            .map(|package| format!("{}{}", request.prefix, package));
    }

    if request.prefix.ends_with(' ') {
        return request
            .file_candidates
            .iter()
            .find(|candidate| !candidate.trim().is_empty())
            .map(|candidate| format!("{}{}", request.prefix, candidate));
    }

    None
}

fn matching_history_command(request: &LocalCommandAutocompleteRequest) -> Option<String> {
    request
        .history
        .iter()
        .find(|command| command.starts_with(&request.prefix))
        .cloned()
}

fn docker_container_command_candidate(request: &LocalCommandAutocompleteRequest) -> Option<String> {
    let (token_start, partial) = last_token(&request.prefix);
    if !is_docker_container_argument_prefix(&request.prefix[..token_start]) {
        return None;
    }

    docker_ps_candidates(&request.recent_blocks)
        .into_iter()
        .find(|candidate| candidate.starts_with(partial))
        .map(|candidate| format!("{}{}", &request.prefix[..token_start], candidate))
}

fn last_token(value: &str) -> (usize, &str) {
    if value.ends_with(char::is_whitespace) {
        return (value.len(), "");
    }
    let start = value
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(index, ch)| index + ch.len_utf8())
        .unwrap_or(0);
    (start, &value[start..])
}

fn is_docker_container_argument_prefix(prefix_before_partial: &str) -> bool {
    let mut tokens = prefix_before_partial.split_whitespace();
    if tokens.next() != Some("docker") {
        return false;
    }
    let Some(subcommand) = tokens.next() else {
        return false;
    };
    matches!(
        subcommand,
        "logs" | "exec" | "stop" | "restart" | "inspect" | "rm" | "attach"
    )
}

fn docker_ps_candidates(recent_blocks: &[AutocompleteBlockContext]) -> Vec<String> {
    let mut ids = Vec::new();
    let mut names = Vec::new();
    for block in recent_blocks {
        let command = block.command.trim();
        if command != "docker ps" && !command.starts_with("docker ps ") {
            continue;
        }
        let Some(output) = &block.output else {
            continue;
        };
        for line in output.lines().skip(1) {
            let Some(container_id) = line.split_whitespace().next() else {
                continue;
            };
            if is_container_id(container_id) {
                ids.push(container_id.to_string());
            }
            if let Some(name) = line.split_whitespace().last()
                && !name.is_empty()
            {
                names.push(name.to_string());
            }
        }
    }
    ids.into_iter().chain(names).collect()
}

fn is_container_id(value: &str) -> bool {
    value.len() >= 4 && value.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn is_package_argument_prefix(prefix: &str) -> bool {
    let prefix = prefix.trim_end();
    prefix.ends_with(" -p")
        || prefix.ends_with(" --package")
        || prefix.ends_with(" --package-name")
}

fn package_candidate(file_candidates: &[String]) -> Option<String> {
    file_candidates
        .iter()
        .filter_map(|candidate| {
            candidate
                .strip_prefix("crates/")
                .and_then(|path| path.split('/').next())
                .filter(|name| !name.is_empty())
                .map(str::to_string)
        })
        .next()
        .or_else(|| {
            file_candidates
                .iter()
                .filter(|candidate| {
                    let candidate = candidate.as_str();
                    !candidate.trim().is_empty()
                        && candidate != "Cargo.toml"
                        && !candidate.ends_with(".toml")
                })
                .next()
                .map(|candidate| {
                    candidate
                        .rsplit('/')
                        .next()
                        .unwrap_or(candidate)
                        .to_string()
                })
        })
}

fn strip_harmony_thought(raw_output: &str) -> &str {
    for marker in ["<|channel>final", "<channel>final"] {
        if let Some((_, final_output)) = raw_output.rsplit_once(marker) {
            return final_output.trim();
        }
    }
    if has_harmony_internal_without_final(raw_output) {
        return "";
    }
    raw_output
}

fn has_harmony_internal_without_final(raw_output: &str) -> bool {
    !raw_output.contains("<|channel>final")
        && !raw_output.contains("<channel>final")
        && (raw_output.contains("<|channel>thought")
            || raw_output.contains("<channel>thought")
            || raw_output.contains("<|channel>analysis")
            || raw_output.contains("<channel>analysis"))
}

#[cfg(test)]
#[path = "autocomplete_tests.rs"]
mod tests;
