use std::{collections::BTreeMap, fs, path::Path};

use anyhow::{Context, Result};

pub const DEFAULT_MODEL: &str = "Qwen/Qwen3.6-27B-FP8";
pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const DEFAULT_CONTEXT_WINDOW_TOKENS: u32 = 128 * 1024;
pub const DEFAULT_MAX_COMPLETION_TOKENS: u32 = 2048;
pub const SERVICE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
pub struct Config {
    pub host: String,
    pub port: u16,
    pub openai_api_key: Option<String>,
    pub openai_base_url: Option<String>,
    pub openai_model: Option<String>,
    pub local_model_aliases: Option<String>,
    pub local_model_list: String,
    pub local_enable_tools: bool,
    pub local_max_history_messages: usize,
    pub local_max_completion_tokens: u32,
    pub local_model_context_tokens: Option<String>,
    pub local_graphql_db_path: String,
    pub local_service_log_path: Option<String>,
    pub log_level: LogLevel,
    pub local_multi_agent_system_prompt: Option<String>,
    pub local_config_hash: Option<String>,
    pub warp_url_scheme: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }

    fn parse(value: Option<String>) -> Self {
        match value.as_deref().map(str::trim).map(str::to_ascii_lowercase) {
            Some(value) if value == "debug" => Self::Debug,
            Some(value) if value == "warn" => Self::Warn,
            Some(value) if value == "error" => Self::Error,
            _ => Self::Info,
        }
    }
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let port = std::env::var("PORT")
            .ok()
            .and_then(|value| value.trim().parse::<u16>().ok())
            .unwrap_or(8787);
        let local_max_history_messages = std::env::var("LOCAL_MAX_HISTORY_MESSAGES")
            .ok()
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(80)
            .max(4);
        let local_max_completion_tokens = std::env::var("LOCAL_MAX_COMPLETION_TOKENS")
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|value| *value > 0)
            .unwrap_or(DEFAULT_MAX_COMPLETION_TOKENS);

        let local_graphql_db_path = non_empty(std::env::var("LOCAL_GRAPHQL_DB_PATH").ok())
            .unwrap_or_else(|| {
                std::env::current_exe()
                    .ok()
                    .and_then(|path| {
                        path.parent()
                            .map(|parent| parent.join("local-graphql.sqlite"))
                    })
                    .unwrap_or_else(|| Path::new("local-graphql.sqlite").to_path_buf())
                    .to_string_lossy()
                    .into_owned()
            });

        let local_service_log_path = match non_empty(std::env::var("LOCAL_SERVICE_LOG_PATH").ok()) {
            Some(value) if matches!(value.to_ascii_lowercase().as_str(), "false" | "off" | "0") => {
                None
            }
            Some(value) => Some(value),
            None => Some(
                std::env::current_exe()
                    .ok()
                    .and_then(|path| path.parent().map(|parent| parent.join("local-service.log")))
                    .unwrap_or_else(|| Path::new("local-service.log").to_path_buf())
                    .to_string_lossy()
                    .into_owned(),
            ),
        };

        let warp_url_scheme =
            non_empty(std::env::var("WARP_URL_SCHEME").ok()).unwrap_or_else(|| "warp".to_owned());
        let warp_url_scheme = if is_valid_url_scheme(&warp_url_scheme) {
            warp_url_scheme
        } else {
            "warp".to_owned()
        };

        Ok(Self {
            host: non_empty(std::env::var("HOST").ok()).unwrap_or_else(|| "127.0.0.1".to_owned()),
            port,
            openai_api_key: non_empty(std::env::var("OPENAI_API_KEY").ok()),
            openai_base_url: non_empty(std::env::var("OPENAI_BASE_URL").ok()),
            openai_model: non_empty(std::env::var("OPENAI_MODEL").ok()),
            local_model_aliases: non_empty(std::env::var("LOCAL_MODEL_ALIASES").ok()),
            local_model_list: non_empty(std::env::var("LOCAL_MODEL_LIST").ok())
                .unwrap_or_else(|| DEFAULT_MODEL.to_owned()),
            local_enable_tools: std::env::var("LOCAL_ENABLE_TOOLS")
                .map(|value| value.trim().to_ascii_lowercase() != "false")
                .unwrap_or(true),
            local_max_history_messages,
            local_max_completion_tokens,
            local_model_context_tokens: non_empty(std::env::var("LOCAL_MODEL_CONTEXT_TOKENS").ok())
                .or_else(|| non_empty(std::env::var("LOCAL_CONTEXT_WINDOW_TOKENS").ok())),
            local_graphql_db_path,
            local_service_log_path,
            log_level: LogLevel::parse(std::env::var("LOG_LEVEL").ok()),
            local_multi_agent_system_prompt: non_empty(
                std::env::var("LOCAL_MULTI_AGENT_SYSTEM_PROMPT").ok(),
            ),
            local_config_hash: non_empty(std::env::var("LOCAL_CONFIG_HASH").ok()),
            warp_url_scheme,
        })
    }

    pub fn root_url(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }

    pub fn provider_base_url(&self, request_base_url: Option<&str>) -> String {
        trim_trailing_slash(
            request_base_url
                .and_then(non_empty_str)
                .or(self.openai_base_url.as_deref().and_then(non_empty_str))
                .unwrap_or(DEFAULT_BASE_URL),
        )
    }

    pub fn model_aliases(&self) -> Result<BTreeMap<String, String>> {
        crate::model::configured_model_aliases(self.local_model_aliases.as_deref())
    }
}

pub fn non_empty(value: Option<String>) -> Option<String> {
    value.and_then(|value| non_empty_str(&value).map(str::to_owned))
}

pub fn non_empty_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub fn trim_trailing_slash(value: &str) -> String {
    value.trim().trim_end_matches('/').to_owned()
}

pub fn load_dotenv(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let contents = match fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    for raw_line in contents.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let without_export = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((key, value)) = without_export.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if !is_valid_env_key(key) || std::env::var_os(key).is_some() {
            continue;
        }
        unsafe {
            std::env::set_var(key, unquote_env_value(value));
        }
    }

    Ok(())
}

fn unquote_env_value(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.len() < 2 {
        return trimmed.to_owned();
    }
    let first = trimmed.as_bytes()[0] as char;
    let last = trimmed.as_bytes()[trimmed.len() - 1] as char;
    if (first != '"' && first != '\'') || first != last {
        return trimmed.to_owned();
    }
    let inner = &trimmed[1..trimmed.len() - 1];
    if first == '"' {
        inner.replace("\\n", "\n").replace("\\\"", "\"")
    } else {
        inner.to_owned()
    }
}

fn is_valid_env_key(key: &str) -> bool {
    let mut chars = key.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn is_valid_url_scheme(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    first.is_ascii_alphabetic()
        && chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '+' | '.' | '-'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dotenv_preserves_existing_env() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".env");
        fs::write(
            &path,
            "OPENAI_BASE_URL=http://dotenv.example/v1\nQUOTED=\"a\\nb\"\n",
        )
        .unwrap();
        unsafe {
            std::env::set_var("OPENAI_BASE_URL", "http://shell.example/v1");
            std::env::remove_var("QUOTED");
        }

        load_dotenv(&path).unwrap();

        assert_eq!(
            std::env::var("OPENAI_BASE_URL").as_deref(),
            Ok("http://shell.example/v1")
        );
        assert_eq!(std::env::var("QUOTED").as_deref(), Ok("a\nb"));
    }
}
