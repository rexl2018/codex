use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{atomic::AtomicU64, Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_channel::Sender;
use codex_mcp_client::McpClient;
use codex_protocol::{
    config_types::{ReasoningEffort as ReasoningEffortConfig, ReasoningSummary as ReasoningSummaryConfig},
    mcp_protocol::ConversationId,
    models::{ResponseInputItem, ResponseItem},
    protocol::{AskForApproval, Event, EventMsg, InitialHistory, ReviewDecision, RolloutItem, SandboxPolicy, SessionConfiguredEvent},
};
use mcp_types::CallToolResult;
use serde_json;
use tokio::sync::Mutex as TokioMutex;
use tracing::{debug, error, info, warn};

use crate::{
    agent_task::AgentTask,
    client::ModelClient,
    config::Config,
    config_types::ShellEnvironmentPolicy,
    context_store::InMemoryContextRepository,
    conversation_history::ConversationHistory,
    environment_context::EnvironmentContext,
    error::{CodexErr, Result as CodexResult},
    event_mapping::map_response_item_to_event_messages,
    exec::ExecToolCallOutput,
    exec_command::ExecSessionManager,
    mcp_connection_manager::McpConnectionManager,
    parse_command::parse_command,
    protocol::{
        BackgroundEventEvent, ErrorEvent, ExecApprovalRequestEvent, ExecCommandBeginEvent,
        ExecCommandEndEvent, InputItem, PatchApplyBeginEvent, PatchApplyEndEvent, StreamErrorEvent,
        TurnAbortReason, TurnDiffEvent,
    },
    rollout::{RolloutRecorder, RolloutRecorderParams},
    shell,
    state::State,
    subagent_manager::{ExecutorType, InMemorySubagentManager},
    tool_config::UnifiedToolConfig,
    turn_context::TurnContext,
    turn_diff_tracker::TurnDiffTracker,
    types::{ApplyPatchCommandContext, ExecCommandContext},
    unified_exec::UnifiedExecSessionManager,
    user_instructions::UserInstructions,
    user_notification::UserNotification,
    AuthManager, ModelProviderInfo,
};

// A convenience extension trait for acquiring mutex locks where poisoning is
// unrecoverable and should abort the program. This avoids scattered `.unwrap()`
// calls on `lock()` while still surfacing a clear panic message when a lock is
// poisoned.
pub(crate) trait MutexExt<T> {
    fn lock_unchecked(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_unchecked(&self) -> MutexGuard<'_, T> {
        #[expect(clippy::expect_used)]
        self.lock().expect("poisoned lock")
    }
}

/// Multi-agent components container
pub(crate) struct MultiAgentComponents {
    pub context_repo: Arc<InMemoryContextRepository>,
    pub subagent_manager: Arc<InMemorySubagentManager>,
    pub subtask_coordinator: Option<crate::multi_agent_coordinator::SubtaskCoordinator>,
    /// Flag to track if a new user task has been created
    pub new_user_task_created: Arc<std::sync::atomic::AtomicBool>,
}

/// Configure the model session.
pub(crate) struct ConfigureSession {
    /// Provider identifier ("openai", "openrouter", ...).
    pub(crate) provider: ModelProviderInfo,

    /// If not specified, server will use its default model.
    pub(crate) model: String,

    pub(crate) model_reasoning_effort: Option<ReasoningEffortConfig>,
    pub(crate) model_reasoning_summary: ReasoningSummaryConfig,

    /// Model instructions that are appended to the base instructions.
    pub(crate) user_instructions: Option<String>,

    /// Base instructions override.
    pub(crate) base_instructions: Option<String>,

    /// When to escalate for approval for execution
    pub(crate) approval_policy: AskForApproval,
    /// How to sandbox commands executed in the system
    pub(crate) sandbox_policy: SandboxPolicy,

    /// Optional external notifier command tokens. Present only when the
    /// client wants the agent to spawn a program after each completed
    /// turn.
    pub(crate) notify: Option<Vec<String>>,

    /// Working directory that should be treated as the *root* of the
    /// session. All relative paths supplied by the model as well as the
    /// execution sandbox are resolved against this directory **instead**
    /// of the process-wide current working directory. CLI front-ends are
    /// expected to expand this to an absolute path before sending the
    /// `ConfigureSession` operation so that the business-logic layer can
    /// operate deterministically.
    pub(crate) cwd: PathBuf,
}

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by user input.
pub struct Session {
    pub(crate) conversation_id: ConversationId,
    pub(crate) tx_event: Sender<Event>,

    /// Manager for external MCP servers/tools.
    pub(crate) mcp_connection_manager: McpConnectionManager,
    pub(crate) session_manager: ExecSessionManager,
    pub(crate) unified_exec_manager: UnifiedExecSessionManager,
    pub(crate) mcp_clients: Arc<TokioMutex<Vec<McpClient>>>,

    /// External notifier command (will be passed as args to exec()). When
    /// `None` this feature is disabled.
    pub(crate) notify: Option<Vec<String>>,

    /// Optional rollout recorder for persisting the conversation transcript so
    /// sessions can be replayed or inspected later.
    pub(crate) rollout: Mutex<Option<RolloutRecorder>>,
    pub(crate) state: Mutex<State>,
    pub(crate) codex_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) user_shell: shell::Shell,
    pub(crate) show_raw_agent_reasoning: bool,
    pub(crate) next_internal_sub_id: AtomicU64,

    /// Multi-agent components (optional)
    pub(crate) multi_agent_components: Option<MultiAgentComponents>,
}

impl Session {
    pub(crate) async fn new(
        configure_session: ConfigureSession,
        config: Arc<Config>,
        auth_manager: Arc<AuthManager>,
        tx_event: Sender<Event>,
        initial_history: InitialHistory,
    ) -> anyhow::Result<(Arc<Self>, TurnContext)> {
        // Clone provider before destructuring for subagent use
        let subagent_provider = configure_session.provider.clone();

        let ConfigureSession {
            provider,
            model,
            model_reasoning_effort,
            model_reasoning_summary,
            user_instructions,
            base_instructions,
            approval_policy,
            sandbox_policy,
            notify,
            cwd,
        } = configure_session;
        debug!("Configuring session: model={model}; provider={provider:?}");
        if !cwd.is_absolute() {
            return Err(anyhow::anyhow!("cwd is not absolute: {cwd:?}"));
        }

        let (conversation_id, rollout_params) = match &initial_history {
            InitialHistory::New | InitialHistory::Forked(_) => {
                let conversation_id = ConversationId::default();
                (
                    conversation_id,
                    RolloutRecorderParams::new(conversation_id, user_instructions.clone()),
                )
            }
            InitialHistory::Resumed(resumed_history) => (
                resumed_history.conversation_id,
                RolloutRecorderParams::resume(resumed_history.rollout_path.clone()),
            ),
        };

        // Error messages to dispatch after SessionConfigured is sent.
        let mut post_session_configured_error_events = Vec::<Event>::new();

        // Kick off independent async setup tasks in parallel to reduce startup latency.
        //
        // - initialize RolloutRecorder with new or resumed session info
        // - spin up MCP connection manager
        // - perform default shell discovery
        // - load history metadata
        let rollout_fut = RolloutRecorder::new(&config, rollout_params);

        let mcp_fut = McpConnectionManager::new(config.mcp_servers.clone());
        let default_shell_fut = shell::default_user_shell();
        let history_meta_fut = crate::message_history::history_metadata(&config);

        // Join all independent futures.
        let (rollout_recorder, mcp_res, default_shell, (history_log_id, history_entry_count)) =
            tokio::join!(rollout_fut, mcp_fut, default_shell_fut, history_meta_fut);

        let rollout_recorder = rollout_recorder.map_err(|e| {
            error!("failed to initialize rollout recorder: {e:#}");
            anyhow::anyhow!("failed to initialize rollout recorder: {e:#}")
        })?;
        let rollout_path = rollout_recorder.rollout_path.clone();
        // Create the mutable state for the Session.
        let state = State {
            history: ConversationHistory::new(),
            ..Default::default()
        };

        // Handle MCP manager result and record any startup failures.
        let (mcp_connection_manager, failed_clients) = match mcp_res {
            Ok((mgr, failures)) => (mgr, failures),
            Err(e) => {
                let message = format!("Failed to create MCP connection manager: {e:#}");
                error!("{message}");
                post_session_configured_error_events.push(Event {
                    id: crate::codex::INITIAL_SUBMIT_ID.to_owned(),
                    msg: EventMsg::Error(ErrorEvent { message }),
                });
                (McpConnectionManager::default(), Default::default())
            }
        };

        // Surface individual client start-up failures to the user.
        if !failed_clients.is_empty() {
            for (server_name, err) in failed_clients {
                let message = format!("MCP client for `{server_name}` failed to start: {err:#}");
                error!("{message}");
                post_session_configured_error_events.push(Event {
                    id: crate::codex::INITIAL_SUBMIT_ID.to_owned(),
                    msg: EventMsg::Error(ErrorEvent { message }),
                });
            }
        }

        // Now that the conversation id is final (may have been updated by resume),
        // construct the model client.
        let client = ModelClient::new(
            config.clone(),
            Some(auth_manager.clone()),
            provider.clone(),
            model_reasoning_effort,
            model_reasoning_summary,
            conversation_id,
        );
        let turn_context = TurnContext {
            client,
            unified_tool_config: UnifiedToolConfig::default(),
            user_instructions,
            base_instructions,
            approval_policy,
            sandbox_policy,
            shell_environment_policy: config.shell_environment_policy.clone(),
            cwd,
        };

        // Create Arc for mcp_connection_manager to be used in both subagent and main session
        let mcp_connection_manager_arc = Arc::new(mcp_connection_manager);

        // Initialize multi-agent components if subagent task tool is enabled
        let multi_agent_components = if config.include_subagent_task_tool {
            let (event_tx, mut event_rx) = tokio::sync::mpsc::unbounded_channel();
            let context_repo = Arc::new(InMemoryContextRepository::new());

            // Create model client for subagent LLM executor
            let model_client = Arc::new(ModelClient::new(
                config.clone(),
                Some(auth_manager.clone()),
                subagent_provider,
                model_reasoning_effort,
                model_reasoning_summary,
                conversation_id,
            ));
            let subagent_manager = Arc::new(InMemorySubagentManager::new(
                context_repo.clone(),
                event_tx,
                ExecutorType::LLM {
                    model_client,
                    mcp_tools: Some(mcp_connection_manager_arc.list_all_tools()),
                    mcp_connection_manager: Some(mcp_connection_manager_arc.clone()),
                }, // Use LLM executor for production with MCP tools
            ));

            // Spawn a task to handle subagent events
            let tx_event_clone = tx_event.clone();
            tokio::spawn(async move {
                while let Some(event) = event_rx.recv().await {
                    if let Err(e) = tx_event_clone.send(event).await {
                        tracing::error!("Failed to send subagent event: {}", e);
                        break;
                    }
                }
            });

            Some(MultiAgentComponents {
                context_repo,
                subagent_manager,
                subtask_coordinator: None,
                new_user_task_created: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            })
        } else {
            None
        };

        let sess = Arc::new(Session {
            conversation_id,
            tx_event: tx_event.clone(),
            mcp_connection_manager: (*mcp_connection_manager_arc).clone(),
            session_manager: ExecSessionManager::default(),
            unified_exec_manager: UnifiedExecSessionManager::default(),
            mcp_clients: Arc::new(TokioMutex::new(Vec::new())),
            notify,
            state: Mutex::new(state),
            rollout: Mutex::new(Some(rollout_recorder)),
            codex_linux_sandbox_exe: config.codex_linux_sandbox_exe.clone(),
            user_shell: default_shell,
            show_raw_agent_reasoning: config.show_raw_agent_reasoning,
            next_internal_sub_id: AtomicU64::new(0),
            multi_agent_components,
        });

        // Dispatch the SessionConfiguredEvent first and then report any errors.
        // If resuming, include converted initial messages in the payload so UIs can render them immediately.
        let initial_messages = initial_history.get_event_msgs();
        sess.record_initial_history(&turn_context, initial_history)
            .await;

        let events = std::iter::once(Event {
            id: crate::codex::INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: conversation_id,
                model,
                reasoning_effort: model_reasoning_effort,
                history_log_id,
                history_entry_count,
                initial_messages,
                rollout_path,
            }),
        })
        .chain(post_session_configured_error_events.into_iter());
        for event in events {
            sess.send_event(event).await;
        }

        Ok((sess, turn_context))
    }

    pub fn set_task(&self, task: AgentTask) {
        let mut state = self.state.lock_unchecked();
        if let Some(current_task) = state.current_task.take() {
            current_task.abort(TurnAbortReason::Replaced);
        }
        state.current_task = Some(task);
    }

    pub fn remove_task(&self, sub_id: &str) {
        let mut state = self.state.lock_unchecked();
        if let Some(task) = &state.current_task
            && task.get_sub_id() == sub_id
        {
            state.current_task.take();
        }
    }

    async fn record_initial_history(
        &self,
        turn_context: &TurnContext,
        conversation_history: InitialHistory,
    ) {
        match conversation_history {
            InitialHistory::New => {
                // Build and record initial items (user instructions + environment context)
                let items = self.build_initial_context(turn_context);
                self.record_conversation_items(&items).await;
            }
            InitialHistory::Resumed(_) | InitialHistory::Forked(_) => {
                let rollout_items = conversation_history.get_rollout_items();
                let persist = matches!(conversation_history, InitialHistory::Forked(_));

                // Always add response items to conversation history
                let rollout_items = conversation_history.get_rollout_items();
                let response_items: Vec<ResponseItem> = rollout_items
                    .iter()
                    .filter_map(|item| match item {
                        RolloutItem::ResponseItem(response_item) => Some(response_item.clone()),
                        _ => None,
                    })
                    .collect();
                if !response_items.is_empty() {
                    self.record_into_history(&response_items);
                }

                // If persisting, persist all rollout items as-is (recorder filters)
                if persist && !rollout_items.is_empty() {
                    self.persist_rollout_items(&rollout_items).await;
                }
            }
        }
    }

    /// Persist the event to rollout and send it to clients.
    pub(crate) async fn send_event(&self, event: Event) {
        // Persist the event into rollout (recorder filters as needed)
        let rollout_items = vec![RolloutItem::EventMsg(event.msg.clone())];
        self.persist_rollout_items(&rollout_items).await;
        if let Err(e) = self.tx_event.send(event).await {
            error!("failed to send tool call event: {e}");
        }
    }

    pub async fn request_command_approval(
        &self,
        sub_id: String,
        call_id: String,
        command: Vec<String>,
        cwd: PathBuf,
        reason: Option<String>,
    ) -> tokio::sync::oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = tokio::sync::oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut state = self.state.lock_unchecked();
            state.pending_approvals.insert(sub_id, tx_approve)
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for sub_id: {event_id}");
        }

        let event = Event {
            id: event_id,
            msg: EventMsg::ExecApprovalRequest(ExecApprovalRequestEvent {
                call_id,
                command,
                cwd,
                reason,
            }),
        };
        self.send_event(event).await;
        rx_approve
    }

    pub async fn request_patch_approval(
        &self,
        sub_id: String,
        call_id: String,
        action: &codex_apply_patch::ApplyPatchAction,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> tokio::sync::oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = tokio::sync::oneshot::channel();
        let event_id = sub_id.clone();
        let prev_entry = {
            let mut state = self.state.lock_unchecked();
            state.pending_approvals.insert(sub_id, tx_approve)
        };
        if prev_entry.is_some() {
            warn!("Overwriting existing pending approval for sub_id: {event_id}");
        }

        let event = Event {
            id: event_id,
            msg: EventMsg::ApplyPatchApprovalRequest(
                crate::protocol::ApplyPatchApprovalRequestEvent {
                    call_id,
                    changes: crate::apply_patch::convert_apply_patch_to_protocol(action),
                    reason,
                    grant_root,
                },
            ),
        };
        self.send_event(event).await;
        rx_approve
    }

    pub fn notify_approval(&self, sub_id: &str, decision: ReviewDecision) {
        let entry = {
            let mut state = self.state.lock_unchecked();
            state.pending_approvals.remove(sub_id)
        };
        match entry {
            Some(tx_approve) => {
                tx_approve.send(decision).ok();
            }
            None => {
                warn!("No pending approval found for sub_id: {sub_id}");
            }
        }
    }

    pub fn add_approved_command(&self, cmd: Vec<String>) {
        let mut state = self.state.lock_unchecked();
        state.approved_commands.insert(cmd);
    }

    /// Get access to multi-agent components for agent coordination
    pub fn get_multi_agent_components(&self) -> Option<&MultiAgentComponents> {
        self.multi_agent_components.as_ref()
    }

    /// Get the conversation ID for this session
    pub fn get_conversation_id(&self) -> ConversationId {
        self.conversation_id
    }

    /// Get access to the MCP connection manager
    pub fn get_mcp_connection_manager(&self) -> &McpConnectionManager {
        &self.mcp_connection_manager
    }

    /// Records input items: always append to conversation history and
    /// persist these response items to rollout.
    pub(crate) async fn record_conversation_items(&self, items: &[ResponseItem]) {
        self.record_into_history(items);
        self.persist_rollout_response_items(items).await;
    }

    /// Append ResponseItems to the in-memory conversation history only.
    fn record_into_history(&self, items: &[ResponseItem]) {
        self.state
            .lock_unchecked()
            .history
            .record_items(items.iter());
    }

    async fn persist_rollout_response_items(&self, items: &[ResponseItem]) {
        let rollout_items: Vec<RolloutItem> = items
            .iter()
            .cloned()
            .map(RolloutItem::ResponseItem)
            .collect();
        self.persist_rollout_items(&rollout_items).await;
    }

    pub(crate) fn build_initial_context(&self, turn_context: &TurnContext) -> Vec<ResponseItem> {
        let mut items = Vec::<ResponseItem>::with_capacity(2);
        if let Some(user_instructions) = turn_context.user_instructions.as_deref() {
            items.push(UserInstructions::new(user_instructions.to_string()).into());
        }
        items.push(ResponseItem::from(EnvironmentContext::new(
            Some(turn_context.cwd.clone()),
            Some(turn_context.approval_policy),
            Some(turn_context.sandbox_policy.clone()),
            Some(self.user_shell.clone()),
        )));
        items
    }

    pub(crate) async fn persist_rollout_items(&self, items: &[RolloutItem]) {
        let recorder = {
            let guard = self.rollout.lock_unchecked();
            guard.as_ref().cloned()
        };
        if let Some(rec) = recorder
            && let Err(e) = rec.record_items(items).await
        {
            error!("failed to record rollout items: {e:#}");
        }
    }

    /// Record a user input item to conversation history and also persist a
    /// corresponding UserMessage EventMsg to rollout.
    pub(crate) async fn record_input_and_rollout_usermsg(&self, response_input: &ResponseInputItem) {
        let response_item: ResponseItem = response_input.clone().into();
        // Add to conversation history and persist response item to rollout
        self.record_conversation_items(std::slice::from_ref(&response_item))
            .await;

        // Derive user message events and persist only UserMessage to rollout
        let msgs = map_response_item_to_event_messages(&response_item, self.show_raw_agent_reasoning);
        let user_msgs: Vec<RolloutItem> = msgs
            .into_iter()
            .filter_map(|m| match m {
                EventMsg::UserMessage(ev) => Some(RolloutItem::EventMsg(EventMsg::UserMessage(ev))),
                _ => None,
            })
            .collect();
        if !user_msgs.is_empty() {
            self.persist_rollout_items(&user_msgs).await;
        }
    }

    pub(crate) async fn on_exec_command_begin(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        exec_command_context: ExecCommandContext,
    ) {
        let ExecCommandContext {
            sub_id,
            call_id,
            command_for_display,
            cwd,
            apply_patch,
        } = exec_command_context;
        let msg = match apply_patch {
            Some(ApplyPatchCommandContext {
                user_explicitly_approved_this_action,
                changes,
            }) => {
                turn_diff_tracker.on_patch_begin(&changes);

                EventMsg::PatchApplyBegin(PatchApplyBeginEvent {
                    call_id,
                    auto_approved: !user_explicitly_approved_this_action,
                    changes,
                })
            }
            None => EventMsg::ExecCommandBegin(ExecCommandBeginEvent {
                call_id,
                command: command_for_display.clone(),
                cwd,
                parsed_cmd: parse_command(&command_for_display)
                    .into_iter()
                    .map(Into::into)
                    .collect(),
            }),
        };
        let event = Event {
            id: sub_id.to_string(),
            msg,
        };
        self.send_event(event).await;
    }

    pub(crate) async fn on_exec_command_end(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        sub_id: &str,
        call_id: &str,
        output: &ExecToolCallOutput,
        is_apply_patch: bool,
    ) {
        let ExecToolCallOutput {
            stdout,
            stderr,
            aggregated_output,
            duration,
            exit_code,
            timed_out: _,
        } = output;
        // Send full stdout/stderr to clients; do not truncate.
        let stdout = stdout.text.clone();
        let stderr = stderr.text.clone();
        let formatted_output = crate::events::format_exec_output_str(output);
        let aggregated_output: String = aggregated_output.text.clone();

        let msg = if is_apply_patch {
            EventMsg::PatchApplyEnd(PatchApplyEndEvent {
                call_id: call_id.to_string(),
                stdout,
                stderr,
                success: *exit_code == 0,
            })
        } else {
            EventMsg::ExecCommandEnd(ExecCommandEndEvent {
                call_id: call_id.to_string(),
                stdout,
                stderr,
                aggregated_output,
                exit_code: *exit_code,
                duration: *duration,
                formatted_output,
            })
        };

        let event = Event {
            id: sub_id.to_string(),
            msg,
        };
        self.send_event(event).await;

        // If this is an apply_patch, after we emit the end patch, emit a second event
        // with the full turn diff if there is one.
        if is_apply_patch {
            let unified_diff = turn_diff_tracker.get_unified_diff();
            if let Ok(Some(unified_diff)) = unified_diff {
                let msg = EventMsg::TurnDiff(TurnDiffEvent { unified_diff });
                let event = Event {
                    id: sub_id.into(),
                    msg,
                };
                self.send_event(event).await;
            }
        }
    }

    /// Runs the exec tool call and emits events for the begin and end of the
    /// command even on error.
    ///
    /// Returns the output of the exec tool call.
    pub(crate) async fn run_exec_with_events<'a>(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        begin_ctx: ExecCommandContext,
        exec_args: crate::codex::ExecInvokeArgs<'a>,
    ) -> crate::error::Result<ExecToolCallOutput> {
        let is_apply_patch = begin_ctx.apply_patch.is_some();
        let sub_id = begin_ctx.sub_id.clone();
        let call_id = begin_ctx.call_id.clone();

        self.on_exec_command_begin(turn_diff_tracker, begin_ctx.clone())
            .await;

        let result = crate::exec::process_exec_tool_call(
            exec_args.params,
            exec_args.sandbox_type,
            exec_args.sandbox_policy,
            exec_args.sandbox_cwd,
            exec_args.codex_linux_sandbox_exe,
            exec_args.stdout_stream,
        )
        .await;

        let output_stderr;
        let borrowed: &ExecToolCallOutput = match &result {
            Ok(output) => output,
            Err(e) => {
                output_stderr = ExecToolCallOutput {
                    exit_code: -1,
                    stdout: crate::exec::StreamOutput::new(String::new()),
                    stderr: crate::exec::StreamOutput::new(crate::error::get_error_message_ui(e)),
                    aggregated_output: crate::exec::StreamOutput::new(crate::error::get_error_message_ui(e)),
                    duration: Duration::default(),
                    timed_out: false,
                };
                &output_stderr
            }
        };
        self.on_exec_command_end(
            turn_diff_tracker,
            &sub_id,
            &call_id,
            borrowed,
            is_apply_patch,
        )
        .await;

        result
    }

    /// Helper that emits a BackgroundEvent with the given message. This keeps
    /// the call‑sites terse so adding more diagnostics does not clutter the
    /// core agent logic.
    pub(crate) async fn notify_background_event(&self, sub_id: &str, message: impl Into<String>) {
        let event = Event {
            id: sub_id.to_string(),
            msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                message: message.into(),
            }),
        };
        self.send_event(event).await;
    }

    pub(crate) async fn notify_stream_error(&self, sub_id: &str, message: impl Into<String>) {
        let event = Event {
            id: sub_id.to_string(),
            msg: EventMsg::StreamError(StreamErrorEvent {
                message: message.into(),
            }),
        };
        self.send_event(event).await;
    }

    /// Build the full turn input by concatenating the current conversation
    /// history with additional items for this turn.
    pub fn turn_input_with_history(&self, extra: Vec<ResponseItem>) -> Vec<ResponseItem> {
        [self.state.lock_unchecked().history.contents(), extra].concat()
    }

    /// Returns the input if there was no task running to inject into
    pub fn inject_input(&self, input: Vec<InputItem>) -> Result<(), Vec<InputItem>> {
        let mut state = self.state.lock_unchecked();
        if state.current_task.is_some() {
            state.pending_input.push(input.into());
            Ok(())
        } else {
            Err(input)
        }
    }

    pub fn get_pending_input(&self) -> Vec<ResponseInputItem> {
        let mut state = self.state.lock_unchecked();
        if state.pending_input.is_empty() {
            Vec::with_capacity(0)
        } else {
            let mut ret = Vec::new();
            std::mem::swap(&mut ret, &mut state.pending_input);
            ret
        }
    }

    pub async fn call_tool(
        &self,
        server: &str,
        tool: &str,
        arguments: Option<serde_json::Value>,
        timeout: Option<Duration>,
    ) -> anyhow::Result<CallToolResult> {
        self.mcp_connection_manager
            .call_tool(server, tool, arguments, timeout)
            .await
    }

    pub fn interrupt_task(&self) {
        info!("interrupt received: abort current task, if any");
        let mut state = self.state.lock_unchecked();
        state.pending_approvals.clear();
        state.pending_input.clear();
        if let Some(task) = state.current_task.take() {
            task.abort(TurnAbortReason::Interrupted);
        }
    }

    /// Spawn the configured notifier (if any) with the given JSON payload as
    /// the last argument. Failures are logged but otherwise ignored so that
    /// notification issues do not interfere with the main workflow.
    pub(crate) fn maybe_notify(&self, notification: UserNotification) {
        let Some(notify_command) = &self.notify else {
            return;
        };

        if notify_command.is_empty() {
            return;
        }

        let Ok(json) = serde_json::to_string(&notification) else {
            error!("failed to serialise notification payload");
            return;
        };

        let mut command = std::process::Command::new(&notify_command[0]);
        if notify_command.len() > 1 {
            command.args(&notify_command[1..]);
        }
        command.arg(json);

        // Fire-and-forget – we do not wait for completion.
        if let Err(e) = command.spawn() {
            warn!("failed to spawn notifier '{}': {e}", notify_command[0]);
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.interrupt_task();
    }
}