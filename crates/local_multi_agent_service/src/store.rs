use anyhow::{Context, Result};
use chrono::Utc;
use diesel::{
    QueryableByName, RunQueryDsl,
    connection::SimpleConnection,
    prelude::*,
    sql_query,
    sql_types::{Bool, Text},
    sqlite::SqliteConnection,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct IntegrationConfigPatch {
    pub base_prompt: Option<Option<String>>,
    pub environment_uid: Option<Option<String>>,
    pub mcp_servers_json: Option<Option<String>>,
    pub model_id: Option<Option<String>>,
    pub remove_mcp_server_names: Option<Vec<String>>,
    pub worker_host: Option<Option<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationRecord {
    pub provider_slug: String,
    pub enabled: bool,
    pub environment_uid: Option<String>,
    pub base_prompt: Option<String>,
    pub model_id: Option<String>,
    pub mcp_servers_json: String,
    pub worker_host: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConversationRecord {
    pub conversation_id: String,
    pub messages: Vec<serde_json::Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericStringObjectInput {
    pub client_id: Option<String>,
    pub format: String,
    pub serialized_model: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GenericStringObjectRecord {
    pub uid: String,
    pub client_id: Option<String>,
    pub format: String,
    pub serialized_model: String,
    pub revision_ts: String,
    pub metadata_last_updated_ts: String,
    pub permissions_last_updated_ts: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(QueryableByName)]
struct IntegrationRow {
    #[diesel(sql_type = Text)]
    provider_slug: String,
    #[diesel(sql_type = Bool)]
    enabled: bool,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    environment_uid: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    base_prompt: Option<String>,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    model_id: Option<String>,
    #[diesel(sql_type = Text)]
    mcp_servers_json: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    worker_host: Option<String>,
    #[diesel(sql_type = Text)]
    created_at: String,
    #[diesel(sql_type = Text)]
    updated_at: String,
}

#[derive(QueryableByName)]
struct AiConversationRow {
    #[diesel(sql_type = Text)]
    conversation_id: String,
    #[diesel(sql_type = Text)]
    messages_json: String,
    #[diesel(sql_type = Text)]
    created_at: String,
    #[diesel(sql_type = Text)]
    updated_at: String,
}

#[derive(QueryableByName)]
struct GenericStringObjectRow {
    #[diesel(sql_type = Text)]
    uid: String,
    #[diesel(sql_type = diesel::sql_types::Nullable<Text>)]
    client_id: Option<String>,
    #[diesel(sql_type = Text)]
    format: String,
    #[diesel(sql_type = Text)]
    serialized_model: String,
    #[diesel(sql_type = Text)]
    revision_ts: String,
    #[diesel(sql_type = Text)]
    metadata_last_updated_ts: String,
    #[diesel(sql_type = Text)]
    permissions_last_updated_ts: String,
    #[diesel(sql_type = Text)]
    created_at: String,
    #[diesel(sql_type = Text)]
    updated_at: String,
}

pub struct IntegrationStore {
    connection: SqliteConnection,
}

impl IntegrationStore {
    pub fn open(path: &str) -> Result<Self> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create parent directory for {path}"))?;
        }
        let mut connection = SqliteConnection::establish(path)
            .with_context(|| format!("failed to open local SQLite database at {path}"))?;
        diesel::sql_query("PRAGMA journal_mode = WAL").execute(&mut connection)?;
        let mut store = Self { connection };
        store.init()?;
        Ok(store)
    }

    fn init(&mut self) -> Result<()> {
        self.connection.batch_execute(
            r#"
            CREATE TABLE IF NOT EXISTS integrations (
              provider_slug TEXT PRIMARY KEY NOT NULL,
              enabled INTEGER NOT NULL,
              environment_uid TEXT,
              base_prompt TEXT,
              model_id TEXT,
              mcp_servers_json TEXT NOT NULL DEFAULT '{}',
              worker_host TEXT,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS ai_conversations (
              conversation_id TEXT PRIMARY KEY NOT NULL,
              messages_json TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS generic_string_objects (
              uid TEXT PRIMARY KEY NOT NULL,
              client_id TEXT,
              format TEXT NOT NULL,
              serialized_model TEXT NOT NULL,
              revision_ts TEXT NOT NULL,
              metadata_last_updated_ts TEXT NOT NULL,
              permissions_last_updated_ts TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL
            );
            "#,
        )?;
        self.repair_generic_string_object_formats()?;
        Ok(())
    }

    pub fn create_or_update(
        &mut self,
        integration_type: &str,
        enabled: bool,
        config: IntegrationConfigPatch,
        is_update: bool,
    ) -> Result<IntegrationRecord> {
        let provider_slug = normalize_provider_slug(integration_type)?;
        let existing = self.get(&provider_slug)?;
        let now = now();
        let is_update = is_update && existing.is_some();
        let created_at = existing
            .as_ref()
            .map(|record| record.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let record = IntegrationRecord {
            provider_slug,
            enabled,
            environment_uid: apply_nullable_string(
                existing
                    .as_ref()
                    .and_then(|record| record.environment_uid.clone()),
                config.environment_uid,
                is_update,
            ),
            base_prompt: apply_nullable_string(
                existing
                    .as_ref()
                    .and_then(|record| record.base_prompt.clone()),
                config.base_prompt,
                is_update,
            ),
            model_id: apply_nullable_string(
                existing.as_ref().and_then(|record| record.model_id.clone()),
                config.model_id,
                is_update,
            ),
            mcp_servers_json: merge_mcp_servers(
                existing
                    .as_ref()
                    .map(|record| record.mcp_servers_json.as_str()),
                config.mcp_servers_json.flatten().as_deref(),
                config
                    .remove_mcp_server_names
                    .as_deref()
                    .unwrap_or_default(),
                is_update,
            )?,
            worker_host: apply_nullable_string(
                existing
                    .as_ref()
                    .and_then(|record| record.worker_host.clone()),
                config.worker_host,
                is_update,
            ),
            created_at,
            updated_at: now,
        };

        sql_query(
            r#"
            INSERT INTO integrations (
              provider_slug, enabled, environment_uid, base_prompt, model_id,
              mcp_servers_json, worker_host, created_at, updated_at
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(provider_slug) DO UPDATE SET
              enabled = excluded.enabled,
              environment_uid = excluded.environment_uid,
              base_prompt = excluded.base_prompt,
              model_id = excluded.model_id,
              mcp_servers_json = excluded.mcp_servers_json,
              worker_host = excluded.worker_host,
              updated_at = excluded.updated_at
            "#,
        )
        .bind::<Text, _>(&record.provider_slug)
        .bind::<Bool, _>(record.enabled)
        .bind::<diesel::sql_types::Nullable<Text>, _>(&record.environment_uid)
        .bind::<diesel::sql_types::Nullable<Text>, _>(&record.base_prompt)
        .bind::<diesel::sql_types::Nullable<Text>, _>(&record.model_id)
        .bind::<Text, _>(&record.mcp_servers_json)
        .bind::<diesel::sql_types::Nullable<Text>, _>(&record.worker_host)
        .bind::<Text, _>(&record.created_at)
        .bind::<Text, _>(&record.updated_at)
        .execute(&mut self.connection)?;

        Ok(record)
    }

    pub fn get(&mut self, provider_slug: &str) -> Result<Option<IntegrationRecord>> {
        let provider_slug = normalize_provider_slug(provider_slug)?;
        let rows = sql_query(
            "SELECT provider_slug, enabled, environment_uid, base_prompt, model_id, mcp_servers_json, worker_host, created_at, updated_at FROM integrations WHERE provider_slug = ?",
        )
        .bind::<Text, _>(provider_slug)
        .load::<IntegrationRow>(&mut self.connection)?;
        Ok(rows.into_iter().next().map(row_to_integration))
    }

    pub fn list(
        &mut self,
        provider_slugs: &[String],
    ) -> Result<Vec<(String, Option<IntegrationRecord>)>> {
        provider_slugs
            .iter()
            .map(|provider_slug| {
                let normalized = normalize_provider_slug(provider_slug)?;
                let record = self.get(&normalized)?;
                Ok((normalized, record))
            })
            .collect()
    }

    pub fn providers_using_environment(&mut self, environment_id: &str) -> Result<Vec<String>> {
        #[derive(QueryableByName)]
        struct ProviderSlug {
            #[diesel(sql_type = Text)]
            provider_slug: String,
        }
        let rows = sql_query(
            "SELECT provider_slug FROM integrations WHERE environment_uid = ? ORDER BY provider_slug ASC",
        )
        .bind::<Text, _>(environment_id)
        .load::<ProviderSlug>(&mut self.connection)?;
        Ok(rows.into_iter().map(|row| row.provider_slug).collect())
    }

    pub fn upsert_ai_conversation(
        &mut self,
        conversation_id: &str,
        messages: &[serde_json::Value],
    ) -> Result<AiConversationRecord> {
        let conversation_id = normalize_conversation_id(conversation_id)?;
        let existing = self.get_ai_conversation(&conversation_id)?;
        let now = now();
        let record = AiConversationRecord {
            conversation_id,
            messages: messages.to_vec(),
            created_at: existing
                .as_ref()
                .map(|record| record.created_at.clone())
                .unwrap_or_else(|| now.clone()),
            updated_at: now,
        };
        let messages_json = serde_json::to_string(&record.messages)?;
        sql_query(
            r#"
            INSERT INTO ai_conversations (conversation_id, messages_json, created_at, updated_at)
            VALUES (?, ?, ?, ?)
            ON CONFLICT(conversation_id) DO UPDATE SET
              messages_json = excluded.messages_json,
              updated_at = excluded.updated_at
            "#,
        )
        .bind::<Text, _>(&record.conversation_id)
        .bind::<Text, _>(messages_json)
        .bind::<Text, _>(&record.created_at)
        .bind::<Text, _>(&record.updated_at)
        .execute(&mut self.connection)?;
        Ok(record)
    }

    pub fn get_ai_conversation(
        &mut self,
        conversation_id: &str,
    ) -> Result<Option<AiConversationRecord>> {
        let conversation_id = normalize_conversation_id(conversation_id)?;
        let rows = sql_query(
            "SELECT conversation_id, messages_json, created_at, updated_at FROM ai_conversations WHERE conversation_id = ?",
        )
        .bind::<Text, _>(conversation_id)
        .load::<AiConversationRow>(&mut self.connection)?;
        rows.into_iter()
            .next()
            .map(row_to_ai_conversation)
            .transpose()
    }

    pub fn list_ai_conversations(&mut self) -> Result<Vec<AiConversationRecord>> {
        sql_query(
            "SELECT conversation_id, messages_json, created_at, updated_at FROM ai_conversations ORDER BY updated_at ASC",
        )
        .load::<AiConversationRow>(&mut self.connection)?
        .into_iter()
        .map(row_to_ai_conversation)
        .collect()
    }

    pub fn create_generic_string_object(
        &mut self,
        input: GenericStringObjectInput,
    ) -> Result<GenericStringObjectRecord> {
        let now = now();
        let record = GenericStringObjectRecord {
            uid: format!("local-gso-{}", Uuid::new_v4()),
            client_id: input.client_id,
            format: normalize_generic_string_object_format(&input.format)?,
            serialized_model: input.serialized_model,
            revision_ts: now.clone(),
            metadata_last_updated_ts: now.clone(),
            permissions_last_updated_ts: now.clone(),
            created_at: now.clone(),
            updated_at: now,
        };
        self.insert_generic_string_object(&record)?;
        Ok(record)
    }

    pub fn bulk_create_generic_string_objects(
        &mut self,
        inputs: &[GenericStringObjectInput],
    ) -> Result<Vec<GenericStringObjectRecord>> {
        self.connection
            .transaction::<_, anyhow::Error, _>(|connection| {
                inputs
                    .iter()
                    .cloned()
                    .map(|input| {
                        let now = now();
                        let record = GenericStringObjectRecord {
                            uid: format!("local-gso-{}", Uuid::new_v4()),
                            client_id: input.client_id,
                            format: normalize_generic_string_object_format(&input.format)?,
                            serialized_model: input.serialized_model,
                            revision_ts: now.clone(),
                            metadata_last_updated_ts: now.clone(),
                            permissions_last_updated_ts: now.clone(),
                            created_at: now.clone(),
                            updated_at: now,
                        };
                        insert_generic_string_object(connection, &record)?;
                        Ok(record)
                    })
                    .collect()
            })
    }

    pub fn update_generic_string_object(
        &mut self,
        uid: &str,
        serialized_model: &str,
    ) -> Result<GenericStringObjectRecord> {
        if self.get_generic_string_object(uid)?.is_none() {
            let now = now();
            let record = GenericStringObjectRecord {
                uid: uid.to_owned(),
                client_id: None,
                format: infer_generic_string_object_format(serialized_model)
                    .unwrap_or("JsonMCPServer")
                    .to_owned(),
                serialized_model: serialized_model.to_owned(),
                revision_ts: now.clone(),
                metadata_last_updated_ts: now.clone(),
                permissions_last_updated_ts: now.clone(),
                created_at: now.clone(),
                updated_at: now,
            };
            self.insert_generic_string_object(&record)?;
            return Ok(record);
        }

        let now = now();
        let existing = self
            .get_generic_string_object(uid)?
            .context("generic string object disappeared")?;
        let format = infer_generic_string_object_format(serialized_model)
            .unwrap_or(&existing.format)
            .to_owned();
        sql_query(
            r#"
            UPDATE generic_string_objects
            SET format = ?, serialized_model = ?, revision_ts = ?, metadata_last_updated_ts = ?, updated_at = ?
            WHERE uid = ?
            "#,
        )
        .bind::<Text, _>(format)
        .bind::<Text, _>(serialized_model)
        .bind::<Text, _>(&now)
        .bind::<Text, _>(&now)
        .bind::<Text, _>(&now)
        .bind::<Text, _>(uid)
        .execute(&mut self.connection)?;
        self.get_generic_string_object(uid)?
            .with_context(|| format!("generic string object disappeared after update: {uid}"))
    }

    pub fn get_generic_string_object(
        &mut self,
        uid: &str,
    ) -> Result<Option<GenericStringObjectRecord>> {
        let rows = sql_query(
            "SELECT uid, client_id, format, serialized_model, revision_ts, metadata_last_updated_ts, permissions_last_updated_ts, created_at, updated_at FROM generic_string_objects WHERE uid = ?",
        )
        .bind::<Text, _>(uid)
        .load::<GenericStringObjectRow>(&mut self.connection)?;
        Ok(rows.into_iter().next().map(row_to_generic_string_object))
    }

    pub fn list_generic_string_objects(&mut self) -> Result<Vec<GenericStringObjectRecord>> {
        Ok(sql_query(
            "SELECT uid, client_id, format, serialized_model, revision_ts, metadata_last_updated_ts, permissions_last_updated_ts, created_at, updated_at FROM generic_string_objects ORDER BY created_at ASC, uid ASC",
        )
        .load::<GenericStringObjectRow>(&mut self.connection)?
        .into_iter()
        .map(row_to_generic_string_object)
        .collect())
    }

    fn insert_generic_string_object(&mut self, record: &GenericStringObjectRecord) -> Result<()> {
        insert_generic_string_object(&mut self.connection, record)
    }

    fn repair_generic_string_object_formats(&mut self) -> Result<()> {
        for mut record in self.list_generic_string_objects()? {
            if let Some(format) = infer_generic_string_object_format(&record.serialized_model)
                && format != record.format
            {
                record.format = format.to_owned();
                sql_query("UPDATE generic_string_objects SET format = ? WHERE uid = ?")
                    .bind::<Text, _>(record.format)
                    .bind::<Text, _>(record.uid)
                    .execute(&mut self.connection)?;
            }
        }
        Ok(())
    }
}

fn insert_generic_string_object(
    connection: &mut SqliteConnection,
    record: &GenericStringObjectRecord,
) -> Result<()> {
    sql_query(
        r#"
        INSERT INTO generic_string_objects (
          uid, client_id, format, serialized_model, revision_ts,
          metadata_last_updated_ts, permissions_last_updated_ts, created_at, updated_at
        )
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        "#,
    )
    .bind::<Text, _>(&record.uid)
    .bind::<diesel::sql_types::Nullable<Text>, _>(&record.client_id)
    .bind::<Text, _>(&record.format)
    .bind::<Text, _>(&record.serialized_model)
    .bind::<Text, _>(&record.revision_ts)
    .bind::<Text, _>(&record.metadata_last_updated_ts)
    .bind::<Text, _>(&record.permissions_last_updated_ts)
    .bind::<Text, _>(&record.created_at)
    .bind::<Text, _>(&record.updated_at)
    .execute(connection)?;
    Ok(())
}

fn row_to_integration(row: IntegrationRow) -> IntegrationRecord {
    IntegrationRecord {
        provider_slug: row.provider_slug,
        enabled: row.enabled,
        environment_uid: row.environment_uid,
        base_prompt: row.base_prompt,
        model_id: row.model_id,
        mcp_servers_json: row.mcp_servers_json,
        worker_host: row.worker_host,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn row_to_ai_conversation(row: AiConversationRow) -> Result<AiConversationRecord> {
    let messages = serde_json::from_str::<Vec<serde_json::Value>>(&row.messages_json)
        .with_context(|| {
            format!(
                "stored AI conversation {} does not contain a message array",
                row.conversation_id
            )
        })?;
    Ok(AiConversationRecord {
        conversation_id: row.conversation_id,
        messages,
        created_at: row.created_at,
        updated_at: row.updated_at,
    })
}

fn row_to_generic_string_object(row: GenericStringObjectRow) -> GenericStringObjectRecord {
    let format = infer_generic_string_object_format(&row.serialized_model)
        .unwrap_or(&row.format)
        .to_owned();
    GenericStringObjectRecord {
        uid: row.uid,
        client_id: row.client_id,
        format,
        serialized_model: row.serialized_model,
        revision_ts: row.revision_ts,
        metadata_last_updated_ts: row.metadata_last_updated_ts,
        permissions_last_updated_ts: row.permissions_last_updated_ts,
        created_at: row.created_at,
        updated_at: row.updated_at,
    }
}

fn normalize_provider_slug(value: &str) -> Result<String> {
    let slug = value.trim().to_ascii_lowercase();
    if slug.is_empty() {
        anyhow::bail!("integration_type is required");
    }
    Ok(slug)
}

fn normalize_conversation_id(value: &str) -> Result<String> {
    let conversation_id = value.trim();
    if conversation_id.is_empty() {
        anyhow::bail!("conversation_id is required");
    }
    Ok(conversation_id.to_owned())
}

fn apply_nullable_string(
    current: Option<String>,
    next: Option<Option<String>>,
    is_update: bool,
) -> Option<String> {
    match next {
        Some(Some(value)) if value.is_empty() => None,
        Some(value) => value,
        None if is_update => current,
        None => None,
    }
}

fn parse_mcp_map(json: Option<&str>) -> Result<Option<serde_json::Map<String, serde_json::Value>>> {
    let Some(json) = json.filter(|json| !json.trim().is_empty()) else {
        return Ok(None);
    };
    let value: serde_json::Value = serde_json::from_str(json)?;
    let Some(object) = value.as_object() else {
        anyhow::bail!("mcp_servers_json must encode a JSON object");
    };
    Ok(Some(object.clone()))
}

fn merge_mcp_servers(
    current_json: Option<&str>,
    patch_json: Option<&str>,
    remove_names: &[String],
    is_update: bool,
) -> Result<String> {
    let mut merged = if is_update {
        parse_mcp_map(current_json)?.unwrap_or_default()
    } else {
        serde_json::Map::new()
    };
    if let Some(patch) = parse_mcp_map(patch_json)? {
        merged.extend(patch);
    }
    for name in remove_names {
        merged.remove(name);
    }
    Ok(serde_json::Value::Object(merged).to_string())
}

fn normalize_generic_string_object_format(value: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() {
        anyhow::bail!("generic string object format is required");
    }
    Ok(value.to_owned())
}

fn infer_generic_string_object_format(serialized_model: &str) -> Option<&'static str> {
    let parsed = serde_json::from_str::<serde_json::Value>(serialized_model).ok()?;
    let object = parsed.as_object()?;
    if object.contains_key("storage_key") {
        Some("JsonPreference")
    } else if object.contains_key("template") || object.contains_key("json_template") {
        Some("JsonTemplatableMCPServer")
    } else if object.contains_key("is_default_profile")
        || object.contains_key("apply_code_diffs")
        || object.contains_key("mcp_allowlist")
        || object.contains_key("mcp_denylist")
    {
        Some("JsonAIExecutionProfile")
    } else if object.contains_key("transport_type") || object.contains_key("command") {
        Some("JsonMCPServer")
    } else {
        None
    }
}

fn now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sqlite_store_round_trips_records() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("local.sqlite");
        let mut store = IntegrationStore::open(path.to_str().unwrap()).unwrap();

        let record = store
            .create_or_update(
                "Linear",
                true,
                IntegrationConfigPatch {
                    mcp_servers_json: Some(Some(r#"{"local":{"command":"node"}}"#.to_owned())),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        assert_eq!(record.provider_slug, "linear");
        assert_eq!(
            store.get("linear").unwrap().unwrap().mcp_servers_json,
            r#"{"local":{"command":"node"}}"#
        );

        store
            .upsert_ai_conversation("conversation", &[serde_json::json!({"role":"user"})])
            .unwrap();
        assert_eq!(
            store
                .get_ai_conversation("conversation")
                .unwrap()
                .unwrap()
                .messages
                .len(),
            1
        );
    }
}
