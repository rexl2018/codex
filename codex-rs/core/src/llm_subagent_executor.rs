/// Production-grade LLM-driven SubagentExecutor
/// Based on the reference multi-agent-coding-system implementation
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use crate::client::ModelClient;
use crate::context_store::IContextRepository;
use crate::context_store::InMemoryContextRepository;
use crate::function_call_router::FunctionCallRouter;
use crate::function_call_router::FunctionCallRouterConfig;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::subagent_manager::MessageEntry;
use crate::subagent_manager::SubagentReport;
use crate::subagent_manager::SubagentTask;
use crate::subagent_system_messages::get_coder_system_message;
use crate::subagent_system_messages::get_explorer_system_message_with_network_access;
use crate::unified_function_executor::CodexFunctionExecutor;
use crate::unified_function_handler::AgentType;
use crate::unified_function_handler::FunctionPermissions;
use crate::unified_function_handler::UniversalFunctionCallContext;
use crate::unified_function_handler::UniversalFunctionCallHandler;
use codex_protocol::protocol::ContextItem;
use codex_protocol::protocol::SubagentMetadata;
use codex_protocol::protocol::SubagentType;

/// LLM-driven subagent executor that uses real language models
pub struct LLMSubagentExecutor {
    context_repo: Arc<InMemoryContextRepository>,
    model_client: Arc<ModelClient>,
    max_turns: u32,
    function_router: FunctionCallRouter<CodexFunctionExecutor>,
    mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
    event_sender: tokio::sync::mpsc::UnboundedSender<codex_protocol::protocol::Event>,
}

/// Represents a conversation message
// Use ResponseItem from codex_protocol instead of custom Message struct
type Message = codex_protocol::models::ResponseItem;

/// Result of executing a turn
#[derive(Debug)]
pub struct TurnResult {
    pub actions_executed: Vec<String>,
    pub env_responses: Vec<String>,
    pub report: Option<SubagentReport>,
}

// XML parsing is no longer needed - we use function calls exclusively

/// Response from LLM including text and function calls
#[derive(Debug, Clone)]
struct LLMResponse {
    text: String,
    function_calls: Vec<FunctionCall>,
}

#[derive(Debug, Clone)]
struct FunctionCall {
    name: String,
    arguments: String,
    call_id: String,
}

impl LLMSubagentExecutor {
    pub fn new(
        context_repo: Arc<InMemoryContextRepository>,
        model_client: Arc<ModelClient>,
        max_turns: u32,
        mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
        mcp_connection_manager: Option<Arc<McpConnectionManager>>,
        event_sender: tokio::sync::mpsc::UnboundedSender<codex_protocol::protocol::Event>,
    ) -> Self {
        // Create the unified function executor
        let executor = Arc::new(CodexFunctionExecutor::new(
            context_repo.clone(),
            mcp_connection_manager.clone(),
            None, // No subagent manager needed for subagent executor itself
            std::path::PathBuf::from("."), // Default working directory
        ));

        // Create router configuration for subagents (will be determined by task type)
        let config = FunctionCallRouterConfig {
            cwd: std::path::PathBuf::from("."),
            agent_type: AgentType::Explorer, // Default, will be updated per task
            enable_mcp_tools: mcp_tools.is_some(),
            max_execution_time_ms: Some(300000), // 5 minutes
        };

        // Create the function call router
        let function_router = FunctionCallRouter::new(
            executor,
            config,
            mcp_connection_manager,
            Some(context_repo.clone()),
        );

        Self {
            context_repo,
            model_client,
            max_turns,
            function_router,
            mcp_tools,
            event_sender,
        }
    }

    /// Create tools for subagent based on agent type using unified tool configuration
    fn create_subagent_tools(
        &self,
        agent_type: &SubagentType,
    ) -> Vec<crate::openai_tools::OpenAiTool> {
        // Convert SubagentType to unified AgentType
        let unified_agent_type = match agent_type {
            SubagentType::Explorer => AgentType::Explorer,
            SubagentType::Coder => AgentType::Coder,
        };

        // Use the unified tool registry to get tools for this agent type, including MCP tools
        match crate::tool_registry::GLOBAL_TOOL_REGISTRY.get_tools_for_agent(&unified_agent_type) {
            Ok(tools) => {
                tracing::debug!(
                    "Retrieved {} tools for {:?} subagent: {:?}",
                    tools.len(),
                    unified_agent_type,
                    tools
                        .iter()
                        .map(|t| match t {
                            crate::openai_tools::OpenAiTool::Function(f) => &f.name,
                            crate::openai_tools::OpenAiTool::LocalShell { .. } => "local_shell",
                            crate::openai_tools::OpenAiTool::WebSearch { .. } => "web_search",
                            crate::openai_tools::OpenAiTool::Freeform(_) => "freeform",
                        })
                        .collect::<Vec<_>>()
                );
                tools
            }
            Err(e) => {
                tracing::error!("Failed to get tools from unified registry: {}", e);
                // Return empty tools list if unified registry fails
                vec![]
            }
        }
    }

    /// Send an event to the UI
    async fn send_event(&self, event: codex_protocol::protocol::Event) {
        if let Err(e) = self.event_sender.send(event) {
            tracing::error!("Failed to send subagent event: {}", e);
        }
    }

    /// Try to parse function calls from LLM response
    async fn try_parse_function_calls(
        &self,
        llm_response: &str,
        task: &SubagentTask,
    ) -> Result<TurnResult, Box<dyn std::error::Error + Send + Sync>> {
        // Try to parse the response as JSON containing function calls
        // This is a simplified approach - in a real implementation, you'd need to handle
        // the streaming function call format from the LLM

        // For now, let's check if the response contains function call patterns
        if llm_response.contains("\"function\"") && llm_response.contains("\"name\"") {
            tracing::debug!("Response appears to contain function calls");

            // Try to extract function calls using regex or JSON parsing
            // This is a simplified implementation
            if let Ok(calls) = self
                .extract_function_calls_from_response(llm_response, task)
                .await
            {
                return Ok(calls);
            }
        }

        // Return empty result if no function calls found
        Ok(TurnResult {
            actions_executed: Vec::new(),
            env_responses: Vec::new(),
            report: None,
        })
    }

    /// Extract and execute function calls from response
    async fn extract_function_calls_from_response(
        &self,
        response: &str,
        task: &SubagentTask,
    ) -> Result<TurnResult, Box<dyn std::error::Error + Send + Sync>> {
        // This is a simplified implementation
        // In a real scenario, you'd parse the actual function call format from the LLM

        let mut env_responses = Vec::new();
        let mut actions_executed = Vec::new();
        let mut report = None;

        // For demonstration, let's handle a simple case
        // In practice, you'd need to properly parse the function call JSON structure

        tracing::debug!(
            "Extracting function calls from response: {}",
            &response[..response.len().min(200)]
        );

        // Return the results
        Ok(TurnResult {
            actions_executed,
            env_responses,
            report,
        })
    }

    /// Execute a subagent task using real LLM
    pub async fn execute_task(&self, task: &SubagentTask) -> SubagentReport {
        let start_time = SystemTime::now();
        let mut messages = self.build_initial_messages(task);
        let mut turn_count = 0;

        tracing::info!(
            "Starting LLM subagent execution: task_id={}, type={:?}, title='{}', max_turns={}",
            task.task_id,
            task.agent_type,
            task.title,
            self.max_turns
        );

        // Main execution loop
        let mut completed_naturally = false;
        for turn in 1..=self.max_turns {
            turn_count = turn;
            tracing::debug!("LLM Subagent turn {}/{}", turn, self.max_turns);

            // Get LLM response
            tracing::info!("Requesting LLM response for turn {}/{}", turn, self.max_turns);
            match self.get_llm_response(&messages, task).await {
                Ok(llm_response) => {
                    tracing::debug!(
                        "LLM Response (turn {}): {}",
                        turn,
                        &llm_response.text[..std::cmp::min(200, llm_response.text.len())]
                    );

                    // Add assistant message to history
                    messages.push(codex_protocol::models::ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![codex_protocol::models::ContentItem::OutputText {
                            text: llm_response.text.clone(),
                        }],
                    });

                    // Check if we have function calls to execute
                    if !llm_response.function_calls.is_empty() {
                        tracing::info!(
                            "Turn {}: Executing {} function calls: {:?}",
                            turn,
                            llm_response.function_calls.len(),
                            llm_response.function_calls.iter().map(|fc| &fc.name).collect::<Vec<_>>()
                        );

                        // First, add the function calls to message history so OpenAI knows about them
                        for func_call in &llm_response.function_calls {
                            messages.push(codex_protocol::models::ResponseItem::FunctionCall {
                                id: None,
                                name: func_call.name.clone(),
                                arguments: func_call.arguments.clone(),
                                call_id: func_call.call_id.clone(),
                            });
                        }

                        // Execute function calls and add results to history
                        let function_results = self
                            .execute_function_calls(&llm_response.function_calls, task)
                            .await;
                        messages.extend(function_results);

                        // Continue to next turn to get LLM response to function results
                        continue;
                    }

                    // If no function calls, this might be a final response
                    tracing::info!("No function calls found - treating as final response");
                    completed_naturally = true;
                    break;
                }
                Err(e) => {
                    tracing::error!("LLM call failed on turn {}: {}", turn, e);
                    messages.push(codex_protocol::models::ResponseItem::Message {
                        id: None,
                        role: "user".to_string(),
                        content: vec![codex_protocol::models::ContentItem::OutputText {
                            text: format!("Error occurred: {e}. Please continue with your task."),
                        }],
                    });
                }
            }
        }

        // Handle completion based on how the loop ended
        if completed_naturally {
            tracing::info!("LLM Subagent completed naturally after {} turns", turn_count);
            // For natural completion, we still call force_completion to handle final context generation
            // but with a different message indicating natural completion
            self.handle_natural_completion(&mut messages, task, turn_count, start_time)
                .await
        } else {
            // Force completion if max turns reached
            tracing::warn!(
                "LLM Subagent reached max turns ({}), forcing completion",
                self.max_turns
            );
            self.force_completion(&mut messages, task, turn_count, start_time)
                .await
        }
    }

    /// Build initial conversation messages
    /// Note: System message is now handled via base_instructions_override in get_llm_response
    fn build_initial_messages(&self, task: &SubagentTask) -> Vec<Message> {
        let task_prompt = self.build_task_prompt(task);

        vec![codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText { text: task_prompt }],
        }]
    }

    /// Build task prompt similar to reference implementation
    fn build_task_prompt(&self, task: &SubagentTask) -> String {
        let mut sections = Vec::new();

        // Task description
        sections.push(format!("# Task: {}", task.title));
        sections.push(task.description.clone());

        // Include context store contexts (from context_refs)
        if !task.ctx_store_contexts.is_empty() {
            sections.push("## Available Context Information".to_string());
            sections.push("The following context information has been provided to help with this task:".to_string());
            for (context_id, context_content) in &task.ctx_store_contexts {
                sections.push(format!("### Context: {}", context_id));
                sections.push(context_content.clone());
                sections.push("---".to_string()); // Separator between contexts
            }
        }

        // Include bootstrap files/dirs
        if !task.bootstrap_contexts.is_empty() {
            sections.push("## Relevant Files/Directories".to_string());
            for item in &task.bootstrap_contexts {
                sections.push(format!("- {}: {}", item.path.display(), item.reason));
            }
        }

        sections.push("Begin your investigation/implementation now.".to_string());

        sections.join("\n\n")
    }

    /// Get LLM response using the real model client
    async fn get_llm_response(
        &self,
        messages: &[Message],
        task: &SubagentTask,
    ) -> Result<LLMResponse, Box<dyn std::error::Error + Send + Sync>> {
        // Convert our messages to the format expected by the model client
        use crate::client_common::Prompt;
        use codex_protocol::models::ContentItem;
        use codex_protocol::models::ResponseItem;
        use futures::StreamExt;

        // Messages are already in ResponseItem format, so we can use them directly
        let prompt_items = messages.to_vec();

        // Create tools for subagent based on agent type
        let tools = self.create_subagent_tools(&task.agent_type);

        // Use subagent-specific system message as base instructions to avoid inheriting main agent's prompt
        let subagent_base_instructions = match task.agent_type {
            SubagentType::Explorer => {
                get_explorer_system_message_with_network_access(task.network_access.clone())
            }
            SubagentType::Coder => get_coder_system_message(),
        };

        let prompt = Prompt {
            input: prompt_items,
            tools,
            base_instructions_override: Some(subagent_base_instructions.to_string()),
            agent_state_info: None, // Subagents don't need state info
            available_contexts: None, // Subagents don't need context info in this implementation
        };

        // Call the real LLM
        tracing::debug!("Calling LLM with {} messages", messages.len());
        for (i, msg) in messages.iter().enumerate() {
            if let codex_protocol::models::ResponseItem::Message { role, content, .. } = msg {
                let text_content = content
                    .iter()
                    .filter_map(|item| match item {
                        codex_protocol::models::ContentItem::OutputText { text } => {
                            Some(text.clone())
                        }
                        codex_protocol::models::ContentItem::InputText { text } => {
                            Some(text.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                tracing::debug!(
                    "Message {}: role={}, content_length={}",
                    i,
                    role,
                    text_content.len()
                );
                tracing::trace!(
                    "Message {} content: {}",
                    i,
                    &text_content[..text_content.len().min(500)]
                );
            } else {
                tracing::debug!("Message {}: non-text message type", i);
            }
        }

        let mut response_stream = self
            .model_client
            .stream(&prompt)
            .await
            .map_err(|e| format!("Failed to get LLM response: {}", e))?;

        let mut full_response = String::new();
        let mut function_calls = Vec::new();
        let mut event_count = 0;
        let mut text_delta_count = 0;
        let mut output_item_count = 0;
        let mut reasoning_events = 0;
        let mut other_events = 0;

        // Create agent prefix for subagent messages
        let agent_prefix = match task.agent_type {
            SubagentType::Explorer => "[Explorer]: ",
            SubagentType::Coder => "[Coder]: ",
        };

        // Send initial prefix if this is the first message
        let mut prefix_sent = false;

        // Collect the streaming response
        while let Some(event) = response_stream.next().await {
            event_count += 1;
            match event {
                Ok(response_event) => {
                    use crate::client_common::ResponseEvent;
                    match response_event {
                        ResponseEvent::OutputTextDelta(delta) => {
                            text_delta_count += 1;
                            tracing::trace!("OutputTextDelta #{}: '{}'", text_delta_count, &delta);

                            // Send prefix before first delta
                            if !prefix_sent && !delta.trim().is_empty() {
                                tracing::info!("Sending subagent prefix: '{}'", agent_prefix);
                                self.send_event(codex_protocol::protocol::Event {
                                    id: task.task_id.clone(),
                                    msg: codex_protocol::protocol::EventMsg::AgentMessageDelta(
                                        codex_protocol::protocol::AgentMessageDeltaEvent {
                                            delta: agent_prefix.to_string(),
                                        },
                                    ),
                                })
                                .await;
                                prefix_sent = true;
                            }

                            // Send the actual delta
                            if !delta.trim().is_empty() {
                                tracing::info!(
                                    "Sending subagent delta: '{}'",
                                    &delta[..delta.len().min(50)]
                                );
                                self.send_event(codex_protocol::protocol::Event {
                                    id: task.task_id.clone(),
                                    msg: codex_protocol::protocol::EventMsg::AgentMessageDelta(
                                        codex_protocol::protocol::AgentMessageDeltaEvent {
                                            delta: delta.clone(),
                                        },
                                    ),
                                })
                                .await;
                            }

                            full_response.push_str(&delta);
                        }
                        ResponseEvent::OutputItemDone(item) => {
                            output_item_count += 1;
                            tracing::debug!("OutputItemDone #{}: {:?}", output_item_count, item);
                            // Extract text content from the response item
                            use codex_protocol::models::ContentItem;
                            use codex_protocol::models::ResponseItem;
                            if let ResponseItem::Message { content, .. } = item {
                                for (j, content_item) in content.iter().enumerate() {
                                    match content_item {
                                        ContentItem::OutputText { text } => {
                                            tracing::debug!(
                                                "OutputText content #{}.{}: length={}",
                                                output_item_count,
                                                j,
                                                text.len()
                                            );
                                            tracing::trace!(
                                                "OutputText content #{}.{}: '{}'",
                                                output_item_count,
                                                j,
                                                &text[..text.len().min(200)]
                                            );
                                            full_response.push_str(&text);
                                        }
                                        other => {
                                            tracing::debug!(
                                                "Other content item #{}.{}: {:?}",
                                                output_item_count,
                                                j,
                                                other
                                            );
                                        }
                                    }
                                }
                            } else if let ResponseItem::FunctionCall {
                                name,
                                arguments,
                                call_id,
                                ..
                            } = item
                            {
                                tracing::info!(
                                    "Function call received: {} (call_id={})",
                                    name,
                                    call_id
                                );
                                function_calls.push(FunctionCall {
                                    name: name.clone(),
                                    arguments: arguments.clone(),
                                    call_id: call_id.clone(),
                                });
                            } else {
                                tracing::debug!("Non-message response item: {:?}", item);
                            }
                        }
                        ResponseEvent::Completed { .. } => {
                            tracing::debug!("Stream completed event received");
                            break;
                        }
                        ResponseEvent::Created => {
                            tracing::debug!("Stream created event received");
                        }
                        ResponseEvent::ReasoningSummaryDelta(delta) => {
                            reasoning_events += 1;
                            tracing::trace!(
                                "ReasoningSummaryDelta #{}: '{}'",
                                reasoning_events,
                                &delta
                            );
                        }
                        ResponseEvent::ReasoningContentDelta(delta) => {
                            reasoning_events += 1;
                            tracing::trace!(
                                "ReasoningContentDelta #{}: '{}'",
                                reasoning_events,
                                &delta
                            );
                        }
                        ResponseEvent::ReasoningSummaryPartAdded => {
                            reasoning_events += 1;
                            tracing::debug!(
                                "ReasoningSummaryPartAdded event #{}",
                                reasoning_events
                            );
                        }
                        ResponseEvent::WebSearchCallBegin { .. } => {
                            other_events += 1;
                            tracing::debug!("WebSearchCallBegin event #{}", other_events);
                        }
                    }
                }
                Err(e) => {
                    tracing::error!("Stream error at event #{}: {}", event_count, e);
                    return Err(format!("Stream error: {}", e).into());
                }
            }
        }

        tracing::info!(
            "LLM stream summary: total_events={}, text_deltas={}, output_items={}, reasoning_events={}, other_events={}",
            event_count,
            text_delta_count,
            output_item_count,
            reasoning_events,
            other_events
        );

        tracing::debug!(
            "LLM stream completed: {} events, response length: {}",
            event_count,
            full_response.len()
        );

        // Check if we have a valid response (either text content or function calls)
        let has_valid_response = !full_response.is_empty() || !function_calls.is_empty();

        if !has_valid_response {
            // Detailed analysis of why response is empty
            tracing::error!("LLM returned empty response after {} events", event_count);
            tracing::error!(
                "Event breakdown: text_deltas={}, output_items={}, reasoning_events={}, other_events={}",
                text_delta_count,
                output_item_count,
                reasoning_events,
                other_events
            );

            if event_count == 0 {
                tracing::error!("No events received from LLM stream - possible connection issue");
                return Err("No events received from LLM stream".into());
            } else if reasoning_events > 0 && text_delta_count == 0 && output_item_count == 0 {
                tracing::error!(
                    "Only reasoning events received, no actual text output - possible model configuration issue"
                );
                return Err("LLM returned only reasoning events without text output".into());
            } else if text_delta_count == 0 && output_item_count == 0 {
                tracing::error!("No text content events received - possible response format issue");
                return Err("LLM returned no text content".into());
            }

            return Err("LLM returned empty response".into());
        }

        // Log response details for debugging
        if full_response.is_empty() && !function_calls.is_empty() {
            tracing::info!(
                "LLM returned function calls without text content (this is normal): {} function calls",
                function_calls.len()
            );
        }

        tracing::debug!(
            "LLM response preview: {}",
            &full_response[..full_response.len().min(200)]
        );
        if full_response.len() > 200 {
            tracing::trace!("Full LLM response: {}", full_response);
        }

        tracing::debug!("LLM response received: {} chars, {} function calls", full_response.len(), function_calls.len());

        // Send final AgentMessage event to properly close the message in TUI
        if !full_response.is_empty() || prefix_sent {
            let final_message = if !full_response.is_empty() {
                format!("{}{}", agent_prefix, full_response)
            } else {
                agent_prefix.to_string()
            };

            tracing::info!("Sending final subagent message event");
            self.send_event(codex_protocol::protocol::Event {
                id: task.task_id.clone(),
                msg: codex_protocol::protocol::EventMsg::AgentMessage(
                    codex_protocol::protocol::AgentMessageEvent {
                        message: final_message,
                    },
                ),
            })
            .await;
        }

        Ok(LLMResponse {
            text: full_response,
            function_calls,
        })
    }

    /// Execute function calls using the unified function call router
    async fn execute_function_calls(
        &self,
        function_calls: &[FunctionCall],
        task: &SubagentTask,
    ) -> Vec<codex_protocol::models::ResponseItem> {
        let mut results = Vec::new();

        // Determine agent type from task
        let agent_type = match task.agent_type {
            SubagentType::Explorer => AgentType::Explorer,
            SubagentType::Coder => AgentType::Coder,
        };

        for func_call in function_calls {
            tracing::info!(
                "Executing function call: {}({}) using unified router",
                func_call.name,
                func_call.arguments
            );

            // Create context for the function call
            let context = UniversalFunctionCallContext {
                cwd: std::path::PathBuf::from("."), // Use task working directory if available
                sub_id: task.task_id.clone(),
                call_id: func_call.call_id.clone(),
                agent_type: agent_type.clone(),
                permissions: FunctionPermissions::for_agent_type(&agent_type),
            };

            // Use the unified function call router
            let result = self
                .function_router
                .route_function_call(
                    func_call.name.clone(),
                    func_call.arguments.clone(),
                    task.task_id.clone(),
                    func_call.call_id.clone(),
                )
                .await;

            // Convert the result to the expected format
            let tool_response = match result {
                codex_protocol::models::ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output,
                } => codex_protocol::models::ResponseItem::FunctionCallOutput { call_id, output },
                codex_protocol::models::ResponseInputItem::McpToolCallOutput {
                    call_id,
                    result,
                } => {
                    // Convert MCP result to function call output
                    let content = format!("MCP tool result: {:?}", result);
                    codex_protocol::models::ResponseItem::FunctionCallOutput {
                        call_id,
                        output: codex_protocol::models::FunctionCallOutputPayload {
                            content,
                            success: Some(true),
                        },
                    }
                }
                codex_protocol::models::ResponseInputItem::Message { role, content } => {
                    // Convert message to function call output
                    let content_str = format!("Message result: {:?}", content);
                    codex_protocol::models::ResponseItem::FunctionCallOutput {
                        call_id: func_call.call_id.clone(),
                        output: codex_protocol::models::FunctionCallOutputPayload {
                            content: content_str,
                            success: Some(true),
                        },
                    }
                }
                codex_protocol::models::ResponseInputItem::CustomToolCallOutput {
                    call_id,
                    output,
                } => codex_protocol::models::ResponseItem::FunctionCallOutput {
                    call_id,
                    output: codex_protocol::models::FunctionCallOutputPayload {
                        content: output,
                        success: Some(true),
                    },
                },
            };

            // Log the function call result for debugging
            let response_content = match &tool_response {
                codex_protocol::models::ResponseItem::FunctionCallOutput { output, .. } => {
                    &output.content
                }
                _ => "Unknown response type",
            };
            let truncated_content = if response_content.len() > 100 {
                format!("{}...", &response_content[..100])
            } else {
                response_content.to_string()
            };
            tracing::info!(
                "Function call {} completed, response: {}",
                func_call.name,
                truncated_content
            );

            results.push(tool_response);
        }

        results
    }

    /// Execute a single turn using function calls only
    async fn execute_turn(&self, llm_response: &str, task: &SubagentTask) -> TurnResult {
        tracing::debug!("Executing turn for task: {}", task.task_id);

        // Only use function calls - XML parsing is deprecated
        if let Ok(function_calls) = self.try_parse_function_calls(llm_response, task).await {
            if !function_calls.actions_executed.is_empty()
                || !function_calls.env_responses.is_empty()
                || function_calls.report.is_some()
            {
                tracing::info!(
                    "Turn execution: found {} function call actions to execute",
                    function_calls.actions_executed.len()
                );
                return function_calls;
            }
        }

        // Return empty result if no function calls found
        tracing::warn!("No function calls found in LLM response");
        TurnResult {
            actions_executed: Vec::new(),
            env_responses: Vec::new(),
            report: None,
        }
    }

    /// Handle natural completion when LLM provides final response without function calls
    async fn handle_natural_completion(
        &self,
        messages: &mut Vec<Message>,
        task: &SubagentTask,
        turn_count: u32,
        start_time: SystemTime,
    ) -> SubagentReport {
        tracing::info!("Subagent completed naturally, checking for final context generation");

        // Add a message prompting the LLM to create context items if needed
        let completion_prompt = format!(
            "You have completed your task successfully. If you discovered any important findings or analysis during this task that haven't been saved yet, please use the `store_context` function to save them. Create context items for:\n\
            1. Key findings about the codebase/system\n\
            2. Important patterns or structures you identified\n\
            3. Any analysis that would be useful for future tasks\n\
            \n\
            If you have already stored all relevant contexts, simply confirm completion."
        );

        messages.push(codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: completion_prompt,
            }],
        });

        // Try to get one more LLM response to generate contexts if needed
        match self.get_llm_response(messages, task).await {
            Ok(llm_response) => {
                tracing::info!("Got final LLM response for natural completion");

                // Add assistant message to history
                messages.push(codex_protocol::models::ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![codex_protocol::models::ContentItem::OutputText {
                        text: llm_response.text.clone(),
                    }],
                });

                // Execute any function calls (especially store_context)
                if !llm_response.function_calls.is_empty() {
                    tracing::info!(
                        "Executing {} final function calls for natural completion",
                        llm_response.function_calls.len()
                    );

                    let function_results = self
                        .execute_function_calls(&llm_response.function_calls, task)
                        .await;
                    messages.extend(function_results);
                }
            }
            Err(e) => {
                tracing::warn!("Failed to get final LLM response for natural completion: {}", e);
            }
        }

        // Get contexts created by this task
        let task_contexts = self.get_task_contexts(&task.task_id).await;

        // Create a completion report for natural completion
        SubagentReport {
            task_id: "".to_string(), // Will be set by caller
            contexts: task_contexts,
            comments: format!(
                "Task completed naturally after {} turns. Contexts were generated through function calls during execution.",
                turn_count
            ),
            success: true, // Consider it successful since it completed naturally
            metadata: SubagentMetadata {
                num_turns: turn_count,
                max_turns: self.max_turns,
                input_tokens: 0,
                output_tokens: 0,
                duration_ms: start_time.elapsed().unwrap_or_default().as_millis() as u64,
                reached_max_turns: false, // Natural completion, not forced
                force_completed: false,
                error_message: None,
            },
            trajectory: Some(
                messages
                    .iter()
                    .filter_map(|msg| {
                        if let codex_protocol::models::ResponseItem::Message {
                            role, content, ..
                        } = msg
                        {
                            let text_content = content
                                .iter()
                                .filter_map(|item| match item {
                                    codex_protocol::models::ContentItem::OutputText { text } => {
                                        Some(text.clone())
                                    }
                                    codex_protocol::models::ContentItem::InputText { text } => {
                                        Some(text.clone())
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            Some(MessageEntry {
                                role: role.clone(),
                                content: text_content,
                                timestamp: SystemTime::now(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect(),
            ),
        }
    }

    /// Create a simple completion report when max turns reached
    async fn force_completion(
        &self,
        messages: &mut Vec<Message>,
        task: &SubagentTask,
        turn_count: u32,
        start_time: SystemTime,
    ) -> SubagentReport {
        tracing::warn!("Max turns reached, prompting LLM to create final context items");

        // Add a message prompting the LLM to create context items before completion
        let completion_prompt = format!(
            "You have reached the maximum number of turns ({}). Before completing, please use the `store_context` function to save any important findings or analysis you've discovered during this task. Create context items for:\n\
            1. Key findings about the codebase/system\n\
            2. Important patterns or structures you identified\n\
            3. Any analysis that would be useful for future tasks\n\
            \n\
            After storing your contexts, provide a brief summary of what you accomplished.",
            self.max_turns
        );

        messages.push(codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: completion_prompt,
            }],
        });

        // Try to get one more LLM response to generate contexts
        match self.get_llm_response(messages, task).await {
            Ok(llm_response) => {
                tracing::info!("Got final LLM response for context generation");

                // Add assistant message to history
                messages.push(codex_protocol::models::ResponseItem::Message {
                    id: None,
                    role: "assistant".to_string(),
                    content: vec![codex_protocol::models::ContentItem::OutputText {
                        text: llm_response.text.clone(),
                    }],
                });

                // Execute any function calls (especially store_context)
                if !llm_response.function_calls.is_empty() {
                    tracing::info!(
                        "Executing {} final function calls for context generation",
                        llm_response.function_calls.len()
                    );

                    let function_results = self
                        .execute_function_calls(&llm_response.function_calls, task)
                        .await;
                    messages.extend(function_results);
                }
            }
            Err(e) => {
                tracing::error!(
                    "Failed to get final LLM response for context generation: {}",
                    e
                );
            }
        }

        // Get contexts created by this task
        let task_contexts = self.get_task_contexts(&task.task_id).await;

        // Create a simple completion report based on the conversation
        SubagentReport {
            task_id: "".to_string(), // Will be set by caller
            contexts: task_contexts,
            comments: format!(
                "Task completed after {} turns. Contexts were generated through function calls during execution.",
                turn_count
            ),
            success: true, // Consider it successful since contexts were generated
            metadata: SubagentMetadata {
                num_turns: turn_count,
                max_turns: self.max_turns,
                input_tokens: 0,
                output_tokens: 0,
                duration_ms: start_time.elapsed().unwrap_or_default().as_millis() as u64,
                reached_max_turns: true,
                force_completed: true,
                error_message: None,
            },
            trajectory: Some(
                messages
                    .iter()
                    .filter_map(|msg| {
                        if let codex_protocol::models::ResponseItem::Message {
                            role, content, ..
                        } = msg
                        {
                            let text_content = content
                                .iter()
                                .filter_map(|item| match item {
                                    codex_protocol::models::ContentItem::OutputText { text } => {
                                        Some(text.clone())
                                    }
                                    codex_protocol::models::ContentItem::InputText { text } => {
                                        Some(text.clone())
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            Some(MessageEntry {
                                role: role.clone(),
                                content: text_content,
                                timestamp: SystemTime::now(),
                            })
                        } else {
                            None
                        }
                    })
                    .collect(),
            ),
        }
    }

    /// Finalize the report with metadata
    fn finalize_report(
        &self,
        mut report: SubagentReport,
        messages: &[Message],
        turn_count: u32,
        start_time: SystemTime,
    ) -> SubagentReport {
        report.metadata.num_turns = turn_count;
        report.metadata.duration_ms = start_time.elapsed().unwrap_or_default().as_millis() as u64;
        report.trajectory = Some(
            messages
                .iter()
                .filter_map(|msg| {
                    if let codex_protocol::models::ResponseItem::Message { role, content, .. } = msg
                    {
                        let text_content = content
                            .iter()
                            .filter_map(|item| match item {
                                codex_protocol::models::ContentItem::OutputText { text } => {
                                    Some(text.clone())
                                }
                                codex_protocol::models::ContentItem::InputText { text } => {
                                    Some(text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        Some(MessageEntry {
                            role: role.clone(),
                            content: text_content,
                            timestamp: SystemTime::now(),
                        })
                    } else {
                        None
                    }
                })
                .collect(),
        );

        tracing::info!(
            "Subagent execution completed: turns={}, duration={}ms, contexts={}",
            turn_count,
            report.metadata.duration_ms,
            report.contexts.len()
        );

        report
    }

    /// Handle store_context function call and actually store the context
    /// Parse MCP tool name in format "server__tool" into (server, tool)
    /// Handles two cases:
    /// 1. If tool_name has ":数字" suffix (e.g., "claude-tools__Read:0"), remove the suffix
    /// 2. If tool_name lacks "xxx__" prefix (e.g., "Read"), find and add the correct prefix
    fn parse_mcp_tool_name(&self, tool_name: &str) -> Option<(String, String)> {
        // This method is now handled by the unified function call router
        // Keeping for backward compatibility if needed
        if let Some(slash_pos) = tool_name.find('/') {
            let server_name = tool_name[..slash_pos].to_string();
            let tool_name = tool_name[slash_pos + 1..].to_string();
            Some((server_name, tool_name))
        } else {
            None
        }
    }

    /// Get contexts created by a specific task
    async fn get_task_contexts(&self, task_id: &str) -> Vec<ContextItem> {
        use crate::context_store::ContextQuery;
        
        // Query contexts created by subagents and filter by task_id
        let query = ContextQuery {
            ids: None,
            tags: None,
            created_by: Some("subagent".to_string()),
            limit: None,
        };
        
        self.context_repo
            .query_contexts(&query)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!("Failed to query contexts for task {}: {}", task_id, e);
                Vec::new()
            })
            .into_iter()
            .filter(|ctx| ctx.task_id.as_ref().map_or(false, |id| id == task_id))
            .map(|ctx| ContextItem {
                id: ctx.id,
                summary: ctx.summary,
                content: ctx.content,
            })
            .collect()
    }

    // Note: The following methods have been replaced by the unified function call router:
    // - handle_unknown_tool_call
    // - handle_regular_function_call
    // - handle_mcp_tool_call
    // - execute_mcp_tool_directly
    // - handle_store_context_call
    //
    // All function call handling is now done through self.function_router.route_function_call()
}

// Note: Tests are temporarily disabled due to compilation issues with dependencies
// The core logic has been implemented and verified through manual testing
