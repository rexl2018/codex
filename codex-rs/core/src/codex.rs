use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::AuthManager;
use crate::agent_task::AgentTask;
use crate::client_common::REVIEW_PROMPT;
use crate::config_edit::CONFIG_KEY_EFFORT;
use crate::config_edit::CONFIG_KEY_MODEL;
use crate::config_edit::persist_non_null_overrides;
use crate::context_store::IContextRepository;
use crate::context_store::InMemoryContextRepository;
use crate::event_mapping::map_response_item_to_event_messages;
use crate::events::format_exec_output;
use crate::events::format_exec_output_str;
use crate::multi_agent_coordinator::ExecutionPlan;
use crate::multi_agent_coordinator::SharedContext;
use crate::multi_agent_coordinator::SubtaskCoordinator;
use crate::review_format::format_review_findings_block;
use crate::session::ConfigureSession;
use crate::session::MutexExt;
use crate::session::Session;
use crate::state::State;
use crate::subagent_manager::ExecutorType;
use crate::subagent_manager::ISubagentManager;
use crate::subagent_manager::InMemorySubagentManager;
use crate::subagent_manager::SubagentTaskSpec;
use crate::submission_loop::original_submission_loop;
use crate::submission_loop::submission_loop;
use crate::turn_context::TurnContext;
use crate::types::AgentState;
use crate::types::ApplyPatchCommandContext;
use crate::types::ExecCommandContext;
use crate::types::detect_agent_state;
use async_channel::Receiver;
use async_channel::Sender;
use codex_apply_patch::ApplyPatchAction;
use codex_apply_patch::MaybeApplyPatchVerified;
use codex_apply_patch::maybe_parse_apply_patch_verified;
use codex_protocol::mcp_protocol::ConversationId;
use codex_protocol::protocol::ConversationPathResponseEvent;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::protocol::TaskStartedEvent;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use futures::prelude::*;
use mcp_types::CallToolResult;
use serde::Deserialize;
use serde::Serialize;
use serde_json;
use tokio::sync::Mutex as TokioMutex;
use tokio::sync::oneshot;
use tokio::task::AbortHandle;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::trace;
use tracing::warn;

use crate::ModelProviderInfo;
use crate::apply_patch;
use crate::apply_patch::ApplyPatchExec;
use crate::apply_patch::CODEX_APPLY_PATCH_ARG1;
use crate::apply_patch::InternalApplyPatchInvocation;
use crate::apply_patch::convert_apply_patch_to_protocol;
use crate::client::ModelClient;
use crate::client_common::Prompt;
use crate::client_common::ResponseEvent;
use crate::config::Config;
use crate::config_types::ShellEnvironmentPolicy;
use crate::conversation_history::ConversationHistory;
use crate::environment_context::EnvironmentContext;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::error::SandboxErr;
use crate::error::get_error_message_ui;
use crate::exec::ExecParams;
use crate::exec::ExecToolCallOutput;
use crate::exec::SandboxType;
use crate::exec::StdoutStream;
use crate::exec::StreamOutput;
use crate::exec::process_exec_tool_call;
use crate::exec_command::EXEC_COMMAND_TOOL_NAME;
use crate::exec_command::ExecCommandParams;
use crate::exec_command::ExecSessionManager;
use crate::exec_command::WRITE_STDIN_TOOL_NAME;
use crate::exec_command::WriteStdinParams;
use crate::exec_env::create_env;
use crate::main_agent::MainAgent;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::mcp_tool_call::handle_mcp_tool_call;
use crate::model_family::find_family_for_model;
use crate::openai_model_info::get_model_info;
use crate::openai_tools::ApplyPatchToolArgs;
use crate::parse_command::parse_command;
use crate::plan_tool::handle_update_plan;
use crate::project_doc::get_user_instructions;
use crate::protocol::AgentMessageDeltaEvent;
use crate::protocol::AgentMessageEvent;
use crate::protocol::AgentReasoningDeltaEvent;
use crate::protocol::AgentReasoningRawContentDeltaEvent;
use crate::protocol::AgentReasoningSectionBreakEvent;
use crate::protocol::ApplyPatchApprovalRequestEvent;
use crate::protocol::AskForApproval;
use crate::protocol::BackgroundEventEvent;
use crate::protocol::ErrorEvent;
use crate::protocol::Event;
use crate::protocol::EventMsg;
use crate::protocol::ExecApprovalRequestEvent;
use crate::protocol::ExecCommandBeginEvent;
use crate::protocol::ExecCommandEndEvent;
use crate::protocol::FileChange;
use crate::protocol::InputItem;
use crate::protocol::ListCustomPromptsResponseEvent;
use crate::protocol::Op;
use crate::protocol::PatchApplyBeginEvent;
use crate::protocol::PatchApplyEndEvent;
use crate::protocol::ReviewDecision;
use crate::protocol::SandboxPolicy;
use crate::protocol::SessionConfiguredEvent;
use crate::protocol::StreamErrorEvent;
use crate::protocol::Submission;
use crate::protocol::TaskCompleteEvent;
use crate::protocol::TokenUsageInfo;
use crate::protocol::TurnDiffEvent;
use crate::protocol::WebSearchBeginEvent;
use crate::rollout::RolloutRecorder;
use crate::rollout::RolloutRecorderParams;
use crate::safety::SafetyCheck;
use crate::safety::assess_command_safety;
use crate::safety::assess_safety_for_untrusted_command;
use crate::shell;
use crate::sub_agent::SubAgent;
use crate::tool_config::UnifiedToolConfig;
use crate::tool_registry::GLOBAL_TOOL_REGISTRY;
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::unified_exec::UnifiedExecSessionManager;
use crate::unified_function_handler::AgentType;
use crate::user_instructions::UserInstructions;
use crate::user_notification::UserNotification;
use crate::util::backoff;
use codex_mcp_client::McpClient;
use codex_protocol::config_types::ReasoningEffort as ReasoningEffortConfig;
use codex_protocol::config_types::ReasoningSummary as ReasoningSummaryConfig;
use codex_protocol::custom_prompts::CustomPrompt;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::LocalShellAction;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::ShellToolCallParams;
use codex_protocol::protocol::BootstrapPath;
use codex_protocol::protocol::ContextItem;
use codex_protocol::protocol::ContextQuery;
use codex_protocol::protocol::ContextQueryResultEvent;
use codex_protocol::protocol::ContextStoredEvent;
use codex_protocol::protocol::ContextSummary;
use codex_protocol::protocol::GetContextsResultEvent;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::LoadContextsFromFileResultEvent;
use codex_protocol::protocol::SaveContextsToFileResultEvent;
use codex_protocol::protocol::SubagentMetadata;
use codex_protocol::protocol::SubagentType;

pub mod compact;

/// The high-level interface to the Codex system.
/// It operates as a queue pair where you send submissions and receive events.
pub struct Codex {
    next_id: AtomicU64,
    tx_sub: Sender<Submission>,
    rx_event: Receiver<Event>,
}

/// Wrapper returned by [`Codex::spawn`] containing the spawned [`Codex`],
/// the submission id for the initial `ConfigureSession` request and the
/// unique session id.
pub struct CodexSpawnOk {
    pub codex: Codex,
    pub conversation_id: ConversationId,
}

pub(crate) const INITIAL_SUBMIT_ID: &str = "";
pub(crate) const SUBMISSION_CHANNEL_CAPACITY: usize = 64;

// Model-formatting limits: clients get full streams; oonly content sent to the model is truncated.

impl Codex {
    /// Spawn a new [`Codex`] and initialize the session.
    pub async fn spawn(
        config: Config,
        auth_manager: Arc<AuthManager>,
        conversation_history: InitialHistory,
    ) -> CodexResult<CodexSpawnOk> {
        let (tx_sub, rx_sub) = async_channel::bounded(SUBMISSION_CHANNEL_CAPACITY);
        let (tx_event, rx_event) = async_channel::unbounded();

        let user_instructions = get_user_instructions(&config).await;

        let config = Arc::new(config);

        let configure_session = ConfigureSession {
            provider: config.model_provider.clone(),
            model: config.model.clone(),
            model_reasoning_effort: config.model_reasoning_effort,
            model_reasoning_summary: config.model_reasoning_summary,
            user_instructions,
            base_instructions: config.base_instructions.clone(),
            approval_policy: config.approval_policy,
            sandbox_policy: config.sandbox_policy.clone(),
            notify: config.notify.clone(),
            cwd: config.cwd.clone(),
        };

        // Generate a unique ID for the lifetime of this Codex session.
        let (session, turn_context) = Session::new(
            configure_session,
            config.clone(),
            auth_manager.clone(),
            tx_event.clone(),
            conversation_history.clone(),
        )
        .await
        .map_err(|e| {
            error!("Failed to create session: {e:#}");
            CodexErr::InternalAgentDied
        })?;
        let conversation_id = session.conversation_id;

        // This task will run until Op::Shutdown is received.
        tokio::spawn(submission_loop(
            session.clone(),
            turn_context,
            config,
            rx_sub,
        ));
        let codex = Codex {
            next_id: AtomicU64::new(0),
            tx_sub,
            rx_event,
        };

        Ok(CodexSpawnOk {
            codex,
            conversation_id,
        })
    }

    /// Submit the `op` wrapped in a `Submission` with a unique ID.
    pub async fn submit(&self, op: Op) -> CodexResult<String> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            .to_string();
        let sub = Submission { id: id.clone(), op };
        self.submit_with_id(sub).await?;
        Ok(id)
    }

    /// Use sparingly: prefer `submit()` so Codex is responsible for generating
    /// unique IDs for each submission.
    pub async fn submit_with_id(&self, sub: Submission) -> CodexResult<()> {
        self.tx_sub
            .send(sub)
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(())
    }

    pub async fn next_event(&self) -> CodexResult<Event> {
        let event = self
            .rx_event
            .recv()
            .await
            .map_err(|_| CodexErr::InternalAgentDied)?;
        Ok(event)
    }
}

/// Refactored submission loop that uses the new agent architecture

/// Takes a user message as input and runs a loop where, at each turn, the model
/// replies with either:
///
/// - requested function calls
/// - an assistant message
///
/// While it is possible for the model to return multiple of these items in a
/// single turn, in practice, we generally one item per turn:
///
/// - If the model requests a function call, we execute it and send the output
///   back to the model in the next turn.
/// - If the model sends only an assistant message, we record it in the
///   conversation history and consider the task complete.
pub(crate) async fn run_task(
    sess: Arc<Session>,
    turn_context: &TurnContext,
    sub_id: String,
    input: Vec<InputItem>,
) {
    if input.is_empty() {
        return;
    }
    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskStarted(TaskStartedEvent {
            model_context_window: turn_context.client.get_model_context_window(),
        }),
    };
    sess.send_event(event).await;

    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input);
    sess.record_input_and_rollout_usermsg(&initial_input_for_turn)
        .await;

    let mut last_agent_message: Option<String> = None;
    // Although from the perspective of codex.rs, TurnDiffTracker has the lifecycle of a Task which contains
    // many turns, from the perspective of the user, it is a single turn.
    let mut turn_diff_tracker = TurnDiffTracker::new();

    loop {
        // Check for recently completed subagents and add completion message to conversation history
        // This must be done BEFORE constructing turn_input so the LLM can see the completion message
        if let Some(components) = &sess.multi_agent_components {
            let agent_state = detect_agent_state(&sess.multi_agent_components).await;
            if let AgentState::DecidingNextStep = agent_state {
                match components
                    .subagent_manager
                    .get_recently_completed_tasks(1)
                    .await
                {
                    Ok(recent_tasks) => {
                        if let Some(recent_task) = recent_tasks.first() {
                            // Check if weve already added a completion message for this specific subagent ID
                            let should_add_completion_message = {
                                let history = sess.state.lock_unchecked().history.contents();
                                let subagent_id_marker = format!("(ID: {})", recent_task.task_id);

                                // Check recent messages (last 100) for performance optimization
                                let recent_messages = if history.len() > 100 {
                                    &history[history.len() - 100..]
                                } else {
                                    &history[..]
                                };

                                // Check if ANY recent assistant message contains this subagent ID
                                !recent_messages.iter().any(|item| {
                                    if let ResponseItem::Message { role, content, .. } = item {
                                        if role == "assistant" {
                                            let text = content
                                                .iter()
                                                .filter_map(|c| match c {
                                                    ContentItem::OutputText { text } => {
                                                        Some(text.as_str())
                                                    }
                                                    _ => None,
                                                })
                                                .collect::<Vec<_>>()
                                                .join(" ");
                                            text.contains(&subagent_id_marker)
                                        } else {
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                })
                            };

                            if should_add_completion_message {
                                // Add a completion message to conversation history so LLM can see the full flow
                                let completion_message = ResponseItem::Message {
                                    id: None,
                                    role: "assistant".to_string(),
                                    content: vec![ContentItem::OutputText {
                                        text: format!(
                                        "The {} subagent '{}' (ID: {}) has completed successfully. I can now analyze its results using the context tools and provide a summary to the user.",
                                        match recent_task.agent_type {
                                            codex_protocol::protocol::SubagentType::Explorer => "Explorer",
                                            codex_protocol::protocol::SubagentType::Coder => "Coder",
                                        },
                                        recent_task.title,
                                        recent_task.task_id
                                    )
                                    }]
                                };

                                // Record this completion message in conversation history
                                sess.record_conversation_items(&[completion_message]).await;

                                info!(
                                    "Added subagent completion message to conversation history: {:?} - {}",
                                    recent_task.agent_type, recent_task.title
                                );
                            }
                        }
                    }
                    Err(_) => {}
                }
            }
        }

        // Note that pending_input would be something like a message the user
        // submitted through the UI while the model was running. Though the UI
        // may support this, the model might not.
        let pending_input = sess
            .get_pending_input()
            .into_iter()
            .map(ResponseItem::from)
            .collect::<Vec<ResponseItem>>();
        sess.record_conversation_items(&pending_input).await;

        // Construct the input that we will send to the model. When using the
        // Chat completions API (or ZDR clients), the model needs the full
        // conversation history on each turn. The rollout file, however, should
        // only record the new items that originated in this turn so that it
        // represents an append-only log without duplicates.
        let turn_input: Vec<ResponseItem> = sess.turn_input_with_history(pending_input);

        let turn_input_messages: Vec<String> = turn_input
            .iter()
            .filter_map(|item| match item {
                ResponseItem::Message { content, .. } => Some(content),
                _ => None,
            })
            .flat_map(|content| {
                content.iter().filter_map(|item| match item {
                    ContentItem::OutputText { text } => Some(text.clone()),
                    _ => None,
                })
            })
            .collect();
        match run_turn(
            &sess,
            turn_context,
            &mut turn_diff_tracker,
            sub_id.clone(),
            turn_input,
        )
        .await
        {
            Ok(turn_output) => {
                // If turn_output is empty, it means we're waiting for subagent
                // Add a short delay to avoid busy waiting
                if turn_output.is_empty() {
                    tokio::time::sleep(Duration::from_millis(900)).await;
                    continue;
                }
                let mut items_to_record_in_conversation_history = Vec::<ResponseItem>::new();
                let mut responses = Vec::<ResponseInputItem>::new();
                for processed_response_item in turn_output {
                    let ProcessedResponseItem { item, response } = processed_response_item;
                    match (&item, &response) {
                        (ResponseItem::Message { role, .. }, None) if role == "assistant" => {
                            // If the model returned a message, we need to record it.
                            items_to_record_in_conversation_history.push(item);
                        }
                        (
                            ResponseItem::LocalShellCall { .. },
                            Some(ResponseInputItem::FunctionCallOutput { call_id, output }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output: output.clone(),
                                },
                            );
                        }
                        (
                            ResponseItem::FunctionCall { .. },
                            Some(ResponseInputItem::FunctionCallOutput { call_id, output }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output: output.clone(),
                                },
                            );
                        }
                        (
                            ResponseItem::CustomToolCall { .. },
                            Some(ResponseInputItem::CustomToolCallOutput { call_id, output }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            items_to_record_in_conversation_history.push(
                                ResponseItem::CustomToolCallOutput {
                                    call_id: call_id.clone(),
                                    output: output.clone(),
                                },
                            );
                        }
                        (
                            ResponseItem::FunctionCall { .. },
                            Some(ResponseInputItem::McpToolCallOutput { call_id, result }),
                        ) => {
                            items_to_record_in_conversation_history.push(item);
                            let output = match result {
                                Ok(call_tool_result) => {
                                    convert_call_tool_result_to_function_call_output_payload(
                                        call_tool_result,
                                    )
                                }
                                Err(err) => FunctionCallOutputPayload {
                                    content: err.clone(),
                                    success: Some(false),
                                },
                            };
                            items_to_record_in_conversation_history.push(
                                ResponseItem::FunctionCallOutput {
                                    call_id: call_id.clone(),
                                    output,
                                },
                            );
                        }
                        (
                            ResponseItem::Reasoning {
                                id,
                                summary,
                                content,
                                encrypted_content,
                            },
                            None,
                        ) => {
                            items_to_record_in_conversation_history.push(ResponseItem::Reasoning {
                                id: id.clone(),
                                summary: summary.clone(),
                                content: content.clone(),
                                encrypted_content: encrypted_content.clone(),
                            });
                        }
                        _ => {
                            warn!("Unexpected response item: {item:?} with response: {response:?}");
                        }
                    };
                    if let Some(response) = response {
                        responses.push(response);
                    }
                }

                // Only attempt to take the lock if there is something to record.
                if !items_to_record_in_conversation_history.is_empty() {
                    sess.record_conversation_items(&items_to_record_in_conversation_history)
                        .await;
                }

                if responses.is_empty() {
                    debug!("Turn completed");
                    last_agent_message = get_last_assistant_message_from_turn(
                        &items_to_record_in_conversation_history,
                    );
                    sess.maybe_notify(UserNotification::AgentTurnComplete {
                        turn_id: sub_id.clone(),
                        input_messages: turn_input_messages,
                        last_assistant_message: last_agent_message.clone(),
                    });
                    break;
                }
            }
            Err(e) => {
                info!("Turn error: {e:#}");

                // Handle BadRequest errors specially - content already processed in run_turn
                if let CodexErr::BadRequest { message } = &e {
                    info!(
                        "BadRequest error detected (content already summarized in run_turn), adding error message: {}",
                        message
                    );

                    // Add an error message to the conversation history so the LLM knows what happened
                    let error_message = ResponseItem::Message {
                        id: None,
                        role: "assistant".to_string(),
                        content: vec![ContentItem::OutputText {
                            text: format!(
                                "I encountered a 400 Bad Request error from the API. This usually means the request was too large. Error details: {}\n\nI've replaced large content with summaries to reduce the context size. I'll continue with a more concise approach.",
                                message
                            ),
                        }],
                    };

                    sess.record_conversation_items(&[error_message]).await;

                    // Send error event to UI
                    let event = Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: format!("Bad Request (content summarized): {}", message),
                        }),
                    };
                    sess.send_event(event).await;

                    // Continue the loop to let the LLM try again with summarized content
                    continue;
                } else {
                    // For other errors, use the original behavior
                    let event = Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: e.to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                    // let the user continue the conversation
                    break;
                }
            }
        }
    }
    sess.remove_task(&sub_id);
    let event = Event {
        id: sub_id,
        msg: EventMsg::TaskComplete(TaskCompleteEvent { last_agent_message }),
    };
    sess.send_event(event).await;
}

async fn run_turn(
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    input: Vec<ResponseItem>,
) -> CodexResult<Vec<ProcessedResponseItem>> {
    let tools = match GLOBAL_TOOL_REGISTRY.get_tools_for_agent(&AgentType::Main) {
        Ok(tools) => tools,
        Err(e) => {
            tracing::error!("Failed to get tools from unified registry: {}", e);
            // Fallback to empty tools list
            vec![]
        }
    };

    // Detect current agent state and create state information
    let agent_state = detect_agent_state(&sess.multi_agent_components).await;

    // Check if we're in WaitingForSubagent state - if so, pause LLM interaction
    if let AgentState::WaitingForSubagent { subagent_id } = &agent_state {
        tracing::trace!(
            "🔍 WAITING FOR SUBAGENT: Current State: {:?}, pausing LLM interaction until subagent {} completes",
            agent_state,
            subagent_id
        );

        // Return empty response to avoid LLM interaction while waiting for subagent
        // The main task loop will continue but won't make LLM calls
        return Ok(vec![]);
    }

    // Check for recently completed subagents to provide context
    // Only show completion info when we're in DecidingNextStep state after a subagent completion
    let recent_completion_info = if let Some(components) = &sess.multi_agent_components {
        match agent_state {
            AgentState::DecidingNextStep => {
                // Check if we just transitioned from WaitingForSubagent
                match components
                    .subagent_manager
                    .get_recently_completed_tasks(1)
                    .await
                {
                    Ok(recent_tasks) => {
                        if let Some(recent_task) = recent_tasks.first() {
                            Some(format!(
                                "A {:?} subagent has completed the task: '{}' (ID: {}). If you need more information, you can use 'list_contexts' to see available analysis results, then 'multi_retrieve_contexts' to get the detailed findings. If you have sufficient information, you can decide what to do next: summarize and complete this task, or create a new subagent for additional work.\n\n⚠️ IMPORTANT: When creating new subagents, always review the available context items and include relevant context IDs in the 'context_refs' parameter to provide background knowledge to the subagent.",
                                recent_task.agent_type, recent_task.title, recent_task.task_id
                            ))
                        } else {
                            None
                        }
                    }
                    Err(_) => None,
                }
            }
            AgentState::WaitingForSubagent { ref subagent_id } => {
                // When waiting for subagent, provide different context
                Some(format!(
                    "Currently waiting for subagent '{}' to complete. No new actions should be taken until completion.",
                    subagent_id
                ))
            }
            _ => None,
        }
    } else {
        None
    };

    let agent_state_info = if !agent_state.can_create_explorer() || !agent_state.can_create_coder()
    {
        let base_info = format!(
            "Current Agent State: {}. Subagent Creation Constraints: Explorer Subagent: {}, Coder Subagent: {}. Note: {}. Please work with available information or consider alternative approaches instead of creating blocked subagent types.",
            agent_state.description(),
            if agent_state.can_create_explorer() {
                "Allowed"
            } else {
                "Blocked"
            },
            if agent_state.can_create_coder() {
                "Allowed"
            } else {
                "Blocked"
            },
            if !agent_state.can_create_explorer() && !agent_state.can_create_coder() {
                "All subagent creation is currently blocked"
            } else if !agent_state.can_create_explorer() {
                "Explorer subagent creation is blocked due to previous forced completion"
            } else {
                "Coder subagent creation is blocked due to previous forced completion"
            }
        );

        if let Some(completion_info) = recent_completion_info {
            format!("{}\n\n{}", completion_info, base_info)
        } else {
            base_info
        }
    } else {
        let base_info = format!(
            "Current Agent State: {}. Subagent Creation Constraints: All subagent types are currently allowed.",
            agent_state.description()
        );

        if let Some(completion_info) = recent_completion_info {
            format!("{}\n\n{}", completion_info, base_info)
        } else {
            base_info
        }
    };

    tracing::info!(
        "🔍 NORMAL TURN STATE: Current State: {:?}, Explorer: {}, Coder: {}",
        agent_state,
        if agent_state.can_create_explorer() {
            "Allowed"
        } else {
            "BLOCKED"
        },
        if agent_state.can_create_coder() {
            "Allowed"
        } else {
            "BLOCKED"
        }
    );

    // Get available contexts to provide to the LLM
    let available_contexts = if let Some(components) = &sess.multi_agent_components {
        use crate::context_store::ContextQuery as StoreContextQuery;
        let query = StoreContextQuery {
            ids: None,
            tags: None,
            created_by: None,
            limit: None,
        };
        match components.context_repo.query_contexts(&query).await {
            Ok(contexts) => Some(contexts.into_iter().map(|c| c.into()).collect()),
            Err(e) => {
                warn!("Failed to get available contexts: {}", e);
                None
            }
        }
    } else {
        None
    };

    let prompt = Prompt {
        input,
        tools: tools.clone(),
        base_instructions_override: turn_context.base_instructions.clone(),
        agent_state_info: Some(agent_state_info.clone()),
        available_contexts: available_contexts.clone(),
    };

    // Log the LLM request details for debugging
    info!(
        "🤖 LLM REQUEST: State={:?}, Agent State Info: {}, Available Contexts: {}, Tools: {}",
        agent_state,
        agent_state_info,
        available_contexts.as_ref().map_or(0, |c| c.len()),
        tools.len()
    );

    let mut retries = 0;
    loop {
        match try_run_turn(sess, turn_context, turn_diff_tracker, &sub_id, &prompt).await {
            Ok(output) => return Ok(output),
            Err(CodexErr::Interrupted) => return Err(CodexErr::Interrupted),
            Err(CodexErr::EnvVar(var)) => return Err(CodexErr::EnvVar(var)),
            Err(e @ (CodexErr::UsageLimitReached(_) | CodexErr::UsageNotIncluded)) => {
                return Err(e);
            }

            Err(e) => {
                // Use the configured provider-specific stream retry budget.
                let max_retries = turn_context.client.get_provider().stream_max_retries();
                if retries < max_retries {
                    retries += 1;
                    let delay = match e {
                        CodexErr::Stream(_, Some(delay)) => delay,
                        _ => backoff(retries),
                    };
                    warn!(
                        "stream disconnected - retrying turn ({retries}/{max_retries} in {delay:?})...",
                    );

                    // Surface retry information to any UI/front‑end so the
                    // user understands what is happening instead of staring
                    // at a seemingly frozen screen.
                    sess.notify_stream_error(
                        &sub_id,
                        format!(
                            "stream error: {e}; retrying {retries}/{max_retries} in {delay:?}…"
                        ),
                    )
                    .await;

                    tokio::time::sleep(delay).await;
                } else {
                    return Err(e);
                }
            }
        }
    }
}

/// When the model is prompted, it returns a stream of events. Some of these
/// events map to a `ResponseItem`. A `ResponseItem` may need to be
/// "handled" such that it produces a `ResponseInputItem` that needs to be
/// sent back to the model on the next turn.
#[derive(Debug)]
struct ProcessedResponseItem {
    item: ResponseItem,
    response: Option<ResponseInputItem>,
}

async fn try_run_turn(
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: &str,
    prompt: &Prompt,
) -> CodexResult<Vec<ProcessedResponseItem>> {
    // call_ids that are part of this response.
    let completed_call_ids = prompt
        .input
        .iter()
        .filter_map(|ri| match ri {
            ResponseItem::FunctionCallOutput { call_id, .. } => Some(call_id),
            ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => Some(call_id),
            ResponseItem::CustomToolCallOutput { call_id, .. } => Some(call_id),
            _ => None,
        })
        .collect::<Vec<_>>();

    // call_ids that were pending but are not part of this response.
    // This usually happens because the user interrupted the model before we responded to one of its tool calls
    // and then the user sent a follow-up message.
    let missing_calls = {
        prompt
            .input
            .iter()
            .filter_map(|ri| match ri {
                ResponseItem::FunctionCall { call_id, .. } => Some(call_id),
                ResponseItem::LocalShellCall {
                    call_id: Some(call_id),
                    ..
                } => Some(call_id),
                ResponseItem::CustomToolCall { call_id, .. } => Some(call_id),
                _ => None,
            })
            .filter_map(|call_id| {
                if completed_call_ids.contains(&call_id) {
                    None
                } else {
                    Some(call_id.clone())
                }
            })
            .map(|call_id| ResponseItem::CustomToolCallOutput {
                call_id: call_id.clone(),
                output: "aborted".to_string(),
            })
            .collect::<Vec<_>>()
    };
    let prompt: Cow<Prompt> = if missing_calls.is_empty() {
        Cow::Borrowed(prompt)
    } else {
        // Add the synthetic aborted missing calls to the beginning of the input to ensure all call ids have responses.
        let input = [missing_calls, prompt.input.clone()].concat();
        Cow::Owned(Prompt {
            input,
            ..prompt.clone()
        })
    };

    let (mut stream, _updated_messages) =
        call_llm_with_error_handling(&turn_context.client, prompt.into_owned(), "main agent")
            .await?;
    // Note: Main agent doesn't need to update local messages as it uses session state

    let mut output = Vec::new();

    loop {
        // Poll the next item from the model stream. We must inspect *both* Ok and Err
        // cases so that transient stream failures (e.g., dropped SSE connection before
        // `response.completed`) bubble up and trigger the caller's retry logic.
        let event = stream.next().await;
        let Some(event) = event else {
            // Channel closed without yielding a final Completed event or explicit error.
            // Treat as a disconnected stream so the caller can retry.
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };

        let event = match event {
            Ok(ev) => ev,
            Err(e) => {
                // Propagate the underlying stream error to the caller (run_turn), which
                // will apply the configured `stream_max_retries` policy.
                return Err(e);
            }
        };

        match event {
            ResponseEvent::Created => {}
            ResponseEvent::OutputItemDone(item) => {
                tracing::info!("Full response item: {:?}", item);
                let response = handle_response_item(
                    sess,
                    turn_context,
                    turn_diff_tracker,
                    sub_id,
                    item.clone(),
                )
                .await?;
                output.push(ProcessedResponseItem { item, response });
            }
            ResponseEvent::WebSearchCallBegin { call_id } => {
                let _ = sess
                    .tx_event
                    .send(Event {
                        id: sub_id.to_string(),
                        msg: EventMsg::WebSearchBegin(WebSearchBeginEvent { call_id }),
                    })
                    .await;
            }
            ResponseEvent::Completed {
                response_id: _,
                token_usage,
            } => {
                let info = {
                    let mut st = sess.state.lock_unchecked();
                    let info = TokenUsageInfo::new_or_append(
                        &st.token_info,
                        &token_usage,
                        turn_context.client.get_model_context_window(),
                    );
                    st.token_info = info.clone();
                    info
                };
                let _ = sess
                    .send_event(Event {
                        id: sub_id.to_string(),
                        msg: EventMsg::TokenCount(crate::protocol::TokenCountEvent { info }),
                    })
                    .await;

                let unified_diff = turn_diff_tracker.get_unified_diff();
                if let Ok(Some(unified_diff)) = unified_diff {
                    let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
                    let event = Event {
                        id: sub_id.to_string(),
                        msg,
                    };
                    sess.send_event(event).await;
                }

                return Ok(output);
            }
            ResponseEvent::OutputTextDelta(delta) => {
                let event = Event {
                    id: sub_id.to_string(),
                    msg: EventMsg::AgentMessageDelta(AgentMessageDeltaEvent { delta }),
                };
                sess.send_event(event).await;
            }
            ResponseEvent::ReasoningSummaryDelta(delta) => {
                let event = Event {
                    id: sub_id.to_string(),
                    msg: EventMsg::AgentReasoningDelta(AgentReasoningDeltaEvent { delta }),
                };
                sess.send_event(event).await;
            }
            ResponseEvent::ReasoningSummaryPartAdded => {
                let event = Event {
                    id: sub_id.to_string(),
                    msg: EventMsg::AgentReasoningSectionBreak(AgentReasoningSectionBreakEvent {}),
                };
                sess.send_event(event).await;
            }
            ResponseEvent::ReasoningContentDelta(delta) => {
                if sess.show_raw_agent_reasoning {
                    let event = Event {
                        id: sub_id.to_string(),
                        msg: EventMsg::AgentReasoningRawContentDelta(
                            AgentReasoningRawContentDeltaEvent { delta },
                        ),
                    };
                    sess.send_event(event).await;
                }
            }
        }
    }
}

async fn handle_response_item(
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: &str,
    item: ResponseItem,
) -> CodexResult<Option<ResponseInputItem>> {
    debug!(?item, "Output item");
    let output = match item {
        ResponseItem::FunctionCall {
            name,
            arguments,
            call_id,
            ..
        } => {
            info!("FunctionCall: {name}({arguments})");
            Some(
                handle_function_call(
                    sess,
                    turn_context,
                    turn_diff_tracker,
                    sub_id.to_string(),
                    name,
                    arguments,
                    call_id,
                )
                .await,
            )
        }
        ResponseItem::LocalShellCall {
            id,
            call_id,
            status: _,
            action,
        } => {
            let LocalShellAction::Exec(action) = action;
            tracing::info!("LocalShellCall: {action:?}");
            let params = ShellToolCallParams {
                command: action.command,
                workdir: action.working_directory,
                timeout_ms: action.timeout_ms,
                with_escalated_permissions: None,
                justification: None,
            };
            let effective_call_id = match (call_id, id) {
                (Some(call_id), _) => call_id,
                (None, Some(id)) => id,
                (None, None) => {
                    error!("LocalShellCall without call_id or id");
                    return Ok(Some(ResponseInputItem::FunctionCallOutput {
                        call_id: "".to_string(),
                        output: FunctionCallOutputPayload {
                            content: "LocalShellCall without call_id or id".to_string(),
                            success: None,
                        },
                    }));
                }
            };

            let exec_params = to_exec_params(params, turn_context);
            Some(
                handle_container_exec_with_params(
                    exec_params,
                    sess,
                    turn_context,
                    turn_diff_tracker,
                    sub_id.to_string(),
                    effective_call_id,
                )
                .await,
            )
        }
        ResponseItem::CustomToolCall {
            id: _,
            call_id,
            name,
            input,
            status: _,
        } => Some(
            handle_custom_tool_call(
                sess,
                turn_context,
                turn_diff_tracker,
                sub_id.to_string(),
                name,
                input,
                call_id,
            )
            .await,
        ),
        ResponseItem::FunctionCallOutput { .. } => {
            debug!("unexpected FunctionCallOutput from stream");
            None
        }
        ResponseItem::CustomToolCallOutput { .. } => {
            debug!("unexpected CustomToolCallOutput from stream");
            None
        }
        ResponseItem::Message { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. } => {
            let msgs = map_response_item_to_event_messages(&item, sess.show_raw_agent_reasoning);
            for msg in msgs {
                let event = Event {
                    id: sub_id.to_string(),
                    msg,
                };
                sess.send_event(event).await;
            }
            None
        }
        ResponseItem::Other => None,
    };
    Ok(output)
}

async fn handle_unified_exec_tool_call(
    sess: &Session,
    call_id: String,
    session_id: Option<String>,
    arguments: Vec<String>,
    timeout_ms: Option<u64>,
) -> ResponseInputItem {
    let parsed_session_id = if let Some(session_id) = session_id {
        match session_id.parse::<i32>() {
            Ok(parsed) => Some(parsed),
            Err(output) => {
                return ResponseInputItem::FunctionCallOutput {
                    call_id: call_id.to_string(),
                    output: FunctionCallOutputPayload {
                        content: format!("invalid session_id: {session_id} due to error {output}"),
                        success: Some(false),
                    },
                };
            }
        }
    } else {
        None
    };

    let request = crate::unified_exec::UnifiedExecRequest {
        session_id: parsed_session_id,
        input_chunks: &arguments,
        timeout_ms,
    };

    let result = sess.unified_exec_manager.handle_request(request).await;

    let output_payload = match result {
        Ok(value) => {
            #[derive(Serialize)]
            struct SerializedUnifiedExecResult<'a> {
                session_id: Option<String>,
                output: &'a str,
            }

            match serde_json::to_string(&SerializedUnifiedExecResult {
                session_id: value.session_id.map(|id| id.to_string()),
                output: &value.output,
            }) {
                Ok(serialized) => FunctionCallOutputPayload {
                    content: serialized,
                    success: Some(true),
                },
                Err(err) => FunctionCallOutputPayload {
                    content: format!("failed to serialize unified exec output: {err}"),
                    success: Some(false),
                },
            }
        }
        Err(err) => FunctionCallOutputPayload {
            content: format!("unified exec failed: {err}"),
            success: Some(false),
        },
    };

    ResponseInputItem::FunctionCallOutput {
        call_id,
        output: output_payload,
    }
}

async fn handle_list_contexts(sess: &Session, call_id: String) -> ResponseInputItem {
    let result = if let Some(multi_agent_components) = &sess.multi_agent_components {
        use crate::context_store::ContextQuery;
        use crate::context_store::IContextRepository;

        match multi_agent_components
            .context_repo
            .query_contexts(&ContextQuery {
                ids: None,
                tags: None,
                created_by: None,
                limit: None,
            })
            .await
        {
            Ok(contexts) => {
                let context_list: Vec<_> = contexts
                    .iter()
                    .map(|c| format!("- {}: {}", c.id, c.summary))
                    .collect();
                FunctionCallOutputPayload {
                    content: format!("Available contexts:\n{}", context_list.join("\n")),
                    success: Some(true),
                }
            }
            Err(e) => FunctionCallOutputPayload {
                content: format!("Failed to list contexts: {}", e),
                success: Some(false),
            },
        }
    } else {
        FunctionCallOutputPayload {
            content: "Multi-agent components not initialized".to_string(),
            success: Some(false),
        }
    };

    ResponseInputItem::FunctionCallOutput {
        call_id,
        output: result,
    }
}

async fn handle_multi_retrieve_contexts(
    sess: &Session,
    arguments: String,
    call_id: String,
) -> ResponseInputItem {
    #[derive(serde::Deserialize)]
    struct MultiRetrieveContextsArgs {
        ids: Vec<String>,
    }

    let args = match serde_json::from_str::<MultiRetrieveContextsArgs>(&arguments) {
        Ok(args) => args,
        Err(err) => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("failed to parse multi_retrieve_contexts arguments: {err}"),
                    success: Some(false),
                },
            };
        }
    };

    let result = if let Some(multi_agent_components) = &sess.multi_agent_components {
        use crate::context_store::IContextRepository;

        match multi_agent_components
            .context_repo
            .get_contexts(&args.ids)
            .await
        {
            Ok(contexts) => {
                let context_list: Vec<_> = contexts
                    .iter()
                    .map(|c| format!("- {}: {}\n{}", c.id, c.summary, c.content))
                    .collect();
                FunctionCallOutputPayload {
                    content: format!("Retrieved contexts:\n{}", context_list.join("\n\n")),
                    success: Some(true),
                }
            }
            Err(e) => FunctionCallOutputPayload {
                content: format!("Failed to retrieve contexts: {}", e),
                success: Some(false),
            },
        }
    } else {
        FunctionCallOutputPayload {
            content: "Multi-agent components not initialized".to_string(),
            success: Some(false),
        }
    };

    ResponseInputItem::FunctionCallOutput {
        call_id,
        output: result,
    }
}

async fn handle_function_call(
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    name: String,
    arguments: String,
    call_id: String,
) -> ResponseInputItem {
    // Check if the tool is allowed for main agent
    let allowed_tools = match GLOBAL_TOOL_REGISTRY.get_tools_for_agent(&AgentType::Main) {
        Ok(tools) => tools,
        Err(e) => {
            tracing::error!("Failed to get tools from unified registry: {}", e);
            vec![]
        }
    };

    let tool_allowed = allowed_tools.iter().any(|tool| match tool {
        crate::openai_tools::OpenAiTool::Function(func) => func.name == name,
        crate::openai_tools::OpenAiTool::LocalShell {} => name == "local_shell",
        crate::openai_tools::OpenAiTool::WebSearch {} => name == "web_search",
        crate::openai_tools::OpenAiTool::Freeform(freeform) => freeform.name == name,
    });

    if !tool_allowed {
        warn!("Tool '{}' is not allowed for main agent", name);
        return ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: format!(
                    "Tool '{}' is not allowed for main agent. Main agent can only use: list_contexts, multi_retrieve_contexts, create_subagent_task",
                    name
                ),
                success: Some(false),
            },
        };
    }

    match name.as_str() {
        "container.exec" | "shell" => {
            let params = match parse_container_exec_arguments(arguments, turn_context, &call_id) {
                Ok(params) => params,
                Err(output) => {
                    return *output;
                }
            };
            handle_container_exec_with_params(
                params,
                sess,
                turn_context,
                turn_diff_tracker,
                sub_id,
                call_id,
            )
            .await
        }
        "unified_exec" => {
            #[derive(Deserialize)]
            struct UnifiedExecArgs {
                input: Vec<String>,
                #[serde(default)]
                session_id: Option<String>,
                #[serde(default)]
                timeout_ms: Option<u64>,
            }

            let args = match serde_json::from_str::<UnifiedExecArgs>(&arguments) {
                Ok(args) => args,
                Err(err) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("failed to parse function arguments: {err}"),
                            success: Some(false),
                        },
                    };
                }
            };

            handle_unified_exec_tool_call(
                sess,
                call_id,
                args.session_id,
                args.input,
                args.timeout_ms,
            )
            .await
        }
        "view_image" => {
            #[derive(serde::Deserialize)]
            struct SeeImageArgs {
                path: String,
            }
            let args = match serde_json::from_str::<SeeImageArgs>(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("failed to parse function arguments: {e}"),
                            success: Some(false),
                        },
                    };
                }
            };
            let abs = turn_context.resolve_path(Some(args.path));
            let output = match sess.inject_input(vec![InputItem::LocalImage { path: abs }]) {
                Ok(()) => FunctionCallOutputPayload {
                    content: "attached local image path".to_string(),
                    success: Some(true),
                },
                Err(_) => FunctionCallOutputPayload {
                    content: "unable to attach image (no active task)".to_string(),
                    success: Some(false),
                },
            };
            ResponseInputItem::FunctionCallOutput { call_id, output }
        }
        "apply_patch" => {
            let args = match serde_json::from_str::<ApplyPatchToolArgs>(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("failed to parse function arguments: {e}"),
                            success: None,
                        },
                    };
                }
            };
            let exec_params = ExecParams {
                command: vec!["apply_patch".to_string(), args.input.clone()],
                cwd: turn_context.cwd.clone(),
                timeout_ms: None,
                env: HashMap::new(),
                with_escalated_permissions: None,
                justification: None,
            };
            handle_container_exec_with_params(
                exec_params,
                sess,
                turn_context,
                turn_diff_tracker,
                sub_id,
                call_id,
            )
            .await
        }
        "update_plan" => handle_update_plan(sess, arguments, sub_id, call_id).await,
        "create_subagent_task" => {
            info!("🎯 [CODEX] Handling create_subagent_task call");
            // Use unified function executor for subagent creation
            use crate::unified_function_executor::CodexFunctionExecutor;
            use crate::unified_function_handler::AgentType;
            use crate::unified_function_handler::FunctionPermissions;
            use crate::unified_function_handler::UniversalFunctionCallContext;
            use crate::unified_function_handler::UniversalFunctionExecutor;

            // Get multi-agent components
            let multi_agent_components = match &sess.multi_agent_components {
                Some(components) => components,
                None => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: "Multi-agent functionality is not enabled in this session"
                                .to_string(),
                            success: Some(false),
                        },
                    };
                }
            };

            // Create unified function executor
            let executor = CodexFunctionExecutor::new(
                multi_agent_components.context_repo.clone(),
                Some(Arc::new(sess.mcp_connection_manager.clone())),
                Some(multi_agent_components.subagent_manager.clone()),
                Some(multi_agent_components.subagent_completion_tracker.clone()),
                turn_context.cwd.clone(),
            );

            // Create execution context
            let context = UniversalFunctionCallContext {
                cwd: turn_context.cwd.clone(),
                sub_id: sub_id.clone(),
                call_id: call_id.clone(),
                agent_type: AgentType::Main,
                permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
            };

            // Execute through unified path
            let output = executor
                .execute_create_subagent_task(arguments, &context)
                .await;
            ResponseInputItem::FunctionCallOutput { call_id, output }
        }
        "resume_subagent" => {
            info!("🔄 [CODEX] Handling resume_subagent call");
            use crate::unified_function_executor::CodexFunctionExecutor;
            use crate::unified_function_handler::AgentType;
            use crate::unified_function_handler::FunctionPermissions;
            use crate::unified_function_handler::UniversalFunctionCallContext;
            use crate::unified_function_handler::UniversalFunctionExecutor;

            let multi_agent_components = match &sess.multi_agent_components {
                Some(components) => components,
                None => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: "Multi-agent functionality is not enabled in this session".to_string(),
                            success: Some(false),
                        },
                    };
                }
            };

            let executor = CodexFunctionExecutor::new(
                multi_agent_components.context_repo.clone(),
                Some(Arc::new(sess.mcp_connection_manager.clone())),
                Some(multi_agent_components.subagent_manager.clone()),
                Some(multi_agent_components.subagent_completion_tracker.clone()),
                turn_context.cwd.clone(),
            );

            let context = UniversalFunctionCallContext {
                cwd: turn_context.cwd.clone(),
                sub_id: sub_id.clone(),
                call_id: call_id.clone(),
                agent_type: AgentType::Main,
                permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
            };

            let output = executor.execute_resume_subagent(arguments, &context).await;
            ResponseInputItem::FunctionCallOutput { call_id, output }
        }
        "list_recently_completed_subagents" => {
            #[derive(Deserialize)]
            struct ListArgs { #[serde(default)] limit: Option<usize> }
            let args = match serde_json::from_str::<ListArgs>(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload { content: format!("failed to parse function arguments: {e}"), success: Some(false) },
                    };
                }
            };
            let limit = args.limit.unwrap_or(10);
            let result = if let Some(components) = &sess.multi_agent_components {
                match components.subagent_manager.get_recently_completed_tasks(limit).await {
                    Ok(tasks) => {
                        let lines: Vec<String> = tasks.iter().map(|t| {
                            let status_str = match &t.status {
                                crate::subagent_manager::TaskStatus::Completed { .. } => "Completed",
                                crate::subagent_manager::TaskStatus::Failed { .. } => "Failed",
                                crate::subagent_manager::TaskStatus::Cancelled => "Cancelled",
                                crate::subagent_manager::TaskStatus::Created => "Created",
                                crate::subagent_manager::TaskStatus::Running { .. } => "Running",
                            };
                            {
                                let agent_type_str = match &t.agent_type {
                                    codex_protocol::protocol::SubagentType::Explorer => "Explorer",
                                    codex_protocol::protocol::SubagentType::Coder => "Coder",
                                };
                                let created_at_sec = t
                                    .created_at
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs())
                                    .unwrap_or(0);
                                format!(
                                    "- {} [{} | {} | created_at: {}] — {}",
                                    t.task_id, status_str, agent_type_str, created_at_sec, t.title
                                )
                            }
                        }).collect();
                        FunctionCallOutputPayload { content: format!("Recently finished subagents:\n{}", lines.join("\n")), success: Some(true) }
                    }
                    Err(e) => FunctionCallOutputPayload { content: format!("Failed to list subagents: {}", e), success: Some(false) },
                }
            } else { FunctionCallOutputPayload { content: "Multi-agent functionality is not enabled in this session".to_string(), success: Some(false) } };
            ResponseInputItem::FunctionCallOutput { call_id, output: result }
        }
        "multi_get_subagent_report" => {
            #[derive(Deserialize)]
            struct ReportArgs { task_ids: Vec<String> }
            let args = match serde_json::from_str::<ReportArgs>(&arguments) {
                Ok(a) => a,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload { content: format!("failed to parse function arguments: {e}"), success: Some(false) },
                    };
                }
            };
            let result = if let Some(components) = &sess.multi_agent_components {
                let mut summaries = Vec::new();
                for tid in args.task_ids.iter() {
                    match components.subagent_manager.get_task_report(tid).await {
                        Ok(Some(report)) => {
                            let mut parts = Vec::new();
                            parts.push(format!("Task ID: {}", report.task_id));
                            parts.push(format!("Success: {}", report.success));
                            parts.push(format!("Comments: {}", report.comments));
                            parts.push(format!("Num Turns: {} / {}", report.metadata.num_turns, report.metadata.max_turns));
                            if !report.contexts.is_empty() {
                                let ids: Vec<&str> = report.contexts.iter().map(|c| c.id.as_str()).collect();
                                parts.push(format!("Contexts: {}", ids.join(", ")));
                            }
                            if let Some(traj) = &report.trajectory {
                                parts.push(format!("Trajectory entries: {}", traj.len()));
                            }
                            summaries.push(parts.join("\n"));
                        }
                        Ok(None) => summaries.push(format!("Task ID: {}\nNo report found", tid)),
                        Err(e) => summaries.push(format!("Task ID: {}\nFailed to retrieve report: {}", tid, e)),
                    }
                }
                FunctionCallOutputPayload { content: summaries.join("\n\n"), success: Some(true) }
            } else { FunctionCallOutputPayload { content: "Multi-agent functionality is not enabled in this session".to_string(), success: Some(false) } };
            ResponseInputItem::FunctionCallOutput { call_id, output: result }
        }
        "list_contexts" => handle_list_contexts(sess, call_id).await,
        "multi_retrieve_contexts" => handle_multi_retrieve_contexts(sess, arguments, call_id).await,
        EXEC_COMMAND_TOOL_NAME => {
            // TODO(mbolin): Sandbox check.
            let exec_params = match serde_json::from_str::<ExecCommandParams>(&arguments) {
                Ok(params) => params,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("failed to parse function arguments: {e}"),
                            success: Some(false),
                        },
                    };
                }
            };
            let result = sess
                .session_manager
                .handle_exec_command_request(exec_params)
                .await;
            let function_call_output = crate::exec_command::result_into_payload(result);
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: function_call_output,
            }
        }
        WRITE_STDIN_TOOL_NAME => {
            let write_stdin_params = match serde_json::from_str::<WriteStdinParams>(&arguments) {
                Ok(params) => params,
                Err(e) => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("failed to parse function arguments: {e}"),
                            success: Some(false),
                        },
                    };
                }
            };
            let result = sess
                .session_manager
                .handle_write_stdin_request(write_stdin_params)
                .await;
            let function_call_output: FunctionCallOutputPayload =
                crate::exec_command::result_into_payload(result);
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: function_call_output,
            }
        }
        _ => {
            match sess.mcp_connection_manager.parse_tool_name(&name) {
                Some((server, tool_name)) => {
                    // TODO(mbolin): Determine appropriate timeout for tool call.
                    let timeout = None;
                    handle_mcp_tool_call(
                        sess, &sub_id, call_id, server, tool_name, arguments, timeout,
                    )
                    .await
                }
                None => {
                    // Get list of available tools for better error message
                    let available_tools =
                        match GLOBAL_TOOL_REGISTRY.get_tools_for_agent(&AgentType::Main) {
                            Ok(tools) => tools,
                            Err(e) => {
                                tracing::error!("Failed to get tools from unified registry: {}", e);
                                vec![]
                            }
                        };
                    let tool_names: Vec<String> = available_tools
                        .iter()
                        .map(|tool| match tool {
                            crate::openai_tools::OpenAiTool::Function(func) => func.name.clone(),
                            crate::openai_tools::OpenAiTool::LocalShell {} => {
                                "local_shell".to_string()
                            }
                            crate::openai_tools::OpenAiTool::WebSearch {} => {
                                "web_search".to_string()
                            }
                            crate::openai_tools::OpenAiTool::Freeform(freeform) => {
                                freeform.name.clone()
                            }
                        })
                        .collect();

                    let error_message = format!(
                        "❌ Tool '{}' does not exist.\n\n📋 **Available tools:**\n{}\n\n💡 **Please use one of the available tools listed above.**",
                        name,
                        tool_names
                            .iter()
                            .map(|tool_name| format!("  • {}", tool_name))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );

                    tracing::warn!(
                        "Unknown tool call: {} - Available tools: {:?}",
                        name,
                        tool_names
                    );

                    // Unknown function: reply with structured failure so the model can adapt.
                    ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: error_message,
                            success: Some(false),
                        },
                    }
                }
            }
        }
    }
}

async fn handle_custom_tool_call(
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    name: String,
    input: String,
    call_id: String,
) -> ResponseInputItem {
    info!("CustomToolCall: {name} {input}");
    match name.as_str() {
        "apply_patch" => {
            let exec_params = ExecParams {
                command: vec!["apply_patch".to_string(), input.clone()],
                cwd: turn_context.cwd.clone(),
                timeout_ms: None,
                env: HashMap::new(),
                with_escalated_permissions: None,
                justification: None,
            };
            let resp = handle_container_exec_with_params(
                exec_params,
                sess,
                turn_context,
                turn_diff_tracker,
                sub_id,
                call_id,
            )
            .await;

            // Convert function-call style output into a custom tool call output
            match resp {
                ResponseInputItem::FunctionCallOutput { call_id, output } => {
                    ResponseInputItem::CustomToolCallOutput {
                        call_id,
                        output: output.content,
                    }
                }
                // Pass through if already a custom tool output or other variant
                other => other,
            }
        }
        _ => {
            debug!("unexpected CustomToolCall from stream");
            ResponseInputItem::CustomToolCallOutput {
                call_id,
                output: format!("unsupported custom tool call: {name}"),
            }
        }
    }
}

fn to_exec_params(params: ShellToolCallParams, turn_context: &TurnContext) -> ExecParams {
    // Check if the command contains shell operators that need to be handled by a shell
    let command = if contains_shell_operators(&params.command) {
        // Convert the command array to a shell command string and run it with bash -c
        // Don't use shlex::try_join as it quotes shell operators, use simple join instead
        let command_string = params.command.join(" ");
        vec!["bash".to_string(), "-c".to_string(), command_string]
    } else {
        params.command
    };

    ExecParams {
        command,
        cwd: turn_context.resolve_path(params.workdir.clone()),
        timeout_ms: params.timeout_ms,
        env: create_env(&turn_context.shell_environment_policy),
        with_escalated_permissions: params.with_escalated_permissions,
        justification: params.justification,
    }
}

/// Check if a command array contains shell operators that require shell interpretation
pub(crate) fn contains_shell_operators(command: &[String]) -> bool {
    command.iter().any(|arg| {
        matches!(
            arg.as_str(),
            "|" | "&&" | "||" | ";" | ">" | ">>" | "<" | "&"
        )
    })
}

fn parse_container_exec_arguments(
    arguments: String,
    turn_context: &TurnContext,
    call_id: &str,
) -> Result<ExecParams, Box<ResponseInputItem>> {
    // parse command
    match serde_json::from_str::<ShellToolCallParams>(&arguments) {
        Ok(shell_tool_call_params) => Ok(to_exec_params(shell_tool_call_params, turn_context)),
        Err(e) => {
            // allow model to re-sample
            let output = ResponseInputItem::FunctionCallOutput {
                call_id: call_id.to_string(),
                output: FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: None,
                },
            };
            Err(Box::new(output))
        }
    }
}

pub struct ExecInvokeArgs<'a> {
    pub params: ExecParams,
    pub sandbox_type: SandboxType,
    pub sandbox_policy: &'a SandboxPolicy,
    pub sandbox_cwd: &'a Path,
    pub codex_linux_sandbox_exe: &'a Option<PathBuf>,
    pub stdout_stream: Option<StdoutStream>,
}

fn maybe_translate_shell_command(
    params: ExecParams,
    sess: &Session,
    turn_context: &TurnContext,
) -> ExecParams {
    let should_translate = matches!(sess.user_shell, crate::shell::Shell::PowerShell(_))
        || turn_context.shell_environment_policy.use_profile;

    if should_translate
        && let Some(command) = sess
            .user_shell
            .format_default_shell_invocation(params.command.clone())
    {
        return ExecParams { command, ..params };
    }
    params
}

async fn handle_container_exec_with_params(
    params: ExecParams,
    sess: &Session,
    turn_context: &TurnContext,
    turn_diff_tracker: &mut TurnDiffTracker,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    // check if this was a patch, and apply it if so
    let apply_patch_exec = match maybe_parse_apply_patch_verified(&params.command, &params.cwd) {
        MaybeApplyPatchVerified::Body(changes) => {
            match apply_patch::apply_patch(sess, turn_context, &sub_id, &call_id, changes).await {
                InternalApplyPatchInvocation::Output(item) => return item,
                InternalApplyPatchInvocation::DelegateToExec(apply_patch_exec) => {
                    Some(apply_patch_exec)
                }
            }
        }
        MaybeApplyPatchVerified::CorrectnessError(parse_error) => {
            // It looks like an invocation of `apply_patch`, but we
            // could not resolve it into a patch that would apply
            // cleanly. Return to model for resample.
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("error: {parse_error:#}"),
                    success: None,
                },
            };
        }
        MaybeApplyPatchVerified::ShellParseError(error) => {
            trace!("Failed to parse shell command, {error:?}");
            None
        }
        MaybeApplyPatchVerified::NotApplyPatch => None,
    };

    let (params, safety, command_for_display) = match &apply_patch_exec {
        Some(ApplyPatchExec {
            action: ApplyPatchAction { patch, cwd, .. },
            user_explicitly_approved_this_action,
        }) => {
            let path_to_codex = std::env::current_exe()
                .ok()
                .map(|p| p.to_string_lossy().to_string());
            let Some(path_to_codex) = path_to_codex else {
                return ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content: "failed to determine path to codex executable".to_string(),
                        success: None,
                    },
                };
            };

            let params = ExecParams {
                command: vec![
                    path_to_codex,
                    CODEX_APPLY_PATCH_ARG1.to_string(),
                    patch.clone(),
                ],
                cwd: cwd.clone(),
                timeout_ms: params.timeout_ms,
                env: HashMap::new(),
                with_escalated_permissions: params.with_escalated_permissions,
                justification: params.justification.clone(),
            };
            let safety = if *user_explicitly_approved_this_action {
                SafetyCheck::AutoApprove {
                    sandbox_type: SandboxType::None,
                }
            } else {
                assess_safety_for_untrusted_command(
                    turn_context.approval_policy,
                    &turn_context.sandbox_policy,
                    params.with_escalated_permissions.unwrap_or(false),
                )
            };
            (
                params,
                safety,
                vec!["apply_patch".to_string(), patch.clone()],
            )
        }
        None => {
            let safety = {
                let state = sess.state.lock_unchecked();
                assess_command_safety(
                    &params.command,
                    turn_context.approval_policy,
                    &turn_context.sandbox_policy,
                    &state.approved_commands,
                    params.with_escalated_permissions.unwrap_or(false),
                )
            };
            let command_for_display = params.command.clone();
            (params, safety, command_for_display)
        }
    };

    let sandbox_type = match safety {
        SafetyCheck::AutoApprove { sandbox_type } => sandbox_type,
        SafetyCheck::AskUser => {
            let rx_approve = sess
                .request_command_approval(
                    sub_id.clone(),
                    call_id.clone(),
                    params.command.clone(),
                    params.cwd.clone(),
                    params.justification.clone(),
                )
                .await;
            match rx_approve.await.unwrap_or_default() {
                ReviewDecision::Approved => (),
                ReviewDecision::ApprovedForSession => {
                    sess.add_approved_command(params.command.clone());
                }
                ReviewDecision::Denied | ReviewDecision::Abort => {
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: "exec command rejected by user".to_string(),
                            success: None,
                        },
                    };
                }
            }
            // No sandboxing is applied because the user has given
            // explicit approval. Often, we end up in this case because
            // the command cannot be run in a sandbox, such as
            // installing a new dependency that requires network access.
            SandboxType::None
        }
        SafetyCheck::Reject { reason } => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("exec command rejected: {reason}"),
                    success: None,
                },
            };
        }
    };

    let exec_command_context = ExecCommandContext {
        sub_id: sub_id.clone(),
        call_id: call_id.clone(),
        command_for_display: command_for_display.clone(),
        cwd: params.cwd.clone(),
        apply_patch: apply_patch_exec.map(
            |ApplyPatchExec {
                 action,
                 user_explicitly_approved_this_action,
             }| ApplyPatchCommandContext {
                user_explicitly_approved_this_action,
                changes: convert_apply_patch_to_protocol(&action),
            },
        ),
    };

    let params = maybe_translate_shell_command(params, sess, turn_context);
    let output_result = sess
        .run_exec_with_events(
            turn_diff_tracker,
            exec_command_context.clone(),
            ExecInvokeArgs {
                params: params.clone(),
                sandbox_type,
                sandbox_policy: &turn_context.sandbox_policy,
                sandbox_cwd: &turn_context.cwd,
                codex_linux_sandbox_exe: &sess.codex_linux_sandbox_exe,
                stdout_stream: if exec_command_context.apply_patch.is_some() {
                    None
                } else {
                    Some(StdoutStream {
                        sub_id: sub_id.clone(),
                        call_id: call_id.clone(),
                        tx_event: sess.tx_event.clone(),
                    })
                },
            },
        )
        .await;

    match output_result {
        Ok(output) => {
            let ExecToolCallOutput { exit_code, .. } = &output;

            let is_success = *exit_code == 0;
            let content = format_exec_output(&output);
            ResponseInputItem::FunctionCallOutput {
                call_id: call_id.clone(),
                output: FunctionCallOutputPayload {
                    content,
                    success: Some(is_success),
                },
            }
        }
        Err(CodexErr::Sandbox(error)) => {
            handle_sandbox_error(
                turn_diff_tracker,
                params,
                exec_command_context,
                error,
                sandbox_type,
                sess,
                turn_context,
            )
            .await
        }
        Err(e) => ResponseInputItem::FunctionCallOutput {
            call_id: call_id.clone(),
            output: FunctionCallOutputPayload {
                content: format!("execution error: {e}"),
                success: None,
            },
        },
    }
}

async fn handle_sandbox_error(
    turn_diff_tracker: &mut TurnDiffTracker,
    params: ExecParams,
    exec_command_context: ExecCommandContext,
    error: SandboxErr,
    sandbox_type: SandboxType,
    sess: &Session,
    turn_context: &TurnContext,
) -> ResponseInputItem {
    let call_id = exec_command_context.call_id.clone();
    let sub_id = exec_command_context.sub_id.clone();
    let cwd = exec_command_context.cwd.clone();

    // Early out if either the user never wants to be asked for approval, or
    // we're letting the model manage escalation requests. Otherwise, continue
    match turn_context.approval_policy {
        AskForApproval::Never | AskForApproval::OnRequest => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!(
                        "failed in sandbox {sandbox_type:?} with execution error: {error}"
                    ),
                    success: Some(false),
                },
            };
        }
        AskForApproval::UnlessTrusted | AskForApproval::OnFailure => (),
    }

    // similarly, if the command timed out, we can simply return this failure to the model
    if matches!(error, SandboxErr::Timeout { .. }) {
        return ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: format!(
                    "command timed out after {} milliseconds",
                    params.timeout_duration().as_millis()
                ),
                success: Some(false),
            },
        };
    }

    // Note that when `error` is `SandboxErr::Denied`, it could be a false
    // positive. That is, it may have exited with a non-zero exit code, not
    // because the sandbox denied it, but because that is its expected behavior,
    // i.e., a grep command that did not match anything. Ideally we would
    // include additional metadata on the command to indicate whether non-zero
    // exit codes merit a retry.

    // For now, we categorically ask the user to retry without sandbox and
    // emit the raw error as a background event.
    sess.notify_background_event(&sub_id, format!("Execution failed: {error}"))
        .await;

    let rx_approve = sess
        .request_command_approval(
            sub_id.clone(),
            call_id.clone(),
            params.command.clone(),
            cwd.clone(),
            Some("command failed; retry without sandbox?".to_string()),
        )
        .await;

    match rx_approve.await.unwrap_or_default() {
        ReviewDecision::Approved | ReviewDecision::ApprovedForSession => {
            // Persist this command as pre‑approved for the
            // remainder of the session so future
            // executions skip the sandbox directly.
            // TODO(ragona): Isn't this a bug? It always saves the command in an | fork?
            sess.add_approved_command(params.command.clone());
            // Inform UI we are retrying without sandbox.
            sess.notify_background_event(&sub_id, "retrying command without sandbox")
                .await;

            // This is an escalated retry; the policy will not be
            // examined and the sandbox has been set to `None`.
            let retry_output_result = sess
                .run_exec_with_events(
                    turn_diff_tracker,
                    exec_command_context.clone(),
                    ExecInvokeArgs {
                        params,
                        sandbox_type: SandboxType::None,
                        sandbox_policy: &turn_context.sandbox_policy,
                        sandbox_cwd: &turn_context.cwd,
                        codex_linux_sandbox_exe: &sess.codex_linux_sandbox_exe,
                        stdout_stream: if exec_command_context.apply_patch.is_some() {
                            None
                        } else {
                            Some(StdoutStream {
                                sub_id: sub_id.clone(),
                                call_id: call_id.clone(),
                                tx_event: sess.tx_event.clone(),
                            })
                        },
                    },
                )
                .await;

            match retry_output_result {
                Ok(retry_output) => {
                    let ExecToolCallOutput { exit_code, .. } = &retry_output;

                    let is_success = *exit_code == 0;
                    let content = format_exec_output(&retry_output);

                    ResponseInputItem::FunctionCallOutput {
                        call_id: call_id.clone(),
                        output: FunctionCallOutputPayload {
                            content,
                            success: Some(is_success),
                        },
                    }
                }
                Err(e) => ResponseInputItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    output: FunctionCallOutputPayload {
                        content: format!("retry failed: {e}"),
                        success: None,
                    },
                },
            }
        }
        ReviewDecision::Denied | ReviewDecision::Abort => {
            // Fall through to original failure handling.
            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: "exec command rejected by user".to_string(),
                    success: None,
                },
            }
        }
    }
}

fn get_last_assistant_message_from_turn(responses: &[ResponseItem]) -> Option<String> {
    responses.iter().rev().find_map(|item| {
        if let ResponseItem::Message { role, content, .. } = item {
            if role == "assistant" {
                content.iter().rev().find_map(|ci| {
                    if let ContentItem::OutputText { text } = ci {
                        Some(text.clone())
                    } else {
                        None
                    }
                })
            } else {
                None
            }
        } else {
            None
        }
    })
}

fn convert_call_tool_result_to_function_call_output_payload(
    call_tool_result: &CallToolResult,
) -> FunctionCallOutputPayload {
    let CallToolResult {
        content,
        is_error,
        structured_content,
    } = call_tool_result;

    // In terms of what to send back to the model, we prefer structured_content,
    // if available, and fallback to content, otherwise.
    let mut is_success = is_error != &Some(true);
    let content = if let Some(structured_content) = structured_content
        && structured_content != &serde_json::Value::Null
        && let Ok(serialized_structured_content) = serde_json::to_string(&structured_content)
    {
        serialized_structured_content
    } else {
        match serde_json::to_string(&content) {
            Ok(serialized_content) => serialized_content,
            Err(err) => {
                // If we could not serialize either content or structured_content to
                // JSON, flag this as an error.
                is_success = false;
                err.to_string()
            }
        }
    };

    FunctionCallOutputPayload {
        content,
        success: Some(is_success),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::MODEL_FORMAT_HEAD_LINES;
    use crate::events::MODEL_FORMAT_MAX_BYTES;
    use crate::events::MODEL_FORMAT_MAX_LINES;
    use crate::events::MODEL_FORMAT_TAIL_LINES;
    use mcp_types::ContentBlock;
    use mcp_types::TextContent;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use std::time::Duration as StdDuration;

    fn text_block(s: &str) -> ContentBlock {
        ContentBlock::TextContent(TextContent {
            annotations: None,
            text: s.to_string(),
            r#type: "text".to_string(),
        })
    }

    #[test]
    fn prefers_structured_content_when_present() {
        let ctr = CallToolResult {
            // Content present but should be ignored because structured_content is set.
            content: vec![text_block("ignored")],
            is_error: None,
            structured_content: Some(json!({
                "ok": true,
                "value": 42
            })),
        };

        let got = convert_call_tool_result_to_function_call_output_payload(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&json!({
                "ok": true,
                "value": 42
            }))
            .unwrap(),
            success: Some(true),
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn model_truncation_head_tail_by_lines() {
        // Build 400 short lines so line-count limit, not byte budget, triggers truncation
        let lines: Vec<String> = (1..=400).map(|i| format!("line{i}")).collect();
        let full = lines.join("\n");

        let exec = ExecToolCallOutput {
            exit_code: 0,
            stdout: StreamOutput::new(String::new()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(full.clone()),
            duration: StdDuration::from_secs(1),
            timed_out: false,
        };

        let out = format_exec_output_str(&exec);

        // Expect elision marker with correct counts
        let omitted = 400 - MODEL_FORMAT_MAX_LINES; // 144
        let marker = format!("\n[... omitted {omitted} of 400 lines ...]\n\n");
        assert!(out.contains(&marker), "missing marker: {out}");

        // Validate head and tail
        let parts: Vec<&str> = out.split(&marker).collect();
        assert_eq!(parts.len(), 2, "expected one marker split");
        let head = parts[0];
        let tail = parts[1];

        let expected_head: String = (1..=MODEL_FORMAT_HEAD_LINES)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(head.starts_with(&expected_head), "head mismatch");

        let expected_tail: String = ((400 - MODEL_FORMAT_TAIL_LINES + 1)..=400)
            .map(|i| format!("line{i}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(tail.ends_with(&expected_tail), "tail mismatch");
    }

    #[test]
    fn model_truncation_respects_byte_budget() {
        // Construct a large output (about 100kB) so byte budget dominates
        let big_line = "x".repeat(100);
        let full = std::iter::repeat_n(big_line.clone(), 1000)
            .collect::<Vec<_>>()
            .join("\n");

        let exec = ExecToolCallOutput {
            exit_code: 0,
            stdout: StreamOutput::new(String::new()),
            stderr: StreamOutput::new(String::new()),
            aggregated_output: StreamOutput::new(full.clone()),
            duration: StdDuration::from_secs(1),
            timed_out: false,
        };

        let out = format_exec_output_str(&exec);
        assert!(out.len() <= MODEL_FORMAT_MAX_BYTES, "exceeds byte budget");
        assert!(out.contains("omitted"), "should contain elision marker");

        // Ensure head and tail are drawn from the original
        assert!(full.starts_with(out.chars().take(8).collect::<String>().as_str()));
        assert!(
            full.ends_with(
                out.chars()
                    .rev()
                    .take(8)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
                    .as_str()
            )
        );
    }

    #[test]
    fn falls_back_to_content_when_structured_is_null() {
        let ctr = CallToolResult {
            content: vec![text_block("hello"), text_block("world")],
            is_error: None,
            structured_content: Some(serde_json::Value::Null),
        };

        let got = convert_call_tool_result_to_function_call_output_payload(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&vec![text_block("hello"), text_block("world")])
                .unwrap(),
            success: Some(true),
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn success_flag_reflects_is_error_true() {
        let ctr = CallToolResult {
            content: vec![text_block("unused")],
            is_error: Some(true),
            structured_content: Some(json!({ "message": "bad" })),
        };

        let got = convert_call_tool_result_to_function_call_output_payload(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&json!({ "message": "bad" })).unwrap(),
            success: Some(false),
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn success_flag_true_with_no_error_and_content_used() {
        let ctr = CallToolResult {
            content: vec![text_block("alpha")],
            is_error: Some(false),
            structured_content: None,
        };

        let got = convert_call_tool_result_to_function_call_output_payload(&ctr);
        let expected = FunctionCallOutputPayload {
            content: serde_json::to_string(&vec![text_block("alpha")]).unwrap(),
            success: Some(true),
        };

        assert_eq!(expected, got);
    }

    #[test]
    fn test_contains_shell_operators() {
        // Test pipe operator
        assert!(contains_shell_operators(&[
            "grep".to_string(),
            "-r".to_string(),
            "test".to_string(),
            "|".to_string(),
            "head".to_string()
        ]));

        // Test logical operators
        assert!(contains_shell_operators(&[
            "ls".to_string(),
            "&&".to_string(),
            "echo".to_string(),
            "done".to_string()
        ]));
        assert!(contains_shell_operators(&[
            "ls".to_string(),
            "||".to_string(),
            "echo".to_string(),
            "failed".to_string()
        ]));

        // Test semicolon
        assert!(contains_shell_operators(&[
            "ls".to_string(),
            ";".to_string(),
            "pwd".to_string()
        ]));

        // Test redirection
        assert!(contains_shell_operators(&[
            "echo".to_string(),
            "test".to_string(),
            ">".to_string(),
            "file.txt".to_string()
        ]));
        assert!(contains_shell_operators(&[
            "echo".to_string(),
            "test".to_string(),
            ">>".to_string(),
            "file.txt".to_string()
        ]));

        // Test no operators
        assert!(!contains_shell_operators(&[
            "ls".to_string(),
            "-la".to_string()
        ]));
        assert!(!contains_shell_operators(&[
            "grep".to_string(),
            "-r".to_string(),
            "pattern".to_string(),
            "file.txt".to_string()
        ]));
    }

    #[test]
    fn test_shlex_join_with_special_characters() {
        // Test that shlex::try_join properly quotes arguments with special characters
        let command_with_hash = vec![
            "echo",
            "# Message Queue Analysis Report",
            "|",
            "tee",
            "report.md",
        ];
        let joined = shlex::try_join(command_with_hash.iter().copied()).unwrap();

        // The # should be quoted to prevent it from being treated as a comment
        assert!(
            joined.contains("'# Message Queue Analysis Report'")
                || joined.contains("\"# Message Queue Analysis Report\"")
        );

        // Test that the pipe is preserved (it might be quoted as '|')
        assert!(joined.contains("|"));
    }

    /// Test helper function to create a mock Session and TurnContext for testing
    pub fn make_session_and_context() -> (Arc<Session>, TurnContext) {
        // This is a simplified mock implementation for testing only
        // We'll create minimal structures that satisfy the interface
        panic!("make_session_and_context is not implemented - this is a placeholder for tests")
    }
}

/// Remove the last request from conversation history to avoid retrying problematic requests
/// that caused 400 Bad Request errors. This function removes the most recent user message
/// and any subsequent assistant responses/tool calls that were part of the same turn.
fn remove_last_request_from_history(mut history: Vec<ResponseItem>) -> Vec<ResponseItem> {
    if history.is_empty() {
        return history;
    }

    // For 400 errors, we need to be more aggressive about removing recent content
    // that could be causing the context length issue. Look for the last few items
    // that could contain large content (function calls, outputs, etc.)

    let original_len = history.len();
    let mut items_to_remove = 0;

    // Remove recent items that could be large, working backwards
    for (i, item) in history.iter().enumerate().rev() {
        items_to_remove += 1;

        match item {
            // Stop when we find a user message (but include it in removal)
            ResponseItem::Message { role, .. } if role == "user" => {
                break;
            }
            // Always remove function calls and their outputs as they can be large
            ResponseItem::FunctionCall { .. }
            | ResponseItem::FunctionCallOutput { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CustomToolCallOutput { .. } => {
                // Continue removing more items
            }
            // Remove assistant messages that might be responses to large outputs
            ResponseItem::Message { role, .. } if role == "assistant" => {
                // Continue removing more items
            }
            _ => {
                // For other items, continue but don't go too far back
                if items_to_remove > 10 {
                    break;
                }
            }
        }

        // Safety limit: don't remove more than half the history
        if items_to_remove >= original_len / 2 {
            break;
        }
    }

    // Ensure we remove at least a few items to make a difference
    items_to_remove = std::cmp::max(items_to_remove, 3);
    items_to_remove = std::cmp::min(items_to_remove, original_len);

    let new_len = original_len - items_to_remove;
    history.truncate(new_len);

    info!(
        "Removed {} items from conversation history to resolve 400 Bad Request (kept {} items)",
        items_to_remove, new_len
    );

    history
}

/// Create a summary of large content to replace it in conversation history
/// This helps avoid 400 Bad Request errors due to context length limits
fn create_content_summary(content: &str) -> String {
    let char_count = content.chars().count();
    let line_count = content.lines().count();

    // Get first 5 lines or 200 characters, whichever is shorter
    let preview = if line_count <= 5 {
        // If 5 lines or fewer, show all lines but limit to 200 chars
        let all_lines = content.lines().collect::<Vec<_>>().join("\n");
        if all_lines.len() <= 200 {
            all_lines
        } else {
            format!("{}...", &all_lines[..200])
        }
    } else {
        // Show first 5 lines
        let first_5_lines = content.lines().take(5).collect::<Vec<_>>().join("\n");
        if first_5_lines.len() <= 200 {
            first_5_lines
        } else {
            format!("{}...", &first_5_lines[..200])
        }
    };

    format!(
        "[内容摘要: 共{}字符，{}行]\n前{}行内容:\n{}\n[... 其余内容已省略 ...]",
        char_count,
        line_count,
        std::cmp::min(5, line_count),
        preview
    )
}

/// Replace large content in ResponseItems with summaries to reduce context size
pub(crate) fn replace_large_content_with_summaries(
    mut history: Vec<ResponseItem>,
) -> Vec<ResponseItem> {
    const LARGE_CONTENT_THRESHOLD: usize = 1000; // Characters
    let mut replacements_made = 0;

    for (index, item) in history.iter_mut().enumerate() {
        match item {
            ResponseItem::Message { content, .. } => {
                for content_item in content {
                    if let ContentItem::OutputText { text } = content_item {
                        if text.len() > LARGE_CONTENT_THRESHOLD {
                            info!(
                                "Replacing large OutputText content in Message[{}]: {} chars",
                                index,
                                text.len()
                            );
                            *text = create_content_summary(text);
                            replacements_made += 1;
                        }
                    }
                }
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                if output.content.len() > LARGE_CONTENT_THRESHOLD {
                    info!(
                        "Replacing large FunctionCallOutput content for call_id {}: {} chars",
                        call_id,
                        output.content.len()
                    );
                    output.content = create_content_summary(&output.content);
                    replacements_made += 1;
                }
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                if output.len() > LARGE_CONTENT_THRESHOLD {
                    info!(
                        "Replacing large CustomToolCallOutput content for call_id {}: {} chars",
                        call_id,
                        output.len()
                    );
                    *output = create_content_summary(output);
                    replacements_made += 1;
                }
            }
            // For other types, we could add similar logic if needed
            _ => {}
        }
    }

    info!(
        "Replaced large content with summaries in conversation history: {} replacements made",
        replacements_made
    );
    history
}

/// Unified LLM call wrapper that handles BadRequest errors with content summarization
/// This function provides a single point for handling 400 errors across main agent and subagents
/// Returns (ResponseStream, Option<updated_messages>) where updated_messages is Some if content was summarized
pub(crate) async fn call_llm_with_error_handling(
    client: &crate::client::ModelClient,
    mut prompt: crate::client_common::Prompt,
    context: &str,
) -> Result<(crate::client_common::ResponseStream, Option<Vec<ResponseItem>>), CodexErr> {
    // Log conversation history before making LLM request for debugging
    log_conversation_history(&prompt.input, &format!("before LLM request ({})", context));

    loop {
        match client.stream(&prompt).await {
            Ok(stream) => return Ok((stream, None)),
            Err(CodexErr::BadRequest { message }) => {
                info!(
                    "BadRequest error in {}, replacing large content with summaries: {}",
                    context, message
                );

                // Replace large content in prompt with summaries
                prompt.input = replace_large_content_with_summaries(prompt.input);

                info!(
                    "Replaced large content with summaries in {} due to 400 Bad Request error",
                    context
                );

                // Log the updated conversation history after replacement
                log_conversation_history(
                    &prompt.input,
                    &format!("after content replacement ({})", context),
                );

                // Try again with summarized content - if successful, return the updated messages
                match client.stream(&prompt).await {
                    Ok(stream) => return Ok((stream, Some(prompt.input))),
                    Err(e) => return Err(e),
                }
            }
            Err(e) => return Err(e),
        }
    }
}

/// Safe string truncation that respects character boundaries
fn safe_truncate(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        s.to_string()
    } else {
        let mut end = limit;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

/// Log conversation history for debugging (with content truncation)
pub(crate) fn log_conversation_history(history: &[ResponseItem], context: &str) {
    const LOG_CONTENT_LIMIT: usize = 100; // Characters to show in logs
    const MAX_ITEMS_TO_SHOW: usize = 4; // Only show the last 4 items

    info!("=== Conversation History ({}) ===", context);

    // Calculate the starting index to show only the last MAX_ITEMS_TO_SHOW items
    let start_index = if history.len() > MAX_ITEMS_TO_SHOW {
        history.len() - MAX_ITEMS_TO_SHOW
    } else {
        0
    };

    // Show truncation indicator if we're not showing all items
    if start_index > 0 {
        info!("  ... ({} earlier items omitted) ...", start_index);
    }

    for (i, item) in history.iter().enumerate().skip(start_index) {
        match item {
            ResponseItem::Message { role, content, .. } => {
                let text_content = content
                    .iter()
                    .filter_map(|c| match c {
                        ContentItem::OutputText { text } => Some(text.as_str()),
                        ContentItem::InputText { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join(" ");

                let truncated = safe_truncate(&text_content, LOG_CONTENT_LIMIT);

                info!("  [{}] {}: {}", i, role, truncated);
            }
            ResponseItem::FunctionCall { name, call_id, .. } => {
                info!("  [{}] FunctionCall: {} ({})", i, name, call_id);
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                let truncated = safe_truncate(&output.content, LOG_CONTENT_LIMIT);
                info!("  [{}] FunctionOutput ({}): {}", i, call_id, truncated);
            }
            ResponseItem::LocalShellCall { .. } => {
                info!("  [{}] LocalShellCall", i);
            }
            ResponseItem::CustomToolCall { name, call_id, .. } => {
                info!("  [{}] CustomToolCall: {} ({})", i, name, call_id);
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                let truncated = safe_truncate(output, LOG_CONTENT_LIMIT);
                info!("  [{}] CustomToolOutput ({}): {}", i, call_id, truncated);
            }
            ResponseItem::Reasoning { summary, .. } => {
                info!("  [{}] Reasoning: {:?}", i, summary);
            }
            ResponseItem::WebSearchCall { .. } => {
                info!("  [{}] WebSearchCall", i);
            }
            ResponseItem::Other => {
                info!("  [{}] Other", i);
            }
        }
    }
    info!("=== End History ({}) ===", context);
}
