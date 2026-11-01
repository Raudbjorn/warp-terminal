//! Tests for the proto ↔ neutral-model adapter: transcript rebuilding from
//! the task tree, tool schema/argument mapping, tool result rendering, and
//! golden `ResponseEvent` sequences from [`EventMapper`].

use local_agent::{AgentEvent, ChatMessage, LocalAgentError, LocalEndpointConfig, ToolCallRequest};
use serde_json::json;
use warp_multi_agent_api as api;

use super::*;

const ENDPOINT_URL: &str = "https://llm.example.com/v1";
const CONFIG_KEY: &str = "cfg-123";
const MODEL_SLUG: &str = "llama-3.3-70b";

fn route() -> LocalRoute {
    LocalRoute {
        endpoint: LocalEndpointConfig {
            base_url: ENDPOINT_URL.to_string(),
            api_key: "sk-test".to_string(),
            model_slug: MODEL_SLUG.to_string(),
            schema: local_agent::EndpointSchema::ChatCompletions,
        },
        config_key: CONFIG_KEY.to_string(),
    }
}

fn message(id: &str, content: api::message::Message) -> api::Message {
    api::Message {
        id: id.to_string(),
        task_id: "task-1".to_string(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        fetched_memories: vec![],
        message: Some(content),
    }
}

fn user_query_message(id: &str, query: &str) -> api::Message {
    message(
        id,
        api::message::Message::UserQuery(api::message::UserQuery {
            query: query.to_string(),
            ..Default::default()
        }),
    )
}

fn agent_output_message(id: &str, text: &str) -> api::Message {
    message(
        id,
        api::message::Message::AgentOutput(api::message::AgentOutput {
            text: text.to_string(),
        }),
    )
}

fn shell_tool_call_message(id: &str, call_id: &str, command: &str) -> api::Message {
    message(
        id,
        api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: call_id.to_string(),
            tool: Some(api::message::tool_call::Tool::RunShellCommand(
                api::message::tool_call::RunShellCommand {
                    command: command.to_string(),
                    ..Default::default()
                },
            )),
        }),
    )
}

fn shell_finished_result(output: &str, exit_code: i32) -> api::RunShellCommandResult {
    api::RunShellCommandResult {
        result: Some(api::run_shell_command_result::Result::CommandFinished(
            api::ShellCommandFinished {
                output: output.to_string(),
                exit_code,
                ..Default::default()
            },
        )),
        ..Default::default()
    }
}

fn shell_result_message(id: &str, call_id: &str, output: &str, exit_code: i32) -> api::Message {
    message(
        id,
        api::message::Message::ToolCallResult(api::message::ToolCallResult {
            tool_call_id: call_id.to_string(),
            context: None,
            result: Some(api::message::tool_call_result::Result::RunShellCommand(
                shell_finished_result(output, exit_code),
            )),
        }),
    )
}

fn root_task(messages: Vec<api::Message>) -> api::Task {
    api::Task {
        id: "task-1".to_string(),
        description: String::new(),
        dependencies: None,
        messages,
        summary: String::new(),
        server_data: String::new(),
    }
}

fn user_query_input(query: &str) -> api::request::input::user_inputs::user_input::Input {
    api::request::input::user_inputs::user_input::Input::UserQuery(api::request::input::UserQuery {
        query: query.to_string(),
        ..Default::default()
    })
}

fn shell_result_input(
    call_id: &str,
    output: &str,
    exit_code: i32,
) -> api::request::input::user_inputs::user_input::Input {
    api::request::input::user_inputs::user_input::Input::ToolCallResult(
        api::request::input::ToolCallResult {
            tool_call_id: call_id.to_string(),
            result: Some(
                api::request::input::tool_call_result::Result::RunShellCommand(
                    shell_finished_result(output, exit_code),
                ),
            ),
        },
    )
}

fn user_inputs(
    inputs: Vec<api::request::input::user_inputs::user_input::Input>,
) -> api::request::input::Type {
    api::request::input::Type::UserInputs(api::request::input::UserInputs {
        inputs: inputs
            .into_iter()
            .map(|input| api::request::input::user_inputs::UserInput { input: Some(input) })
            .collect(),
    })
}

fn request(
    tasks: Vec<api::Task>,
    input_type: api::request::input::Type,
    conversation_id: &str,
) -> api::Request {
    api::Request {
        task_context: Some(api::request::TaskContext { tasks }),
        input: Some(api::request::Input {
            context: None,
            r#type: Some(input_type),
        }),
        settings: Some(api::request::Settings {
            model_config: Some(api::request::settings::ModelConfig {
                base: CONFIG_KEY.to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }),
        metadata: Some(api::request::Metadata {
            conversation_id: conversation_id.to_string(),
            ..Default::default()
        }),
        existing_suggestions: None,
        mcp_context: None,
    }
}

fn json_schema_struct() -> prost_types::Struct {
    prost_types::Struct {
        fields: [(
            "type".to_string(),
            prost_types::Value {
                kind: Some(prost_types::value::Kind::StringValue("object".to_string())),
            },
        )]
        .into_iter()
        .collect(),
    }
}

fn mcp_context() -> api::request::McpContext {
    #[allow(deprecated)]
    api::request::McpContext {
        resources: vec![],
        tools: vec![],
        servers: vec![api::request::mcp_context::McpServer {
            name: "Linear".to_string(),
            description: "Issue tracker".to_string(),
            id: "srv-1".to_string(),
            identity: None,
            resources: vec![],
            tools: vec![api::request::mcp_context::McpTool {
                name: "create_issue".to_string(),
                description: "Creates an issue".to_string(),
                input_schema: Some(json_schema_struct()),
            }],
        }],
    }
}

/// Flattens a `ResponseEvent` sequence into human-readable kind labels so
/// golden tests can assert ordering without exhaustively matching payloads.
fn event_kinds(events: &[Event]) -> Vec<String> {
    let mut kinds = Vec::new();
    for event in events {
        match event {
            Err(_) => kinds.push("error".to_string()),
            Ok(response_event) => match response_event.r#type.as_ref().expect("event type") {
                api::response_event::Type::Init(_) => kinds.push("init".to_string()),
                api::response_event::Type::Finished(_) => kinds.push("finished".to_string()),
                api::response_event::Type::ClientActions(actions) => {
                    for action in &actions.actions {
                        let kind = match action.action.as_ref().expect("action type") {
                            api::client_action::Action::BeginTransaction(_) => "begin_transaction",
                            api::client_action::Action::CommitTransaction(_) => {
                                "commit_transaction"
                            }
                            api::client_action::Action::RollbackTransaction(_) => {
                                "rollback_transaction"
                            }
                            api::client_action::Action::CreateTask(_) => "create_task",
                            api::client_action::Action::AddMessagesToTask(_) => "add_messages",
                            api::client_action::Action::AppendToMessageContent(_) => "append",
                            api::client_action::Action::UpdateTaskMessage(_) => "update",
                            _ => "other_action",
                        };
                        kinds.push(kind.to_string());
                    }
                }
            },
        }
    }
    kinds
}

fn added_messages(events: &[Event]) -> Vec<api::Message> {
    events
        .iter()
        .filter_map(|event| event.as_ref().ok())
        .filter_map(|event| match event.r#type.as_ref() {
            Some(api::response_event::Type::ClientActions(actions)) => Some(&actions.actions),
            _ => None,
        })
        .flatten()
        .filter_map(|action| match action.action.as_ref() {
            Some(api::client_action::Action::AddMessagesToTask(add)) => Some(add.messages.clone()),
            _ => None,
        })
        .flatten()
        .collect()
}

mod transcript {
    use super::*;

    #[test]
    fn rebuilds_history_from_task_tree_and_appends_new_query() {
        let req = request(
            vec![root_task(vec![
                user_query_message("m1", "first question"),
                agent_output_message("m2", "first answer"),
            ])],
            user_inputs(vec![user_query_input("second question")]),
            "conv-1",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        assert!(
            matches!(&plan.transcript[0], ChatMessage::System(prompt) if !prompt.is_empty()),
            "transcript must start with a non-empty system prompt"
        );
        assert_eq!(
            &plan.transcript[1..],
            &[
                ChatMessage::User("first question".to_string()),
                ChatMessage::Assistant {
                    text: "first answer".to_string(),
                    tool_calls: vec![],
                    replay_items: vec![],
                },
                ChatMessage::User("second question".to_string()),
            ]
        );
        assert!(!plan.is_first_turn);
        assert_eq!(plan.conversation_id, "conv-1");
        assert_eq!(plan.root_task_id, "task-1");
        assert_eq!(plan.config_key, CONFIG_KEY);
    }

    #[test]
    fn maps_historic_tool_call_and_result_to_assistant_and_tool_messages() {
        let req = request(
            vec![root_task(vec![
                user_query_message("m1", "list files"),
                shell_tool_call_message("m2", "call_1", "ls"),
                shell_result_message("m3", "call_1", "file.txt\n", 0),
            ])],
            user_inputs(vec![user_query_input("now what?")]),
            "conv-1",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        let assistant = plan
            .transcript
            .iter()
            .find_map(|m| match m {
                ChatMessage::Assistant { tool_calls, .. } if !tool_calls.is_empty() => {
                    Some(tool_calls[0].clone())
                }
                _ => None,
            })
            .expect("assistant message with the historic tool call");
        assert_eq!(assistant.id, "call_1");
        assert_eq!(assistant.name, "run_shell_command");
        assert_eq!(assistant.arguments["command"], json!("ls"));

        let tool_message = plan
            .transcript
            .iter()
            .find_map(|m| match m {
                ChatMessage::Tool {
                    tool_call_id,
                    content,
                } if tool_call_id == "call_1" => Some(content.clone()),
                _ => None,
            })
            .expect("tool result message for call_1");
        assert!(
            tool_message.contains("file.txt"),
            "missing output: {tool_message}"
        );
    }

    #[test]
    fn fresh_tool_call_results_become_tool_messages_and_are_echoed() {
        let req = request(
            vec![root_task(vec![
                user_query_message("m1", "list files"),
                shell_tool_call_message("m2", "call_1", "ls"),
            ])],
            user_inputs(vec![shell_result_input("call_1", "file.txt\n", 0)]),
            "conv-1",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        assert!(
            matches!(
                plan.transcript.last(),
                Some(ChatMessage::Tool { tool_call_id, .. }) if tool_call_id == "call_1"
            ),
            "fresh tool result must be the last transcript message, got {:?}",
            plan.transcript.last()
        );
        assert!(
            plan.input_echo_messages.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::ToolCallResult(r)) if r.tool_call_id == "call_1"
            )),
            "tool result must be echoed into the task tree for the next turn"
        );
    }

    #[test]
    fn first_turn_mints_conversation_and_task_ids() {
        let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");

        let plan = plan_turn(&req, &route()).expect("plan");

        assert!(plan.is_first_turn);
        assert!(!plan.root_task_id.is_empty());
        // The client parses conversation tokens as UUIDs in places, so the
        // minted id must be one.
        assert!(
            uuid::Uuid::parse_str(&plan.conversation_id).is_ok(),
            "conversation id must be a UUID, got {:?}",
            plan.conversation_id
        );
    }

    #[test]
    fn rejects_unsupported_system_inputs() {
        let req = request(
            vec![],
            api::request::input::Type::CodeReview(api::request::input::CodeReview::default()),
            "",
        );

        assert!(matches!(
            plan_turn(&req, &route()),
            Err(AdapterError::UnsupportedInput(_))
        ));
    }
}

mod tools {
    use super::*;

    fn plan_for(req: &api::Request) -> TurnPlan {
        plan_turn(req, &route()).expect("plan")
    }

    fn simple_request() -> api::Request {
        request(vec![], user_inputs(vec![user_query_input("hi")]), "")
    }

    #[test]
    fn builtin_definitions_cover_core_tools() {
        let plan = plan_for(&simple_request());
        let names: Vec<String> = plan
            .tools
            .definitions()
            .into_iter()
            .map(|d| d.name)
            .collect();

        for expected in [
            "run_shell_command",
            "read_files",
            "edit_files",
            "grep",
            "file_glob",
        ] {
            assert!(
                names.contains(&expected.to_string()),
                "missing tool {expected}: {names:?}"
            );
        }
        for definition in plan.tools.definitions() {
            assert_eq!(
                definition.parameters["type"],
                json!("object"),
                "{} must have an object schema",
                definition.name
            );
            assert!(!definition.description.is_empty());
        }
    }

    #[test]
    fn maps_run_shell_command_arguments_to_proto() {
        let plan = plan_for(&simple_request());

        let tool_call = plan
            .tools
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_9".to_string(),
                name: "run_shell_command".to_string(),
                arguments: json!({ "command": "echo hi" }),
            })
            .expect("mapping");

        assert_eq!(tool_call.tool_call_id, "call_9");
        let Some(api::message::tool_call::Tool::RunShellCommand(shell)) = tool_call.tool else {
            panic!("expected RunShellCommand, got {:?}", tool_call.tool);
        };
        assert_eq!(shell.command, "echo hi");
        // v1 always waits for completion: no long-running command support.
        assert_eq!(
            shell.wait_until_complete_value,
            Some(api::message::tool_call::run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(true))
        );
    }

    #[test]
    fn maps_edit_files_arguments_to_apply_file_diffs() {
        let plan = plan_for(&simple_request());

        let tool_call = plan
            .tools
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_2".to_string(),
                name: "edit_files".to_string(),
                arguments: json!({
                    "summary": "rename variable",
                    "diffs": [{
                        "file_path": "src/main.rs",
                        "search": "let a = 1;",
                        "replace": "let b = 1;",
                    }],
                }),
            })
            .expect("mapping");

        let Some(api::message::tool_call::Tool::ApplyFileDiffs(apply)) = tool_call.tool else {
            panic!("expected ApplyFileDiffs, got {:?}", tool_call.tool);
        };
        assert_eq!(apply.summary, "rename variable");
        assert_eq!(apply.diffs.len(), 1);
        assert_eq!(apply.diffs[0].file_path, "src/main.rs");
        assert_eq!(apply.diffs[0].search, "let a = 1;");
        assert_eq!(apply.diffs[0].replace, "let b = 1;");
    }

    #[test]
    fn projects_mcp_tools_and_routes_calls_back_to_the_server() {
        let mut req = simple_request();
        req.mcp_context = Some(mcp_context());
        let plan = plan_for(&req);

        let mcp_definition = plan
            .tools
            .definitions()
            .into_iter()
            .find(|d| d.name.starts_with("mcp__") && d.name.ends_with("__create_issue"))
            .expect("projected MCP tool definition");
        assert_eq!(mcp_definition.parameters["type"], json!("object"));

        let tool_call = plan
            .tools
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_3".to_string(),
                name: mcp_definition.name.clone(),
                arguments: json!({ "title": "bug" }),
            })
            .expect("mapping");

        let Some(api::message::tool_call::Tool::CallMcpTool(call)) = tool_call.tool else {
            panic!("expected CallMcpTool, got {:?}", tool_call.tool);
        };
        assert_eq!(call.server_id, "srv-1");
        assert_eq!(call.name, "create_issue");
        let args = call.args.expect("args struct");
        assert_eq!(
            args.fields["title"].kind,
            Some(prost_types::value::Kind::StringValue("bug".to_string()))
        );
    }

    #[test]
    fn unknown_tool_names_are_an_error() {
        let plan = plan_for(&simple_request());

        assert!(matches!(
            plan.tools.to_proto_tool_call(&ToolCallRequest {
                id: "call_4".to_string(),
                name: "made_up_tool".to_string(),
                arguments: json!({}),
            }),
            Err(AdapterError::UnknownTool(_))
        ));
    }

    #[test]
    fn regular_turns_reject_suggest_prompt_which_they_did_not_offer() {
        let plan = plan_for(&simple_request());

        assert!(matches!(
            plan.tools.to_proto_tool_call(&ToolCallRequest {
                id: "call_5".to_string(),
                name: "suggest_prompt".to_string(),
                arguments: json!({ "prompt": "Run the tests" }),
            }),
            Err(AdapterError::UnknownTool(_))
        ));
    }
}

mod mcp_tools {
    use super::*;

    fn mcp_tool(
        name: &str,
        description: &str,
        schema: Option<serde_json::Value>,
    ) -> api::request::mcp_context::McpTool {
        api::request::mcp_context::McpTool {
            name: name.to_string(),
            description: description.to_string(),
            input_schema: schema.map(|schema| {
                let serde_json::Value::Object(map) = schema else {
                    panic!("test schema must be an object");
                };
                json_map_to_prost_struct(&map)
            }),
        }
    }

    fn mcp_server(
        name: &str,
        id: &str,
        tools: Vec<api::request::mcp_context::McpTool>,
    ) -> api::request::mcp_context::McpServer {
        api::request::mcp_context::McpServer {
            name: name.to_string(),
            description: String::new(),
            id: id.to_string(),
            identity: None,
            resources: vec![],
            tools,
        }
    }

    fn registry_for(servers: Vec<api::request::mcp_context::McpServer>) -> ToolRegistry {
        #[allow(deprecated)]
        let context = api::request::McpContext {
            resources: vec![],
            tools: vec![],
            servers,
        };
        ToolRegistry::new(Some(&context), &[])
    }

    fn mcp_definitions(registry: &ToolRegistry) -> Vec<local_agent::ToolDefinition> {
        registry
            .definitions()
            .into_iter()
            .filter(|definition| definition.name.starts_with("mcp__"))
            .collect()
    }

    fn call_for(registry: &ToolRegistry, name: &str) -> api::message::tool_call::CallMcpTool {
        let tool_call = registry
            .to_proto_tool_call(&ToolCallRequest {
                id: "call".to_string(),
                name: name.to_string(),
                arguments: json!({}),
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::CallMcpTool(call)) = tool_call.tool else {
            panic!("expected CallMcpTool, got {:?}", tool_call.tool);
        };
        call
    }

    fn valid_function_name(name: &str) -> bool {
        name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }

    #[test]
    fn schema_without_type_or_properties_is_completed() {
        let schema = normalize_mcp_input_schema(
            Some(json!({ "description": "no arguments", "required": [] })),
            "tool",
        );

        assert_eq!(
            schema,
            json!({
                "description": "no arguments",
                "required": [],
                "type": "object",
                "properties": {},
            })
        );
    }

    #[test]
    fn missing_schema_means_no_arguments() {
        assert_eq!(
            normalize_mcp_input_schema(None, "tool"),
            json!({ "type": "object", "properties": {} })
        );
    }

    #[test]
    fn schema_marker_is_dropped_but_refs_defs_and_required_pass_through() {
        let schema = normalize_mcp_input_schema(
            Some(json!({
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "label": { "$ref": "#/$defs/Label" } },
                "required": ["label"],
                "$defs": { "Label": { "type": "string", "nullable": true } },
                "additionalProperties": false,
            })),
            "tool",
        );

        assert_eq!(
            schema,
            json!({
                "type": "object",
                "properties": { "label": { "$ref": "#/$defs/Label" } },
                "required": ["label"],
                "$defs": { "Label": { "type": "string", "nullable": true } },
                "additionalProperties": false,
            })
        );
    }

    #[test]
    fn nullable_object_type_collapses_to_object() {
        let schema = normalize_mcp_input_schema(
            Some(
                json!({ "type": ["object", "null"], "properties": { "a": { "type": "string" } } }),
            ),
            "tool",
        );

        assert_eq!(schema["type"], json!("object"));
        assert_eq!(schema["properties"]["a"]["type"], json!("string"));
    }

    #[test]
    fn schemas_that_do_not_describe_an_object_fall_back_to_no_arguments() {
        for schema in [
            json!({ "type": "string", "description": "a bare string" }),
            json!(["not", "a", "schema"]),
            json!(true),
        ] {
            assert_eq!(
                normalize_mcp_input_schema(Some(schema.clone()), "tool"),
                json!({ "type": "object", "properties": {} }),
                "{schema}"
            );
        }
    }

    #[test]
    fn projected_definitions_always_have_object_schemas_and_descriptions() {
        let registry = registry_for(vec![mcp_server(
            "Docs",
            "srv-1",
            vec![
                mcp_tool("ping", "", None),
                mcp_tool(
                    "search",
                    "Searches",
                    Some(json!({ "properties": { "q": { "type": "string" } } })),
                ),
            ],
        )]);

        let definitions = mcp_definitions(&registry);

        assert_eq!(definitions.len(), 2);
        for definition in &definitions {
            assert_eq!(definition.parameters["type"], json!("object"));
            assert!(definition.parameters["properties"].is_object());
            assert!(!definition.description.is_empty());
        }
        let ping = definitions
            .iter()
            .find(|definition| definition.name.ends_with("__ping"))
            .expect("ping");
        assert!(
            ping.description.contains("ping") && ping.description.contains("Docs"),
            "fallback description should name the tool and server: {}",
            ping.description
        );
    }

    #[test]
    fn same_named_tools_on_two_servers_stay_separately_reachable() {
        let registry = registry_for(vec![
            mcp_server("Linear", "srv-1", vec![mcp_tool("create_issue", "A", None)]),
            mcp_server("Linear", "srv-2", vec![mcp_tool("create_issue", "B", None)]),
        ]);

        let definitions = mcp_definitions(&registry);

        assert_eq!(definitions.len(), 2, "no tool may be silently dropped");
        assert_ne!(definitions[0].name, definitions[1].name);
        let mut routed: Vec<String> = definitions
            .iter()
            .map(|definition| call_for(&registry, &definition.name).server_id)
            .collect();
        routed.sort();
        assert_eq!(routed, vec!["srv-1".to_string(), "srv-2".to_string()]);
    }

    #[test]
    fn long_server_names_do_not_make_distinct_tools_collide() {
        let long_server = "s".repeat(80);
        let registry = registry_for(vec![mcp_server(
            &long_server,
            "srv-1",
            vec![mcp_tool("alpha", "", None), mcp_tool("beta", "", None)],
        )]);

        let definitions = mcp_definitions(&registry);

        assert_eq!(definitions.len(), 2);
        for definition in &definitions {
            assert!(valid_function_name(&definition.name), "{}", definition.name);
        }
        let mut tools: Vec<String> = definitions
            .iter()
            .map(|definition| call_for(&registry, &definition.name).name)
            .collect();
        tools.sort();
        assert_eq!(tools, vec!["alpha".to_string(), "beta".to_string()]);
    }

    #[test]
    fn non_ascii_tool_names_remain_distinct_and_valid() {
        let registry = registry_for(vec![mcp_server(
            "搜索",
            "srv-1",
            vec![mcp_tool("查询", "", None), mcp_tool("删除", "", None)],
        )]);

        let definitions = mcp_definitions(&registry);

        assert_eq!(definitions.len(), 2);
        for definition in &definitions {
            assert!(valid_function_name(&definition.name), "{}", definition.name);
        }
    }

    #[test]
    fn a_tool_listed_twice_for_one_server_is_registered_once() {
        let registry = registry_for(vec![mcp_server(
            "Docs",
            "srv-1",
            vec![mcp_tool("ping", "", None), mcp_tool("ping", "", None)],
        )]);

        assert_eq!(mcp_definitions(&registry).len(), 1);
    }

    #[test]
    fn server_identity_round_trips_onto_the_proto_call() {
        let identity = api::McpServerIdentity {
            display_name: "Linear".to_string(),
            ..Default::default()
        };
        let registry = registry_for(vec![api::request::mcp_context::McpServer {
            identity: Some(identity.clone()),
            ..mcp_server("Linear", "srv-1", vec![mcp_tool("create_issue", "", None)])
        }]);
        let name = mcp_definitions(&registry)[0].name.clone();

        let call = call_for(&registry, &name);

        assert_eq!(call.server_identity, Some(identity));
        assert_eq!(call.server_id, "srv-1");
        assert_eq!(call.name, "create_issue");
    }

    #[test]
    fn historic_calls_to_a_disambiguated_tool_reuse_its_current_name() {
        let registry = registry_for(vec![
            mcp_server("Linear", "srv-1", vec![mcp_tool("create_issue", "", None)]),
            mcp_server("Linear", "srv-2", vec![mcp_tool("create_issue", "", None)]),
        ]);

        for definition in mcp_definitions(&registry) {
            let proto = registry
                .to_proto_tool_call(&ToolCallRequest {
                    id: "call".to_string(),
                    name: definition.name.clone(),
                    arguments: json!({ "title": "bug" }),
                })
                .expect("mapping");

            let request = registry
                .to_request_tool_call(&proto)
                .expect("history rebuild");

            assert_eq!(request.name, definition.name);
            assert_eq!(request.arguments, json!({ "title": "bug" }));
        }
    }

    #[test]
    fn integral_schema_numbers_are_not_rendered_as_floats() {
        let registry = registry_for(vec![mcp_server(
            "Docs",
            "srv-1",
            vec![mcp_tool(
                "search",
                "",
                Some(
                    json!({ "properties": { "limit": { "type": "integer", "minimum": 1, "default": 10, "ratio": 0.5 } } }),
                ),
            )],
        )]);

        let limit = &mcp_definitions(&registry)[0].parameters["properties"]["limit"];

        assert_eq!(limit["minimum"], json!(1));
        assert_eq!(limit["default"], json!(10));
        assert_eq!(limit["ratio"], json!(0.5));
        assert!(limit["default"].is_i64(), "{limit}");
    }
}

mod shell_and_glob_tools {
    use super::*;

    fn registry() -> ToolRegistry {
        ToolRegistry::new(None, &[])
    }

    fn shell_call(arguments: serde_json::Value) -> api::message::tool_call::RunShellCommand {
        let tool_call = registry()
            .to_proto_tool_call(&ToolCallRequest {
                id: "call".to_string(),
                name: "run_shell_command".to_string(),
                arguments,
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::RunShellCommand(shell)) = tool_call.tool else {
            panic!("expected RunShellCommand, got {:?}", tool_call.tool);
        };
        shell
    }

    #[test]
    fn shell_schema_declares_the_pager_and_safety_flags() {
        let definition = registry()
            .definitions()
            .into_iter()
            .find(|definition| definition.name == "run_shell_command")
            .expect("shell tool");

        for property in ["command", "is_read_only", "is_risky", "uses_pager"] {
            assert!(
                definition.parameters["properties"][property].is_object(),
                "missing {property}"
            );
            assert!(
                definition.parameters["required"]
                    .as_array()
                    .is_some_and(|required| required.contains(&json!(property))),
                "{property} must be required"
            );
        }
    }

    #[test]
    fn shell_uses_pager_reaches_the_client_action() {
        let shell = shell_call(json!({ "command": "git log", "uses_pager": true }));

        assert!(shell.uses_pager);
    }

    #[test]
    fn omitted_risk_flags_default_to_the_safe_side() {
        let mutating = shell_call(json!({ "command": "rm -rf build" }));
        let read_only = shell_call(json!({ "command": "ls", "is_read_only": true }));
        let explicit = shell_call(json!({ "command": "make", "is_risky": false }));

        assert!(
            !mutating.is_read_only && mutating.is_risky,
            "unknown must be risky"
        );
        assert!(read_only.is_read_only && !read_only.is_risky);
        assert!(!explicit.is_risky, "an explicit flag wins over the default");
        assert!(!mutating.uses_pager);
    }

    #[test]
    fn historic_shell_calls_keep_their_pager_flag() {
        let request = registry()
            .to_request_tool_call(&api::message::ToolCall {
                tool_call_id: "call_1".to_string(),
                tool: Some(api::message::tool_call::Tool::RunShellCommand(
                    api::message::tool_call::RunShellCommand {
                        command: "git diff".to_string(),
                        uses_pager: true,
                        ..Default::default()
                    },
                )),
            })
            .expect("history rebuild");

        assert_eq!(request.arguments["uses_pager"], json!(true));
    }

    #[test]
    fn file_glob_limits_reach_the_client_action_and_history() {
        let tool_call = registry()
            .to_proto_tool_call(&ToolCallRequest {
                id: "call".to_string(),
                name: "file_glob".to_string(),
                arguments: json!({
                    "patterns": ["*.rs"],
                    "search_dir": "/repo",
                    "max_matches": 50,
                    "max_depth": 3,
                    "min_depth": 1,
                }),
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::FileGlobV2(glob)) = tool_call.tool.clone() else {
            panic!("expected FileGlobV2, got {:?}", tool_call.tool);
        };
        assert_eq!(
            (glob.max_matches, glob.max_depth, glob.min_depth),
            (50, 3, 1)
        );

        let request = registry()
            .to_request_tool_call(&tool_call)
            .expect("history rebuild");
        assert_eq!(request.arguments["max_matches"], json!(50));
        assert_eq!(request.arguments["max_depth"], json!(3));
        assert_eq!(request.arguments["min_depth"], json!(1));
    }

    #[test]
    fn file_glob_limits_are_optional() {
        let tool_call = registry()
            .to_proto_tool_call(&ToolCallRequest {
                id: "call".to_string(),
                name: "file_glob".to_string(),
                arguments: json!({ "patterns": ["*.rs"] }),
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::FileGlobV2(glob)) = tool_call.tool else {
            panic!("expected FileGlobV2");
        };

        assert_eq!(
            (glob.max_matches, glob.max_depth, glob.min_depth),
            (0, 0, 0)
        );
    }
}

mod ask_user_question {
    use super::*;

    fn registry(supported: &[api::ToolType]) -> ToolRegistry {
        let supported: Vec<i32> = supported.iter().map(|tool| *tool as i32).collect();
        ToolRegistry::new(None, &supported)
    }

    fn ask_arguments() -> serde_json::Value {
        json!({
            "questions": [
                {
                    "question": "Which database?",
                    "options": ["Postgres", "SQLite"],
                    "recommended_option_index": 1,
                    "type": "single_select",
                },
                {
                    "question": "Which features?",
                    "options": ["Auth", "Billing", "Search"],
                    "recommended_option_index": 2,
                    "type": "multi_select",
                },
            ]
        })
    }

    fn ask_call(registry: &ToolRegistry) -> api::AskUserQuestion {
        let tool_call = registry
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_ask".to_string(),
                name: "ask_user_question".to_string(),
                arguments: ask_arguments(),
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::AskUserQuestion(ask)) = tool_call.tool else {
            panic!("expected AskUserQuestion, got {:?}", tool_call.tool);
        };
        ask
    }

    fn multiple_choice(
        question: &api::ask_user_question::Question,
    ) -> &api::ask_user_question::MultipleChoice {
        let Some(api::ask_user_question::question::QuestionType::MultipleChoice(choice)) =
            question.question_type.as_ref()
        else {
            panic!("expected a multiple choice question");
        };
        choice
    }

    fn answer_result() -> api::AskUserQuestionResult {
        use api::ask_user_question_result::{
            AnswerItem, Result as AskResult, Success, answer_item,
        };
        api::AskUserQuestionResult {
            result: Some(AskResult::Success(Success {
                answers: vec![
                    AnswerItem {
                        question_id: "q1".to_string(),
                        answer: Some(answer_item::Answer::MultipleChoice(
                            answer_item::MultipleChoiceAnswer {
                                selected_options: vec!["SQLite".to_string()],
                                other_text: String::new(),
                            },
                        )),
                    },
                    AnswerItem {
                        question_id: "q2".to_string(),
                        answer: Some(answer_item::Answer::Skipped(())),
                    },
                    AnswerItem {
                        question_id: "q3".to_string(),
                        answer: Some(answer_item::Answer::MultipleChoice(
                            answer_item::MultipleChoiceAnswer {
                                selected_options: vec![],
                                other_text: "something else".to_string(),
                            },
                        )),
                    },
                ],
            })),
        }
    }

    #[test]
    fn the_tool_is_only_offered_when_the_client_declares_support() {
        let names = |registry: &ToolRegistry| -> Vec<String> {
            registry
                .definitions()
                .into_iter()
                .map(|definition| definition.name)
                .collect()
        };

        assert!(!names(&registry(&[])).contains(&"ask_user_question".to_string()));
        assert!(
            !names(&registry(&[api::ToolType::Grep])).contains(&"ask_user_question".to_string())
        );
        assert!(
            names(&registry(&[api::ToolType::AskUserQuestion]))
                .contains(&"ask_user_question".to_string())
        );
    }

    #[test]
    fn a_call_to_the_tool_when_it_was_not_offered_is_an_unknown_tool() {
        assert!(matches!(
            registry(&[]).to_proto_tool_call(&ToolCallRequest {
                id: "call_ask".to_string(),
                name: "ask_user_question".to_string(),
                arguments: ask_arguments(),
            }),
            Err(AdapterError::UnknownTool(_))
        ));
    }

    #[test]
    fn arguments_map_to_numbered_multiple_choice_questions() {
        let ask = ask_call(&registry(&[api::ToolType::AskUserQuestion]));

        assert_eq!(ask.questions.len(), 2);
        assert_eq!(ask.questions[0].question_id, "q1");
        assert_eq!(ask.questions[1].question_id, "q2");
        let single = multiple_choice(&ask.questions[0]);
        assert!(!single.is_multiselect && single.supports_other);
        assert_eq!(single.recommended_option_index, 1);
        assert_eq!(
            single
                .options
                .iter()
                .map(|o| o.label.as_str())
                .collect::<Vec<_>>(),
            vec!["Postgres", "SQLite"]
        );
        let multi = multiple_choice(&ask.questions[1]);
        assert!(multi.is_multiselect);
        assert_eq!(
            multi.recommended_option_index, -1,
            "multi-select questions carry no recommendation"
        );
    }

    #[test]
    fn a_question_without_a_recommendation_uses_the_wire_sentinel() {
        let registry = registry(&[api::ToolType::AskUserQuestion]);
        let tool_call = registry
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_ask".to_string(),
                name: "ask_user_question".to_string(),
                arguments: json!({ "questions": [{ "question": "Ok?", "options": ["yes", "no"], "type": "single_select" }] }),
            })
            .expect("mapping");
        let Some(api::message::tool_call::Tool::AskUserQuestion(ask)) = tool_call.tool else {
            panic!("expected AskUserQuestion");
        };

        assert_eq!(
            multiple_choice(&ask.questions[0]).recommended_option_index,
            -1
        );
    }

    #[test]
    fn no_questions_is_a_bad_argument_error() {
        assert!(matches!(
            registry(&[api::ToolType::AskUserQuestion]).to_proto_tool_call(&ToolCallRequest {
                id: "call_ask".to_string(),
                name: "ask_user_question".to_string(),
                arguments: json!({ "questions": [] }),
            }),
            Err(AdapterError::BadToolArguments { .. })
        ));
    }

    #[test]
    fn historic_questions_rebuild_into_the_same_arguments() {
        let registry = registry(&[api::ToolType::AskUserQuestion]);
        let tool_call = registry
            .to_proto_tool_call(&ToolCallRequest {
                id: "call_ask".to_string(),
                name: "ask_user_question".to_string(),
                arguments: ask_arguments(),
            })
            .expect("mapping");

        let request = registry
            .to_request_tool_call(&tool_call)
            .expect("history rebuild");

        assert_eq!(request.name, "ask_user_question");
        assert_eq!(
            request.arguments["questions"][0],
            ask_arguments()["questions"][0]
        );
        // The multi-select recommendation is dropped on the way in.
        assert_eq!(
            request.arguments["questions"][1].get("recommended_option_index"),
            None
        );
    }

    #[test]
    fn answers_render_as_structured_json() {
        let text = render_ask_user_question_result(&answer_result());

        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["status"], json!("success"));
        assert_eq!(value["answers"][0]["question_id"], json!("q1"));
        assert_eq!(value["answers"][0]["selected_options"], json!(["SQLite"]));
        assert_eq!(value["answers"][1]["skipped"], json!(true));
        assert_eq!(value["answers"][2]["other_text"], json!("something else"));
    }

    #[test]
    fn errors_render_as_structured_json() {
        let text = render_ask_user_question_result(&api::AskUserQuestionResult {
            result: Some(api::ask_user_question_result::Result::Error(
                api::ask_user_question_result::Error {
                    message: "dialog dismissed".to_string(),
                },
            )),
        });

        let value: serde_json::Value = serde_json::from_str(&text).expect("valid JSON");
        assert_eq!(value["status"], json!("error"));
        assert_eq!(value["message"], json!("dialog dismissed"));
    }

    #[test]
    fn fresh_answers_reach_the_model_through_the_transcript_and_are_echoed() {
        let registry_request = {
            let mut req = request(
                vec![root_task(vec![
                    user_query_message("m1", "set up the db"),
                    message(
                        "m2",
                        api::message::Message::ToolCall(api::message::ToolCall {
                            tool_call_id: "call_ask".to_string(),
                            tool: Some(api::message::tool_call::Tool::AskUserQuestion(ask_call(
                                &registry(&[api::ToolType::AskUserQuestion]),
                            ))),
                        }),
                    ),
                ])],
                user_inputs(vec![
                    api::request::input::user_inputs::user_input::Input::ToolCallResult(
                        api::request::input::ToolCallResult {
                            tool_call_id: "call_ask".to_string(),
                            result: Some(
                                api::request::input::tool_call_result::Result::AskUserQuestion(
                                    answer_result(),
                                ),
                            ),
                        },
                    ),
                ]),
                "conv-1",
            );
            req.settings
                .as_mut()
                .expect("settings")
                .supported_tools
                .push(api::ToolType::AskUserQuestion as i32);
            req
        };

        let plan = plan_turn(&registry_request, &route()).expect("plan");

        let Some(ChatMessage::Tool { content, .. }) = plan.transcript.last() else {
            panic!(
                "expected the answers as the last message, got {:?}",
                plan.transcript.last()
            );
        };
        assert!(
            content.contains("\"selected_options\":[\"SQLite\"]"),
            "{content}"
        );
        assert!(plan.input_echo_messages.iter().any(|m| matches!(
            &m.message,
            Some(api::message::Message::ToolCallResult(
                api::message::ToolCallResult {
                    result: Some(api::message::tool_call_result::Result::AskUserQuestion(_)),
                    ..
                }
            ))
        )));
        assert!(
            plan.tools
                .definitions()
                .iter()
                .any(|definition| definition.name == "ask_user_question"),
            "the supported_tools declaration must reach the planned registry"
        );
    }
}

mod tool_result_rendering {
    use super::*;

    #[test]
    fn renders_finished_shell_command_with_output_and_exit_code() {
        let result = api::message::ToolCallResult {
            tool_call_id: "call_1".to_string(),
            context: None,
            result: Some(api::message::tool_call_result::Result::RunShellCommand(
                shell_finished_result("file.txt\n", 2),
            )),
        };

        let text = render_tool_result_text(&result);

        assert!(text.contains("file.txt"), "missing output: {text}");
        assert!(text.contains('2'), "missing exit code: {text}");
    }

    #[test]
    fn renders_read_files_error() {
        let result = api::message::ToolCallResult {
            tool_call_id: "call_1".to_string(),
            context: None,
            result: Some(api::message::tool_call_result::Result::ReadFiles(
                api::ReadFilesResult {
                    result: Some(api::read_files_result::Result::Error(
                        api::read_files_result::Error {
                            message: "no such file: missing.rs".to_string(),
                        },
                    )),
                },
            )),
        };

        let text = render_tool_result_text(&result);

        assert!(
            text.contains("no such file: missing.rs"),
            "missing error: {text}"
        );
    }

    fn tool_result(result: api::message::tool_call_result::Result) -> api::message::ToolCallResult {
        api::message::ToolCallResult {
            tool_call_id: "call_1".to_string(),
            context: None,
            result: Some(result),
        }
    }

    #[test]
    fn shell_results_show_the_command_that_actually_ran() {
        let edited = api::RunShellCommandResult {
            command: "ls -la".to_string(),
            ..shell_finished_result("total 0\n", 0)
        };
        let unreported = shell_finished_result("total 0\n", 0);

        let edited_text = render_shell_result(&edited);
        let unreported_text = render_shell_result(&unreported);

        assert!(
            edited_text.starts_with("Command run: ls -la\n"),
            "{edited_text}"
        );
        assert!(edited_text.contains("total 0"));
        assert!(
            unreported_text.starts_with("Exit code: 0"),
            "{unreported_text}"
        );
    }

    #[test]
    fn long_running_snapshots_are_readable_not_a_debug_dump() {
        let snapshot = api::RunShellCommandResult {
            command: "tail -f log".to_string(),
            result: Some(
                api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                    api::LongRunningShellCommandSnapshot {
                        output: "line 1\nline 2".to_string(),
                        command_id: "cmd-7".to_string(),
                        is_alt_screen_active: true,
                        ..Default::default()
                    },
                ),
            ),
            ..Default::default()
        };

        let text = render_shell_result(&snapshot);

        assert!(text.contains("still running"), "{text}");
        assert!(text.contains("cmd-7") && text.contains("line 2"), "{text}");
        assert!(text.contains("alternate screen"), "{text}");
        assert!(!text.contains("LongRunningShellCommandSnapshot"), "{text}");
    }

    #[test]
    fn mcp_images_are_described_not_dumped() {
        use api::call_mcp_tool_result::success::result::{Image, Result as McpContent};
        let result = tool_result(api::message::tool_call_result::Result::CallMcpTool(
            api::CallMcpToolResult {
                result: Some(api::call_mcp_tool_result::Result::Success(
                    api::call_mcp_tool_result::Success {
                        results: vec![api::call_mcp_tool_result::success::Result {
                            result: Some(McpContent::Image(Image {
                                data: vec![65; 4096],
                                mime_type: "image/png".to_string(),
                            })),
                        }],
                    },
                )),
            },
        ));

        let text = render_tool_result_text(&result);

        assert_eq!(text, "(image content, 4096 bytes, image/png)");
    }

    #[test]
    fn mcp_embedded_resources_render_like_read_mcp_resource() {
        use api::call_mcp_tool_result::success::result::Result as McpContent;
        let resource = api::McpResourceContent {
            uri: "file:///notes.md".to_string(),
            content_type: Some(api::mcp_resource_content::ContentType::Text(
                api::mcp_resource_content::Text {
                    content: "# Notes".to_string(),
                    mime_type: "text/markdown".to_string(),
                },
            )),
        };
        let result = tool_result(api::message::tool_call_result::Result::CallMcpTool(
            api::CallMcpToolResult {
                result: Some(api::call_mcp_tool_result::Result::Success(
                    api::call_mcp_tool_result::Success {
                        results: vec![api::call_mcp_tool_result::success::Result {
                            result: Some(McpContent::Resource(resource)),
                        }],
                    },
                )),
            },
        ));

        assert_eq!(
            render_tool_result_text(&result),
            "=== file:///notes.md ===\n# Notes"
        );
    }
}

mod event_mapping {
    use super::*;

    fn first_turn_plan() -> TurnPlan {
        let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
        plan_turn(&req, &route()).expect("plan")
    }

    fn follow_up_plan() -> TurnPlan {
        let req = request(
            vec![root_task(vec![
                user_query_message("m1", "list files"),
                shell_tool_call_message("m2", "call_1", "ls"),
            ])],
            user_inputs(vec![shell_result_input("call_1", "file.txt\n", 0)]),
            "conv-1",
        );
        plan_turn(&req, &route()).expect("plan")
    }

    #[test]
    fn first_turn_starts_with_init_transaction_create_task_and_echoes() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);

        let events = mapper.initial_events();

        assert_eq!(
            event_kinds(&events),
            vec!["init", "begin_transaction", "create_task", "add_messages"],
        );
        let Some(Ok(api::ResponseEvent {
            r#type: Some(api::response_event::Type::Init(init)),
        })) = events.first()
        else {
            panic!("first event must be Init");
        };
        assert_eq!(init.conversation_id, plan.conversation_id);
        assert!(!init.request_id.is_empty());

        let echoes = added_messages(&events);
        assert!(
            echoes.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::UserQuery(q)) if q.query == "hi"
            )),
            "user query must be echoed into the task tree"
        );
        assert!(
            echoes.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::ModelUsed(used)) if used.model_id == CONFIG_KEY
            )),
            "ModelUsed must report the custom model's config key"
        );
    }

    #[test]
    fn follow_up_turn_does_not_recreate_the_root_task() {
        let plan = follow_up_plan();
        let mut mapper = EventMapper::new(&plan);

        let events = mapper.initial_events();

        assert!(
            !event_kinds(&events).contains(&"create_task".to_string()),
            "follow-up turns must not re-emit CreateTask"
        );
        let Some(Ok(api::ResponseEvent {
            r#type: Some(api::response_event::Type::Init(init)),
        })) = events.first()
        else {
            panic!("first event must be Init");
        };
        assert_eq!(init.conversation_id, "conv-1");
    }

    #[test]
    fn text_deltas_stream_as_one_add_then_appends() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let first = mapper.on_event(Ok(AgentEvent::TextDelta("Hel".to_string())));
        let second = mapper.on_event(Ok(AgentEvent::TextDelta("lo".to_string())));

        let first_added = added_messages(&first);
        assert_eq!(first_added.len(), 1, "first delta creates the message");
        let Some(api::message::Message::AgentOutput(output)) = &first_added[0].message else {
            panic!("expected AgentOutput, got {:?}", first_added[0].message);
        };
        assert_eq!(output.text, "Hel");

        assert_eq!(event_kinds(&second), vec!["append"]);
        let appended = second
            .iter()
            .filter_map(|e| e.as_ref().ok())
            .find_map(|e| match e.r#type.as_ref() {
                Some(api::response_event::Type::ClientActions(actions)) => actions
                    .actions
                    .iter()
                    .find_map(|a| match a.action.as_ref() {
                        Some(api::client_action::Action::AppendToMessageContent(append)) => {
                            Some(append.clone())
                        }
                        _ => None,
                    }),
                _ => None,
            })
            .expect("append action");
        assert_eq!(appended.task_id, plan.root_task_id);
        let appended_message = appended.message.expect("appended message");
        assert_eq!(
            appended_message.id, first_added[0].id,
            "appends must target the message created by the first delta"
        );
        assert!(
            matches!(
                &appended_message.message,
                Some(api::message::Message::AgentOutput(o)) if o.text == "lo"
            ),
            "append must carry only the delta"
        );
        assert!(appended.mask.is_some_and(|mask| !mask.paths.is_empty()));
    }

    /// Applies the mapper's append events through the same field-mask
    /// machinery `task.rs` uses, proving the emitted mask path actually
    /// appends text on the real client (risky assumption #2 of the plan).
    #[test]
    fn emitted_append_mask_path_works_with_the_client_field_mask_machinery() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let first = mapper.on_event(Ok(AgentEvent::TextDelta("Hel".to_string())));
        let second = mapper.on_event(Ok(AgentEvent::TextDelta("lo".to_string())));

        let existing = added_messages(&first).remove(0);
        let append = second
            .iter()
            .filter_map(|e| e.as_ref().ok())
            .find_map(|e| match e.r#type.as_ref() {
                Some(api::response_event::Type::ClientActions(actions)) => actions
                    .actions
                    .iter()
                    .find_map(|a| match a.action.as_ref() {
                        Some(api::client_action::Action::AppendToMessageContent(append)) => {
                            Some(append.clone())
                        }
                        _ => None,
                    }),
                _ => None,
            })
            .expect("append action");

        let merged = field_mask::FieldMaskOperation::append(
            &api::MESSAGE_DESCRIPTOR,
            &existing,
            &append.message.expect("appended message"),
            append.mask.expect("mask"),
        )
        .apply()
        .expect("field mask append must succeed");

        assert!(
            matches!(
                &merged.message,
                Some(api::message::Message::AgentOutput(o)) if o.text == "Hello"
            ),
            "append must concatenate the delta onto the existing text, got {:?}",
            merged.message
        );
    }

    #[test]
    fn tool_calls_are_added_to_the_task_with_their_openai_call_id() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let events = mapper.on_event(Ok(AgentEvent::ToolCall(ToolCallRequest {
            id: "call_9".to_string(),
            name: "run_shell_command".to_string(),
            arguments: json!({ "command": "echo hi" }),
        })));

        let added = added_messages(&events);
        let tool_call = added
            .iter()
            .find_map(|m| match &m.message {
                Some(api::message::Message::ToolCall(call)) => Some(call.clone()),
                _ => None,
            })
            .expect("tool call message");
        assert_eq!(tool_call.tool_call_id, "call_9");
        assert!(matches!(
            tool_call.tool,
            Some(api::message::tool_call::Tool::RunShellCommand(shell)) if shell.command == "echo hi"
        ));
    }

    #[test]
    fn done_commits_the_transaction_and_finishes_with_token_usage() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();
        mapper.on_event(Ok(AgentEvent::TextDelta("hi".to_string())));
        mapper.on_event(Ok(AgentEvent::Usage {
            input_tokens: 42,
            output_tokens: 7,
        }));

        let events = mapper.on_event(Ok(AgentEvent::Done));

        assert_eq!(event_kinds(&events), vec!["commit_transaction", "finished"]);
        let finished = events
            .iter()
            .filter_map(|e| e.as_ref().ok())
            .find_map(|e| match e.r#type.as_ref() {
                Some(api::response_event::Type::Finished(finished)) => Some(finished.clone()),
                _ => None,
            })
            .expect("finished event");
        assert!(matches!(
            finished.reason,
            Some(api::response_event::stream_finished::Reason::Done(_))
        ));
        assert_eq!(finished.token_usage.len(), 1);
        assert_eq!(finished.token_usage[0].model_id, CONFIG_KEY);
        assert_eq!(finished.token_usage[0].total_input, 42);
        assert_eq!(finished.token_usage[0].output, 7);
    }

    #[test]
    fn invalid_api_key_rolls_back_and_finishes_with_invalid_api_key() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();
        mapper.on_event(Ok(AgentEvent::TextDelta("partial".to_string())));

        let events = mapper.on_event(Err(LocalAgentError::InvalidApiKey {
            model_slug: MODEL_SLUG.to_string(),
        }));

        let kinds = event_kinds(&events);
        assert!(
            kinds.contains(&"rollback_transaction".to_string()),
            "got {kinds:?}"
        );
        let finished = events
            .iter()
            .filter_map(|e| e.as_ref().ok())
            .find_map(|e| match e.r#type.as_ref() {
                Some(api::response_event::Type::Finished(finished)) => Some(finished.clone()),
                _ => None,
            })
            .expect("finished event");
        assert!(matches!(
            finished.reason,
            Some(api::response_event::stream_finished::Reason::InvalidApiKey(
                _
            ))
        ));
    }

    #[test]
    fn context_window_overflow_finishes_with_context_window_exceeded() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let events = mapper.on_event(Err(LocalAgentError::ContextWindowExceeded {
            message: "maximum context length exceeded".to_string(),
        }));

        let finished = events
            .iter()
            .filter_map(|e| e.as_ref().ok())
            .find_map(|e| match e.r#type.as_ref() {
                Some(api::response_event::Type::Finished(finished)) => Some(finished.clone()),
                _ => None,
            })
            .expect("finished event");
        assert!(matches!(
            finished.reason,
            Some(api::response_event::stream_finished::Reason::ContextWindowExceeded(_))
        ));
    }

    /// Tool calls the registry refuses to map, each of which ends the turn.
    fn unmappable_tool_calls() -> Vec<(&'static str, ToolCallRequest)> {
        let call = |name: &str, arguments: serde_json::Value| ToolCallRequest {
            id: "call_bad".to_string(),
            name: name.to_string(),
            arguments,
        };
        vec![
            ("unknown tool", call("made_up_tool", json!({}))),
            (
                "bad arguments",
                call("run_shell_command", json!({ "command": 7 })),
            ),
            (
                "tool not offered",
                call("suggest_prompt", json!({ "prompt": "x" })),
            ),
        ]
    }

    #[test]
    fn tool_call_mapping_failure_ends_the_turn_exactly_once() {
        for (what, call) in unmappable_tool_calls() {
            let plan = first_turn_plan();
            let mut mapper = EventMapper::new(&plan);
            mapper.initial_events();
            mapper.on_event(Ok(AgentEvent::TextDelta("partial".to_string())));

            // The engine buffers tool calls until the response completes, so
            // `Usage` and `Done` follow the call that could not be mapped.
            let mut events = mapper.on_event(Ok(AgentEvent::ToolCall(call)));
            events.extend(mapper.on_event(Ok(AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
            })));
            events.extend(mapper.on_event(Ok(AgentEvent::Done)));

            assert_eq!(
                event_kinds(&events),
                vec!["rollback_transaction", "finished"],
                "{what}: one rollback and one Finished, no commit after the rollback"
            );
            assert!(mapper.is_terminated());
        }
    }

    #[test]
    fn nothing_is_emitted_after_a_terminal_event() {
        let terminals: Vec<(&str, Box<dyn Fn() -> Result<AgentEvent, LocalAgentError>>)> = vec![
            ("done", Box::new(|| Ok(AgentEvent::Done))),
            (
                "invalid api key",
                Box::new(|| {
                    Err(LocalAgentError::InvalidApiKey {
                        model_slug: MODEL_SLUG.to_string(),
                    })
                }),
            ),
            (
                "context window",
                Box::new(|| {
                    Err(LocalAgentError::ContextWindowExceeded {
                        message: "too long".to_string(),
                    })
                }),
            ),
            (
                "transport error",
                Box::new(|| Err(LocalAgentError::Transport("connection reset".to_string()))),
            ),
        ];
        let later: Vec<Box<dyn Fn() -> Result<AgentEvent, LocalAgentError>>> = vec![
            Box::new(|| Ok(AgentEvent::TextDelta("late".to_string()))),
            Box::new(|| Ok(AgentEvent::ReasoningDelta("late".to_string()))),
            Box::new(|| Ok(AgentEvent::ReplayItem(json!({ "type": "reasoning" })))),
            Box::new(|| {
                Ok(AgentEvent::Usage {
                    input_tokens: 1,
                    output_tokens: 1,
                })
            }),
            Box::new(|| Ok(AgentEvent::Done)),
            Box::new(|| Err(LocalAgentError::Transport("again".to_string()))),
        ];

        for (what, terminal) in &terminals {
            let plan = first_turn_plan();
            let mut mapper = EventMapper::new(&plan);
            mapper.initial_events();

            assert!(
                !mapper.on_event(terminal()).is_empty(),
                "{what} is terminal"
            );
            for event in &later {
                assert!(
                    mapper.on_event(event()).is_empty(),
                    "an event after {what} must be dropped"
                );
            }
        }
    }

    #[test]
    fn transport_errors_surface_as_stream_errors_for_retry() {
        let plan = first_turn_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let events = mapper.on_event(Err(LocalAgentError::Transport(
            "connection reset".to_string(),
        )));

        assert!(
            events.iter().any(|e| e.is_err()),
            "transport failures must surface as stream errors so the existing retry machinery applies"
        );
    }
}

/// Reasoning surfacing and replay-item persistence: engine events -> task-tree
/// messages -> rebuilt transcript.
mod reasoning {
    use super::*;

    const CALL_ID: &str = "call_1";

    fn replay_item() -> serde_json::Value {
        json!({
            "type": "reasoning",
            "summary": [{ "type": "summary_text", "text": "Plan: list the files \u{1F4C1}" }],
            "encrypted_content": "gAAAAABo+/=="
        })
    }

    fn second_replay_item() -> serde_json::Value {
        json!({ "type": "reasoning", "summary": [], "encrypted_content": "gAAAAACp//==" })
    }

    fn shell_call(id: &str) -> AgentEvent {
        AgentEvent::ToolCall(ToolCallRequest {
            id: id.to_string(),
            name: "run_shell_command".to_string(),
            arguments: json!({ "command": "ls" }),
        })
    }

    /// The task-tree actions of a batch of events, in order.
    fn task_actions(events: &[Event]) -> Vec<api::client_action::Action> {
        events
            .iter()
            .filter_map(|event| event.as_ref().ok())
            .filter_map(|event| match event.r#type.as_ref() {
                Some(api::response_event::Type::ClientActions(actions)) => Some(&actions.actions),
                _ => None,
            })
            .flatten()
            .filter_map(|action| action.action.clone())
            .collect()
    }

    /// Applies task-tree actions through the same field-mask machinery the
    /// client's `task.rs` uses, so a bad mask path fails here.
    fn apply_task_actions(messages: &mut Vec<api::Message>, events: &[Event]) {
        use api::client_action::Action;
        for action in task_actions(events) {
            match action {
                Action::AddMessagesToTask(add) => messages.extend(add.messages),
                Action::AppendToMessageContent(append) => {
                    let patch = append.message.expect("append message");
                    let existing = messages
                        .iter_mut()
                        .find(|message| message.id == patch.id)
                        .expect("message to append to");
                    let merged = field_mask::FieldMaskOperation::append(
                        &api::MESSAGE_DESCRIPTOR,
                        &*existing,
                        &patch,
                        append.mask.expect("mask"),
                    )
                    .apply()
                    .expect("append must apply");
                    *existing = merged;
                }
                Action::UpdateTaskMessage(update) => {
                    let patch = update.message.expect("update message");
                    let existing = messages
                        .iter_mut()
                        .find(|message| message.id == patch.id)
                        .expect("message to update");
                    let merged = field_mask::FieldMaskOperation::update(
                        &api::MESSAGE_DESCRIPTOR,
                        &*existing,
                        &patch,
                        update.mask.expect("mask"),
                    )
                    .apply()
                    .expect("update must apply");
                    *existing = merged;
                }
                _ => {}
            }
        }
    }

    /// Runs one first turn through the mapper and returns the task messages the
    /// next request's `task_context` would carry.
    fn persisted_messages(events: Vec<AgentEvent>) -> Vec<api::Message> {
        persisted_messages_via(&route(), events)
    }

    /// [`persisted_messages`] for a turn served by `route`.
    fn persisted_messages_via(route: &LocalRoute, events: Vec<AgentEvent>) -> Vec<api::Message> {
        let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
        let plan = plan_turn(&req, route).expect("plan");
        let mut mapper = EventMapper::new(&plan);
        let mut messages = Vec::new();
        apply_task_actions(&mut messages, &mapper.initial_events());
        for event in events {
            apply_task_actions(&mut messages, &mapper.on_event(Ok(event)));
        }
        messages
    }

    fn transcript_after(
        messages: Vec<api::Message>,
        input: api::request::input::user_inputs::user_input::Input,
        route: &LocalRoute,
    ) -> Vec<ChatMessage> {
        let req = request(
            vec![root_task(messages)],
            user_inputs(vec![input]),
            "conv-1",
        );
        plan_turn(&req, route).expect("plan").transcript
    }

    fn tool_result_follow_up(messages: Vec<api::Message>) -> Vec<ChatMessage> {
        transcript_after(messages, shell_result_input(CALL_ID, "ok", 0), &route())
    }

    fn assistants(transcript: &[ChatMessage]) -> Vec<&ChatMessage> {
        transcript
            .iter()
            .filter(|message| matches!(message, ChatMessage::Assistant { .. }))
            .collect()
    }

    fn reasoning_messages(
        messages: &[api::Message],
    ) -> Vec<(&api::Message, &api::message::AgentReasoning)> {
        messages
            .iter()
            .filter_map(|message| match &message.message {
                Some(api::message::Message::AgentReasoning(reasoning)) => {
                    Some((message, reasoning))
                }
                _ => None,
            })
            .collect()
    }

    #[test]
    fn summary_deltas_stream_into_one_reasoning_message_that_is_finished_by_the_answer() {
        let plan = {
            let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
            plan_turn(&req, &route()).expect("plan")
        };
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let first = mapper.on_event(Ok(AgentEvent::ReasoningDelta("Think".to_string())));
        let second = mapper.on_event(Ok(AgentEvent::ReasoningDelta("ing".to_string())));
        let answer = mapper.on_event(Ok(AgentEvent::TextDelta("Hi".to_string())));

        assert_eq!(event_kinds(&first), vec!["add_messages"]);
        assert_eq!(event_kinds(&second), vec!["append"]);
        assert_eq!(
            event_kinds(&answer),
            vec!["update", "add_messages"],
            "the answer finishes the reasoning message, then opens its own output message"
        );

        let added = added_messages(&first);
        assert!(matches!(
            &added[0].message,
            Some(api::message::Message::AgentReasoning(r)) if r.reasoning == "Think"
        ));
        let Some(api::client_action::Action::AppendToMessageContent(append)) =
            task_actions(&second).pop()
        else {
            panic!("expected an append action");
        };
        assert_eq!(
            append.mask.expect("mask").paths,
            vec!["agent_reasoning.reasoning"]
        );
        assert_eq!(append.message.expect("message").id, added[0].id);
    }

    #[test]
    fn reasoning_text_lands_in_the_task_tree_and_is_marked_finished() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("Think".to_string()),
            AgentEvent::ReasoningDelta("ing".to_string()),
            AgentEvent::TextDelta("Hi".to_string()),
            AgentEvent::Done,
        ]);

        let reasoning = reasoning_messages(&messages);
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0].1.reasoning, "Thinking");
        assert!(
            reasoning[0].1.finished_duration.is_some(),
            "finished reasoning must stop showing the live 'Thinking' header"
        );
        assert!(
            reasoning[0].0.server_message_data.is_empty(),
            "reasoning without a replay item (Chat Completions) carries no payload"
        );
    }

    #[test]
    fn done_and_tool_calls_finish_an_open_reasoning_message() {
        let plan = {
            let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
            plan_turn(&req, &route()).expect("plan")
        };
        let mut reasoning_then_done = EventMapper::new(&plan);
        reasoning_then_done.initial_events();
        reasoning_then_done.on_event(Ok(AgentEvent::ReasoningDelta("hm".to_string())));
        let mut reasoning_then_call = EventMapper::new(&plan);
        reasoning_then_call.initial_events();
        reasoning_then_call.on_event(Ok(AgentEvent::ReasoningDelta("hm".to_string())));

        let done = reasoning_then_done.on_event(Ok(AgentEvent::Done));
        let call = reasoning_then_call.on_event(Ok(shell_call(CALL_ID)));

        assert_eq!(
            event_kinds(&done),
            vec!["update", "commit_transaction", "finished"]
        );
        assert_eq!(event_kinds(&call), vec!["update", "add_messages"]);
    }

    #[test]
    fn empty_reasoning_deltas_are_ignored() {
        let plan = {
            let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
            plan_turn(&req, &route()).expect("plan")
        };
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        assert!(
            mapper
                .on_event(Ok(AgentEvent::ReasoningDelta(String::new())))
                .is_empty()
        );
    }

    #[test]
    fn replay_item_rides_on_the_reasoning_message_that_shows_its_summary() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("Plan".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let reasoning = reasoning_messages(&messages);
        assert_eq!(reasoning.len(), 1, "one provider item, one carrier message");
        assert_eq!(reasoning[0].1.reasoning, "Plan");
        assert!(reasoning[0].1.finished_duration.is_some());
        assert!(!reasoning[0].0.server_message_data.is_empty());
    }

    #[test]
    fn replay_item_without_summary_text_gets_an_empty_carrier_message() {
        let messages = persisted_messages(vec![
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let reasoning = reasoning_messages(&messages);
        assert_eq!(reasoning.len(), 1);
        assert_eq!(
            reasoning[0].1.reasoning, "",
            "an empty reasoning message is not rendered by the UI"
        );
        assert!(!reasoning[0].0.server_message_data.is_empty());

        let transcript = tool_result_follow_up(messages);
        assert!(matches!(
            assistants(&transcript)[0],
            ChatMessage::Assistant { replay_items, .. } if *replay_items == vec![replay_item()]
        ));
    }

    #[test]
    fn tool_turn_replays_reasoning_items_before_the_assistant_tool_calls() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("Plan".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let transcript = tool_result_follow_up(messages);

        // [system, user, assistant(replay items + tool call), tool result]
        assert_eq!(transcript.len(), 4, "got {transcript:?}");
        let ChatMessage::Assistant {
            text,
            tool_calls,
            replay_items,
        } = &transcript[2]
        else {
            panic!("expected the assistant turn, got {:?}", transcript[2]);
        };
        assert_eq!(text, "");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].id, CALL_ID);
        assert_eq!(replay_items, &vec![replay_item()]);
        assert!(matches!(
            &transcript[3],
            ChatMessage::Tool { tool_call_id, .. } if tool_call_id == CALL_ID
        ));
    }

    #[test]
    fn text_answer_replays_reasoning_items_with_the_assistant_text() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("Plan".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            AgentEvent::TextDelta("All done.".to_string()),
            AgentEvent::Done,
        ]);

        let transcript = transcript_after(messages, user_query_input("thanks"), &route());

        let assistants = assistants(&transcript);
        assert_eq!(assistants.len(), 1);
        assert_eq!(
            assistants[0],
            &ChatMessage::Assistant {
                text: "All done.".to_string(),
                tool_calls: vec![],
                replay_items: vec![replay_item()],
            }
        );
        assert_eq!(
            transcript.last(),
            Some(&ChatMessage::User("thanks".to_string()))
        );
    }

    #[test]
    fn several_reasoning_items_keep_stream_order() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("first".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            AgentEvent::ReplayItem(second_replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let transcript = tool_result_follow_up(messages);

        assert!(matches!(
            assistants(&transcript)[0],
            ChatMessage::Assistant { replay_items, .. }
                if *replay_items == vec![replay_item(), second_replay_item()]
        ));
    }

    #[test]
    fn engine_event_order_with_preamble_text_keeps_replay_items_first() {
        // The Responses engine streams reasoning and text live and releases a
        // turn's tool calls only at `response.completed`, so the order is:
        // reasoning, replay item, text, tool call, usage, done.
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("Plan".to_string()),
            AgentEvent::ReasoningDelta("\n\n".to_string()),
            AgentEvent::ReasoningDelta("then act".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            AgentEvent::TextDelta("Checking the directory.".to_string()),
            shell_call(CALL_ID),
            AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
            },
            AgentEvent::Done,
        ]);

        let reasoning = reasoning_messages(&messages);
        assert_eq!(reasoning.len(), 1);
        assert_eq!(reasoning[0].1.reasoning, "Plan\n\nthen act");

        let transcript = tool_result_follow_up(messages);

        let ChatMessage::Assistant {
            text,
            tool_calls,
            replay_items,
        } = assistants(&transcript)[0]
        else {
            unreachable!();
        };
        assert_eq!(text, "Checking the directory.");
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(replay_items, &vec![replay_item()]);
    }

    #[test]
    fn parallel_tool_calls_share_one_assistant_message_that_adopts_the_replay_items() {
        let messages = persisted_messages(vec![
            AgentEvent::ReplayItem(replay_item()),
            shell_call("call_a"),
            shell_call("call_b"),
            AgentEvent::Done,
        ]);

        let transcript =
            transcript_after(messages, shell_result_input("call_a", "ok", 0), &route());

        let assistants = assistants(&transcript);
        assert_eq!(assistants.len(), 1);
        let ChatMessage::Assistant {
            tool_calls,
            replay_items,
            ..
        } = assistants[0]
        else {
            unreachable!();
        };
        assert_eq!(tool_calls.len(), 2);
        assert_eq!(replay_items, &vec![replay_item()]);
    }

    #[test]
    fn reasoning_summary_text_is_never_replayed_as_assistant_text() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("secret chain of thought".to_string()),
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let transcript = tool_result_follow_up(messages);

        let transcript_text = format!("{transcript:?}");
        assert!(
            !transcript_text.contains("secret chain of thought"),
            "reasoning text leaked into the transcript: {transcript_text}"
        );
    }

    #[test]
    fn chat_completions_reasoning_adds_nothing_to_the_transcript() {
        let messages = persisted_messages(vec![
            AgentEvent::ReasoningDelta("thinking out loud".to_string()),
            AgentEvent::TextDelta("answer".to_string()),
            AgentEvent::Done,
        ]);

        let transcript = transcript_after(messages, user_query_input("next"), &route());

        assert_eq!(
            &transcript[1..],
            &[
                ChatMessage::User("hi".to_string()),
                ChatMessage::Assistant {
                    text: "answer".to_string(),
                    tool_calls: vec![],
                    replay_items: vec![],
                },
                ChatMessage::User("next".to_string()),
            ]
        );
    }

    #[test]
    fn replay_items_round_trip_losslessly() {
        let item = json!({
            "type": "reasoning",
            "summary": [
                { "type": "summary_text", "text": "line one\nline \"two\" \\ \t" },
                { "type": "summary_text", "text": "" }
            ],
            "encrypted_content": "A".repeat(4096),
            "nested": { "numbers": [0, -1, 9007199254740993_i64, 1.5, 1e-7], "none": null, "flag": false },
        });
        let messages = persisted_messages(vec![
            AgentEvent::ReplayItem(item.clone()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);

        let transcript = tool_result_follow_up(messages);

        let ChatMessage::Assistant { replay_items, .. } = assistants(&transcript)[0] else {
            unreachable!();
        };
        assert_eq!(replay_items, &vec![item]);
    }

    #[test]
    fn replay_items_are_not_handed_to_a_different_model() {
        let messages = persisted_messages(vec![
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]);
        let other_model = LocalRoute {
            config_key: "cfg-other".to_string(),
            ..route()
        };

        let transcript = transcript_after(
            messages.clone(),
            shell_result_input(CALL_ID, "ok", 0),
            &other_model,
        );
        let same_model = transcript_after(messages, shell_result_input(CALL_ID, "ok", 0), &route());

        assert!(matches!(
            assistants(&transcript)[0],
            ChatMessage::Assistant { replay_items, tool_calls, .. }
                if replay_items.is_empty() && tool_calls.len() == 1
        ));
        assert!(matches!(
            assistants(&same_model)[0],
            ChatMessage::Assistant { replay_items, .. } if replay_items.len() == 1
        ));
    }

    #[test]
    fn replay_items_without_a_following_assistant_message_do_not_leak_into_later_turns() {
        // The first turn ended after reasoning (no text, no tool call).
        let mut messages = persisted_messages(vec![
            AgentEvent::ReplayItem(replay_item()),
            AgentEvent::Done,
        ]);
        messages.push(user_query_message("m-next", "try again"));
        messages.push(agent_output_message("m-answer", "here you go"));

        let transcript = transcript_after(messages, user_query_input("thanks"), &route());

        assert!(
            assistants(&transcript).iter().all(|message| matches!(
                message,
                ChatMessage::Assistant { replay_items, .. } if replay_items.is_empty()
            )),
            "dangling reasoning must not be attached to a later turn: {transcript:?}"
        );
    }

    #[test]
    fn foreign_or_malformed_payloads_on_reasoning_messages_are_ignored() {
        let reasoning = |id: &str, payload: &str| api::Message {
            server_message_data: payload.to_string(),
            ..message(
                id,
                api::message::Message::AgentReasoning(api::message::AgentReasoning {
                    reasoning: "server reasoning".to_string(),
                    finished_duration: None,
                }),
            )
        };
        let messages = vec![
            user_query_message("m1", "list files"),
            // An opaque server-issued blob, not JSON.
            reasoning("m2", "CgdvcGFxdWUSCQ=="),
            // Our envelope, but the item is not an object.
            reasoning(
                "m3",
                &format!(
                    r#"{{"config_key":"{CONFIG_KEY}","model_fingerprint":"{}","local_agent_replay_item":"oops"}}"#,
                    endpoint_fingerprint(&route().endpoint)
                ),
            ),
            shell_tool_call_message("m4", CALL_ID, "ls"),
        ];

        let transcript = tool_result_follow_up(messages);

        assert!(matches!(
            assistants(&transcript)[0],
            ChatMessage::Assistant { replay_items, tool_calls, .. }
                if replay_items.is_empty() && tool_calls.len() == 1
        ));
    }

    /// A route to the same custom model entry (`config_key`) as [`route`], with
    /// the endpoint edited.
    fn edited_route(edit: impl FnOnce(&mut LocalEndpointConfig)) -> LocalRoute {
        let mut edited = route();
        edit(&mut edited.endpoint);
        edited
    }

    /// Whether the replay item survived into the transcript rebuilt for `route`.
    fn replayed_to(messages: &[api::Message], route: &LocalRoute) -> bool {
        let transcript = transcript_after(
            messages.to_vec(),
            shell_result_input(CALL_ID, "ok", 0),
            route,
        );
        let ChatMessage::Assistant { replay_items, .. } = assistants(&transcript)[0] else {
            unreachable!();
        };
        !replay_items.is_empty()
    }

    fn call_with_replay_item() -> Vec<AgentEvent> {
        vec![
            AgentEvent::ReplayItem(replay_item()),
            shell_call(CALL_ID),
            AgentEvent::Done,
        ]
    }

    #[test]
    fn fnv1a_matches_the_published_test_vectors() {
        assert_eq!(fnv1a_64(b""), 0xcbf2_9ce4_8422_2325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63_dc4c_8601_ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x8594_4171_f739_67e8);
    }

    #[test]
    fn endpoint_fingerprint_is_pinned() {
        // Persisted in task trees: changing this value silently orphans every
        // stored replay item. If the format has to change, do it on purpose.
        assert_eq!(endpoint_fingerprint(&route().endpoint), "471cf8c9d520d9a6");
        assert_eq!(
            endpoint_fingerprint(
                &edited_route(|e| e.schema = local_agent::EndpointSchema::Responses).endpoint
            ),
            "9a369eeb351c1cd2"
        );
    }

    #[test]
    fn fingerprint_changes_with_schema_base_url_and_slug() {
        let base = endpoint_fingerprint(&route().endpoint);
        let edits: [(&str, fn(&mut LocalEndpointConfig)); 4] = [
            ("schema", |e| {
                e.schema = local_agent::EndpointSchema::Responses
            }),
            ("host", |e| {
                e.base_url = "https://other.example.com/v1".to_string()
            }),
            ("path", |e| {
                e.base_url = "https://llm.example.com/v2".to_string()
            }),
            ("slug", |e| e.model_slug = "llama-3.1-8b".to_string()),
        ];
        for (what, edit) in edits {
            assert_ne!(
                endpoint_fingerprint(&edited_route(edit).endpoint),
                base,
                "editing the {what} must change the fingerprint"
            );
        }
    }

    #[test]
    fn fingerprint_ignores_credentials_and_url_spelling() {
        let base = endpoint_fingerprint(&route().endpoint);
        let same_target: [fn(&mut LocalEndpointConfig); 7] = [
            |e| e.api_key = "sk-rotated".to_string(),
            |e| e.base_url = "https://user:hunter2@llm.example.com/v1".to_string(),
            |e| e.base_url = "https://llm.example.com/v1?api-key=hunter2".to_string(),
            |e| e.base_url = "https://llm.example.com/v1#frag".to_string(),
            |e| e.base_url = "https://llm.example.com/v1/".to_string(),
            |e| e.base_url = "  HTTPS://LLM.Example.com/v1  ".to_string(),
            |e| e.model_slug = format!(" {MODEL_SLUG} "),
        ];
        for edit in same_target {
            let edited = edited_route(edit);
            assert_eq!(
                endpoint_fingerprint(&edited.endpoint),
                base,
                "{:?} names the same target",
                edited.endpoint
            );
        }
    }

    #[test]
    fn persisted_payload_carries_nothing_derived_from_credentials() {
        let credentialed = edited_route(|e| {
            e.api_key = "sk-live-0123456789".to_string();
            e.base_url = "https://user:hunter2@llm.example.com/v1?api-key=hunter2".to_string();
        });

        let messages = persisted_messages_via(&credentialed, call_with_replay_item());

        let payloads: Vec<&str> = messages
            .iter()
            .map(|message| message.server_message_data.as_str())
            .filter(|data| !data.is_empty())
            .collect();
        assert_eq!(payloads.len(), 1);
        for secret in ["sk-live-0123456789", "hunter2", "user:", "api-key"] {
            assert!(
                !payloads[0].contains(secret),
                "{secret:?} leaked into {}",
                payloads[0]
            );
        }
        // Same target as `route()`, so it replays there.
        assert!(replayed_to(&messages, &route()));
    }

    #[test]
    fn replay_items_are_not_handed_to_an_edited_endpoint_with_the_same_config_key() {
        let messages = persisted_messages(call_with_replay_item());
        assert!(replayed_to(&messages, &route()), "control: unedited");

        let edits: [(&str, fn(&mut LocalEndpointConfig)); 4] = [
            ("model slug", |e| e.model_slug = "llama-3.1-8b".to_string()),
            ("base url", |e| {
                e.base_url = "https://other.example.com/v1".to_string()
            }),
            ("host path", |e| {
                e.base_url = "https://llm.example.com/proxy/v1".to_string()
            }),
            ("schema", |e| {
                e.schema = local_agent::EndpointSchema::Responses
            }),
        ];
        for (what, edit) in edits {
            let edited = edited_route(edit);
            assert_eq!(edited.config_key, route().config_key);
            assert!(
                !replayed_to(&messages, &edited),
                "an edited {what} must not receive reasoning issued by the old target"
            );
        }
        // Rotating the key keeps the target, so continuity is kept.
        assert!(replayed_to(
            &messages,
            &edited_route(|e| e.api_key = "sk-rotated".to_string())
        ));
    }

    #[test]
    fn switching_targets_and_back_replays_only_to_the_issuing_target() {
        // Responses -> Chat Completions -> Responses on the same model entry.
        let responses = edited_route(|e| e.schema = local_agent::EndpointSchema::Responses);
        let chat = route();
        let first_turn = persisted_messages_via(&responses, call_with_replay_item());
        assert!(replayed_to(&first_turn, &responses));

        assert!(
            !replayed_to(&first_turn, &chat),
            "the Chat Completions turn must not see Responses reasoning"
        );
        assert!(
            replayed_to(&first_turn, &responses),
            "returning to the issuing target replays its own items again"
        );
    }

    #[test]
    fn replay_items_persisted_without_a_fingerprint_are_dropped() {
        // The format before `model_fingerprint`: right model key, no target.
        let legacy = format!(
            r#"{{"config_key":"{CONFIG_KEY}","local_agent_replay_item":{}}}"#,
            replay_item()
        );
        let messages = vec![
            user_query_message("m1", "list files"),
            api::Message {
                server_message_data: legacy,
                ..message(
                    "m2",
                    api::message::Message::AgentReasoning(api::message::AgentReasoning {
                        reasoning: String::new(),
                        finished_duration: None,
                    }),
                )
            },
            shell_tool_call_message("m3", CALL_ID, "ls"),
        ];

        assert!(!replayed_to(&messages, &route()));
    }
}

mod context_sizing {
    use super::*;

    fn huge_output(chars: usize) -> String {
        "你".repeat(chars)
    }

    #[test]
    fn text_within_the_cap_is_untouched() {
        let at_cap = "x".repeat(MAX_CONTEXT_CHARS);
        let multibyte_at_cap = huge_output(MAX_CONTEXT_CHARS);

        assert_eq!(truncate_for_context(&at_cap, "output"), at_cap.as_str());
        assert!(matches!(
            truncate_for_context(&multibyte_at_cap, "output"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn oversized_text_is_cut_on_a_character_boundary_and_flagged() {
        let text = huge_output(MAX_CONTEXT_CHARS + 1);

        let truncated = truncate_for_context(&text, "shell command output");

        assert_eq!(truncated.chars().count(), MAX_CONTEXT_CHARS);
        assert!(
            truncated.ends_with("\n[truncated: shell command output exceeded 20000 characters]"),
            "missing truncation notice"
        );
        assert!(truncated.starts_with('你'));
    }

    #[test]
    fn passive_suggestion_trigger_output_is_capped() {
        let req = request(
            vec![],
            api::request::input::Type::GeneratePassiveSuggestions(
                api::request::input::GeneratePassiveSuggestions {
                    attachments: vec![],
                    trigger: Some(
                        api::request::input::generate_passive_suggestions::Trigger::ShellCommandCompleted(
                            api::request::input::generate_passive_suggestions::ShellCommandCompleted {
                                executed_shell_command: Some(api::ExecutedShellCommand {
                                    command: "cat big.log".to_string(),
                                    output: huge_output(MAX_CONTEXT_CHARS * 3),
                                    exit_code: 0,
                                    ..Default::default()
                                }),
                                relevant_files: vec![],
                            },
                        ),
                    ),
                },
            ),
            "",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        let Some(ChatMessage::User(instruction)) = plan.transcript.last() else {
            panic!("expected the trigger as the last user message");
        };
        assert!(instruction.contains("cat big.log"));
        assert!(instruction.contains("[truncated: shell command output exceeded"));
        assert!(
            instruction.chars().count() < MAX_CONTEXT_CHARS + 1_000,
            "the prompt carries one bounded block, got {} chars",
            instruction.chars().count()
        );
    }

    #[test]
    fn accepted_suggestion_trigger_output_is_capped() {
        let input = api::request::input::user_inputs::user_input::Input::PassiveSuggestionResult(
            api::request::input::user_inputs::PassiveSuggestionResultInput {
                result: Some(api::PassiveSuggestionResultType {
                    trigger: Some(
                        api::passive_suggestion_result_type::Trigger::ExecutedShellCommand(
                            api::ExecutedShellCommand {
                                command: "cat big.log".to_string(),
                                output: huge_output(MAX_CONTEXT_CHARS * 3),
                                exit_code: 0,
                                ..Default::default()
                            },
                        ),
                    ),
                    suggestion: Some(api::passive_suggestion_result_type::Suggestion::Prompt(
                        api::passive_suggestion_result_type::Prompt {
                            prompt: "Summarize it".to_string(),
                        },
                    )),
                }),
            },
        );
        let req = request(vec![], user_inputs(vec![input]), "");

        let plan = plan_turn(&req, &route()).expect("plan");

        let trigger = plan
            .transcript
            .iter()
            .find_map(|message| match message {
                ChatMessage::User(text) if text.contains("cat big.log") => Some(text),
                _ => None,
            })
            .expect("trigger context");
        assert!(trigger.contains("[truncated: shell command output exceeded"));
        assert!(trigger.chars().count() < MAX_CONTEXT_CHARS + 1_000);
    }
}

mod cancellation {
    use std::sync::atomic::{AtomicBool, Ordering};

    use futures::{FutureExt, StreamExt};

    use super::*;

    /// Flips a flag when dropped, standing in for the engine's HTTP
    /// connection (which is closed when its stream is dropped).
    struct DropFlag(Arc<AtomicBool>);

    impl Drop for DropFlag {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    /// An engine stream that never produces an event, like a model that is
    /// still thinking.
    fn stalled_engine(
        flag: DropFlag,
    ) -> futures::stream::BoxStream<'static, Result<AgentEvent, LocalAgentError>> {
        Box::pin(async_stream::stream! {
            let _connection = flag;
            futures::future::pending::<()>().await;
            yield Ok(AgentEvent::Done);
        })
    }

    fn stalled_response_stream() -> (ResponseStream, Arc<AtomicBool>) {
        let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
        let plan = plan_turn(&req, &route()).expect("plan");
        let dropped = Arc::new(AtomicBool::new(false));
        let stream = response_stream(
            EventMapper::new(&plan),
            stalled_engine(DropFlag(dropped.clone())),
        );
        (stream, dropped)
    }

    /// Reads every event that is available without waiting on the engine.
    fn drain_ready(stream: &mut (impl futures::Stream<Item = Event> + Unpin)) -> usize {
        let mut count = 0;
        while let Some(Some(_)) = stream.next().now_or_never() {
            count += 1;
        }
        count
    }

    #[test]
    fn opening_events_do_not_wait_for_the_engine() {
        let (mut stream, dropped) = stalled_response_stream();

        // init, begin transaction, create task, input echoes.
        assert_eq!(drain_ready(&mut stream), 4);
        assert!(!dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn dropping_the_response_stream_drops_the_engine_stream() {
        let (mut stream, dropped) = stalled_response_stream();
        drain_ready(&mut stream);
        assert!(
            !dropped.load(Ordering::SeqCst),
            "the engine stream must stay alive while the response stream is"
        );

        drop(stream);

        assert!(
            dropped.load(Ordering::SeqCst),
            "dropping the response stream must drop the engine stream and with it the connection"
        );
    }

    #[test]
    fn cancelling_through_take_until_ends_the_stream_and_releases_the_engine() {
        // Mirrors how `generate_multi_agent_output` wires cancellation.
        let (response, dropped) = stalled_response_stream();
        let (cancel, cancellation_rx) = futures::channel::oneshot::channel::<()>();
        let mut stream = response.take_until(cancellation_rx);
        drain_ready(&mut stream);

        cancel.send(()).expect("cancellation receiver is alive");

        assert!(
            matches!(stream.next().now_or_never(), Some(None)),
            "a cancelled stream must end without waiting for the engine"
        );
        drop(stream);
        assert!(dropped.load(Ordering::SeqCst));
    }
}

/// The mapper's terminal events seen through [`response_stream`].
mod terminal_events {
    use futures::{FutureExt, StreamExt};

    use super::*;

    fn bad_call() -> Result<AgentEvent, LocalAgentError> {
        Ok(AgentEvent::ToolCall(ToolCallRequest {
            id: "call_bad".to_string(),
            name: "made_up_tool".to_string(),
            arguments: json!({}),
        }))
    }

    fn stream_for(
        engine: futures::stream::BoxStream<'static, Result<AgentEvent, LocalAgentError>>,
    ) -> ResponseStream {
        let req = request(vec![], user_inputs(vec![user_query_input("hi")]), "");
        let plan = plan_turn(&req, &route()).expect("plan");
        response_stream(EventMapper::new(&plan), engine)
    }

    /// Everything the stream yields, without waiting: `None` if it would block.
    fn collect_ready(stream: ResponseStream) -> Option<Vec<Event>> {
        stream.collect::<Vec<_>>().now_or_never()
    }

    #[test]
    fn a_failed_tool_call_followed_by_usage_and_done_finishes_once_and_never_commits() {
        let engine = futures::stream::iter(vec![
            bad_call(),
            Ok(AgentEvent::Usage {
                input_tokens: 10,
                output_tokens: 5,
            }),
            Ok(AgentEvent::Done),
        ])
        .boxed();

        let events = collect_ready(stream_for(engine)).expect("stream must not block");

        let kinds = event_kinds(&events);
        assert_eq!(kinds.iter().filter(|kind| *kind == "finished").count(), 1);
        assert!(
            !kinds.contains(&"commit_transaction".to_string()),
            "a rolled-back transaction must not be committed: {kinds:?}"
        );
        assert_eq!(
            &kinds[kinds.len() - 2..],
            ["rollback_transaction", "finished"],
            "the rollback and Finished close the stream: {kinds:?}"
        );
    }

    #[test]
    fn the_engine_stream_is_released_at_the_terminal_event() {
        // After the bad call the engine would stall forever; the response
        // stream must end anyway, and must have dropped the engine.
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let flag = DropFlag(dropped.clone());
        let engine = Box::pin(async_stream::stream! {
            let _connection = flag;
            yield bad_call();
            futures::future::pending::<()>().await;
            yield Ok(AgentEvent::Done);
        });

        let events =
            collect_ready(stream_for(engine)).expect("stream must end at the terminal event");

        assert_eq!(
            event_kinds(&events)
                .iter()
                .filter(|kind| *kind == "finished")
                .count(),
            1
        );
        assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
    }
}

mod accepted_passive_suggestion {
    use super::*;

    fn passive_result_input(
        command: &str,
        output: &str,
        exit_code: i32,
        prompt: &str,
    ) -> api::request::input::user_inputs::user_input::Input {
        api::request::input::user_inputs::user_input::Input::PassiveSuggestionResult(
            api::request::input::user_inputs::PassiveSuggestionResultInput {
                result: Some(api::PassiveSuggestionResultType {
                    trigger: Some(
                        api::passive_suggestion_result_type::Trigger::ExecutedShellCommand(
                            api::ExecutedShellCommand {
                                command: command.to_string(),
                                output: output.to_string(),
                                exit_code,
                                ..Default::default()
                            },
                        ),
                    ),
                    suggestion: Some(api::passive_suggestion_result_type::Suggestion::Prompt(
                        api::passive_suggestion_result_type::Prompt {
                            prompt: prompt.to_string(),
                        },
                    )),
                }),
            },
        )
    }

    #[test]
    fn accepted_suggestion_brings_trigger_into_context_alongside_the_query() {
        // The client sends an accepted suggestion only as a PassiveSuggestionResult
        // (no separate UserQuery), so the adapter derives the durable query from it.
        let req = request(
            vec![],
            user_inputs(vec![passive_result_input(
                "fd . .go",
                "[fd error]: Search path '.go' is not a directory.",
                1,
                "Find go files",
            )]),
            "",
        );

        let plan = plan_turn(&req, &route())
            .expect("accepted passive suggestion must plan as an agent turn");

        assert!(
            plan.transcript.iter().any(|m| matches!(
                m,
                ChatMessage::User(text) if text.contains("fd . .go") && text.contains("[fd error]")
            )),
            "the triggering command and its output must be brought into context"
        );
        assert!(
            plan.transcript
                .iter()
                .any(|m| matches!(m, ChatMessage::User(text) if text == "Find go files")),
            "the accepted prompt must be present as the user query"
        );
        assert_eq!(plan.config_key, CONFIG_KEY);
    }

    #[test]
    fn accepted_suggestion_echoes_the_query_into_the_task_tree() {
        let req = request(
            vec![],
            user_inputs(vec![passive_result_input(
                "fd . .go",
                "err",
                1,
                "Find go files",
            )]),
            "",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        // The trigger is transient per-turn context; the accepted prompt is the
        // durable record, echoed into the task tree as a user query so follow-up
        // turns see it.
        assert!(
            plan.input_echo_messages.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::UserQuery(q)) if q.query == "Find go files"
            )),
            "the accepted prompt must be echoed as the durable user query"
        );
        assert!(
            !plan.input_echo_messages.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::PassiveSuggestionResult(_))
            )),
            "the passive suggestion result is not echoed as a durable message"
        );
    }

    #[test]
    fn follow_up_turn_after_accepted_suggestion_keeps_the_new_query() {
        // Regression: after accepting a passive suggestion that ran a tool, the
        // follow-up (tool-result) turn used to rebuild a transcript whose last
        // user message was the *previous* conversation turn, so the model answered
        // the old question. The accepted prompt — echoed as a durable UserQuery on
        // the accept turn — must be the last user message instead.
        let tasks = vec![root_task(vec![
            // A previous, unrelated exchange in the same conversation.
            user_query_message("m1", "Are you working?"),
            agent_output_message("m2", "Yes, I'm working. How can I help?"),
            // The accepted suggestion (echoed as the durable query on the accept
            // turn) plus the tool call the model made for it.
            user_query_message("m3", "Find go files"),
            shell_tool_call_message("m4", "call-1", "fd . -e go"),
        ])];

        let req = request(
            tasks,
            user_inputs(vec![shell_result_input("call-1", "main.go\nutil.go", 0)]),
            "conv-1",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        let last_user = plan
            .transcript
            .iter()
            .rev()
            .find_map(|m| match m {
                ChatMessage::User(text) => Some(text.as_str()),
                _ => None,
            })
            .expect("transcript must contain a user message");
        assert_eq!(
            last_user, "Find go files",
            "the follow-up turn must answer the accepted prompt, not the previous turn"
        );
    }
}

mod passive_suggestions {
    use super::*;

    fn shell_completed_input(
        command: &str,
        output: &str,
        exit_code: i32,
    ) -> api::request::input::Type {
        api::request::input::Type::GeneratePassiveSuggestions(
            api::request::input::GeneratePassiveSuggestions {
                attachments: vec![],
                trigger: Some(
                    api::request::input::generate_passive_suggestions::Trigger::ShellCommandCompleted(
                        api::request::input::generate_passive_suggestions::ShellCommandCompleted {
                            executed_shell_command: Some(api::ExecutedShellCommand {
                                command: command.to_string(),
                                output: output.to_string(),
                                exit_code,
                                ..Default::default()
                            }),
                            relevant_files: vec![],
                        },
                    ),
                ),
            },
        )
    }

    fn agent_response_completed_input() -> api::request::input::Type {
        api::request::input::Type::GeneratePassiveSuggestions(
            api::request::input::GeneratePassiveSuggestions {
                attachments: vec![],
                trigger: Some(
                    api::request::input::generate_passive_suggestions::Trigger::AgentResponseCompleted(
                        api::request::input::generate_passive_suggestions::AgentResponseCompleted {},
                    ),
                ),
            },
        )
    }

    fn shell_trigger_plan() -> TurnPlan {
        let req = request(
            vec![],
            shell_completed_input("cargo test", "error[E0308]: mismatched types\n", 101),
            "",
        );
        plan_turn(&req, &route()).expect("passive suggestion turns must plan")
    }

    #[test]
    fn shell_trigger_plans_a_suggestion_turn_with_only_the_suggest_prompt_tool() {
        let plan = shell_trigger_plan();

        let tool_names: Vec<_> = plan
            .tools
            .definitions()
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert_eq!(
            tool_names,
            vec!["suggest_prompt".to_string()],
            "suggestion turns are one-shot and read-only: no shell/file/MCP tools"
        );
    }

    #[test]
    fn shell_trigger_renders_command_and_output_into_the_transcript() {
        let plan = shell_trigger_plan();

        let ChatMessage::System(system) = &plan.transcript[0] else {
            panic!("transcript must start with a system prompt");
        };
        assert!(
            system.contains("suggest_prompt"),
            "system prompt must instruct the model to use the suggest_prompt tool, got: {system}"
        );

        let ChatMessage::User(instruction) = plan
            .transcript
            .last()
            .expect("transcript must not be empty")
        else {
            panic!("transcript must end with the trigger rendered as a user message");
        };
        assert!(
            instruction.contains("cargo test"),
            "missing command: {instruction}"
        );
        assert!(
            instruction.contains("error[E0308]"),
            "missing command output: {instruction}"
        );
        assert!(
            instruction.contains("101"),
            "missing exit code: {instruction}"
        );
    }

    #[test]
    fn shell_trigger_echoes_the_trigger_message_into_the_task_tree() {
        let plan = shell_trigger_plan();

        assert!(
            plan.input_echo_messages.iter().any(|m| matches!(
                &m.message,
                Some(api::message::Message::SystemQuery(
                    api::message::SystemQuery {
                        r#type: Some(
                            api::message::system_query::Type::GeneratePassiveSuggestions(_)
                        ),
                        ..
                    }
                ))
            )),
            "the trigger must be echoed so follow-up turns can replay it from the task tree"
        );
    }

    #[test]
    fn agent_response_trigger_replays_the_conversation_history() {
        let req = request(
            vec![root_task(vec![
                user_query_message("m1", "how do I deploy this"),
                agent_output_message("m2", "Use kubectl apply with the manifest."),
            ])],
            agent_response_completed_input(),
            "conv-1",
        );

        let plan = plan_turn(&req, &route()).expect("plan");

        assert!(
            plan.transcript
                .iter()
                .any(|m| matches!(m, ChatMessage::User(text) if text == "how do I deploy this")),
            "conversation history must be replayed for follow-up suggestions"
        );
        assert!(
            plan.transcript.iter().any(|m| matches!(
                m,
                ChatMessage::Assistant { text, .. } if text.contains("kubectl apply")
            )),
            "agent responses must be part of the replayed history"
        );
        let ChatMessage::User(instruction) = plan
            .transcript
            .last()
            .expect("transcript must not be empty")
        else {
            panic!("transcript must end with the suggestion instruction");
        };
        assert!(
            instruction.contains("suggest_prompt"),
            "the final instruction must point at the suggest_prompt tool: {instruction}"
        );
    }

    #[test]
    fn suggestion_turns_reject_calls_to_tools_they_did_not_offer() {
        let plan = shell_trigger_plan();
        let not_offered = [
            (
                "run_shell_command",
                json!({ "command": "ls", "is_read_only": true }),
            ),
            (
                "read_files",
                json!({ "files": [{ "path": "src/main.rs" }] }),
            ),
            ("grep", json!({ "queries": ["todo"], "path": "." })),
        ];

        for (name, arguments) in not_offered {
            let call = ToolCallRequest {
                id: "call-1".to_string(),
                name: name.to_string(),
                arguments,
            };
            assert!(
                matches!(
                    plan.tools.to_proto_tool_call(&call),
                    Err(AdapterError::UnknownTool(_))
                ),
                "a read-only suggestion turn must not honour a {name} call"
            );
        }
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();
        let events = mapper.on_event(Ok(AgentEvent::ToolCall(ToolCallRequest {
            id: "call-2".to_string(),
            name: "run_shell_command".to_string(),
            arguments: json!({ "command": "ls", "is_read_only": true }),
        })));
        assert_eq!(
            event_kinds(&events),
            vec!["rollback_transaction", "finished"]
        );
    }

    #[test]
    fn suggest_prompt_tool_call_maps_to_a_prompt_chip_message() {
        let plan = shell_trigger_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let call_events = mapper.on_event(Ok(AgentEvent::ToolCall(ToolCallRequest {
            id: "call-1".to_string(),
            name: "suggest_prompt".to_string(),
            arguments: json!({
                "prompt": "Fix the type mismatch in cargo test",
                "label": "Fix failing test",
            }),
        })));
        let done_events = mapper.on_event(Ok(AgentEvent::Done));

        let added = added_messages(&call_events);
        let suggestion = added
            .iter()
            .find_map(|m| match &m.message {
                Some(api::message::Message::ToolCall(api::message::ToolCall {
                    tool: Some(api::message::tool_call::Tool::SuggestPrompt(suggest)),
                    ..
                })) => Some(suggest.clone()),
                _ => None,
            })
            .expect("a suggest_prompt call must map to a ToolCall::SuggestPrompt message");
        let Some(api::message::tool_call::suggest_prompt::DisplayMode::PromptChip(chip)) =
            suggestion.display_mode
        else {
            panic!("local suggestions must use the PromptChip display mode");
        };
        assert_eq!(chip.prompt, "Fix the type mismatch in cargo test");
        assert_eq!(chip.label, "Fix failing test");

        assert_eq!(
            event_kinds(&done_events),
            vec!["commit_transaction", "finished"],
        );
    }

    #[test]
    fn suggest_prompt_label_is_optional() {
        let plan = shell_trigger_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let events = mapper.on_event(Ok(AgentEvent::ToolCall(ToolCallRequest {
            id: "call-1".to_string(),
            name: "suggest_prompt".to_string(),
            arguments: json!({ "prompt": "Re-run with --verbose" }),
        })));

        let added = added_messages(&events);
        let chip = added
            .iter()
            .find_map(|m| match &m.message {
                Some(api::message::Message::ToolCall(api::message::ToolCall {
                    tool: Some(api::message::tool_call::Tool::SuggestPrompt(
                        api::message::tool_call::SuggestPrompt {
                            display_mode:
                                Some(api::message::tool_call::suggest_prompt::DisplayMode::PromptChip(
                                    chip,
                                )),
                            ..
                        },
                    )),
                    ..
                })) => Some(chip.clone()),
                _ => None,
            })
            .expect("suggestion message");
        assert_eq!(chip.prompt, "Re-run with --verbose");
        assert_eq!(chip.label, "");
    }

    #[test]
    fn suggestion_turn_with_no_tool_call_finishes_cleanly() {
        let plan = shell_trigger_plan();
        let mut mapper = EventMapper::new(&plan);
        mapper.initial_events();

        let text_events = mapper.on_event(Ok(AgentEvent::TextDelta(
            "Nothing worth suggesting.".to_string(),
        )));
        let done_events = mapper.on_event(Ok(AgentEvent::Done));

        assert!(
            !added_messages(&text_events)
                .iter()
                .any(|m| matches!(&m.message, Some(api::message::Message::ToolCall(_)))),
            "declining must not fabricate a suggestion"
        );
        assert_eq!(
            event_kinds(&done_events),
            vec!["commit_transaction", "finished"],
        );
    }

    #[test]
    fn suggestion_turns_run_against_the_routed_endpoint_model() {
        let plan = shell_trigger_plan();
        assert_eq!(plan.config_key, CONFIG_KEY);
        assert!(plan.is_first_turn);
    }
}

#[test]
fn partial_file_read_failures_remain_visible_to_the_local_agent() {
    let failure = api::read_files_result::FailedRead {
        path: "missing.rs".to_string(),
        message: "permission denied".to_string(),
    };
    for result in [
        api::read_files_result::Result::TextFilesSuccess(
            api::read_files_result::TextFilesSuccess {
                files: vec![],
                failed_reads: vec![failure.clone()],
            },
        ),
        api::read_files_result::Result::AnyFilesSuccess(api::read_files_result::AnyFilesSuccess {
            files: vec![],
            failed_reads: vec![failure.clone()],
        }),
    ] {
        let result = api::message::ToolCallResult {
            tool_call_id: "read-1".to_string(),
            context: None,
            result: Some(api::message::tool_call_result::Result::ReadFiles(
                api::ReadFilesResult {
                    result: Some(result),
                },
            )),
        };
        assert_eq!(
            render_tool_result_text(&result),
            "Error reading missing.rs: permission denied"
        );
    }
}

#[test]
fn terminal_busy_does_not_report_command_success() {
    let result = api::RunShellCommandResult {
        result: Some(api::run_shell_command_result::Result::TerminalBusy(
            api::run_shell_command_result::TerminalBusy {
                running_command_id: "running-command".to_string(),
            },
        )),
        ..Default::default()
    };
    let text = render_shell_result(&result);
    assert!(text.contains("running-command"));
    assert!(text.contains("not started"));
}
