#![allow(deprecated)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE};
use prost::Message as _;
use serde_json::Value;
use uuid::Uuid;
use warp_multi_agent_api as api;

use crate::{provider::ProviderToolCall, request::McpToolSummary};

#[derive(Debug, Clone, PartialEq)]
pub struct ReadFilesToolCallFile {
    pub name: String,
    pub line_ranges: Vec<LineRange>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct LineRange {
    pub start: u32,
    pub end: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WarpToolCall {
    RunShellCommand {
        command: String,
        is_read_only: Option<bool>,
        is_risky: Option<bool>,
        uses_pager: Option<bool>,
        wait_until_complete: Option<bool>,
    },
    ReadFiles {
        files: Vec<ReadFilesToolCallFile>,
    },
    Grep {
        queries: Vec<String>,
        path: Option<String>,
    },
    FileGlob {
        patterns: Vec<String>,
        search_dir: Option<String>,
        max_matches: Option<i32>,
        max_depth: Option<i32>,
        min_depth: Option<i32>,
    },
    SearchCodebase {
        query: String,
        path_filters: Vec<String>,
        codebase_path: Option<String>,
    },
    ApplyFileDiffs {
        summary: String,
        diffs: Vec<FileDiff>,
        new_files: Vec<NewFile>,
        deleted_files: Vec<DeletedFile>,
        v4a_updates: Vec<V4aFileUpdate>,
    },
    SuggestPlan {
        summary: String,
        tasks: Vec<SuggestedTask>,
    },
    ReadMcpResource {
        uri: String,
        server_id: Option<String>,
    },
    CallMcpTool {
        name: String,
        args: Option<Value>,
        server_id: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedWarpToolCall {
    pub tool_call_id: String,
    pub tool: WarpToolCall,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FileDiff {
    pub file_path: String,
    pub search: Option<String>,
    pub replace: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewFile {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DeletedFile {
    pub file_path: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct V4aFileUpdate {
    pub file_path: String,
    pub move_to: Option<String>,
    pub hunks: Vec<V4aHunk>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct V4aHunk {
    pub change_context: Vec<String>,
    pub pre_context: Option<String>,
    pub old: Option<String>,
    pub new: Option<String>,
    pub post_context: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SuggestedTask {
    pub description: String,
}

pub fn format_sse_data_event(event: &api::ResponseEvent) -> String {
    format!("data: {}\n\n", URL_SAFE.encode(event.encode_to_vec()))
}

pub fn stream_init(conversation_id: &str, request_id: &str) -> api::ResponseEvent {
    api::ResponseEvent {
        r#type: Some(api::response_event::Type::Init(
            api::response_event::StreamInit {
                conversation_id: conversation_id.to_owned(),
                request_id: request_id.to_owned(),
                run_id: String::new(),
            },
        )),
    }
}

pub fn create_task(task_id: &str, description: &str) -> api::ResponseEvent {
    client_actions(vec![api::ClientAction {
        action: Some(api::client_action::Action::CreateTask(
            api::client_action::CreateTask {
                task: Some(api::Task {
                    id: task_id.to_owned(),
                    description: description.to_owned(),
                    ..Default::default()
                }),
            },
        )),
    }])
}

pub fn add_agent_output(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    text: &str,
) -> api::ResponseEvent {
    add_messages_to_task(
        task_id,
        vec![agent_output_message(message_id, task_id, request_id, text)],
    )
}

pub fn append_agent_output(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    text: &str,
) -> api::ResponseEvent {
    client_actions(vec![api::ClientAction {
        action: Some(api::client_action::Action::AppendToMessageContent(
            api::client_action::AppendToMessageContent {
                task_id: task_id.to_owned(),
                message: Some(agent_output_message(message_id, task_id, request_id, text)),
                mask: Some(prost_types::FieldMask {
                    paths: vec!["agent_output.text".to_owned()],
                }),
            },
        )),
    }])
}

pub fn add_conversation_summary(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    text: &str,
    token_count: i32,
) -> api::ResponseEvent {
    add_messages_to_task(
        task_id,
        vec![api::Message {
            id: message_id.to_owned(),
            task_id: task_id.to_owned(),
            request_id: request_id.to_owned(),
            timestamp: Some(timestamp_now()),
            message: Some(api::message::Message::Summarization(
                api::message::Summarization {
                    summary_type: Some(
                        api::message::summarization::SummaryType::ConversationSummary(
                            api::message::summarization::ConversationSummary {
                                summary: text.to_owned(),
                                token_count,
                            },
                        ),
                    ),
                    finished_duration: None,
                },
            )),
            ..Default::default()
        }],
    )
}

pub fn add_tool_call(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    tool_call: ParsedWarpToolCall,
) -> api::ResponseEvent {
    add_messages_to_task(
        task_id,
        vec![api::Message {
            id: message_id.to_owned(),
            task_id: task_id.to_owned(),
            request_id: request_id.to_owned(),
            timestamp: Some(timestamp_now()),
            message: Some(api::message::Message::ToolCall(api::message::ToolCall {
                tool_call_id: tool_call.tool_call_id,
                tool: Some(encode_tool_call(tool_call.tool)),
            })),
            ..Default::default()
        }],
    )
}

pub fn stream_finished_done(
    context_window_usage: Option<f32>,
    summarized: bool,
) -> api::ResponseEvent {
    stream_finished(api::response_event::stream_finished::Reason::Done(
        api::response_event::stream_finished::Done {},
    ))
    .with_conversation_usage(context_window_usage, summarized)
}

pub fn stream_finished_context_window_exceeded() -> api::ResponseEvent {
    stream_finished(
        api::response_event::stream_finished::Reason::ContextWindowExceeded(
            api::response_event::stream_finished::ContextWindowExceeded {},
        ),
    )
}

pub fn stream_finished_quota_limit() -> api::ResponseEvent {
    stream_finished(api::response_event::stream_finished::Reason::QuotaLimit(
        api::response_event::stream_finished::QuotaLimit {},
    ))
}

pub fn stream_finished_llm_unavailable() -> api::ResponseEvent {
    stream_finished(
        api::response_event::stream_finished::Reason::LlmUnavailable(
            api::response_event::stream_finished::LlmUnavailable {},
        ),
    )
}

pub fn stream_finished_invalid_api_key(model_name: Option<&str>) -> api::ResponseEvent {
    stream_finished(api::response_event::stream_finished::Reason::InvalidApiKey(
        api::response_event::stream_finished::InvalidApiKey {
            provider: api::LlmProvider::Openai as i32,
            model_name: model_name.unwrap_or("").to_owned(),
        },
    ))
}

pub fn stream_finished_internal_error(message: &str) -> api::ResponseEvent {
    stream_finished(api::response_event::stream_finished::Reason::InternalError(
        api::response_event::stream_finished::InternalError {
            message: message.to_owned(),
        },
    ))
}

pub fn parse_tool_call(
    tool_call: &ProviderToolCall,
    mcp_tools: &[McpToolSummary],
) -> anyhow::Result<Option<ParsedWarpToolCall>> {
    let args = parse_tool_arguments(&tool_call.arguments_text)?;
    let parsed = match tool_call.name.as_str() {
        "read_files" => parse_read_files(tool_call, &args),
        "run_shell_command" => parse_run_shell_command(tool_call, &args),
        "grep" => parse_grep(tool_call, &args),
        "search_codebase" => parse_search_codebase(tool_call, &args),
        "file_glob" => parse_file_glob(tool_call, &args),
        "read_mcp_resource" => parse_read_mcp_resource(tool_call, &args),
        "call_mcp_tool" => parse_call_mcp_tool(tool_call, &args),
        "apply_file_diffs" => parse_apply_file_diffs(tool_call, &args),
        "suggest_plan" => parse_suggest_plan(tool_call, &args),
        _ => parse_native_mcp_tool_call(tool_call, &args, mcp_tools),
    };
    Ok(parsed)
}

fn client_actions(actions: Vec<api::ClientAction>) -> api::ResponseEvent {
    api::ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions { actions },
        )),
    }
}

fn add_messages_to_task(task_id: &str, messages: Vec<api::Message>) -> api::ResponseEvent {
    client_actions(vec![api::ClientAction {
        action: Some(api::client_action::Action::AddMessagesToTask(
            api::client_action::AddMessagesToTask {
                task_id: task_id.to_owned(),
                messages,
            },
        )),
    }])
}

fn agent_output_message(
    message_id: &str,
    task_id: &str,
    request_id: &str,
    text: &str,
) -> api::Message {
    api::Message {
        id: message_id.to_owned(),
        task_id: task_id.to_owned(),
        request_id: request_id.to_owned(),
        timestamp: Some(timestamp_now()),
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: text.to_owned(),
            },
        )),
        ..Default::default()
    }
}

fn encode_tool_call(tool: WarpToolCall) -> api::message::tool_call::Tool {
    use api::message::tool_call;
    match tool {
        WarpToolCall::RunShellCommand {
            command,
            is_read_only,
            is_risky,
            uses_pager,
            wait_until_complete,
        } => {
            let is_read_only = is_read_only.unwrap_or(false);
            let is_risky = is_risky.unwrap_or_else(|| !is_read_only);
            tool_call::Tool::RunShellCommand(tool_call::RunShellCommand {
                command,
                is_read_only,
                uses_pager: uses_pager.unwrap_or(false),
                is_risky,
                risk_category: if is_read_only {
                    api::RiskCategory::ReadOnly as i32
                } else if is_risky {
                    api::RiskCategory::Risky as i32
                } else {
                    api::RiskCategory::Unspecified as i32
                },
                wait_until_complete_value: wait_until_complete.map(|value| {
                    tool_call::run_shell_command::WaitUntilCompleteValue::WaitUntilComplete(value)
                }),
                ..Default::default()
            })
        }
        WarpToolCall::ReadFiles { files } => tool_call::Tool::ReadFiles(tool_call::ReadFiles {
            files: files
                .into_iter()
                .map(|file| tool_call::read_files::File {
                    name: file.name,
                    line_ranges: file
                        .line_ranges
                        .into_iter()
                        .map(|range| api::FileContentLineRange {
                            start: range.start,
                            end: range.end,
                        })
                        .collect(),
                })
                .collect(),
        }),
        WarpToolCall::Grep { queries, path } => tool_call::Tool::Grep(tool_call::Grep {
            queries,
            path: path.unwrap_or_else(|| ".".to_owned()),
        }),
        WarpToolCall::FileGlob {
            patterns,
            search_dir,
            max_matches,
            max_depth,
            min_depth,
        } => tool_call::Tool::FileGlobV2(tool_call::FileGlobV2 {
            patterns,
            search_dir: search_dir.unwrap_or_else(|| ".".to_owned()),
            max_matches: max_matches.unwrap_or(0),
            max_depth: max_depth.unwrap_or(0),
            min_depth: min_depth.unwrap_or(0),
        }),
        WarpToolCall::SearchCodebase {
            query,
            path_filters,
            codebase_path,
        } => tool_call::Tool::SearchCodebase(tool_call::SearchCodebase {
            query,
            path_filters,
            codebase_path: codebase_path.unwrap_or_default(),
        }),
        WarpToolCall::ApplyFileDiffs {
            summary,
            diffs,
            new_files,
            deleted_files,
            v4a_updates,
        } => tool_call::Tool::ApplyFileDiffs(tool_call::ApplyFileDiffs {
            summary,
            diffs: diffs
                .into_iter()
                .map(|diff| tool_call::apply_file_diffs::FileDiff {
                    file_path: diff.file_path,
                    search: diff.search.unwrap_or_default(),
                    replace: diff.replace.unwrap_or_default(),
                })
                .collect(),
            new_files: new_files
                .into_iter()
                .map(|file| tool_call::apply_file_diffs::NewFile {
                    file_path: file.file_path,
                    content: file.content,
                })
                .collect(),
            deleted_files: deleted_files
                .into_iter()
                .map(|file| tool_call::apply_file_diffs::DeleteFile {
                    file_path: file.file_path,
                })
                .collect(),
            v4a_updates: v4a_updates
                .into_iter()
                .map(|update| tool_call::apply_file_diffs::V4aFileUpdate {
                    file_path: update.file_path,
                    move_to: update.move_to.unwrap_or_default(),
                    hunks: update
                        .hunks
                        .into_iter()
                        .map(|hunk| tool_call::apply_file_diffs::v4a_file_update::Hunk {
                            change_context: hunk.change_context,
                            pre_context: hunk.pre_context.unwrap_or_default(),
                            old: hunk.old.unwrap_or_default(),
                            new: hunk.new.unwrap_or_default(),
                            post_context: hunk.post_context.unwrap_or_default(),
                        })
                        .collect(),
                })
                .collect(),
        }),
        WarpToolCall::SuggestPlan { summary, tasks } => {
            tool_call::Tool::SuggestPlan(tool_call::SuggestPlan {
                summary,
                proposed_tasks: tasks
                    .into_iter()
                    .map(|task| api::Task {
                        id: Uuid::new_v4().to_string(),
                        description: task.description,
                        ..Default::default()
                    })
                    .collect(),
            })
        }
        WarpToolCall::ReadMcpResource { uri, server_id } => {
            tool_call::Tool::ReadMcpResource(tool_call::ReadMcpResource {
                uri,
                server_id: server_id.unwrap_or_default(),
            })
        }
        WarpToolCall::CallMcpTool {
            name,
            args,
            server_id,
        } => tool_call::Tool::CallMcpTool(tool_call::CallMcpTool {
            name,
            args: Some(json_object_to_struct(
                args.unwrap_or_else(|| Value::Object(Default::default())),
            )),
            server_id: server_id.unwrap_or_default(),
        }),
    }
}

trait WithConversationUsage {
    fn with_conversation_usage(self, context_window_usage: Option<f32>, summarized: bool) -> Self;
}

impl WithConversationUsage for api::ResponseEvent {
    fn with_conversation_usage(
        mut self,
        context_window_usage: Option<f32>,
        summarized: bool,
    ) -> Self {
        if let Some(api::response_event::Type::Finished(finished)) = self.r#type.as_mut()
            && let Some(context_window_usage) = context_window_usage
        {
            finished.conversation_usage_metadata = Some(
                api::response_event::stream_finished::ConversationUsageMetadata {
                    context_window_usage,
                    summarized,
                    credits_spent: 0.0,
                    ..Default::default()
                },
            );
        }
        self
    }
}

fn stream_finished(reason: api::response_event::stream_finished::Reason) -> api::ResponseEvent {
    api::ResponseEvent {
        r#type: Some(api::response_event::Type::Finished(
            api::response_event::StreamFinished {
                reason: Some(reason),
                ..Default::default()
            },
        )),
    }
}

fn parse_tool_arguments(raw: &str) -> anyhow::Result<Value> {
    if raw.trim().is_empty() {
        return Ok(Value::Object(Default::default()));
    }
    let parsed = serde_json::from_str(raw)?;
    Ok(match parsed {
        Value::Object(_) => parsed,
        _ => Value::Object(Default::default()),
    })
}

fn parse_read_files(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    let files = value_array(value_at(args, &["files"]))
        .or_else(|| value_array(value_at(args, &["paths"])))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|file| {
            if let Some(path) = value_string(Some(file)) {
                return Some(ReadFilesToolCallFile {
                    name: path.to_owned(),
                    line_ranges: Vec::new(),
                });
            }
            let name =
                value_string(value_at(file, &["name"]).or_else(|| value_at(file, &["path"])))?;
            let line_ranges = value_array(
                value_at(file, &["line_ranges"]).or_else(|| value_at(file, &["lineRanges"])),
            )
            .unwrap_or_default()
            .into_iter()
            .filter_map(|range| {
                let start = value_i32(value_at(range, &["start"]))?;
                let end = value_i32(value_at(range, &["end"]))?;
                Some(LineRange {
                    start: start.max(0) as u32,
                    end: end.max(0) as u32,
                })
            })
            .collect();
            Some(ReadFilesToolCallFile {
                name: name.to_owned(),
                line_ranges,
            })
        })
        .collect::<Vec<_>>();
    (!files.is_empty()).then(|| ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::ReadFiles { files },
    })
}

fn parse_run_shell_command(
    tool_call: &ProviderToolCall,
    args: &Value,
) -> Option<ParsedWarpToolCall> {
    let command = value_string(value_at(args, &["command"]))?.to_owned();
    let is_read_only = value_bool(value_at(args, &["is_read_only"]));
    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::RunShellCommand {
            command,
            is_read_only,
            is_risky: value_bool(value_at(args, &["is_risky"]))
                .or_else(|| is_read_only.map(|value| !value)),
            uses_pager: value_bool(value_at(args, &["uses_pager"])),
            wait_until_complete: value_bool(value_at(args, &["wait_until_complete"])),
        },
    })
}

fn parse_grep(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    let mut queries = string_list(value_at(args, &["queries"]));
    queries.extend(string_list(value_at(args, &["query"])));
    (!queries.is_empty()).then(|| ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::Grep {
            queries,
            path: value_string(value_at(args, &["path"])).map(str::to_owned),
        },
    })
}

fn parse_search_codebase(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::SearchCodebase {
            query: value_string(value_at(args, &["query"]))?.to_owned(),
            path_filters: string_list(value_at(args, &["path_filters"])),
            codebase_path: value_string(value_at(args, &["codebase_path"])).map(str::to_owned),
        },
    })
}

fn parse_file_glob(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    let mut patterns = string_list(value_at(args, &["patterns"]));
    patterns.extend(string_list(value_at(args, &["pattern"])));
    (!patterns.is_empty()).then(|| ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::FileGlob {
            patterns,
            search_dir: value_string(value_at(args, &["search_dir"])).map(str::to_owned),
            max_matches: value_i32(value_at(args, &["max_matches"])),
            max_depth: value_i32(value_at(args, &["max_depth"])),
            min_depth: value_i32(value_at(args, &["min_depth"])),
        },
    })
}

fn parse_read_mcp_resource(
    tool_call: &ProviderToolCall,
    args: &Value,
) -> Option<ParsedWarpToolCall> {
    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::ReadMcpResource {
            uri: value_string(
                value_at(args, &["uri"]).or_else(|| value_at(args, &["resource_uri"])),
            )?
            .to_owned(),
            server_id: value_string(
                value_at(args, &["server_id"]).or_else(|| value_at(args, &["serverId"])),
            )
            .map(str::to_owned),
        },
    })
}

fn parse_call_mcp_tool(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    let tool_args = value_at(args, &["args"]).or_else(|| value_at(args, &["arguments"]));
    let name = value_string(
        value_at(args, &["name"])
            .or_else(|| value_at(args, &["tool_name"]))
            .or_else(|| value_at(args, &["tool"]))
            .or_else(|| tool_args.and_then(|args| value_at(args, &["name"])))
            .or_else(|| tool_args.and_then(|args| value_at(args, &["tool_name"])))
            .or_else(|| tool_args.and_then(|args| value_at(args, &["tool"]))),
    )?;
    let server_id = value_string(
        value_at(args, &["server_id"])
            .or_else(|| value_at(args, &["serverId"]))
            .or_else(|| tool_args.and_then(|args| value_at(args, &["server_id"])))
            .or_else(|| tool_args.and_then(|args| value_at(args, &["serverId"]))),
    );

    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::CallMcpTool {
            name: name.to_owned(),
            server_id: server_id.map(str::to_owned),
            args: tool_args.and_then(call_mcp_tool_args_value),
        },
    })
}

fn parse_apply_file_diffs(
    tool_call: &ProviderToolCall,
    args: &Value,
) -> Option<ParsedWarpToolCall> {
    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::ApplyFileDiffs {
            summary: value_string(value_at(args, &["summary"]))
                .unwrap_or("Apply file edits")
                .to_owned(),
            diffs: value_array(value_at(args, &["diffs"]))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|diff| {
                    Some(FileDiff {
                        file_path: value_string(
                            value_at(diff, &["file_path"])
                                .or_else(|| value_at(diff, &["filePath"])),
                        )?
                        .to_owned(),
                        search: value_string(value_at(diff, &["search"])).map(str::to_owned),
                        replace: value_string(value_at(diff, &["replace"])).map(str::to_owned),
                    })
                })
                .collect(),
            new_files: value_array(value_at(args, &["new_files"]))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|file| {
                    Some(NewFile {
                        file_path: value_string(
                            value_at(file, &["file_path"])
                                .or_else(|| value_at(file, &["filePath"])),
                        )?
                        .to_owned(),
                        content: value_string(value_at(file, &["content"]))?.to_owned(),
                    })
                })
                .collect(),
            deleted_files: value_array(value_at(args, &["deleted_files"]))
                .unwrap_or_default()
                .into_iter()
                .filter_map(|file| {
                    Some(DeletedFile {
                        file_path: value_string(
                            value_at(file, &["file_path"])
                                .or_else(|| value_at(file, &["filePath"])),
                        )?
                        .to_owned(),
                    })
                })
                .collect(),
            v4a_updates: value_array(
                value_at(args, &["v4a_updates"]).or_else(|| value_at(args, &["v4aUpdates"])),
            )
            .unwrap_or_default()
            .into_iter()
            .filter_map(|update| {
                Some(V4aFileUpdate {
                    file_path: value_string(
                        value_at(update, &["file_path"])
                            .or_else(|| value_at(update, &["filePath"])),
                    )?
                    .to_owned(),
                    move_to: value_string(
                        value_at(update, &["move_to"]).or_else(|| value_at(update, &["moveTo"])),
                    )
                    .map(str::to_owned),
                    hunks: value_array(value_at(update, &["hunks"]))
                        .unwrap_or_default()
                        .into_iter()
                        .map(|hunk| V4aHunk {
                            change_context: string_list(
                                value_at(hunk, &["change_context"])
                                    .or_else(|| value_at(hunk, &["changeContext"])),
                            ),
                            pre_context: value_string(
                                value_at(hunk, &["pre_context"])
                                    .or_else(|| value_at(hunk, &["preContext"])),
                            )
                            .map(str::to_owned),
                            old: value_string(value_at(hunk, &["old"])).map(str::to_owned),
                            new: value_string(value_at(hunk, &["new"])).map(str::to_owned),
                            post_context: value_string(
                                value_at(hunk, &["post_context"])
                                    .or_else(|| value_at(hunk, &["postContext"])),
                            )
                            .map(str::to_owned),
                        })
                        .collect(),
                })
            })
            .collect(),
        },
    })
}

fn parse_suggest_plan(tool_call: &ProviderToolCall, args: &Value) -> Option<ParsedWarpToolCall> {
    let tasks = value_array(value_at(args, &["tasks"]))
        .unwrap_or_default()
        .into_iter()
        .filter_map(|task| {
            if let Some(text) = value_string(Some(task)) {
                return Some(SuggestedTask {
                    description: text.to_owned(),
                });
            }
            let description = value_string(
                value_at(task, &["description"]).or_else(|| value_at(task, &["title"])),
            )?;
            Some(SuggestedTask {
                description: description.to_owned(),
            })
        })
        .collect::<Vec<_>>();
    (!tasks.is_empty()).then(|| ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::SuggestPlan {
            summary: value_string(value_at(args, &["summary"]))
                .unwrap_or("Plan")
                .to_owned(),
            tasks,
        },
    })
}

fn parse_native_mcp_tool_call(
    tool_call: &ProviderToolCall,
    args: &Value,
    mcp_tools: &[McpToolSummary],
) -> Option<ParsedWarpToolCall> {
    let mut matches = mcp_tools.iter().filter(|tool| tool.name == tool_call.name);
    let matched = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    let single_args = args
        .as_object()
        .filter(|object| object.len() == 1)
        .and_then(|object| object.get("args"))
        .and_then(optional_object_value);
    Some(ParsedWarpToolCall {
        tool_call_id: tool_call.id.clone(),
        tool: WarpToolCall::CallMcpTool {
            name: tool_call.name.clone(),
            server_id: matched.server_id.clone(),
            args: single_args.or_else(|| optional_object_value(args)),
        },
    })
}

fn value_at<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a Value> {
    let object = value.as_object()?;
    keys.iter().find_map(|key| object.get(*key))
}

fn value_string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn value_bool(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

fn value_i32(value: Option<&Value>) -> Option<i32> {
    value.and_then(|value| match value {
        Value::Number(number) => number.as_i64().and_then(|value| i32::try_from(value).ok()),
        _ => None,
    })
}

fn value_array(value: Option<&Value>) -> Option<Vec<&Value>> {
    value
        .and_then(Value::as_array)
        .map(|values| values.iter().collect())
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(|value| value_string(Some(value)).map(str::to_owned))
            .collect(),
        Some(Value::String(value)) if !value.trim().is_empty() => vec![value.trim().to_owned()],
        _ => Vec::new(),
    }
}

fn optional_object_value(value: &Value) -> Option<Value> {
    match value {
        Value::Object(_) => Some(value.clone()),
        Value::String(raw) => serde_json::from_str::<Value>(raw)
            .ok()
            .and_then(|parsed| matches!(parsed, Value::Object(_)).then_some(parsed)),
        _ => None,
    }
}

fn call_mcp_tool_args_value(value: &Value) -> Option<Value> {
    let mut value = optional_object_value(value)?;
    let Value::Object(object) = &mut value else {
        return None;
    };
    for key in ["name", "tool_name", "tool", "server_id", "serverId"] {
        object.remove(key);
    }
    (!object.is_empty()).then_some(value)
}

fn json_object_to_struct(value: Value) -> prost_types::Struct {
    match value {
        Value::Object(fields) => prost_types::Struct {
            fields: fields
                .into_iter()
                .map(|(key, value)| (key, json_to_proto_value(value)))
                .collect(),
        },
        _ => prost_types::Struct::default(),
    }
}

fn json_to_proto_value(value: Value) -> prost_types::Value {
    prost_types::Value {
        kind: Some(match value {
            Value::Null => prost_types::value::Kind::NullValue(0),
            Value::Bool(value) => prost_types::value::Kind::BoolValue(value),
            Value::Number(value) => {
                prost_types::value::Kind::NumberValue(value.as_f64().unwrap_or(0.0))
            }
            Value::String(value) => prost_types::value::Kind::StringValue(value),
            Value::Array(values) => prost_types::value::Kind::ListValue(prost_types::ListValue {
                values: values.into_iter().map(json_to_proto_value).collect(),
            }),
            Value::Object(fields) => prost_types::value::Kind::StructValue(prost_types::Struct {
                fields: fields
                    .into_iter()
                    .map(|(key, value)| (key, json_to_proto_value(value)))
                    .collect(),
            }),
        }),
    }
}

fn timestamp_now() -> prost_types::Timestamp {
    let now = chrono::Utc::now();
    prost_types::Timestamp {
        seconds: now.timestamp(),
        nanos: now.timestamp_subsec_nanos() as i32,
    }
}

fn approximate_token_count(chars: usize) -> i32 {
    chars.div_ceil(4) as i32
}

pub fn summary_token_count(text: &str) -> i32 {
    approximate_token_count(text.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_sse_base64_url_event() {
        let event = stream_init("conversation", "request");
        let encoded = format_sse_data_event(&event);
        assert!(encoded.starts_with("data: "));
        assert!(encoded.ends_with("\n\n"));
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
    }

    #[test]
    fn rejects_duplicate_native_mcp_tool_name() {
        let call = ProviderToolCall {
            id: "call".to_owned(),
            name: "search".to_owned(),
            arguments_text: "{}".to_owned(),
        };
        let tools = vec![
            McpToolSummary {
                name: "search".to_owned(),
                server_id: Some("one".to_owned()),
            },
            McpToolSummary {
                name: "search".to_owned(),
                server_id: Some("two".to_owned()),
            },
        ];

        assert!(parse_tool_call(&call, &tools).unwrap().is_none());
    }

    #[test]
    fn parses_call_mcp_tool_name_from_nested_args() {
        let call = ProviderToolCall {
            id: "call".to_owned(),
            name: "call_mcp_tool".to_owned(),
            arguments_text: r#"{"args":{"query":"2026 FIFA World Cup opening match results schedule June 13 2026","name":"tavily-tavily_search"}}"#.to_owned(),
        };

        let parsed = parse_tool_call(&call, &[]).unwrap().unwrap();

        assert_eq!(parsed.tool_call_id, "call");
        assert_eq!(
            parsed.tool,
            WarpToolCall::CallMcpTool {
                name: "tavily-tavily_search".to_owned(),
                server_id: None,
                args: Some(serde_json::json!({
                    "query": "2026 FIFA World Cup opening match results schedule June 13 2026",
                })),
            }
        );
    }
}
