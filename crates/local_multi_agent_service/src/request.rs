#![allow(deprecated)]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::{DateTime, Utc};
use uuid::Uuid;
use warp_multi_agent_api as api;

#[derive(Debug, Clone, PartialEq)]
pub struct WarpRequestSummary {
    pub conversation_id: String,
    pub request_id: String,
    pub root_task_id: String,
    pub should_create_root_task: bool,
    pub prompt: String,
    pub is_summarization_request: bool,
    pub summarization_prompt: Option<String>,
    pub context_text: Option<String>,
    pub context_images: Vec<ContextImage>,
    pub tool_results: Vec<WarpToolResult>,
    pub mcp_tools: Vec<McpToolSummary>,
    pub openai_api_key: Option<String>,
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ContextImage {
    pub data: String,
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolSummary {
    pub name: String,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WarpToolResult {
    ReadFiles {
        tool_call_id: String,
        files: Vec<ToolResultFile>,
        error: Option<String>,
    },
    RunShellCommand {
        tool_call_id: String,
        command: Option<String>,
        output: Option<String>,
        exit_code: Option<i32>,
        error: Option<String>,
    },
    Grep {
        tool_call_id: String,
        matched_files: Vec<GrepMatchedFile>,
        error: Option<String>,
    },
    FileGlob {
        tool_call_id: String,
        matched_files: Vec<String>,
        warnings: Option<String>,
        error: Option<String>,
    },
    ApplyFileDiffs {
        tool_call_id: String,
        updated_files: Vec<String>,
        deleted_files: Vec<String>,
        error: Option<String>,
    },
    SuggestPlan {
        tool_call_id: String,
        status: SuggestPlanStatus,
        plan_text: Option<String>,
    },
    Generic {
        tool_call_id: String,
        name: String,
        content: String,
        error: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultFile {
    pub file_path: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GrepMatchedFile {
    pub file_path: String,
    pub line_numbers: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuggestPlanStatus {
    Accepted,
    Edited,
}

pub fn decode_warp_request(request: api::Request) -> WarpRequestSummary {
    let conversation_id = request
        .metadata
        .as_ref()
        .and_then(|metadata| non_empty(&metadata.conversation_id))
        .map(str::to_owned)
        .unwrap_or_else(random_uuid);
    let request_id = random_uuid();
    let decoded_root_task_id = request
        .task_context
        .as_ref()
        .and_then(|context| context.tasks.first())
        .and_then(|task| non_empty(&task.id))
        .map(str::to_owned);
    let root_task_id = decoded_root_task_id.clone().unwrap_or_else(random_uuid);
    let tool_results = decode_tool_results(request.input.as_ref());
    let (attached_context_text, context_images) = decode_attached_context(
        request
            .input
            .as_ref()
            .and_then(|input| input.context.as_ref()),
    );
    let referenced_attachments = decode_referenced_attachments(request.input.as_ref());
    let (mcp_sections, mcp_tools) = decode_mcp_context(request.mcp_context.as_ref());
    let mut context_sections = Vec::new();
    if let Some(text) = attached_context_text {
        context_sections.push(text);
    }
    context_sections.extend(referenced_attachments);
    context_sections.extend(mcp_sections);
    let context_text = (!context_sections.is_empty()).then(|| context_sections.join("\n\n"));
    let (is_summarization_request, summarization_prompt) =
        decode_summarization_input(request.input.as_ref());
    let prompt = decode_input_prompt(request.input.as_ref())
        .unwrap_or_else(|| format_tool_results_prompt(&tool_results));
    let (openai_api_key, model) = decode_settings(request.settings.as_ref());

    WarpRequestSummary {
        conversation_id,
        request_id,
        root_task_id,
        should_create_root_task: decoded_root_task_id.is_none(),
        prompt,
        is_summarization_request,
        summarization_prompt,
        context_text,
        context_images,
        tool_results,
        mcp_tools,
        openai_api_key,
        model,
    }
}

fn decode_settings(settings: Option<&api::request::Settings>) -> (Option<String>, Option<String>) {
    let model = settings
        .and_then(|settings| settings.model_config.as_ref())
        .and_then(|model_config| {
            first_non_empty([
                model_config.base.as_str(),
                model_config.coding.as_str(),
                model_config.cli_agent.as_str(),
            ])
            .map(str::to_owned)
        });
    let openai_api_key = settings
        .and_then(|settings| settings.api_keys.as_ref())
        .and_then(|keys| non_empty(&keys.openai).map(str::to_owned));
    (openai_api_key, model)
}

fn decode_input_prompt(input: Option<&api::request::Input>) -> Option<String> {
    let input = input?;
    use api::request::input::Type;
    match input.r#type.as_ref()? {
        Type::UserInputs(user_inputs) => user_inputs.inputs.iter().find_map(|user_input| {
            use api::request::input::user_inputs::user_input::Input;
            match user_input.input.as_ref()? {
                Input::UserQuery(query) => non_empty(&query.query).map(str::to_owned),
                Input::CliAgentUserQuery(query) => query
                    .user_query
                    .as_ref()
                    .and_then(|query| non_empty(&query.query).map(str::to_owned)),
                Input::MessagesReceivedFromAgents(messages) => {
                    format_received_messages(&messages.messages)
                }
                _ => None,
            }
        }),
        Type::QueryWithCannedResponse(query) => non_empty(&query.query).map(str::to_owned),
        Type::AutoCodeDiffQuery(query) => non_empty(&query.query).map(str::to_owned),
        Type::ResumeConversation(_) => Some("Resume this conversation.".to_owned()),
        Type::InitProjectRules(_) => Some("Initialize project rules for this project.".to_owned()),
        Type::GeneratePassiveSuggestions(suggestions) => {
            Some(format_passive_suggestion_prompt(suggestions))
        }
        Type::CreateNewProject(project) => non_empty(&project.query).map(str::to_owned),
        Type::CloneRepository(repo) => {
            non_empty(&repo.url).map(|url| format!("Clone repository: {url}"))
        }
        Type::CodeReview(_) => {
            Some("Review the attached code review comments and diff context.".to_owned())
        }
        Type::SummarizeConversation(summary) => non_empty(&summary.prompt).map(str::to_owned),
        Type::CreateEnvironment(environment) => (!environment.repo_paths.is_empty()).then(|| {
            format!(
                "Create a development environment for:\n{}",
                environment.repo_paths.join("\n")
            )
        }),
        Type::FetchReviewComments(comments) => non_empty(&comments.repo_path)
            .map(|repo_path| format!("Fetch review comments for repository: {repo_path}")),
        Type::StartFromAmbientRunPrompt(prompt) => {
            let lines = [
                non_empty(&prompt.runtime_base_prompt).map(str::to_owned),
                non_empty(&prompt.attachments_dir)
                    .map(|dir| format!("Attachments directory: {dir}")),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            (!lines.is_empty()).then(|| lines.join("\n\n"))
        }
        Type::InvokeSkill(invoke) => invoke
            .user_query
            .as_ref()
            .and_then(|query| non_empty(&query.query).map(str::to_owned)),
        Type::UserQuery(query) => non_empty(&query.query).map(str::to_owned),
        Type::ToolCallResult(_) => None,
    }
}

fn decode_summarization_input(input: Option<&api::request::Input>) -> (bool, Option<String>) {
    match input.and_then(|input| input.r#type.as_ref()) {
        Some(api::request::input::Type::SummarizeConversation(summary)) => {
            (true, non_empty(&summary.prompt).map(str::to_owned))
        }
        _ => (false, None),
    }
}

fn format_received_messages(
    messages: &[api::request::input::user_inputs::messages_received_from_agents::ReceivedMessage],
) -> Option<String> {
    let formatted = messages
        .iter()
        .filter_map(|message| {
            if message.subject.is_empty() && message.message_body.is_empty() {
                return None;
            }
            Some(
                [
                    Some("Message received from another agent:".to_owned()),
                    non_empty(&message.sender_agent_id).map(|sender| format!("Sender: {sender}")),
                    non_empty(&message.subject).map(|subject| format!("Subject: {subject}")),
                    non_empty(&message.message_body).map(|body| format!("Body:\n{body}")),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            )
        })
        .collect::<Vec<_>>();
    (!formatted.is_empty()).then(|| formatted.join("\n\n"))
}

fn format_passive_suggestion_prompt(
    suggestions: &api::request::input::GeneratePassiveSuggestions,
) -> String {
    let mut lines = vec!["Generate passive suggestions for the current context.".to_owned()];
    if let Some(
        api::request::input::generate_passive_suggestions::Trigger::ShellCommandCompleted(
            completed,
        ),
    ) = &suggestions.trigger
    {
        if let Some(executed) = completed
            .executed_shell_command
            .as_ref()
            .and_then(format_executed_shell_command)
        {
            lines.push(executed);
        }
    }
    lines.join("\n\n")
}

fn decode_attached_context(
    context: Option<&api::InputContext>,
) -> (Option<String>, Vec<ContextImage>) {
    let Some(context) = context else {
        return (None, Vec::new());
    };
    let mut sections = Vec::new();
    let mut images = Vec::new();

    if let Some(directory) = context.directory.as_ref() {
        if let Some(pwd) = non_empty(&directory.pwd) {
            sections.push(format!("Current directory: {pwd}"));
        }
    }
    if let Some(os) = context.operating_system.as_ref() {
        let os_text = [non_empty(&os.platform), non_empty(&os.distribution)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if !os_text.is_empty() {
            sections.push(format!("Operating system: {os_text}"));
        }
    }
    if let Some(shell) = context.shell.as_ref() {
        let shell_text = [non_empty(&shell.name), non_empty(&shell.version)]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" ");
        if !shell_text.is_empty() {
            sections.push(format!("Shell: {shell_text}"));
        }
    }
    if let Some(timestamp) = context.current_time.as_ref().and_then(format_timestamp) {
        sections.push(format!("Current time: {timestamp}"));
    }
    for selected in &context.selected_text {
        if let Some(text) = non_empty(&selected.text) {
            sections.push(format!("Selected text:\n{text}"));
        }
    }
    for command in &context.executed_shell_commands {
        if let Some(command) = format_executed_shell_command(command) {
            sections.push(command);
        }
    }
    for image in &context.images {
        if !image.data.is_empty() {
            images.push(ContextImage {
                data: STANDARD.encode(&image.data),
                mime_type: non_empty(&image.mime_type)
                    .unwrap_or("image/png")
                    .to_owned(),
            });
        }
    }
    for file in &context.files {
        if let Some(content) = file.content.as_ref().and_then(format_file_content) {
            sections.push(content);
        }
    }
    let codebases = context
        .codebases
        .iter()
        .filter_map(|codebase| {
            (non_empty(&codebase.name).is_some() || non_empty(&codebase.path).is_some()).then(
                || {
                    format!(
                        "{}: {}",
                        non_empty(&codebase.name).unwrap_or("codebase"),
                        non_empty(&codebase.path).unwrap_or("")
                    )
                },
            )
        })
        .collect::<Vec<_>>();
    if !codebases.is_empty() {
        sections.push(format!("Indexed codebases:\n{}", codebases.join("\n")));
    }
    for project_rules in &context.project_rules {
        let active_rules = project_rules
            .active_rule_files
            .iter()
            .filter_map(format_file_content)
            .collect::<Vec<_>>();
        let additional = project_rules
            .additional_rule_file_paths
            .iter()
            .filter_map(|path| non_empty(path).map(str::to_owned))
            .collect::<Vec<_>>();
        if non_empty(&project_rules.root_path).is_some()
            || !active_rules.is_empty()
            || !additional.is_empty()
        {
            sections.push(
                [
                    Some(format!(
                        "Project rules{}:",
                        non_empty(&project_rules.root_path)
                            .map(|root| format!(" for {root}"))
                            .unwrap_or_default()
                    )),
                    (!active_rules.is_empty()).then(|| active_rules.join("\n")),
                    (!additional.is_empty())
                        .then(|| format!("Additional rule files:\n{}", additional.join("\n"))),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            );
        }
    }
    if let Some(git) = context.git.as_ref() {
        let git_text = [
            non_empty(&git.branch).map(|branch| format!("branch={branch}")),
            non_empty(&git.head).map(|head| format!("head={head}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(", ");
        if !git_text.is_empty() {
            sections.push(format!("Git: {git_text}"));
        }
    }
    if let Some(skills_context) = context.updated_skills_context.as_ref() {
        let skills = skills_context
            .available_skills
            .iter()
            .filter_map(|skill| {
                (non_empty(&skill.name).is_some() || non_empty(&skill.description).is_some()).then(
                    || {
                        format!(
                            "{}{}",
                            non_empty(&skill.name).unwrap_or("skill"),
                            non_empty(&skill.description)
                                .map(|description| format!(": {description}"))
                                .unwrap_or_default()
                        )
                    },
                )
            })
            .collect::<Vec<_>>();
        if !skills.is_empty() {
            sections.push(format!("Available skills:\n{}", skills.join("\n")));
        }
    }
    if let Some(lsp_context) = context.updated_lsp_servers_context.as_ref() {
        let servers = lsp_context
            .available_lsp_servers
            .iter()
            .filter_map(|server| {
                (non_empty(&server.server_name).is_some()
                    || non_empty(&server.workspace_root).is_some())
                .then(|| {
                    format!(
                        "{}{}",
                        non_empty(&server.server_name).unwrap_or("LSP"),
                        non_empty(&server.workspace_root)
                            .map(|root| format!(" ({root})"))
                            .unwrap_or_default()
                    )
                })
            })
            .collect::<Vec<_>>();
        if !servers.is_empty() {
            sections.push(format!("Available LSP servers:\n{}", servers.join("\n")));
        }
    }

    (
        (!sections.is_empty()).then(|| sections.join("\n\n")),
        images,
    )
}

fn decode_referenced_attachments(input: Option<&api::request::Input>) -> Vec<String> {
    let mut sections = Vec::new();
    let Some(input) = input else {
        return sections;
    };
    match input.r#type.as_ref() {
        Some(api::request::input::Type::UserInputs(user_inputs)) => {
            for user_input in &user_inputs.inputs {
                use api::request::input::user_inputs::user_input::Input;
                let queries = match user_input.input.as_ref() {
                    Some(Input::UserQuery(query)) => vec![query],
                    Some(Input::CliAgentUserQuery(query)) => {
                        query.user_query.as_ref().into_iter().collect()
                    }
                    _ => Vec::new(),
                };
                for query in queries {
                    for (key, attachment) in &query.referenced_attachments {
                        if let Some(formatted) = format_attachment(attachment, Some(key)) {
                            sections.push(formatted);
                        }
                    }
                }
            }
        }
        Some(api::request::input::Type::UserQuery(query)) => {
            for (key, attachment) in &query.referenced_attachments {
                if let Some(formatted) = format_attachment(attachment, Some(key)) {
                    sections.push(formatted);
                }
            }
        }
        _ => {}
    }
    sections
}

fn decode_mcp_context(
    context: Option<&api::request::McpContext>,
) -> (Vec<String>, Vec<McpToolSummary>) {
    let Some(context) = context else {
        return (Vec::new(), Vec::new());
    };
    let mut sections = Vec::new();
    let mut tools = Vec::new();

    let legacy_resources = context
        .resources
        .iter()
        .filter_map(|resource| {
            (non_empty(&resource.uri).is_some()
                || non_empty(&resource.name).is_some()
                || non_empty(&resource.description).is_some())
            .then(|| {
                format!(
                    "{}{}{}",
                    non_empty(&resource.name)
                        .or_else(|| non_empty(&resource.uri))
                        .unwrap_or("resource"),
                    non_empty(&resource.description)
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default(),
                    non_empty(&resource.uri)
                        .map(|uri| format!(" ({uri})"))
                        .unwrap_or_default()
                )
            })
        })
        .collect::<Vec<_>>();
    let legacy_tools = context
        .tools
        .iter()
        .filter_map(|tool| {
            if let Some(name) = non_empty(&tool.name) {
                tools.push(McpToolSummary {
                    name: name.to_owned(),
                    server_id: None,
                });
            }
            (non_empty(&tool.name).is_some() || non_empty(&tool.description).is_some()).then(|| {
                format!(
                    "{}{}",
                    non_empty(&tool.name).unwrap_or("tool"),
                    non_empty(&tool.description)
                        .map(|description| format!(": {description}"))
                        .unwrap_or_default()
                )
            })
        })
        .collect::<Vec<_>>();
    if !legacy_resources.is_empty() {
        sections.push(format!(
            "Available MCP resources:\n{}",
            legacy_resources.join("\n")
        ));
    }
    if !legacy_tools.is_empty() {
        sections.push(format!("Available MCP tools:\n{}", legacy_tools.join("\n")));
    }

    for server in &context.servers {
        let resources = server
            .resources
            .iter()
            .filter_map(|resource| {
                (non_empty(&resource.name).is_some() || non_empty(&resource.uri).is_some()).then(
                    || {
                        format!(
                            "{}{}",
                            non_empty(&resource.name)
                                .or_else(|| non_empty(&resource.uri))
                                .unwrap_or("resource"),
                            non_empty(&resource.uri)
                                .map(|uri| format!(" ({uri})"))
                                .unwrap_or_default()
                        )
                    },
                )
            })
            .collect::<Vec<_>>();
        let server_tools = server
            .tools
            .iter()
            .filter_map(|tool| {
                if let Some(name) = non_empty(&tool.name) {
                    tools.push(McpToolSummary {
                        name: name.to_owned(),
                        server_id: non_empty(&server.id).map(str::to_owned),
                    });
                }
                (non_empty(&tool.name).is_some() || non_empty(&tool.description).is_some()).then(
                    || {
                        format!(
                            "{}{}",
                            non_empty(&tool.name).unwrap_or("tool"),
                            non_empty(&tool.description)
                                .map(|description| format!(": {description}"))
                                .unwrap_or_default()
                        )
                    },
                )
            })
            .collect::<Vec<_>>();
        if non_empty(&server.name).is_some()
            || non_empty(&server.description).is_some()
            || non_empty(&server.id).is_some()
            || !resources.is_empty()
            || !server_tools.is_empty()
        {
            sections.push(
                [
                    Some(format!(
                        "MCP server: {}",
                        non_empty(&server.name)
                            .or_else(|| non_empty(&server.id))
                            .unwrap_or("unnamed")
                    )),
                    non_empty(&server.description)
                        .map(|description| format!("Description: {description}")),
                    (!resources.is_empty())
                        .then(|| format!("Resources:\n{}", resources.join("\n"))),
                    (!server_tools.is_empty())
                        .then(|| format!("Tools:\n{}", server_tools.join("\n"))),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("\n"),
            );
        }
    }

    (sections, tools)
}

fn decode_tool_results(input: Option<&api::request::Input>) -> Vec<WarpToolResult> {
    let mut results = Vec::new();
    let Some(input) = input else {
        return results;
    };
    match input.r#type.as_ref() {
        Some(api::request::input::Type::UserInputs(user_inputs)) => {
            for user_input in &user_inputs.inputs {
                if let Some(api::request::input::user_inputs::user_input::Input::ToolCallResult(
                    result,
                )) = user_input.input.as_ref()
                {
                    if let Some(decoded) =
                        decode_tool_result(result.tool_call_id.as_str(), result.result.as_ref())
                    {
                        results.push(decoded);
                    }
                }
            }
        }
        Some(api::request::input::Type::ToolCallResult(result)) => {
            if let Some(decoded) =
                decode_tool_result(result.tool_call_id.as_str(), result.result.as_ref())
            {
                results.push(decoded);
            }
        }
        _ => {}
    }
    results
}

fn decode_tool_result(
    tool_call_id: &str,
    result: Option<&api::request::input::tool_call_result::Result>,
) -> Option<WarpToolResult> {
    let tool_call_id = non_empty(tool_call_id)?.to_owned();
    use api::request::input::tool_call_result::Result;
    match result? {
        Result::RunShellCommand(result) => {
            Some(decode_run_shell_command_result(tool_call_id, result))
        }
        Result::ReadFiles(result) => Some(decode_read_files_result(tool_call_id, result)),
        Result::SearchCodebase(result) => Some(decode_search_codebase_result(tool_call_id, result)),
        Result::ApplyFileDiffs(result) => {
            Some(decode_apply_file_diffs_result(tool_call_id, result))
        }
        Result::SuggestPlan(result) => Some(decode_suggest_plan_result(tool_call_id, result)),
        Result::Grep(result) => Some(decode_grep_result(tool_call_id, result)),
        Result::FileGlob(result) => Some(decode_file_glob_result(tool_call_id, result)),
        Result::FileGlobV2(result) => Some(decode_file_glob_v2_result(tool_call_id, result)),
        Result::ReadMcpResource(result) => {
            Some(decode_read_mcp_resource_result(tool_call_id, result))
        }
        Result::CallMcpTool(result) => Some(decode_call_mcp_tool_result(tool_call_id, result)),
        Result::ReadShellCommandOutput(result) => Some(decode_read_shell_command_output_result(
            tool_call_id,
            result,
        )),
        Result::ReadSkill(result) => Some(decode_read_skill_result(tool_call_id, result)),
        Result::FetchConversation(result) => {
            Some(decode_fetch_conversation_result(tool_call_id, result))
        }
        _ => None,
    }
}

fn decode_read_files_result(tool_call_id: String, result: &api::ReadFilesResult) -> WarpToolResult {
    let mut files = Vec::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::read_files_result::Result::TextFilesSuccess(success)) => {
            files.extend(success.files.iter().map(|file| ToolResultFile {
                file_path: file.file_path.clone(),
                content: file.content.clone(),
            }));
        }
        Some(api::read_files_result::Result::AnyFilesSuccess(success)) => {
            files.extend(
                success
                    .files
                    .iter()
                    .filter_map(|file| match file.content.as_ref() {
                        Some(api::any_file_content::Content::TextContent(content)) => {
                            Some(ToolResultFile {
                                file_path: content.file_path.clone(),
                                content: content.content.clone(),
                            })
                        }
                        _ => None,
                    }),
            );
        }
        Some(api::read_files_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::ReadFiles {
        tool_call_id,
        files,
        error,
    }
}

fn decode_run_shell_command_result(
    tool_call_id: String,
    result: &api::RunShellCommandResult,
) -> WarpToolResult {
    match result.result.as_ref() {
        Some(api::run_shell_command_result::Result::CommandFinished(finished)) => {
            WarpToolResult::RunShellCommand {
                tool_call_id,
                command: non_empty(&result.command).map(str::to_owned),
                output: Some(finished.output.clone()),
                exit_code: Some(finished.exit_code),
                error: None,
            }
        }
        Some(api::run_shell_command_result::Result::PermissionDenied(_)) => {
            WarpToolResult::RunShellCommand {
                tool_call_id,
                command: non_empty(&result.command).map(str::to_owned),
                output: None,
                exit_code: None,
                error: Some("Permission denied.".to_owned()),
            }
        }
        _ => WarpToolResult::RunShellCommand {
            tool_call_id,
            command: non_empty(&result.command).map(str::to_owned),
            output: Some(result.output.clone()),
            exit_code: Some(result.exit_code),
            error: None,
        },
    }
}

fn decode_grep_result(tool_call_id: String, result: &api::GrepResult) -> WarpToolResult {
    let mut matched_files = Vec::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::grep_result::Result::Success(success)) => {
            matched_files.extend(success.matched_files.iter().map(|file| {
                GrepMatchedFile {
                    file_path: file.file_path.clone(),
                    line_numbers: file
                        .matched_lines
                        .iter()
                        .map(|line| line.line_number)
                        .collect(),
                }
            }));
        }
        Some(api::grep_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::Grep {
        tool_call_id,
        matched_files,
        error,
    }
}

fn decode_file_glob_result(tool_call_id: String, result: &api::FileGlobResult) -> WarpToolResult {
    let mut matched_files = Vec::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::file_glob_result::Result::Success(success)) => {
            matched_files.extend(
                success
                    .matched_files
                    .lines()
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(str::to_owned),
            );
        }
        Some(api::file_glob_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::FileGlob {
        tool_call_id,
        matched_files,
        warnings: None,
        error,
    }
}

fn decode_file_glob_v2_result(
    tool_call_id: String,
    result: &api::FileGlobV2Result,
) -> WarpToolResult {
    let mut matched_files = Vec::new();
    let mut warnings = None;
    let mut error = None;
    match result.result.as_ref() {
        Some(api::file_glob_v2_result::Result::Success(success)) => {
            matched_files.extend(
                success
                    .matched_files
                    .iter()
                    .map(|file| file.file_path.clone()),
            );
            warnings = non_empty(&success.warnings).map(str::to_owned);
        }
        Some(api::file_glob_v2_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::FileGlob {
        tool_call_id,
        matched_files,
        warnings,
        error,
    }
}

fn decode_apply_file_diffs_result(
    tool_call_id: String,
    result: &api::ApplyFileDiffsResult,
) -> WarpToolResult {
    let mut updated_files = Vec::new();
    let mut deleted_files = Vec::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::apply_file_diffs_result::Result::Success(success)) => {
            updated_files.extend(
                success
                    .updated_files
                    .iter()
                    .map(|file| file.file_path.clone()),
            );
            updated_files.extend(success.updated_files_v2.iter().filter_map(|file| {
                file.file
                    .as_ref()
                    .and_then(|file| non_empty(&file.file_path).map(str::to_owned))
            }));
            deleted_files.extend(
                success
                    .deleted_files
                    .iter()
                    .map(|file| file.file_path.clone()),
            );
        }
        Some(api::apply_file_diffs_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::ApplyFileDiffs {
        tool_call_id,
        updated_files,
        deleted_files,
        error,
    }
}

fn decode_suggest_plan_result(
    tool_call_id: String,
    result: &api::SuggestPlanResult,
) -> WarpToolResult {
    match result.result.as_ref() {
        Some(api::suggest_plan_result::Result::UserEditedPlan(plan)) => {
            WarpToolResult::SuggestPlan {
                tool_call_id,
                status: SuggestPlanStatus::Edited,
                plan_text: Some(plan.plan_text.clone()),
            }
        }
        _ => WarpToolResult::SuggestPlan {
            tool_call_id,
            status: SuggestPlanStatus::Accepted,
            plan_text: None,
        },
    }
}

fn decode_search_codebase_result(
    tool_call_id: String,
    result: &api::SearchCodebaseResult,
) -> WarpToolResult {
    let mut content = String::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::search_codebase_result::Result::Success(success)) => {
            content = success
                .files
                .iter()
                .filter_map(format_file_content)
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        Some(api::search_codebase_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::Generic {
        tool_call_id,
        name: "search_codebase".to_owned(),
        content,
        error,
    }
}

fn decode_read_mcp_resource_result(
    tool_call_id: String,
    result: &api::ReadMcpResourceResult,
) -> WarpToolResult {
    let mut content = String::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::read_mcp_resource_result::Result::Success(success)) => {
            content = success
                .contents
                .iter()
                .filter_map(format_mcp_resource_content)
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        Some(api::read_mcp_resource_result::Result::Error(err)) => {
            error = Some(err.message.clone())
        }
        None => {}
    }
    WarpToolResult::Generic {
        tool_call_id,
        name: "read_mcp_resource".to_owned(),
        content,
        error,
    }
}

fn decode_call_mcp_tool_result(
    tool_call_id: String,
    result: &api::CallMcpToolResult,
) -> WarpToolResult {
    let mut content = String::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::call_mcp_tool_result::Result::Success(success)) => {
            content = success
                .results
                .iter()
                .filter_map(|result| {
                    let result = result.result.as_ref()?;
                    match result {
                        api::call_mcp_tool_result::success::result::Result::Text(text) => {
                            Some(text.text.clone())
                        }
                        api::call_mcp_tool_result::success::result::Result::Image(image) => {
                            Some(format!(
                                "Image result{}",
                                non_empty(&image.mime_type)
                                    .map(|mime| format!(" ({mime})"))
                                    .unwrap_or_default()
                            ))
                        }
                        api::call_mcp_tool_result::success::result::Result::Resource(resource) => {
                            format_mcp_resource_content(resource)
                        }
                    }
                })
                .collect::<Vec<_>>()
                .join("\n\n");
        }
        Some(api::call_mcp_tool_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::Generic {
        tool_call_id,
        name: "call_mcp_tool".to_owned(),
        content,
        error,
    }
}

fn decode_read_shell_command_output_result(
    tool_call_id: String,
    result: &api::ReadShellCommandOutputResult,
) -> WarpToolResult {
    let content = [
        non_empty(&result.command).map(|command| format!("Command: {command}")),
        result.result.as_ref().and_then(|result| match result {
            api::read_shell_command_output_result::Result::LongRunningCommandSnapshot(snapshot) => {
                format_shell_snapshot(snapshot)
            }
            api::read_shell_command_output_result::Result::CommandFinished(finished) => {
                format_shell_finished(finished)
            }
            api::read_shell_command_output_result::Result::Error(_) => None,
        }),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("\n");
    let error = result.result.as_ref().and_then(|result| match result {
        api::read_shell_command_output_result::Result::Error(_) => {
            Some("Shell command not found.".to_owned())
        }
        _ => None,
    });
    WarpToolResult::Generic {
        tool_call_id,
        name: "read_shell_command_output".to_owned(),
        content,
        error,
    }
}

fn decode_read_skill_result(tool_call_id: String, result: &api::ReadSkillResult) -> WarpToolResult {
    let mut content = String::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::read_skill_result::Result::Success(success)) => {
            content = success
                .content
                .as_ref()
                .and_then(format_file_content)
                .unwrap_or_default();
        }
        Some(api::read_skill_result::Result::Error(err)) => error = Some(err.message.clone()),
        None => {}
    }
    WarpToolResult::Generic {
        tool_call_id,
        name: "read_skill".to_owned(),
        content,
        error,
    }
}

fn decode_fetch_conversation_result(
    tool_call_id: String,
    result: &api::FetchConversationResult,
) -> WarpToolResult {
    let mut content = String::new();
    let mut error = None;
    match result.result.as_ref() {
        Some(api::fetch_conversation_result::Result::Success(success)) => {
            if let Some(path) = non_empty(&success.directory_path) {
                content = format!("Conversation materialized at: {path}");
            }
        }
        Some(api::fetch_conversation_result::Result::Error(err)) => {
            error = Some(err.message.clone())
        }
        None => {}
    }
    WarpToolResult::Generic {
        tool_call_id,
        name: "fetch_conversation".to_owned(),
        content,
        error,
    }
}

fn format_tool_results_prompt(tool_results: &[WarpToolResult]) -> String {
    if tool_results.is_empty() {
        return String::new();
    }
    let mut sections = vec![
        "Tool results are available. Use them to continue answering the user's original request."
            .to_owned(),
    ];
    sections.extend(tool_results.iter().map(format_tool_result));
    sections.join("\n\n")
}

fn format_tool_result(result: &WarpToolResult) -> String {
    match result {
        WarpToolResult::ReadFiles { files, error, .. } => format_error_or(error, || {
            files
                .iter()
                .map(|file| format!("File: {}\n{}", file.file_path, file.content))
                .collect::<Vec<_>>()
                .join("\n\n")
        }),
        WarpToolResult::RunShellCommand {
            command,
            output,
            exit_code,
            error,
            ..
        } => format_error_or(error, || {
            format!(
                "Command: {}\nExit code: {}\nOutput:\n{}",
                command.as_deref().unwrap_or(""),
                exit_code.map(|code| code.to_string()).unwrap_or_default(),
                output.as_deref().unwrap_or("")
            )
        }),
        WarpToolResult::Grep {
            matched_files,
            error,
            ..
        } => format_error_or(error, || {
            matched_files
                .iter()
                .map(|file| {
                    if file.line_numbers.is_empty() {
                        file.file_path.clone()
                    } else {
                        format!(
                            "{} lines {}",
                            file.file_path,
                            file.line_numbers
                                .iter()
                                .map(u32::to_string)
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    }
                })
                .collect::<Vec<_>>()
                .join("\n")
        }),
        WarpToolResult::FileGlob {
            matched_files,
            warnings,
            error,
            ..
        } => format_error_or(error, || {
            format!(
                "{}{}",
                matched_files.join("\n"),
                warnings
                    .as_ref()
                    .filter(|warnings| !warnings.is_empty())
                    .map(|warnings| format!("\nWarnings:\n{warnings}"))
                    .unwrap_or_default()
            )
        }),
        WarpToolResult::ApplyFileDiffs {
            updated_files,
            deleted_files,
            error,
            ..
        } => format_error_or(error, || {
            format!(
                "Updated files:\n{}\nDeleted files:\n{}",
                updated_files.join("\n"),
                deleted_files.join("\n")
            )
        }),
        WarpToolResult::SuggestPlan {
            status, plan_text, ..
        } => format!(
            "Status: {}{}",
            match status {
                SuggestPlanStatus::Accepted => "accepted",
                SuggestPlanStatus::Edited => "edited",
            },
            plan_text
                .as_ref()
                .map(|plan| format!("\nPlan:\n{plan}"))
                .unwrap_or_default()
        ),
        WarpToolResult::Generic {
            name,
            content,
            error,
            ..
        } => format_error_or(error, || format!("{name} result:\n{content}")),
    }
}

fn format_error_or(error: &Option<String>, fallback: impl FnOnce() -> String) -> String {
    error
        .as_ref()
        .filter(|error| !error.is_empty())
        .map(|error| format!("Error: {error}"))
        .unwrap_or_else(fallback)
}

fn format_attachment(attachment: &api::Attachment, label: Option<&str>) -> Option<String> {
    use api::attachment::Value;
    let body = match attachment.value.as_ref()? {
        Value::PlainText(text) => non_empty(text).map(str::to_owned),
        Value::ExecutedShellCommand(command) => format_executed_shell_command(command),
        Value::RunningShellCommand(command) => format_running_shell_command(command),
        Value::DocumentContent(document) => non_empty(&document.content).map(|content| {
            format!(
                "Document: {}\n{}",
                non_empty(&document.document_id).unwrap_or(""),
                content
            )
        }),
        Value::FilePathReference(path) => {
            non_empty(&path.file_path).map(|path| format!("File path: {path}"))
        }
        _ => None,
    }?;
    Some(match label {
        Some(label) => format!("Referenced attachment {label}:\n{body}"),
        None => format!("Referenced attachment:\n{body}"),
    })
}

fn format_file_content(content: &api::FileContent) -> Option<String> {
    let file_path = non_empty(&content.file_path)?;
    let text = &content.content;
    let range = content
        .line_range
        .as_ref()
        .map(|range| format!(":{}-{}", range.start, range.end))
        .unwrap_or_default();
    Some(format!("File: {file_path}{range}\n{text}"))
}

fn format_executed_shell_command(command: &api::ExecutedShellCommand) -> Option<String> {
    if command.command.is_empty() && command.output.is_empty() {
        return None;
    }
    Some(
        [
            Some("Executed shell command:".to_owned()),
            non_empty(&command.command).map(|command| format!("Command: {command}")),
            Some(format!("Exit code: {}", command.exit_code)),
            non_empty(&command.output).map(|output| format!("Output:\n{output}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
    )
}

fn format_running_shell_command(command: &api::RunningShellCommand) -> Option<String> {
    if command.command.is_empty() && command.snapshot.is_none() {
        return None;
    }
    Some(
        [
            Some("Running shell command:".to_owned()),
            non_empty(&command.command).map(|command| format!("Command: {command}")),
            command.snapshot.as_ref().and_then(format_shell_snapshot),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
    )
}

fn format_shell_snapshot(snapshot: &api::LongRunningShellCommandSnapshot) -> Option<String> {
    if snapshot.output.is_empty() && snapshot.cursor.is_empty() && snapshot.command_id.is_empty() {
        return None;
    }
    Some(
        [
            non_empty(&snapshot.command_id).map(|id| format!("Command ID: {id}")),
            non_empty(&snapshot.cursor).map(|cursor| format!("Cursor: {cursor}")),
            non_empty(&snapshot.output).map(|output| format!("Output:\n{output}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
    )
}

fn format_shell_finished(finished: &api::ShellCommandFinished) -> Option<String> {
    if finished.output.is_empty() && finished.command_id.is_empty() {
        return None;
    }
    Some(
        [
            non_empty(&finished.command_id).map(|id| format!("Command ID: {id}")),
            Some(format!("Exit code: {}", finished.exit_code)),
            non_empty(&finished.output).map(|output| format!("Output:\n{output}")),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join("\n"),
    )
}

fn format_mcp_resource_content(content: &api::McpResourceContent) -> Option<String> {
    match content.content_type.as_ref() {
        Some(api::mcp_resource_content::ContentType::Text(text)) => Some(
            [
                Some(format!(
                    "Resource: {}",
                    non_empty(&content.uri).unwrap_or("")
                )),
                non_empty(&text.mime_type).map(|mime| format!("MIME: {mime}")),
                non_empty(&text.content).map(str::to_owned),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n"),
        ),
        Some(api::mcp_resource_content::ContentType::Binary(binary)) => Some(format!(
            "Resource: {}\nBinary content{}",
            non_empty(&content.uri).unwrap_or(""),
            non_empty(&binary.mime_type)
                .map(|mime| format!(" ({mime})"))
                .unwrap_or_default()
        )),
        None => non_empty(&content.uri).map(|uri| format!("Resource: {uri}")),
    }
}

fn format_timestamp(timestamp: &prost_types::Timestamp) -> Option<String> {
    DateTime::<Utc>::from_timestamp(timestamp.seconds, timestamp.nanos.max(0) as u32)
        .map(|time| time.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
}

fn first_non_empty<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    values.into_iter().find_map(non_empty)
}

fn non_empty(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn random_uuid() -> String {
    Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use warp_multi_agent_api::request;

    #[test]
    fn decodes_basic_user_query() {
        let request = api::Request {
            input: Some(request::Input {
                r#type: Some(request::input::Type::UserQuery(request::input::UserQuery {
                    query: "hello".to_owned(),
                    ..Default::default()
                })),
                ..Default::default()
            }),
            metadata: Some(request::Metadata {
                conversation_id: "conversation".to_owned(),
                ..Default::default()
            }),
            ..Default::default()
        };

        let decoded = decode_warp_request(request);

        assert_eq!(decoded.conversation_id, "conversation");
        assert_eq!(decoded.prompt, "hello");
        assert!(decoded.should_create_root_task);
    }
}
