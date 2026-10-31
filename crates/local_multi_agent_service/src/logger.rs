use std::{
    fs::{self, OpenOptions},
    io::Write,
    sync::Arc,
};

use chrono::Utc;
use serde_json::{Map, Value, json};
use tokio::sync::Mutex;

use crate::config::LogLevel;

#[derive(Clone)]
pub struct Logger {
    level: LogLevel,
    file_path: Option<String>,
    file_lock: Arc<Mutex<()>>,
}

impl Logger {
    pub fn new(level: LogLevel, file_path: Option<String>) -> Self {
        Self {
            level,
            file_path,
            file_lock: Arc::new(Mutex::new(())),
        }
    }

    pub async fn log(&self, level: LogLevel, event: &str, fields: Value) {
        if level < self.level {
            return;
        }
        let mut payload = Map::new();
        payload.insert(
            "ts".to_owned(),
            json!(Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
        );
        payload.insert("level".to_owned(), json!(level.as_str()));
        payload.insert("event".to_owned(), json!(event));
        if let Value::Object(fields) = sanitize(fields) {
            payload.extend(fields);
        }
        let line = Value::Object(payload).to_string();
        match level {
            LogLevel::Error => eprintln!("{line}"),
            LogLevel::Warn => eprintln!("{line}"),
            _ => println!("{line}"),
        }
        self.write_file(line).await;
    }

    async fn write_file(&self, line: String) {
        let Some(path) = &self.file_path else {
            return;
        };
        let _guard = self.file_lock.lock().await;
        if let Some(parent) = std::path::Path::new(path).parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{line}");
        }
    }
}

fn sanitize(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .map(|(key, value)| {
                    if is_secret_key(&key) {
                        (
                            key,
                            if value.is_null() {
                                value
                            } else {
                                json!("[redacted]")
                            },
                        )
                    } else {
                        (key, sanitize(value))
                    }
                })
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sanitize).collect()),
        other => other,
    }
}

fn is_secret_key(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    key.contains("api_key")
        || key.contains("apikey")
        || key.contains("token")
        || key.contains("authorization")
        || key.contains("secret")
        || key.contains("password")
}
