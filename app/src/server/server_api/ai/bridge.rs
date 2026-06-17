//! oh-my-warp: in-process bridge that routes Warp's agent operations to a custom
//! gRPC harness host (see `examples/agent-grpc-backend`) when a gRPC backend is
//! selected via the backend selector ([`crate::util::agent_backends`]).
//!
//! Built in phases (see `examples/agent-grpc-backend/BRIDGE_SPEC.md` §7):
//!   * **Phase 1 (this file):** a *passthrough* decorator over `Arc<dyn AIClient>`,
//!     injected at [`super::super::ServerApi::get_ai_client`] and gated on the
//!     selected backend. It forwards every method to the real client — proving the
//!     seam compiles + injects with zero behavior change.
//!   * Phase 2 overrides the agent control ops (spawn/send/read/list/report/…) to
//!     call the gRPC harness host.
//!   * Phase 3 bridges the live event stream (`ServerApi::stream_agent_events`).
//!
//! Lives as a child module of `ai` so `use super::*` pulls in the `AIClient` trait
//! and every request/response type without re-importing them.

use super::*;
use crate::ai::ambient_agents::task::AmbientAgentTaskState;
use crate::util::agent_backends::GrpcTarget;
use std::sync::Arc;
use warp_agent_grpc::pb::{self, agent_service_client::AgentServiceClient};
use warp_agent_grpc::tonic::{self, transport::Channel};

/// Decorator over the real [`AIClient`]. The agent control ops are routed to the
/// gRPC harness host; every other method forwards to `inner`.
pub(crate) struct GrpcBridgeAIClient {
    inner: Arc<dyn AIClient>,
    target: GrpcTarget,
}

impl GrpcBridgeAIClient {
    pub(crate) fn new(inner: Arc<dyn AIClient>, target: GrpcTarget) -> Self {
        Self { inner, target }
    }

    /// Connects a fresh client to the harness host. (Phase 2 connects per call for
    /// simplicity; pooling a channel is a later optimization.)
    async fn client(&self) -> anyhow::Result<AgentServiceClient<Channel>> {
        let channel = Channel::from_shared(self.target.endpoint.clone())
            .map_err(|e| anyhow::anyhow!("invalid gRPC endpoint '{}': {e}", self.target.endpoint))?
            .connect()
            .await
            .map_err(|e| {
                anyhow::anyhow!("gRPC connect to '{}' failed: {e}", self.target.endpoint)
            })?;
        Ok(AgentServiceClient::new(channel))
    }

    /// Attaches the configured bearer token (if any) to a request.
    fn authed<T>(&self, mut req: tonic::Request<T>) -> tonic::Request<T> {
        if !self.target.token.is_empty() {
            if let Ok(val) = format!("Bearer {}", self.target.token).parse() {
                req.metadata_mut().insert("authorization", val);
            }
        }
        req
    }
}

/// Wraps `inner` with the gRPC bridge iff a gRPC backend is selected; otherwise
/// returns `inner` unchanged (built-in Warp behavior). Called from
/// `ServerApi::get_ai_client()`.
pub(crate) fn maybe_wrap(inner: Arc<dyn AIClient>) -> Arc<dyn AIClient> {
    match crate::util::agent_backends::selected_grpc_target() {
        Some(target) => {
            log::info!(
                "oh-my-warp: routing agent ops through gRPC bridge → {} (harness: {})",
                target.endpoint,
                if target.harness.is_empty() {
                    "<default>"
                } else {
                    &target.harness
                }
            );
            Arc::new(GrpcBridgeAIClient::new(inner, target))
        }
        None => inner,
    }
}

#[cfg_attr(not(target_family = "wasm"), async_trait)]
#[cfg_attr(target_family = "wasm", async_trait(?Send))]
impl AIClient for GrpcBridgeAIClient {
    // The agent control ops (spawn / send / read / list / report) route to the
    // gRPC harness host; every other method forwards to the real client. The live
    // event stream is bridged separately in Phase 3 (ServerApi::stream_agent_events).

    async fn generate_commands_from_natural_language(
        &self,
        prompt: String,
        ai_execution_context: Option<WarpAiExecutionContext>,
    ) -> Result<Vec<AIGeneratedCommand>, GenerateCommandsFromNaturalLanguageError> {
        self.inner
            .generate_commands_from_natural_language(prompt, ai_execution_context)
            .await
    }

    async fn generate_dialogue_answer(
        &self,
        transcript: Vec<TranscriptPart>,
        prompt: String,
        ai_execution_context: Option<WarpAiExecutionContext>,
    ) -> anyhow::Result<GenerateDialogueResult> {
        self.inner
            .generate_dialogue_answer(transcript, prompt, ai_execution_context)
            .await
    }

    async fn generate_metadata_for_command(
        &self,
        command: String,
    ) -> Result<GeneratedCommandMetadata, GeneratedCommandMetadataError> {
        self.inner.generate_metadata_for_command(command).await
    }

    async fn get_request_limit_info(&self) -> Result<RequestUsageInfo, anyhow::Error> {
        self.inner.get_request_limit_info().await
    }

    async fn rename_conversation(
        &self,
        conversation_id: String,
        title: String,
    ) -> anyhow::Result<RenameConversationResponse, anyhow::Error> {
        self.inner.rename_conversation(conversation_id, title).await
    }

    async fn get_feature_model_choices(&self) -> Result<ModelsByFeature, anyhow::Error> {
        self.inner.get_feature_model_choices().await
    }

    async fn get_available_harnesses(&self) -> Result<Vec<HarnessAvailability>, anyhow::Error> {
        self.inner.get_available_harnesses().await
    }

    async fn list_connected_self_hosted_workers(
        &self,
    ) -> Result<ListConnectedSelfHostedWorkersResponse, anyhow::Error> {
        self.inner.list_connected_self_hosted_workers().await
    }

    async fn get_free_available_models(
        &self,
        referrer: Option<String>,
    ) -> Result<ModelsByFeature, anyhow::Error> {
        self.inner.get_free_available_models(referrer).await
    }

    async fn update_merkle_tree(
        &self,
        embedding_config: EmbeddingConfig,
        nodes: Vec<IntermediateNode>,
    ) -> anyhow::Result<HashMap<NodeHash, bool>> {
        self.inner.update_merkle_tree(embedding_config, nodes).await
    }

    async fn generate_code_embeddings(
        &self,
        embedding_config: EmbeddingConfig,
        fragments: Vec<full_source_code_embedding::Fragment>,
        root_hash: NodeHash,
        repo_metadata: RepoMetadata,
    ) -> anyhow::Result<HashMap<ContentHash, bool>> {
        self.inner
            .generate_code_embeddings(embedding_config, fragments, root_hash, repo_metadata)
            .await
    }

    async fn provide_negative_feedback_response_for_ai_conversation(
        &self,
        conversation_id: String,
        request_ids: Vec<String>,
    ) -> anyhow::Result<i32, anyhow::Error> {
        self.inner
            .provide_negative_feedback_response_for_ai_conversation(conversation_id, request_ids)
            .await
    }

    async fn create_agent_task(
        &self,
        prompt: String,
        environment_uid: Option<String>,
        parent_run_id: Option<String>,
        config: Option<AgentConfigSnapshot>,
    ) -> anyhow::Result<AmbientAgentTaskId, anyhow::Error> {
        self.inner
            .create_agent_task(prompt, environment_uid, parent_run_id, config)
            .await
    }

    async fn update_agent_task(
        &self,
        task_id: AmbientAgentTaskId,
        task_state: Option<AgentTaskState>,
        session_id: Option<session_sharing_protocol::common::SessionId>,
        conversation_id: Option<String>,
        status_message: Option<TaskStatusUpdate>,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner
            .update_agent_task(
                task_id,
                task_state,
                session_id,
                conversation_id,
                status_message,
            )
            .await
    }

    async fn spawn_agent(
        &self,
        request: SpawnAgentRequest,
    ) -> anyhow::Result<SpawnAgentResponse, anyhow::Error> {
        let mut client = self.client().await?;
        let mode = match request.mode {
            UserQueryMode::Normal => pb::AgentMode::Normal,
            UserQueryMode::Plan => pb::AgentMode::Plan,
            UserQueryMode::Orchestrate => pb::AgentMode::Orchestrate,
        } as i32;
        let pb_req = pb::SpawnAgentRequest {
            prompt: request.prompt.unwrap_or_default(),
            mode,
            title: request.title.unwrap_or_default(),
            conversation_id: request.conversation_id.unwrap_or_default(),
            parent_run_id: request.parent_run_id.unwrap_or_default(),
            harness: self.target.harness.clone(),
        };
        let resp = client
            .spawn_agent(self.authed(tonic::Request::new(pb_req)))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC SpawnAgent failed: {e}"))?
            .into_inner();
        let task_id = resp.task_id.parse::<AmbientAgentTaskId>().map_err(|e| {
            anyhow::anyhow!(
                "harness host returned non-UUID task_id '{}': {e}",
                resp.task_id
            )
        })?;
        Ok(SpawnAgentResponse {
            task_id,
            run_id: resp.run_id,
            at_capacity: resp.at_capacity,
        })
    }

    async fn upload_local_handoff_snapshot(
        &self,
        request: UploadLocalHandoffSnapshotRequest,
    ) -> anyhow::Result<UploadLocalHandoffSnapshotResponse, anyhow::Error> {
        self.inner.upload_local_handoff_snapshot(request).await
    }

    async fn fork_conversation(
        &self,
        conversation_id: String,
        title: Option<String>,
    ) -> anyhow::Result<ForkConversationResponse, anyhow::Error> {
        self.inner.fork_conversation(conversation_id, title).await
    }

    async fn list_ambient_agent_tasks(
        &self,
        limit: i32,
        filter: TaskListFilter,
    ) -> anyhow::Result<Vec<AmbientAgentTask>, anyhow::Error> {
        self.inner.list_ambient_agent_tasks(limit, filter).await
    }

    async fn list_agent_runs_raw(
        &self,
        limit: i32,
        filter: TaskListFilter,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.list_agent_runs_raw(limit, filter).await
    }

    async fn get_ambient_agent_task(
        &self,
        task_id: &AmbientAgentTaskId,
    ) -> anyhow::Result<AmbientAgentTask, anyhow::Error> {
        // oh-my-warp: the run lives on the gRPC host, not Warp's cloud — so synthesize
        // a minimal "in progress" task rather than 404ing against Warp (which produced
        // the "conversation ended" tombstone). Step 1: state only; real host-backed
        // status (Succeeded/Failed, prompt, timestamps) follows once the host exposes it.
        let now = Utc::now();
        Ok(AmbientAgentTask {
            task_id: task_id.clone(),
            parent_run_id: None,
            title: format!("Local harness run ({})", self.target.harness),
            state: AmbientAgentTaskState::InProgress,
            prompt: String::new(),
            created_at: now,
            started_at: Some(now),
            updated_at: now,
            status_message: None,
            source: None,
            session_id: None,
            session_link: None,
            creator: None,
            executor: None,
            conversation_id: None,
            request_usage: None,
            is_sandbox_running: true,
            agent_config_snapshot: None,
            artifacts: vec![],
            last_event_sequence: None,
            children: vec![],
            // oh-my-warp: synthesized in-progress task; no run-time duration yet.
            run_time: None,
        })
    }

    async fn get_agent_run_raw(
        &self,
        task_id: &AmbientAgentTaskId,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.get_agent_run_raw(task_id).await
    }

    async fn submit_run_followup(
        &self,
        run_id: &AmbientAgentTaskId,
        request: RunFollowupRequest,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner.submit_run_followup(run_id, request).await
    }

    async fn get_scheduled_agent_history(
        &self,
        schedule_id: &str,
    ) -> anyhow::Result<ScheduledAgentHistory, anyhow::Error> {
        self.inner.get_scheduled_agent_history(schedule_id).await
    }

    async fn get_ai_conversation(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<(ConversationData, ServerAIConversationMetadata), anyhow::Error> {
        self.inner
            .get_ai_conversation(server_conversation_token)
            .await
    }

    async fn list_ai_conversation_metadata(
        &self,
        conversation_ids: Option<Vec<String>>,
    ) -> anyhow::Result<Vec<ServerAIConversationMetadata>> {
        self.inner
            .list_ai_conversation_metadata(conversation_ids)
            .await
    }

    async fn get_ai_conversation_format(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<AIAgentConversationFormat, anyhow::Error> {
        self.inner
            .get_ai_conversation_format(server_conversation_token)
            .await
    }

    async fn get_block_snapshot(
        &self,
        server_conversation_token: ServerConversationToken,
    ) -> anyhow::Result<SerializedBlock, anyhow::Error> {
        self.inner
            .get_block_snapshot(server_conversation_token)
            .await
    }

    async fn delete_ai_conversation(
        &self,
        server_conversation_token: String,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner
            .delete_ai_conversation(server_conversation_token)
            .await
    }

    async fn list_agents(&self) -> anyhow::Result<Vec<AgentResponse>, anyhow::Error> {
        self.inner.list_agents().await
    }

    async fn list_agents_raw(&self) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.list_agents_raw().await
    }

    async fn get_agent(&self, uid: &str) -> anyhow::Result<AgentResponse, anyhow::Error> {
        self.inner.get_agent(uid).await
    }

    async fn get_agent_raw(&self, uid: &str) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.get_agent_raw(uid).await
    }

    async fn create_agent(
        &self,
        request: CreateAgentRequest,
    ) -> anyhow::Result<AgentResponse, anyhow::Error> {
        self.inner.create_agent(request).await
    }

    async fn create_agent_raw(
        &self,
        request: CreateAgentRequest,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.create_agent_raw(request).await
    }

    async fn update_agent(
        &self,
        uid: &str,
        request: UpdateAgentRequest,
    ) -> anyhow::Result<AgentResponse, anyhow::Error> {
        self.inner.update_agent(uid, request).await
    }

    async fn update_agent_raw(
        &self,
        uid: &str,
        request: UpdateAgentRequest,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.update_agent_raw(uid, request).await
    }

    async fn delete_agent(&self, uid: &str) -> anyhow::Result<(), anyhow::Error> {
        self.inner.delete_agent(uid).await
    }

    async fn list_skills(
        &self,
        repo: Option<String>,
    ) -> anyhow::Result<Vec<AgentSkillItem>, anyhow::Error> {
        self.inner.list_skills(repo).await
    }

    async fn get_conversation_usage_history(
        &self,
        days: Option<i32>,
        limit: Option<i32>,
        last_updated_end_timestamp: Option<warp_graphql::scalars::Time>,
    ) -> Result<Vec<ConversationUsage>, anyhow::Error> {
        self.inner
            .get_conversation_usage_history(days, limit, last_updated_end_timestamp)
            .await
    }

    async fn post_agent_run_client_event(
        &self,
        run_id: &AmbientAgentTaskId,
        request: AgentRunClientEventRequest,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner
            .post_agent_run_client_event(run_id, request)
            .await
    }

    async fn cancel_ambient_agent_task(
        &self,
        task_id: &AmbientAgentTaskId,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner.cancel_ambient_agent_task(task_id).await
    }

    async fn get_task_git_credentials(
        &self,
        task_id: String,
        workload_token: String,
    ) -> anyhow::Result<Vec<GitCredential>, anyhow::Error> {
        self.inner
            .get_task_git_credentials(task_id, workload_token)
            .await
    }

    async fn get_task_attachments(
        &self,
        task_id: String,
    ) -> anyhow::Result<Vec<TaskAttachment>, anyhow::Error> {
        self.inner.get_task_attachments(task_id).await
    }

    async fn create_file_artifact_upload_target(
        &self,
        request: CreateFileArtifactUploadRequest,
    ) -> anyhow::Result<CreateFileArtifactUploadResponse, anyhow::Error> {
        self.inner.create_file_artifact_upload_target(request).await
    }

    async fn confirm_file_artifact_upload(
        &self,
        artifact_uid: String,
        checksum: String,
    ) -> anyhow::Result<FileArtifactRecord, anyhow::Error> {
        self.inner
            .confirm_file_artifact_upload(artifact_uid, checksum)
            .await
    }

    async fn get_artifact_download(
        &self,
        artifact_uid: &str,
    ) -> anyhow::Result<ArtifactDownloadResponse, anyhow::Error> {
        self.inner.get_artifact_download(artifact_uid).await
    }

    async fn prepare_attachments_for_upload(
        &self,
        task_id: &AmbientAgentTaskId,
        files: &[AttachmentFileInfo],
    ) -> anyhow::Result<PrepareAttachmentUploadsResponse, anyhow::Error> {
        self.inner
            .prepare_attachments_for_upload(task_id, files)
            .await
    }

    async fn download_task_attachments(
        &self,
        task_id: &AmbientAgentTaskId,
        attachment_ids: &[String],
    ) -> anyhow::Result<DownloadAttachmentsResponse, anyhow::Error> {
        self.inner
            .download_task_attachments(task_id, attachment_ids)
            .await
    }

    async fn get_handoff_snapshot_attachments(
        &self,
        task_id: &AmbientAgentTaskId,
    ) -> anyhow::Result<Vec<TaskAttachment>, anyhow::Error> {
        self.inner.get_handoff_snapshot_attachments(task_id).await
    }

    async fn send_agent_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> anyhow::Result<SendAgentMessageResponse, anyhow::Error> {
        let mut client = self.client().await?;
        let pb_req = pb::SendMessageRequest {
            to: request.to,
            subject: request.subject,
            body: request.body,
            sender_run_id: request.sender_run_id,
        };
        let resp = client
            .send_message(self.authed(tonic::Request::new(pb_req)))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC SendMessage failed: {e}"))?
            .into_inner();
        Ok(SendAgentMessageResponse {
            message_ids: resp.message_ids,
        })
    }

    async fn list_agent_messages(
        &self,
        run_id: &str,
        _request: ListAgentMessagesRequest,
    ) -> anyhow::Result<Vec<AgentMessageHeader>, anyhow::Error> {
        let mut client = self.client().await?;
        let pb_req = pb::ListMessagesRequest {
            run_id: run_id.to_string(),
            limit: 0,
            unread_only: false,
            since: String::new(),
        };
        let resp = client
            .list_messages(self.authed(tonic::Request::new(pb_req)))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC ListMessages failed: {e}"))?
            .into_inner();
        Ok(resp
            .messages
            .into_iter()
            .map(|m| AgentMessageHeader {
                message_id: m.message_id,
                sender_run_id: m.sender_run_id,
                subject: m.subject,
                sent_at: m.sent_at,
                delivered_at: None,
                read_at: (!m.read_at.is_empty()).then_some(m.read_at),
            })
            .collect())
    }

    async fn update_event_sequence_on_server(
        &self,
        run_id: &str,
        sequence: i64,
    ) -> anyhow::Result<(), anyhow::Error> {
        self.inner
            .update_event_sequence_on_server(run_id, sequence)
            .await
    }

    async fn report_agent_event(
        &self,
        run_id: &str,
        request: ReportAgentEventRequest,
    ) -> anyhow::Result<ReportAgentEventResponse, anyhow::Error> {
        let mut client = self.client().await?;
        let pb_req = pb::ReportEventRequest {
            run_id: run_id.to_string(),
            event_type: request.event_type,
            execution_id: request.execution_id.unwrap_or_default(),
            ref_id: request.ref_id.unwrap_or_default(),
        };
        let resp = client
            .report_event(self.authed(tonic::Request::new(pb_req)))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC ReportEvent failed: {e}"))?
            .into_inner();
        Ok(ReportAgentEventResponse {
            sequence: resp.sequence,
        })
    }

    async fn mark_message_delivered(&self, message_id: &str) -> anyhow::Result<(), anyhow::Error> {
        self.inner.mark_message_delivered(message_id).await
    }

    async fn read_agent_message(
        &self,
        message_id: &str,
    ) -> anyhow::Result<ReadAgentMessageResponse, anyhow::Error> {
        let mut client = self.client().await?;
        let pb_req = pb::ReadMessageRequest {
            message_id: message_id.to_string(),
        };
        let m = client
            .read_message(self.authed(tonic::Request::new(pb_req)))
            .await
            .map_err(|e| anyhow::anyhow!("gRPC ReadMessage failed: {e}"))?
            .into_inner()
            .message
            .unwrap_or_default();
        Ok(ReadAgentMessageResponse {
            message_id: m.message_id,
            sender_run_id: m.sender_run_id,
            subject: m.subject,
            body: m.body,
            sent_at: m.sent_at,
            delivered_at: None,
            read_at: (!m.read_at.is_empty()).then_some(m.read_at),
        })
    }

    async fn get_public_conversation(
        &self,
        conversation_id: &str,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.get_public_conversation(conversation_id).await
    }

    async fn get_run_conversation(
        &self,
        run_id: &str,
    ) -> anyhow::Result<serde_json::Value, anyhow::Error> {
        self.inner.get_run_conversation(run_id).await
    }

    async fn generate_code_review_content(
        &self,
        request: GenerateCodeReviewContentRequest,
    ) -> Result<GenerateCodeReviewContentResponse, anyhow::Error> {
        self.inner.generate_code_review_content(request).await
    }
}
