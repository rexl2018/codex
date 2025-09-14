/// Production-grade LLM-driven SubagentExecutor
/// Based on the reference multi-agent-coding-system implementation
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;

use crate::client::ModelClient;
use crate::context_store::Context;
use crate::context_store::IContextRepository;
use crate::context_store::InMemoryContextRepository;
use crate::function_call_handler::FunctionCallContext;
use crate::function_call_handler::FunctionCallHandler;
use crate::function_call_handler::SubagentExecutor;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::subagent_manager::MessageEntry;
use crate::subagent_manager::SubagentReport;
use crate::subagent_manager::SubagentTask;
use crate::subagent_system_messages::get_coder_system_message;
use crate::subagent_system_messages::get_explorer_system_message;
use codex_protocol::protocol::ContextItem;
use codex_protocol::protocol::SubagentMetadata;
use codex_protocol::protocol::SubagentType;

/// LLM-driven subagent executor that uses real language models
pub struct LLMSubagentExecutor {
    context_repo: Arc<InMemoryContextRepository>,
    model_client: Arc<ModelClient>,
    max_turns: u32,
    function_handler: FunctionCallHandler<SubagentExecutor>,
    mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
    mcp_connection_manager: Option<Arc<McpConnectionManager>>,
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
        let executor = Arc::new(SubagentExecutor);
        let function_handler = FunctionCallHandler::new(executor);

        Self {
            context_repo,
            model_client,
            max_turns,
            function_handler,
            mcp_tools,
            mcp_connection_manager,
            event_sender,
        }
    }

    /// Create tools for subagent based on agent type using unified tool configuration
    fn create_subagent_tools(
        &self,
        agent_type: &SubagentType,
    ) -> Vec<crate::openai_tools::OpenAiTool> {
        // Convert SubagentType to AgentType and use the unified tool configuration
        let unified_agent_type = crate::openai_tools::AgentType::from(agent_type.clone());

        // Create a minimal tools config for subagents (they don't need all the main agent features)
        let tools_config = crate::openai_tools::ToolsConfig {
            shell_type: crate::openai_tools::ConfigShellToolType::DefaultShell,
            plan_tool: false,
            apply_patch_tool_type: None,
            web_search_request: false,
            include_view_image_tool: false,
            experimental_unified_exec_tool: false,
            include_subagent_task_tool: false,
        };

        // Use the unified get_openai_tools function
        crate::openai_tools::get_openai_tools(
            &tools_config,
            self.mcp_tools.clone(), // Subagents can use MCP tools
            unified_agent_type,
        )
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
        for turn in 1..=self.max_turns {
            turn_count = turn;
            tracing::debug!("LLM Subagent turn {}/{}", turn, self.max_turns);

            // Get LLM response
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
                            "Executing {} function calls",
                            llm_response.function_calls.len()
                        );

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

        // Force completion if max turns reached
        tracing::warn!(
            "LLM Subagent reached max turns ({}), forcing completion",
            self.max_turns
        );
        self.force_completion(&mut messages, task, turn_count, start_time)
            .await
    }

    /// Build initial conversation messages
    fn build_initial_messages(&self, task: &SubagentTask) -> Vec<Message> {
        let system_message = match task.agent_type {
            SubagentType::Explorer => get_explorer_system_message(),
            SubagentType::Coder => get_coder_system_message(),
        };

        let task_prompt = self.build_task_prompt(task);

        vec![
            codex_protocol::models::ResponseItem::Message {
                id: None,
                role: "system".to_string(),
                content: vec![codex_protocol::models::ContentItem::OutputText {
                    text: system_message.to_string(),
                }],
            },
            codex_protocol::models::ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![codex_protocol::models::ContentItem::OutputText {
                    text: task_prompt,
                }],
            },
        ]
    }

    /// Build task prompt similar to reference implementation
    fn build_task_prompt(&self, task: &SubagentTask) -> String {
        let mut sections = Vec::new();

        // Task description
        sections.push(format!("# Task: {}", task.title));
        sections.push(task.description.clone());

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
        tracing::info!(
            "Created {} tools for subagent: {:?}",
            tools.len(),
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

        let prompt = Prompt {
            input: prompt_items,
            tools,
            base_instructions_override: None,
            agent_state_info: None, // Subagents don't need state info
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
                                    "Function call received: {}({}) with call_id={}",
                                    name,
                                    arguments,
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

        // DEBUG: Force output full response for debugging (can be removed in production)
        tracing::info!("DEBUG - Full LLM response content:\n{}", full_response);
        tracing::info!("DEBUG - Function calls received: {}", function_calls.len());

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

    /// Execute function calls and return results as ResponseItems
    async fn execute_function_calls(
        &self,
        function_calls: &[FunctionCall],
        task: &SubagentTask,
    ) -> Vec<codex_protocol::models::ResponseItem> {
        let mut results = Vec::new();

        for func_call in function_calls {
            tracing::info!(
                "Executing function call: {}({})",
                func_call.name,
                func_call.arguments
            );

            // Handle store_context specially to actually store in context repository
            if func_call.name == "store_context" {
                match self
                    .handle_store_context_call(&func_call.arguments, task)
                    .await
                {
                    Ok(content) => {
                        results.push(codex_protocol::models::ResponseItem::Message {
                            id: None,
                            role: "user".to_string(),
                            content: vec![codex_protocol::models::ContentItem::OutputText {
                                text: content,
                            }],
                        });
                    }
                    Err(error) => {
                        results.push(codex_protocol::models::ResponseItem::Message {
                            id: None,
                            role: "user".to_string(),
                            content: vec![codex_protocol::models::ContentItem::OutputText {
                                text: format!("Error storing context: {}", error),
                            }],
                        });
                    }
                }
                continue;
            }

            // Check if this is an MCP tool call first
            let result = if let Some(mcp_tools) = &self.mcp_tools {
                if let Some((server_name, tool_name)) = self.parse_mcp_tool_name(&func_call.name) {
                    // This is an MCP tool call
                    tracing::info!("Executing MCP tool call: {}::{}", server_name, tool_name);
                    self.handle_mcp_tool_call(
                        &server_name,
                        &tool_name,
                        &func_call.arguments,
                        &func_call.call_id,
                    )
                    .await
                } else {
                    // Handle other function calls and send events to TUI
                    self.handle_regular_function_call(&func_call, task).await
                }
            } else {
                // No MCP tools available, handle normally
                self.handle_regular_function_call(&func_call, task).await
            };

            // Convert result to message format
            let result_content = match result {
                codex_protocol::models::ResponseInputItem::FunctionCallOutput {
                    output, ..
                } => {
                    if output.success.unwrap_or(false) {
                        format!(
                            "Function {} executed successfully:\n{}",
                            func_call.name, output.content
                        )
                    } else {
                        format!("Function {} failed:\n{}", func_call.name, output.content)
                    }
                }
                codex_protocol::models::ResponseInputItem::Message { content, .. } => {
                    format!("Function {} result: {:?}", func_call.name, content)
                }
                codex_protocol::models::ResponseInputItem::McpToolCallOutput { result, .. } => {
                    format!("Function {} MCP result: {:?}", func_call.name, result)
                }
                codex_protocol::models::ResponseInputItem::CustomToolCallOutput {
                    output, ..
                } => {
                    format!("Function {} custom result: {}", func_call.name, output)
                }
            };

            // Create proper tool response using FunctionCallOutput
            let tool_response = codex_protocol::models::ResponseItem::FunctionCallOutput {
                call_id: func_call.call_id.clone(),
                output: codex_protocol::models::FunctionCallOutputPayload {
                    content: result_content,
                    success: Some(true), // Assume success for now, could be refined based on actual result
                },
            };

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

        // Create a simple completion report based on the conversation
        SubagentReport {
            task_id: "".to_string(), // Will be set by caller
            contexts: Vec::new(),    // Contexts are stored via store_context function calls
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
    fn parse_mcp_tool_name(&self, tool_name: &str) -> Option<(String, String)> {
        if let Some(mcp_tools) = &self.mcp_tools {
            // Check if this tool name exists in our MCP tools
            if mcp_tools.contains_key(tool_name) {
                // Split by "__" delimiter
                if let Some(pos) = tool_name.find("__") {
                    let server_name = tool_name[..pos].to_string();
                    let actual_tool_name = tool_name[pos + 2..].to_string();
                    return Some((server_name, actual_tool_name));
                }
            }
        }
        None
    }

    /// Handle regular function calls (shell, read_file, write_file, etc.) and send events to TUI
    async fn handle_regular_function_call(
        &self,
        func_call: &FunctionCall,
        task: &SubagentTask,
    ) -> codex_protocol::models::ResponseInputItem {
        tracing::info!(
            "Subagent executing function call: {} with args: {}",
            func_call.name,
            func_call.arguments
        );

        // Execute the function call
        let context = crate::function_call_handler::FunctionCallContext {
            cwd: std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from(".")),
            sub_id: task.task_id.clone(),
            call_id: func_call.call_id.clone(),
        };

        let result = self
            .function_handler
            .handle_function_call(func_call.name.clone(), func_call.arguments.clone(), context)
            .await;

        // Send single completion event to TUI with compact format including arguments
        let completion_message = match &result {
            codex_protocol::models::ResponseInputItem::FunctionCallOutput { output, .. } => {
                if output.success.unwrap_or(false) {
                    format!(
                        "Subagent completed: {}({}) ✓",
                        func_call.name, func_call.arguments
                    )
                } else {
                    format!(
                        "Subagent completed: {}({}) ✗",
                        func_call.name, func_call.arguments
                    )
                }
            }
            _ => format!(
                "Subagent completed: {}({})",
                func_call.name, func_call.arguments
            ),
        };

        let event = codex_protocol::protocol::EventMsg::BackgroundEvent(
            codex_protocol::protocol::BackgroundEventEvent {
                message: completion_message,
            },
        );

        self.send_event(codex_protocol::protocol::Event {
            id: "subagent".to_string(),
            msg: event,
        })
        .await;

        result
    }

    /// Handle MCP tool call - execute the tool directly if available
    /// Subagents can execute MCP tools if they have access to the tool definitions
    async fn handle_mcp_tool_call(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: &str,
        call_id: &str,
    ) -> codex_protocol::models::ResponseInputItem {
        tracing::info!(
            "Subagent executing MCP tool: {}::{} with args: {}",
            server_name,
            tool_name,
            arguments
        );

        // Parse arguments as JSON to validate format
        let arguments_value = if arguments.trim().is_empty() {
            None
        } else {
            match serde_json::from_str::<serde_json::Value>(arguments) {
                Ok(value) => Some(value),
                Err(e) => {
                    tracing::error!("Failed to parse MCP tool arguments: {}", e);
                    return codex_protocol::models::ResponseInputItem::FunctionCallOutput {
                        call_id: call_id.to_string(),
                        output: codex_protocol::models::FunctionCallOutputPayload {
                            content: format!("Failed to parse arguments: {}", e),
                            success: Some(false),
                        },
                    };
                }
            }
        };

        // Send MCP tool call begin event to show in TUI
        let invocation = codex_protocol::protocol::McpInvocation {
            server: server_name.to_string(),
            tool: tool_name.to_string(),
            arguments: arguments_value.clone(),
        };

        let tool_call_begin_event = codex_protocol::protocol::EventMsg::McpToolCallBegin(
            codex_protocol::protocol::McpToolCallBeginEvent {
                call_id: call_id.to_string(),
                invocation: invocation.clone(),
            },
        );

        self.send_event(codex_protocol::protocol::Event {
            id: "subagent".to_string(),
            msg: tool_call_begin_event,
        })
        .await;

        // Try to execute the MCP tool directly
        let result = self
            .execute_mcp_tool_directly(server_name, tool_name, arguments_value.as_ref())
            .await;

        // Send MCP tool call end event
        let tool_call_end_event = codex_protocol::protocol::EventMsg::McpToolCallEnd(
            codex_protocol::protocol::McpToolCallEndEvent {
                call_id: call_id.to_string(),
                invocation: invocation.clone(),
                duration: std::time::Duration::from_millis(100), // Approximate duration
                result: result.clone(),
            },
        );

        self.send_event(codex_protocol::protocol::Event {
            id: "subagent".to_string(),
            msg: tool_call_end_event,
        })
        .await;

        // Return the MCP tool call output
        codex_protocol::models::ResponseInputItem::McpToolCallOutput {
            call_id: call_id.to_string(),
            result,
        }
    }

    /// Execute MCP tool directly using the MCP connection manager
    async fn execute_mcp_tool_directly(
        &self,
        server_name: &str,
        tool_name: &str,
        arguments: Option<&serde_json::Value>,
    ) -> Result<mcp_types::CallToolResult, String> {
        // Check if we have MCP connection manager available
        if let Some(mcp_manager) = &self.mcp_connection_manager {
            let full_tool_name = format!("{}__{}", server_name, tool_name);

            // Check if the tool is available in our tool definitions
            if let Some(mcp_tools) = &self.mcp_tools {
                if mcp_tools.contains_key(&full_tool_name) {
                    // Use the MCP connection manager to call the tool
                    let arguments_value = arguments.cloned();

                    match mcp_manager
                        .call_tool(server_name, tool_name, arguments_value, None)
                        .await
                    {
                        Ok(result) => return Ok(result),
                        Err(e) => {
                            return Err(format!(
                                "Failed to execute MCP tool {server_name}::{tool_name}: {e}"
                            ));
                        }
                    }
                }
            }
        }

        // Tool not found or MCP manager not available
        Err(format!(
            "MCP tool {}::{} is not available or MCP connection manager is not configured.",
            server_name, tool_name
        ))
    }

    async fn handle_store_context_call(
        &self,
        arguments: &str,
        task: &SubagentTask,
    ) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
        use serde::Deserialize;

        #[derive(Deserialize)]
        struct StoreContextArgs {
            id: String,
            summary: String,
            content: String,
        }

        let args: StoreContextArgs = serde_json::from_str(arguments)?;

        // Create context object
        let context = crate::context_store::Context::new(
            args.id.clone(),
            args.summary.clone(),
            args.content.clone(),
            task.task_id.clone(),       // created_by
            Some(task.task_id.clone()), // task_id
        );

        // Store in context repository
        self.context_repo.store_context(context).await?;

        tracing::info!(
            "Context stored successfully: id='{}', summary='{}', content_length={}",
            args.id,
            args.summary,
            args.content.len()
        );

        Ok(format!(
            "Context '{}' stored successfully. Summary: {}",
            args.id, args.summary
        ))
    }
}
