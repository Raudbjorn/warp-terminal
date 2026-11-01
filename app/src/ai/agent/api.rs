pub(crate) mod convert_conversation;
mod convert_from;
mod convert_to;
mod r#impl;
#[cfg(not(target_family = "wasm"))]
mod local_adapter;
#[cfg(not(target_family = "wasm"))]
mod local_route;

use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;

pub use ai::agent::convert::ConvertToAPITypeError;
use ai::api_keys::ApiKeyManager;
pub(crate) use convert_from::convert_user_query_mode;
pub use convert_from::{
    ConversionParams, ConvertAPIMessageToClientOutputMessage, MaybeAIAgentOutputMessage,
    MessageToAIAgentOutputMessageError, user_inputs_from_messages,
};
#[cfg_attr(target_family = "wasm", allow(unused_imports))]
pub(crate) use convert_to::{mcp_server_identity, serde_json_to_prost};
use futures_lite::Stream;
pub use r#impl::generate_multi_agent_output;
use mcp::TemplatableMCPServerInfo;
use serde::Serialize;
use warp_core::channel::{Channel, ChannelState};
use warp_core::execution_mode::AppExecutionMode;
use warp_core::features::FeatureFlag;
use warp_core::user_preferences::GetUserPreferences;
use warpui::{AppContext, EntityId, SingletonEntity as _};

use super::{AIAgentInput, MCPContext, MCPServer, RequestMetadata, ServerOutputId, Suggestions};
use crate::ai::agent::conversation::AIConversationId;
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::blocklist::{BlocklistAIPermissions, RequestInput, SessionContext};
use crate::ai::execution_profiles::AIExecutionProfileAppExt;
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::llms::{LLMId, LLMPreferences};
use crate::ai::mcp::TemplatableMCPServerManager;

use crate::server::server_api::AIApiError;

use crate::settings::AISettings;
use crate::terminal::safe_mode_settings::get_secret_obfuscation_mode;
use crate::workspaces::user_workspaces::{TeamScope, UserWorkspaces};

/// Unique, server-generated conversation-scoped token to be roundtripped to the API when sending
/// requests that follow-up within a given conversation.
#[derive(Serialize, Debug, Clone, PartialEq, Eq, Hash)]
pub struct ServerConversationToken(String);

impl ServerConversationToken {
    pub fn new(id: String) -> Self {
        Self(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn debug_link(&self) -> String {
        format!(
            "{}/debug/maa/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }

    pub fn debugging_payload(&self, request_id: Option<&ServerOutputId>) -> String {
        self.debugging_payload_for_channel(request_id, ChannelState::channel())
    }

    fn debugging_payload_for_channel(
        &self,
        request_id: Option<&ServerOutputId>,
        channel: Channel,
    ) -> String {
        if channel.is_dogfood() {
            match request_id {
                Some(request_id) => format!("{}?request={request_id}", self.debug_link()),
                None => self.debug_link(),
            }
        } else {
            match request_id {
                Some(request_id) => format!(
                    "{{\"request_id\":\"{request_id}\",\"conversation_id\":\"{}\"}}",
                    self.as_str()
                ),
                None => format!("{{\"conversation_id\":\"{}\"}}", self.as_str()),
            }
        }
    }

    pub fn conversation_link(&self) -> String {
        format!(
            "{}/conversation/{}",
            ChannelState::server_root_url(),
            self.as_str()
        )
    }
}

impl std::fmt::Display for ServerConversationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(test)]
#[path = "api_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "api/injected_attribution_tests.rs"]
mod injected_attribution_tests;
impl From<ServerConversationToken> for String {
    fn from(value: ServerConversationToken) -> Self {
        value.0
    }
}

// Conversions between AI ServerConversationToken and protocol ServerConversationToken
impl From<session_sharing_protocol::common::ServerConversationToken> for ServerConversationToken {
    fn from(token: session_sharing_protocol::common::ServerConversationToken) -> Self {
        Self(token.to_string())
    }
}

impl TryFrom<ServerConversationToken>
    for session_sharing_protocol::common::ServerConversationToken
{
    type Error = uuid::Error;

    fn try_from(token: ServerConversationToken) -> Result<Self, Self::Error> {
        token.as_str().parse()
    }
}

#[derive(Debug, Clone)]
pub struct RequestParams {
    pub input: Vec<AIAgentInput>,
    pub conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    pub ambient_agent_task_id: Option<AmbientAgentTaskId>,
    pub tasks: Vec<warp_multi_agent_api::Task>,
    pub existing_suggestions: Option<Suggestions>,
    pub metadata: Option<RequestMetadata>,
    pub session_context: SessionContext,
    pub model: LLMId,
    #[allow(unused)]
    pub coding_model: LLMId,
    pub cli_agent_model: LLMId,
    pub computer_use_model: LLMId,
    pub is_memory_enabled: bool,
    pub warp_drive_context_enabled: bool,
    pub context_window_limit: Option<u32>,
    pub mcp_context: Option<MCPContext>,
    pub planning_enabled: bool,
    should_redact_secrets: bool,

    /// Whether `scope`'s team allows members to use their own provider credentials.
    ///
    /// A later mutation of [`Self::api_keys`] must gate on this, not on plan entitlement alone:
    /// `api_keys` stays `Some(..)` for org-level credentials that survive the team's policy.
    pub member_byo_credentials_allowed: bool,
    /// User-provided API keys for AI providers (BYO API Key).
    pub api_keys: Option<warp_multi_agent_api::request::settings::ApiKeys>,
    /// User-provided custom model providers (BYOK endpoints).
    pub custom_model_providers:
        Option<warp_multi_agent_api::request::settings::CustomModelProviders>,
    /// User-defined custom model routers referenced by the current selection. Mirrors
    /// `custom_model_providers`: the selected model's `config_key` indexes into this
    /// registry. `None` when no custom router is selected.
    pub custom_model_routers: Option<warp_multi_agent_api::request::settings::CustomModelRouters>,
    pub openai_base_url: Option<String>,
    pub local_multi_agent_server_root_url: Option<String>,
    pub local_model_aliases: Option<String>,
    pub allow_use_of_warp_credits: bool,
    /// Asks the server not to attach the user's delegated ChatGPT token, so OpenAI requests run
    /// on Warp's key. Set for the rest of a conversation after a token-sharing failure.
    pub skip_chatgpt_subscription: bool,
    pub autonomy_level: warp_multi_agent_api::AutonomyLevel,
    pub isolation_level: warp_multi_agent_api::IsolationLevel,
    pub web_search_enabled: bool,
    pub computer_use_enabled: bool,
    pub ask_user_question_enabled: bool,
    pub research_agent_enabled: bool,
    pub orchestration_enabled: bool,
    pub supported_tools_override: Option<Vec<warp_multi_agent_api::ToolType>>,
    /// The conversation ID of the parent agent that spawned this child agent, if any.
    pub parent_agent_id: Option<String>,
    /// The display name for this agent (e.g. "Agent 1"), assigned by the orchestrator.
    pub agent_name: Option<String>,
}

pub type Event = Result<warp_multi_agent_api::ResponseEvent, Arc<AIApiError>>;

#[cfg(not(target_family = "wasm"))]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event> + Send + 'static>>;

// The WASM version of this type has no bound on `Send`, which is an unnecessary bound when
// targeting wasm because the browser is single-threaded (and we don't leverage WebWorkers for async
// execution in WoW).
#[cfg(target_family = "wasm")]
pub type ResponseStream = Pin<Box<dyn Stream<Item = Event>>>;

#[derive(Debug, Clone)]
pub struct ConversationData {
    pub id: AIConversationId,
    pub tasks: Vec<warp_multi_agent_api::Task>,
    pub server_conversation_token: Option<ServerConversationToken>,
    pub forked_from_conversation_token: Option<ServerConversationToken>,
    pub ambient_agent_task_id: Option<AmbientAgentTaskId>,
    pub existing_suggestions: Option<Suggestions>,
    pub use_warp_credits_instead_of_chatgpt: bool,
}

/// oh-my-warp: the AI tools registered by JS plugins (`warp.ai.registerTool`), built as `rmcp`
/// tools to inject into the model's MCP context. They are dispatched in-process by the tool-call
/// executor (`crate::ai::blocklist::action_model::execute::call_mcp_tool`). See PLUGIN_SPEC.md (M4).
#[cfg(feature = "plugin_host")]
fn plugin_ai_tools() -> Vec<rmcp::model::Tool> {
    crate::plugin::ai_tools::all()
        .into_iter()
        .filter_map(|tool| {
            let schema: serde_json::Value = serde_json::from_str(&tool.schema_json)
                .unwrap_or_else(|_| serde_json::json!({ "type": "object" }));
            serde_json::from_value(serde_json::json!({
                "name": tool.name,
                "description": tool.description,
                "inputSchema": schema,
            }))
            .ok()
        })
        .collect()
}

/// oh-my-warp: synthetic MCP "server" id grouping plugin-registered tools in the model request.
/// A fixed UUID so it never collides with a real installed MCP server.
#[cfg(feature = "plugin_host")]
const PLUGIN_TOOLS_SERVER_ID: &str = "0a0a0a0a-0b0b-4c0c-8d0d-0e0e0e0e0e0e";

impl RequestParams {
    #[cfg(test)]
    pub fn new_for_test() -> Self {
        Self {
            input: vec![],
            conversation_token: None,
            forked_from_conversation_token: None,
            ambient_agent_task_id: None,
            tasks: vec![],
            existing_suggestions: None,
            metadata: None,
            session_context: SessionContext::new_for_test(),
            model: LLMId::from("test-model"),
            coding_model: LLMId::from("test-model"),
            cli_agent_model: LLMId::from("test-model"),
            computer_use_model: LLMId::from("test-model"),
            is_memory_enabled: false,
            warp_drive_context_enabled: false,
            context_window_limit: None,
            mcp_context: None,
            planning_enabled: false,
            should_redact_secrets: false,
            member_byo_credentials_allowed: false,
            api_keys: None,
            custom_model_providers: None,
            custom_model_routers: None,
            openai_base_url: None,
            local_multi_agent_server_root_url: None,
            local_model_aliases: None,
            allow_use_of_warp_credits: false,
            skip_chatgpt_subscription: false,
            autonomy_level: Default::default(),
            isolation_level: Default::default(),
            web_search_enabled: false,
            computer_use_enabled: false,
            ask_user_question_enabled: false,
            research_agent_enabled: false,
            orchestration_enabled: false,
            supported_tools_override: None,
            parent_agent_id: None,
            agent_name: None,
        }
    }

    pub(crate) fn new(
        terminal_view_id: Option<EntityId>,
        session_context: SessionContext,
        request_input: &RequestInput,
        conversation: ConversationData,
        metadata: Option<RequestMetadata>,
        scope: &impl TeamScope,
        app: &AppContext,
    ) -> Self {
        let ai_settings = AISettings::as_ref(app);
        let is_memory_enabled = ai_settings.is_memory_enabled(app);
        let warp_drive_context_enabled = ai_settings.is_warp_drive_context_enabled(app);

        // Build MCP context - either grouped by server or flat lists based on feature flag
        let mcp_context = if FeatureFlag::MCPGroupedServerContext.is_enabled() {
            // Group MCP tools and resources by server
            let templatable_manager = TemplatableMCPServerManager::as_ref(app);

            let mut active_servers: Vec<&TemplatableMCPServerInfo> = templatable_manager
                .get_active_templatable_servers()
                .values()
                .copied()
                .collect();

            // If file-based MCP servers are enabled, add active servers in scope of
            // the user's current working directory
            if let Some(cwd) = session_context.current_working_directory() {
                active_servers.extend(
                    templatable_manager
                        .get_active_file_based_servers(Path::new(cwd), app)
                        .values(),
                );
            }

            // Include any ephemeral MCP servers started via the Oz CLI.
            active_servers.extend(
                templatable_manager
                    .get_active_cli_spawned_servers()
                    .values(),
            );

            // Include built-in Warp-hosted servers (e.g. the Factory MCP).
            active_servers.extend(templatable_manager.get_active_builtin_servers().values());

            #[cfg_attr(not(feature = "plugin_host"), allow(unused_mut))]
            let mut servers: Vec<MCPServer> = active_servers
                .into_iter()
                .map(|server| MCPServer {
                    name: server.name().to_string(),
                    description: server.description().unwrap_or_default().to_string(),
                    id: server.installation_id().to_string(),
                    warp_id: server.warp_id().unwrap_or_default().to_string(),
                    resources: server.resources().to_vec(),
                    tools: server.tools().to_vec(),
                })
                .collect();

            // oh-my-warp: surface plugin-registered AI tools as a synthetic MCP server so the
            // model can call them; the call is dispatched in-process (see `call_mcp_tool`).
            #[cfg(feature = "plugin_host")]
            {
                let plugin_tools = plugin_ai_tools();
                if !plugin_tools.is_empty() {
                    servers.push(MCPServer {
                        name: "oh-my-warp plugins".to_string(),
                        description: "Tools registered by oh-my-warp plugins".to_string(),
                        id: PLUGIN_TOOLS_SERVER_ID.to_string(),
                        warp_id: String::new(),
                        resources: vec![],
                        tools: plugin_tools,
                    });
                }
            }

            if servers.is_empty() {
                None
            } else {
                #[allow(deprecated)]
                Some(MCPContext {
                    resources: vec![],
                    tools: vec![],
                    servers,
                })
            }
        } else {
            // Flat lists of resources and tools
            let templatable_mcp_manager = TemplatableMCPServerManager::as_ref(app);
            let resources = templatable_mcp_manager
                .resources()
                .cloned()
                .collect::<Vec<_>>();
            #[cfg_attr(not(feature = "plugin_host"), allow(unused_mut))]
            let mut tools = templatable_mcp_manager.tools().cloned().collect::<Vec<_>>();
            // oh-my-warp: include plugin-registered AI tools (dispatched in-process).
            #[cfg(feature = "plugin_host")]
            tools.extend(plugin_ai_tools());

            #[allow(deprecated)]
            (!resources.is_empty() || !tools.is_empty()).then_some(MCPContext {
                resources,
                tools,
                servers: vec![],
            })
        };

        let should_redact_secrets = get_secret_obfuscation_mode(app).should_redact_secret();

        let user_workspaces = UserWorkspaces::as_ref(app);
        let api_key_manager = ApiKeyManager::as_ref(app);
        let active_profile =
            AIExecutionProfilesModel::as_ref(app).active_profile(terminal_view_id, app);
        let inference_profile_key = active_profile.inference_profile_key();
        // Bedrock and Gemini Enterprise are admin-configured host credentials rather than member
        // BYO keys, so they deliberately skip this gate.
        let member_byo_credentials_allowed = user_workspaces.are_member_byo_keys_allowed(scope);
        let is_byo_enabled =
            user_workspaces.is_byo_api_key_enabled(app) && member_byo_credentials_allowed;
        #[cfg(not(target_family = "wasm"))]
        let geap_binding =
            crate::ai::geap_credentials::current_geap_policy(scope, app).mint_binding();
        #[cfg(target_family = "wasm")]
        let geap_binding: Option<::ai::api_keys::GeapMintBinding> = None;
        let api_keys = api_key_manager.api_keys_for_profile_request(
            &inference_profile_key,
            is_byo_enabled,
            user_workspaces.is_aws_bedrock_credentials_enabled(scope, app),
            geap_binding,
        );
        let is_custom_inference_enabled = user_workspaces.is_byo_endpoint_enabled(app)
            && user_workspaces.are_member_byo_endpoints_allowed(scope);
        let custom_model_providers = api_key_manager.custom_model_providers_for_profile_request(
            &inference_profile_key,
            is_custom_inference_enabled,
        );
        let custom_model_routers = FeatureFlag::CustomModelRouters.is_enabled().then(|| {
            LLMPreferences::as_ref(app).custom_model_routers_for_request(
                &request_input.model_id,
                &request_input.coding_model_id,
            )
        });
        #[cfg(not(target_family = "wasm"))]
        let local_multi_agent_server_root_url =
            crate::local_multi_agent::LocalMultiAgentManager::global_root_url()
                .map(|url| url.to_string())
                .or_else(|| {
                    api_key_manager
                        .local_multi_agent_server_root_url_for_profile(&inference_profile_key)
                });
        #[cfg(target_family = "wasm")]
        let local_multi_agent_server_root_url =
            api_key_manager.local_multi_agent_server_root_url_for_profile(&inference_profile_key);
        let openai_base_url = api_key_manager.openai_base_url_for_profile(&inference_profile_key);
        let local_model_aliases = non_empty_string(
            api_key_manager.local_model_aliases_for_profile(&inference_profile_key),
        );
        let allow_use_of_warp_credits = *AISettings::as_ref(app).can_use_warp_credits_for_fallback;

        let app_execution_mode = AppExecutionMode::as_ref(app);
        let autonomy_level = if app_execution_mode.is_autonomous() {
            warp_multi_agent_api::AutonomyLevel::Unsupervised
        } else {
            warp_multi_agent_api::AutonomyLevel::Supervised
        };

        let isolation_level = if app_execution_mode.is_sandboxed() {
            warp_multi_agent_api::IsolationLevel::Sandbox
        } else {
            warp_multi_agent_api::IsolationLevel::None
        };

        let web_search_enabled =
            BlocklistAIPermissions::as_ref(app).get_web_search_enabled(app, terminal_view_id);
        let research_agent_enabled = app
            .private_user_preferences()
            .read_value("ResearchAgentEnabled")
            .ok()
            .flatten()
            .and_then(|s| s.parse().ok())
            .unwrap_or_default();
        let computer_use_requested = FeatureFlag::AgentModeComputerUse.is_enabled()
            && BlocklistAIPermissions::as_ref(app)
                .get_computer_use_setting(terminal_view_id, scope, app)
                .is_enabled()
            && (FeatureFlag::LocalComputerUse.is_enabled()
                || conversation.ambient_agent_task_id.is_some());
        let computer_use_supported = computer_use::is_supported_on_current_platform();
        let computer_use_enabled = computer_use_requested && computer_use_supported;
        let ask_user_question_enabled = BlocklistAIPermissions::as_ref(app)
            .get_ask_user_question_setting(app, terminal_view_id)
            != crate::ai::execution_profiles::AskUserQuestionPermission::Never;

        let orchestration_enabled = ai_settings.is_orchestration_enabled(app)
            && BlocklistAIPermissions::as_ref(app)
                .get_run_agents_setting(app, terminal_view_id)
                .is_enabled()
            && session_context
                .session_type()
                .as_ref()
                .is_none_or(|t| matches!(t, crate::terminal::model::session::SessionType::Local));

        // Reconcile the persisted override against the active base model's
        // current `LLMContextWindow` instead of trusting whatever was stored
        // last. If the active model isn't configurable or has been removed
        // server-side, drop the override; otherwise clamp it to the model's
        // current `[min, max]` range. This closes the window between an
        // in-flight model metadata refresh and the next request.
        let context_window_limit = active_profile.data().context_window_limit_for_request(app);

        Self {
            input: request_input.all_inputs().cloned().collect(),
            conversation_token: conversation.server_conversation_token,
            forked_from_conversation_token: conversation.forked_from_conversation_token,
            ambient_agent_task_id: conversation.ambient_agent_task_id,
            tasks: conversation.tasks,
            existing_suggestions: conversation.existing_suggestions,
            context_window_limit,
            metadata,
            session_context,
            model: request_input.model_id.clone(),
            coding_model: request_input.coding_model_id.clone(),
            cli_agent_model: request_input.cli_agent_model_id.clone(),
            computer_use_model: request_input.computer_use_model_id.clone(),
            is_memory_enabled,
            warp_drive_context_enabled,
            mcp_context,
            planning_enabled: true,
            should_redact_secrets,
            member_byo_credentials_allowed,
            api_keys,
            custom_model_providers,
            custom_model_routers,
            openai_base_url,
            local_multi_agent_server_root_url,
            local_model_aliases,
            allow_use_of_warp_credits,
            skip_chatgpt_subscription: conversation.use_warp_credits_instead_of_chatgpt,
            autonomy_level,
            isolation_level,
            web_search_enabled,
            computer_use_enabled,
            ask_user_question_enabled,
            research_agent_enabled,
            orchestration_enabled,
            supported_tools_override: request_input.supported_tools_override.clone(),
            parent_agent_id: None,
            agent_name: None,
        }
    }
}

fn non_empty_string(value: String) -> Option<String> {
    let value = value.trim().to_string();
    (!value.is_empty()).then_some(value)
}
