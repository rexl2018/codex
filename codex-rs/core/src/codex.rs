use std::borrow::Cow;
use std::collections::HashMap;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::MutexGuard;
use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::AuthManager;
use crate::config_edit::CONFIG_KEY_EFFORT;
use crate::config_edit::CONFIG_KEY_MODEL;
use crate::config_edit::persist_non_null_overrides;
use crate::context_store::IContextRepository;
use crate::context_store::InMemoryContextRepository;
use crate::event_mapping::map_response_item_to_event_messages;
use crate::multi_agent_coordinator::ExecutionPlan;
use crate::multi_agent_coordinator::SharedContext;
use crate::multi_agent_coordinator::SubtaskCoordinator;
use crate::subagent_manager::ExecutorType;
use crate::subagent_manager::ISubagentManager;
use crate::subagent_manager::InMemorySubagentManager;
use crate::subagent_manager::SubagentTaskSpec;
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
use crate::mcp_connection_manager::McpConnectionManager;
use crate::mcp_tool_call::handle_mcp_tool_call;
use crate::model_family::find_family_for_model;
use crate::openai_model_info::get_model_info;
use crate::openai_tools::ApplyPatchToolArgs;
use crate::openai_tools::ToolsConfig;
use crate::openai_tools::ToolsConfigParams;
use crate::openai_tools::get_openai_tools;
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
use crate::turn_diff_tracker::TurnDiffTracker;
use crate::unified_exec::UnifiedExecSessionManager;
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
use codex_protocol::protocol::GetContextsResultEvent;
use codex_protocol::protocol::SaveContextsToFileResultEvent;
use codex_protocol::protocol::LoadContextsFromFileResultEvent;
use codex_protocol::protocol::ContextSummary;
use codex_protocol::protocol::InitialHistory;
use codex_protocol::protocol::SubagentMetadata;
use codex_protocol::protocol::SubagentType;

/// Agent state based on recent subagent execution history
#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    /// No subagent execution history exists
    Initialization,
    /// A new user instruction has been received and a new agent task has been created.
    AgentTaskCreated,
    /// Last Explorer subagent completed successfully without reaching turn limits
    ExplorerNormalCompletion,
    /// Last Explorer subagent was forced to complete due to reaching maximum turns
    ExplorerForcedCompletion,
    /// Last Coder subagent completed successfully without reaching turn limits
    CoderNormalCompletion,
    /// Last Coder subagent was forced to complete due to reaching maximum turns
    CoderForcedCompletion,
}

impl AgentState {
    /// Get a human-readable description of the current state
    pub fn description(&self) -> &'static str {
        match self {
            AgentState::Initialization => "Initialization - No subagent execution history",
            AgentState::AgentTaskCreated => {
                "AgentTaskCreated - A new user instruction has been received and a new agent task has been created."
            }
            AgentState::ExplorerNormalCompletion => {
                "Explorer Normal Completion - Last Explorer subagent completed successfully"
            }
            AgentState::ExplorerForcedCompletion => {
                "Explorer Forced Completion - Last Explorer subagent reached turn limit"
            }
            AgentState::CoderNormalCompletion => {
                "Coder Normal Completion - Last Coder subagent completed successfully"
            }
            AgentState::CoderForcedCompletion => {
                "Coder Forced Completion - Last Coder subagent reached turn limit"
            }
        }
    }

    /// Check if creating an Explorer subagent is allowed in this state
    pub fn can_create_explorer(&self) -> bool {
        match self {
            AgentState::Initialization => true,
            AgentState::AgentTaskCreated => true,
            AgentState::ExplorerNormalCompletion => false,
            AgentState::ExplorerForcedCompletion => false,
            AgentState::CoderNormalCompletion => true,
            AgentState::CoderForcedCompletion => true,
        }
    }

    /// Check if creating a Coder subagent is allowed in this state
    pub fn can_create_coder(&self) -> bool {
        match self {
            AgentState::Initialization => true,
            AgentState::AgentTaskCreated => true,
            AgentState::ExplorerNormalCompletion => true,
            AgentState::ExplorerForcedCompletion => true,
            AgentState::CoderNormalCompletion => false,
            AgentState::CoderForcedCompletion => false,
        }
    }

    /// Get suggested alternative actions when subagent creation is blocked
    pub fn get_blocked_alternatives(&self, blocked_type: SubagentType) -> &'static str {
        match (self, blocked_type) {
            (
                AgentState::ExplorerNormalCompletion | AgentState::ExplorerForcedCompletion,
                SubagentType::Explorer,
            ) => {
                "Consider creating a 'coder' subagent to implement changes based on the exploration results, or request a summary of the existing analysis."
            }
            (
                AgentState::CoderNormalCompletion | AgentState::CoderForcedCompletion,
                SubagentType::Coder,
            ) => {
                "Consider creating an 'explorer' subagent to analyze additional files or areas, or request a summary of the existing implementation."
            }
            _ => "No alternatives needed - action should be allowed.",
        }
    }
}

/// Detect current agent state based on recent subagent execution history
async fn detect_agent_state(multi_agent_components: &Option<MultiAgentComponents>) -> AgentState {
    let Some(components) = multi_agent_components else {
        return AgentState::Initialization;
    };

    // Check if a new user task has been created
    if components
        .new_user_task_created
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return AgentState::AgentTaskCreated;
    }

    match components
        .subagent_manager
        .get_recently_completed_tasks(1)
        .await
    {
        Ok(recent_tasks) => {
            if let Some(last_task) = recent_tasks.first() {
                if let crate::subagent_manager::TaskStatus::Completed { result } = &last_task.status
                {
                    let was_forced_completion =
                        result.metadata.reached_max_turns || result.metadata.force_completed;

                    match (&last_task.agent_type, was_forced_completion) {
                        (SubagentType::Explorer, false) => AgentState::ExplorerNormalCompletion,
                        (SubagentType::Explorer, true) => AgentState::ExplorerForcedCompletion,
                        (SubagentType::Coder, false) => AgentState::CoderNormalCompletion,
                        (SubagentType::Coder, true) => AgentState::CoderForcedCompletion,
                    }
                } else {
                    // Task exists but not completed - treat as initialization
                    AgentState::Initialization
                }
            } else {
                AgentState::Initialization
            }
        }
        Err(_) => AgentState::Initialization,
    }
}

// A convenience extension trait for acquiring mutex locks where poisoning is
// unrecoverable and should abort the program. This avoids scattered `.unwrap()`
// calls on `lock()` while still surfacing a clear panic message when a lock is
// poisoned.
trait MutexExt<T> {
    fn lock_unchecked(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn lock_unchecked(&self) -> MutexGuard<'_, T> {
        #[expect(clippy::expect_used)]
        self.lock().expect("poisoned lock")
    }
}

/// Get all available context IDs (simplified version for now)
///
/// This function gets all available contexts from the repository.
/// TODO: Implement LLM-based intelligent selection.
async fn get_all_available_contexts(
    context_repo: &Arc<InMemoryContextRepository>,
) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
    use crate::context_store::ContextQuery;
    use crate::context_store::IContextRepository;

    // Get all available contexts
    let all_contexts = context_repo
        .query_contexts(&ContextQuery {
            ids: None,
            tags: None,
            created_by: None,
            limit: None,
        })
        .await?;

    // For now, return all context IDs
    // TODO: Implement LLM-based selection
    let context_ids: Vec<String> = all_contexts.iter().map(|ctx| ctx.id.clone()).collect();

    tracing::info!(
        "Found {} available contexts (returning all for now): {:?}",
        context_ids.len(),
        context_ids
    );

    Ok(context_ids)
}

/// Query contexts generated by a specific task ID
///
/// This function queries the context repository for contexts that were created by a specific subagent task.
async fn query_contexts_by_task_id(
    context_repo: &Arc<InMemoryContextRepository>,
    task_id: &str,
) -> Result<Vec<crate::context_store::Context>, Box<dyn std::error::Error + Send + Sync>> {
    use crate::context_store::ContextQuery;
    use crate::context_store::IContextRepository;

    // Query contexts created by this specific task
    let task_contexts = context_repo
        .query_contexts(&ContextQuery {
            ids: None,
            tags: None,
            created_by: Some(task_id.to_string()),
            limit: None,
        })
        .await?;

    tracing::info!(
        "Found {} contexts generated by task '{}': {:?}",
        task_contexts.len(),
        task_id,
        task_contexts.iter().map(|ctx| &ctx.id).collect::<Vec<_>>()
    );

    Ok(task_contexts)
}

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
pub(crate) const MODEL_FORMAT_MAX_BYTES: usize = 10 * 1024; // 10 KiB
pub(crate) const MODEL_FORMAT_MAX_LINES: usize = 256; // lines
pub(crate) const MODEL_FORMAT_HEAD_LINES: usize = MODEL_FORMAT_MAX_LINES / 2;
pub(crate) const MODEL_FORMAT_TAIL_LINES: usize = MODEL_FORMAT_MAX_LINES - MODEL_FORMAT_HEAD_LINES; // 128
pub(crate) const MODEL_FORMAT_HEAD_BYTES: usize = MODEL_FORMAT_MAX_BYTES / 2;

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

/// Mutable state of the agent
#[derive(Default)]
struct State {
    approved_commands: HashSet<Vec<String>>,
    current_task: Option<AgentTask>,
    pending_approvals: HashMap<String, oneshot::Sender<ReviewDecision>>,
    pending_input: Vec<ResponseInputItem>,
    history: ConversationHistory,
    token_info: Option<TokenUsageInfo>,
}

/// Context for an initialized model agent
///
/// A session has at most 1 running task at a time, and can be interrupted by user input.

pub(crate) struct Session {
    conversation_id: ConversationId,
    tx_event: Sender<Event>,

    /// Manager for external MCP servers/tools.
    mcp_connection_manager: McpConnectionManager,
    session_manager: ExecSessionManager,
    unified_exec_manager: UnifiedExecSessionManager,
    mcp_clients: Arc<TokioMutex<Vec<McpClient>>>,

    /// External notifier command (will be passed as args to exec()). When
    /// `None` this feature is disabled.
    notify: Option<Vec<String>>,

    /// Optional rollout recorder for persisting the conversation transcript so
    /// sessions can be replayed or inspected later.
    rollout: Mutex<Option<RolloutRecorder>>,
    state: Mutex<State>,
    codex_linux_sandbox_exe: Option<PathBuf>,
    user_shell: shell::Shell,
    show_raw_agent_reasoning: bool,

    /// Multi-agent components (optional)
    multi_agent_components: Option<MultiAgentComponents>,
}

/// Multi-agent components container
pub(crate) struct MultiAgentComponents {
    pub context_repo: Arc<InMemoryContextRepository>,
    pub subagent_manager: Arc<InMemorySubagentManager>,
    pub subtask_coordinator: Option<SubtaskCoordinator>,
    /// Flag to track if a new user task has been created
    pub new_user_task_created: Arc<std::sync::atomic::AtomicBool>,
}

/// The context needed for a single turn of the conversation.
#[derive(Debug)]
pub(crate) struct TurnContext {
    pub(crate) client: ModelClient,
    /// The session's current working directory. All relative paths provided by
    /// the model as well as sandbox policies are resolved against this path
    /// instead of `std::env::current_dir()`.
    pub(crate) cwd: PathBuf,
    pub(crate) base_instructions: Option<String>,
    pub(crate) user_instructions: Option<String>,
    pub(crate) approval_policy: AskForApproval,
    pub(crate) sandbox_policy: SandboxPolicy,
    pub(crate) shell_environment_policy: ShellEnvironmentPolicy,
    pub(crate) tools_config: ToolsConfig,
}

impl TurnContext {
    fn resolve_path(&self, path: Option<String>) -> PathBuf {
        path.as_ref()
            .map(PathBuf::from)
            .map_or_else(|| self.cwd.clone(), |p| self.cwd.join(p))
    }
}

/// Configure the model session.
struct ConfigureSession {
    /// Provider identifier ("openai", "openrouter", ...).
    provider: ModelProviderInfo,

    /// If not specified, server will use its default model.
    model: String,

    model_reasoning_effort: ReasoningEffortConfig,
    model_reasoning_summary: ReasoningSummaryConfig,

    /// Model instructions that are appended to the base instructions.
    user_instructions: Option<String>,

    /// Base instructions override.
    base_instructions: Option<String>,

    /// When to escalate for approval for execution
    approval_policy: AskForApproval,
    /// How to sandbox commands executed in the system
    sandbox_policy: SandboxPolicy,

    /// Optional external notifier command tokens. Present only when the
    /// client wants the agent to spawn a program after each completed
    /// turn.
    notify: Option<Vec<String>>,

    /// Working directory that should be treated as the *root* of the
    /// session. All relative paths supplied by the model as well as the
    /// execution sandbox are resolved against this directory **instead**
    /// of the process-wide current working directory. CLI front-ends are
    /// expected to expand this to an absolute path before sending the
    /// `ConfigureSession` operation so that the business-logic layer can
    /// operate deterministically.
    cwd: PathBuf,
}

impl Session {
    async fn new(
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
                    id: INITIAL_SUBMIT_ID.to_owned(),
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
                    id: INITIAL_SUBMIT_ID.to_owned(),
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
            tools_config: ToolsConfig::new(&ToolsConfigParams {
                model_family: &config.model_family,
                approval_policy,
                sandbox_policy: sandbox_policy.clone(),
                include_plan_tool: config.include_plan_tool,
                include_apply_patch_tool: config.include_apply_patch_tool,
                include_web_search_request: config.tools_web_search_request,
                use_streamable_shell_tool: config.use_experimental_streamable_shell_tool,
                include_view_image_tool: config.include_view_image_tool,
                experimental_unified_exec_tool: config.use_experimental_unified_exec_tool,
                include_subagent_task_tool: config.include_subagent_task_tool,
            }),
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
            multi_agent_components,
        });

        // Dispatch the SessionConfiguredEvent first and then report any errors.
        // If resuming, include converted initial messages in the payload so UIs can render them immediately.
        let initial_messages = initial_history.get_event_msgs();
        sess.record_initial_history(&turn_context, initial_history)
            .await;

        let events = std::iter::once(Event {
            id: INITIAL_SUBMIT_ID.to_owned(),
            msg: EventMsg::SessionConfigured(SessionConfiguredEvent {
                session_id: conversation_id,
                model,
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
            && task.sub_id == sub_id
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
                let response_items = conversation_history.get_response_items();
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
    ) -> oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
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
        action: &ApplyPatchAction,
        reason: Option<String>,
        grant_root: Option<PathBuf>,
    ) -> oneshot::Receiver<ReviewDecision> {
        // Add the tx_approve callback to the map before sending the request.
        let (tx_approve, rx_approve) = oneshot::channel();
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
            msg: EventMsg::ApplyPatchApprovalRequest(ApplyPatchApprovalRequestEvent {
                call_id,
                changes: convert_apply_patch_to_protocol(action),
                reason,
                grant_root,
            }),
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

    /// Records input items: always append to conversation history and
    /// persist these response items to rollout.
    async fn record_conversation_items(&self, items: &[ResponseItem]) {
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

    fn build_initial_context(&self, turn_context: &TurnContext) -> Vec<ResponseItem> {
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

    async fn persist_rollout_items(&self, items: &[RolloutItem]) {
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
    async fn record_input_and_rollout_usermsg(&self, response_input: &ResponseInputItem) {
        let response_item: ResponseItem = response_input.clone().into();
        // Add to conversation history and persist response item to rollout
        self.record_conversation_items(std::slice::from_ref(&response_item))
            .await;

        // Derive user message events and persist only UserMessage to rollout
        let msgs =
            map_response_item_to_event_messages(&response_item, self.show_raw_agent_reasoning);
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

    async fn on_exec_command_begin(
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

    async fn on_exec_command_end(
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
        } = output;
        // Send full stdout/stderr to clients; do not truncate.
        let stdout = stdout.text.clone();
        let stderr = stderr.text.clone();
        let formatted_output = format_exec_output_str(output);
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
    async fn run_exec_with_events<'a>(
        &self,
        turn_diff_tracker: &mut TurnDiffTracker,
        begin_ctx: ExecCommandContext,
        exec_args: ExecInvokeArgs<'a>,
    ) -> crate::error::Result<ExecToolCallOutput> {
        let is_apply_patch = begin_ctx.apply_patch.is_some();
        let sub_id = begin_ctx.sub_id.clone();
        let call_id = begin_ctx.call_id.clone();

        self.on_exec_command_begin(turn_diff_tracker, begin_ctx.clone())
            .await;

        let result = process_exec_tool_call(
            exec_args.params,
            exec_args.sandbox_type,
            exec_args.sandbox_policy,
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
                    stdout: StreamOutput::new(String::new()),
                    stderr: StreamOutput::new(get_error_message_ui(e)),
                    aggregated_output: StreamOutput::new(get_error_message_ui(e)),
                    duration: Duration::default(),
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
    async fn notify_background_event(&self, sub_id: &str, message: impl Into<String>) {
        let event = Event {
            id: sub_id.to_string(),
            msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                message: message.into(),
            }),
        };
        self.send_event(event).await;
    }

    async fn notify_stream_error(&self, sub_id: &str, message: impl Into<String>) {
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

    fn interrupt_task(&self) {
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
    fn maybe_notify(&self, notification: UserNotification) {
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

#[derive(Clone, Debug)]
pub(crate) struct ExecCommandContext {
    pub(crate) sub_id: String,
    pub(crate) call_id: String,
    pub(crate) command_for_display: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) apply_patch: Option<ApplyPatchCommandContext>,
}

#[derive(Clone, Debug)]
pub(crate) struct ApplyPatchCommandContext {
    pub(crate) user_explicitly_approved_this_action: bool,
    pub(crate) changes: HashMap<PathBuf, FileChange>,
}

/// A series of Turns in response to user input.
pub(crate) struct AgentTask {
    sess: Arc<Session>,
    sub_id: String,
    handle: AbortHandle,
}

impl AgentTask {
    fn spawn(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
    ) -> Self {
        let handle = {
            let sess = sess.clone();
            let sub_id = sub_id.clone();
            let tc = Arc::clone(&turn_context);
            tokio::spawn(async move { run_task(sess, tc.as_ref(), sub_id, input).await })
                .abort_handle()
        };
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    fn compact(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
        compact_instructions: String,
    ) -> Self {
        let handle = {
            let sess = sess.clone();
            let sub_id = sub_id.clone();
            let tc = Arc::clone(&turn_context);
            tokio::spawn(async move {
                run_compact_task(sess, tc.as_ref(), sub_id, input, compact_instructions).await
            })
            .abort_handle()
        };
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    fn abort(self, reason: TurnAbortReason) {
        // TOCTOU?
        if !self.handle.is_finished() {
            self.handle.abort();
            let event = Event {
                id: self.sub_id,
                msg: EventMsg::TurnAborted(TurnAbortedEvent { reason }),
            };
            let sess = self.sess.clone();
            tokio::spawn(async move {
                sess.send_event(event).await;
            });
        }
    }
}

async fn submission_loop(
    sess: Arc<Session>,
    turn_context: TurnContext,
    config: Arc<Config>,
    rx_sub: Receiver<Submission>,
) {
    // Wrap once to avoid cloning TurnContext for each task.
    let mut turn_context = Arc::new(turn_context);
    // To break out of this loop, send Op::Shutdown.
    while let Ok(sub) = rx_sub.recv().await {
        debug!(?sub, "Submission");
        match sub.op {
            Op::Interrupt => {
                sess.interrupt_task();
            }
            Op::OverrideTurnContext {
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
            } => {
                // Recalculate the persistent turn context with provided overrides.
                let prev = Arc::clone(&turn_context);
                let provider = prev.client.get_provider();

                // Effective model + family
                let (effective_model, effective_family) = if let Some(ref m) = model {
                    let fam =
                        find_family_for_model(m).unwrap_or_else(|| config.model_family.clone());
                    (m.clone(), fam)
                } else {
                    (prev.client.get_model(), prev.client.get_model_family())
                };

                // Effective reasoning settings
                let effective_effort = effort.unwrap_or(prev.client.get_reasoning_effort());
                let effective_summary = summary.unwrap_or(prev.client.get_reasoning_summary());

                let auth_manager = prev.client.get_auth_manager();

                // Build updated config for the client
                let mut updated_config = (*config).clone();
                updated_config.model = effective_model.clone();
                updated_config.model_family = effective_family.clone();
                if let Some(model_info) = get_model_info(&effective_family) {
                    updated_config.model_context_window = Some(model_info.context_window);
                }

                let client = ModelClient::new(
                    Arc::new(updated_config),
                    auth_manager,
                    provider,
                    effective_effort,
                    effective_summary,
                    sess.conversation_id,
                );

                let new_approval_policy = approval_policy.unwrap_or(prev.approval_policy);
                let new_sandbox_policy = sandbox_policy
                    .clone()
                    .unwrap_or(prev.sandbox_policy.clone());
                let new_cwd = cwd.clone().unwrap_or_else(|| prev.cwd.clone());

                let tools_config = ToolsConfig::new(&ToolsConfigParams {
                    model_family: &effective_family,
                    approval_policy: new_approval_policy,
                    sandbox_policy: new_sandbox_policy.clone(),
                    include_plan_tool: config.include_plan_tool,
                    include_apply_patch_tool: config.include_apply_patch_tool,
                    include_web_search_request: config.tools_web_search_request,
                    use_streamable_shell_tool: config.use_experimental_streamable_shell_tool,
                    include_view_image_tool: config.include_view_image_tool,
                    experimental_unified_exec_tool: config.use_experimental_unified_exec_tool,
                    include_subagent_task_tool: config.include_subagent_task_tool,
                });

                let new_turn_context = TurnContext {
                    client,
                    tools_config,
                    user_instructions: prev.user_instructions.clone(),
                    base_instructions: prev.base_instructions.clone(),
                    approval_policy: new_approval_policy,
                    sandbox_policy: new_sandbox_policy.clone(),
                    shell_environment_policy: prev.shell_environment_policy.clone(),
                    cwd: new_cwd.clone(),
                };

                // Install the new persistent context for subsequent tasks/turns.
                turn_context = Arc::new(new_turn_context);

                // Optionally persist changes to model / effort
                let effort_str = effort.map(|_| effective_effort.to_string());

                if let Err(e) = persist_non_null_overrides(
                    &config.codex_home,
                    config.active_profile.as_deref(),
                    &[
                        (&[CONFIG_KEY_MODEL], model.as_deref()),
                        (&[CONFIG_KEY_EFFORT], effort_str.as_deref()),
                    ],
                )
                .await
                {
                    warn!("failed to persist overrides: {e:#}");
                }

                if cwd.is_some() || approval_policy.is_some() || sandbox_policy.is_some() {
                    sess.record_conversation_items(&[ResponseItem::from(EnvironmentContext::new(
                        cwd,
                        approval_policy,
                        sandbox_policy,
                        // Shell is not configurable from turn to turn
                        None,
                    ))])
                    .await;
                }
            }
            Op::UserInput { items } => {
                // attempt to inject input into current task
                if let Err(items) = sess.inject_input(items) {
                    // no current task, spawn a new one
                    tracing::debug!("Creating agent task for UserInput: sub_id={}", sub.id);
                    tracing::info!("Creating agent task for UserInput: sub_id={}", sub.id);

                    // Set the flag to indicate a new user task has been created
                    if let Some(components) = &sess.multi_agent_components {
                        components
                            .new_user_task_created
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }

                    let task =
                        AgentTask::spawn(sess.clone(), Arc::clone(&turn_context), sub.id, items);
                    sess.set_task(task);
                }
            }
            Op::UserTurn {
                items,
                cwd,
                approval_policy,
                sandbox_policy,
                model,
                effort,
                summary,
            } => {
                // attempt to inject input into current task
                if let Err(items) = sess.inject_input(items) {
                    // Derive a fresh TurnContext for this turn using the provided overrides.
                    let provider = turn_context.client.get_provider();
                    let auth_manager = turn_context.client.get_auth_manager();

                    // Derive a model family for the requested model; fall back to the session's.
                    let model_family = find_family_for_model(&model)
                        .unwrap_or_else(|| config.model_family.clone());

                    // Create a per‑turn Config clone with the requested model/family.
                    let mut per_turn_config = (*config).clone();
                    per_turn_config.model = model.clone();
                    per_turn_config.model_family = model_family.clone();
                    if let Some(model_info) = get_model_info(&model_family) {
                        per_turn_config.model_context_window = Some(model_info.context_window);
                    }

                    // Build a new client with per‑turn reasoning settings.
                    // Reuse the same provider and session id; auth defaults to env/API key.
                    let client = ModelClient::new(
                        Arc::new(per_turn_config),
                        auth_manager,
                        provider,
                        effort,
                        summary,
                        sess.conversation_id,
                    );

                    let fresh_turn_context = TurnContext {
                        client,
                        tools_config: ToolsConfig::new(&ToolsConfigParams {
                            model_family: &model_family,
                            approval_policy,
                            sandbox_policy: sandbox_policy.clone(),
                            include_plan_tool: config.include_plan_tool,
                            include_apply_patch_tool: config.include_apply_patch_tool,
                            include_web_search_request: config.tools_web_search_request,
                            use_streamable_shell_tool: config
                                .use_experimental_streamable_shell_tool,
                            include_view_image_tool: config.include_view_image_tool,
                            experimental_unified_exec_tool: config
                                .use_experimental_unified_exec_tool,
                            include_subagent_task_tool: config.include_subagent_task_tool,
                        }),
                        user_instructions: turn_context.user_instructions.clone(),
                        base_instructions: turn_context.base_instructions.clone(),
                        approval_policy,
                        sandbox_policy,
                        shell_environment_policy: turn_context.shell_environment_policy.clone(),
                        cwd,
                    };
                    // TODO: record the new environment context in the conversation history
                    // no current task, spawn a new one with the per‑turn context
                    tracing::debug!("Creating agent task for UserTurn: sub_id={}", sub.id);
                    tracing::info!("Creating agent task for UserTurn: sub_id={}", sub.id);
                    let task =
                        AgentTask::spawn(sess.clone(), Arc::new(fresh_turn_context), sub.id, items);
                    sess.set_task(task);
                }
            }
            Op::ExecApproval { id, decision } => match decision {
                ReviewDecision::Abort => {
                    sess.interrupt_task();
                }
                other => sess.notify_approval(&id, other),
            },
            Op::PatchApproval { id, decision } => match decision {
                ReviewDecision::Abort => {
                    sess.interrupt_task();
                }
                other => sess.notify_approval(&id, other),
            },
            Op::AddToHistory { text } => {
                let id = sess.conversation_id;
                let config = config.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::message_history::append_entry(&text, &id, &config).await
                    {
                        warn!("failed to append to message history: {e}");
                    }
                });
            }

            Op::GetHistoryEntryRequest { offset, log_id } => {
                let config = config.clone();
                let sess_clone = sess.clone();
                let sub_id = sub.id.clone();

                tokio::spawn(async move {
                    // Run lookup in blocking thread because it does file IO + locking.
                    let entry_opt = tokio::task::spawn_blocking(move || {
                        crate::message_history::lookup(log_id, offset, &config)
                    })
                    .await
                    .unwrap_or(None);

                    let event = Event {
                        id: sub_id,
                        msg: EventMsg::GetHistoryEntryResponse(
                            crate::protocol::GetHistoryEntryResponseEvent {
                                offset,
                                log_id,
                                entry: entry_opt.map(|e| {
                                    codex_protocol::message_history::HistoryEntry {
                                        conversation_id: e.session_id,
                                        ts: e.ts,
                                        text: e.text,
                                    }
                                }),
                            },
                        ),
                    };

                    sess_clone.send_event(event).await;
                });
            }
            Op::ListMcpTools => {
                let sub_id = sub.id.clone();

                // This is a cheap lookup from the connection manager's cache.
                let tools = sess.mcp_connection_manager.list_all_tools();
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::McpListToolsResponse(
                        crate::protocol::McpListToolsResponseEvent { tools },
                    ),
                };
                sess.send_event(event).await;
            }
            Op::ListCustomPrompts => {
                let sub_id = sub.id.clone();

                let custom_prompts: Vec<CustomPrompt> =
                    if let Some(dir) = crate::custom_prompts::default_prompts_dir() {
                        crate::custom_prompts::discover_prompts_in(&dir).await
                    } else {
                        Vec::new()
                    };

                let event = Event {
                    id: sub_id,
                    msg: EventMsg::ListCustomPromptsResponse(ListCustomPromptsResponseEvent {
                        custom_prompts,
                    }),
                };
                sess.send_event(event).await;
            }
            Op::Compact => {
                // Create a summarization request as user input
                const SUMMARIZATION_PROMPT: &str = include_str!("prompt_for_compact_command.md");

                // Attempt to inject input into current task
                if let Err(items) = sess.inject_input(vec![InputItem::Text {
                    text: "Start Summarization".to_string(),
                }]) {
                    let task = AgentTask::compact(
                        sess.clone(),
                        Arc::clone(&turn_context),
                        sub.id,
                        items,
                        SUMMARIZATION_PROMPT.to_string(),
                    );
                    sess.set_task(task);
                }
            }
            Op::Shutdown => {
                info!("Shutting down Codex instance");

                // Gracefully flush and shutdown rollout recorder on session end so tests
                // that inspect the rollout file do not race with the background writer.
                let recorder_opt = sess.rollout.lock_unchecked().take();
                if let Some(rec) = recorder_opt
                    && let Err(e) = rec.shutdown().await
                {
                    warn!("failed to shutdown rollout recorder: {e}");
                    let event = Event {
                        id: sub.id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Failed to shutdown rollout recorder".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }

                let event = Event {
                    id: sub.id.clone(),
                    msg: EventMsg::ShutdownComplete,
                };
                sess.send_event(event).await;
                break;
            }
            Op::GetPath => {
                let sub_id = sub.id.clone();
                // Flush rollout writes before returning the path so readers observe a consistent file.
                let (path, rec_opt) = {
                    let guard = sess.rollout.lock_unchecked();
                    match guard.as_ref() {
                        Some(rec) => (rec.get_rollout_path(), Some(rec.clone())),
                        None => {
                            error!("rollout recorder not found");
                            continue;
                        }
                    }
                };
                if let Some(rec) = rec_opt
                    && let Err(e) = rec.flush().await
                {
                    warn!("failed to flush rollout recorder before GetHistory: {e}");
                }
                let event = Event {
                    id: sub_id.clone(),
                    msg: EventMsg::ConversationPath(ConversationPathResponseEvent {
                        conversation_id: sess.conversation_id,
                        path,
                    }),
                };
                sess.send_event(event).await;
            }
            // Subagent operations
            Op::CreateSubagentTask {
                agent_type,
                title,
                description,
                context_refs,
                bootstrap_paths,
            } => {
                if let Some(ref components) = sess.multi_agent_components {
                    let spec = SubagentTaskSpec {
                        agent_type,
                        title,
                        description,
                        context_refs,
                        bootstrap_paths,
                        max_turns: None,
                        timeout_ms: None,
                    };

                    match components.subagent_manager.create_task(spec).await {
                        Ok(task_id) => {
                            // Auto-launch the task
                            if let Err(e) =
                                components.subagent_manager.launch_subagent(&task_id).await
                            {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::Error(ErrorEvent {
                                        message: format!("Failed to launch subagent: {}", e),
                                    }),
                                };
                                sess.send_event(event).await;
                            }
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to create subagent task: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::QueryContextStore { query } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Check if this is a single context query (like /ci get <id>) before moving query
                    let is_single_context_query = query.ids.as_ref()
                        .map(|ids| ids.len() == 1)
                        .unwrap_or(false);
                    
                    // Convert protocol ContextQuery to context_store ContextQuery
                    let context_query = crate::context_store::ContextQuery {
                        ids: query.ids,
                        tags: query.tags,
                        created_by: query.created_by,
                        limit: query.limit,
                    };
                    match components.context_repo.query_contexts(&context_query).await {
                        Ok(contexts) => {
                            let total_count = contexts.len();
                            
                            // Check if this is a single context query (like /ci get <id>)
                            // If so, we'll send the full content via a background event
                            if contexts.len() == 1 && is_single_context_query {
                                let ctx = &contexts[0];
                                let full_content_message = format!(
                                    "📋 Context Item: **{}**\n\n**Summary:** {}\n**Size:** {} bytes\n**Created by:** {}\n**Created at:** {}\n\n**Full Content:**\n{}",
                                    ctx.id,
                                    ctx.summary,
                                    ctx.size_bytes(),
                                    ctx.created_by,
                                    ctx.created_at
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_secs(),
                                    ctx.content
                                );
                                
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
                                        message: full_content_message,
                                    }),
                                };
                                sess.send_event(event).await;
                            } else {
                                // For multiple contexts or list queries, return summaries as before
                                let summaries: Vec<ContextSummary> = contexts
                                    .into_iter()
                                    .map(|ctx| {
                                        let size_bytes = ctx.size_bytes();
                                        ContextSummary {
                                            id: ctx.id,
                                            summary: ctx.summary,
                                            created_by: ctx.created_by,
                                            created_at: ctx
                                                .created_at
                                                .duration_since(std::time::UNIX_EPOCH)
                                                .unwrap_or_default()
                                                .as_secs()
                                                .to_string(),
                                            size_bytes,
                                        }
                                    })
                                    .collect();

                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::ContextQueryResult(ContextQueryResultEvent {
                                        query_id: uuid::Uuid::new_v4().to_string(),
                                        contexts: summaries,
                                        total_count,
                                    }),
                                };
                                sess.send_event(event).await;
                            }
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Context query failed: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::GetContexts { ids } => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.context_repo.get_contexts(&ids).await {
                        Ok(contexts) => {
                            let total_count = contexts.len();
                            // Convert full Context objects to ContextItem for the response
                            let context_items: Vec<ContextItem> = contexts
                                .into_iter()
                                .map(|ctx| ContextItem {
                                    id: ctx.id,
                                    summary: ctx.summary,
                                    content: ctx.content,
                                })
                                .collect();

                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::GetContextsResult(GetContextsResultEvent {
                                    contexts: context_items,
                                    total_count,
                                }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Get contexts failed: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::SaveContextsToFile { file_path, ids } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Get contexts to save
                    let contexts_to_save = if let Some(ids) = ids {
                        // Save specific contexts
                        match components.context_repo.get_contexts(&ids).await {
                            Ok(contexts) => contexts,
                            Err(e) => {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                                        file_path: file_path.clone(),
                                        success: false,
                                        message: format!("Failed to get contexts: {}", e),
                                        contexts_saved: 0,
                                    }),
                                };
                                sess.send_event(event).await;
                                continue;
                            }
                        }
                    } else {
                        // Save all contexts
                        match components.context_repo.query_contexts(&crate::context_store::ContextQuery {
                            ids: None,
                            tags: None,
                            created_by: None,
                            limit: None,
                        }).await {
                            Ok(contexts) => contexts,
                            Err(e) => {
                                let event = Event {
                                    id: sub.id,
                                    msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                                        file_path: file_path.clone(),
                                        success: false,
                                        message: format!("Failed to query contexts: {}", e),
                                        contexts_saved: 0,
                                    }),
                                };
                                sess.send_event(event).await;
                                continue;
                            }
                        }
                    };

                    // Convert to ContextItems for serialization
                    let context_items: Vec<ContextItem> = contexts_to_save
                        .iter()
                        .map(|ctx| ContextItem {
                            id: ctx.id.clone(),
                            summary: ctx.summary.clone(),
                            content: ctx.content.clone(),
                        })
                        .collect();

                    // Save to file
                    let result = tokio::task::spawn_blocking({
                        let file_path = file_path.clone();
                        let context_items = context_items.clone();
                        move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                            let json_content = serde_json::to_string_pretty(&context_items)?;
                            std::fs::write(&file_path, json_content)?;
                            Ok(())
                        }
                    }).await;

                    let event = match result {
                        Ok(Ok(())) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                                file_path: file_path.clone(),
                                success: true,
                                message: format!("Successfully saved {} context items to '{}'", context_items.len(), file_path),
                                contexts_saved: context_items.len(),
                            }),
                        },
                        Ok(Err(e)) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                                file_path: file_path.clone(),
                                success: false,
                                message: format!("Failed to save file: {}", e),
                                contexts_saved: 0,
                            }),
                        },
                        Err(e) => Event {
                            id: sub.id,
                            msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                                file_path: file_path.clone(),
                                success: false,
                                message: format!("Task execution failed: {}", e),
                                contexts_saved: 0,
                            }),
                        },
                    };
                    sess.send_event(event).await;
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::SaveContextsToFileResult(SaveContextsToFileResultEvent {
                            file_path: file_path.clone(),
                            success: false,
                            message: "Multi-agent functionality not enabled".to_string(),
                            contexts_saved: 0,
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::LoadContextsFromFile { file_path } => {
                if let Some(ref components) = sess.multi_agent_components {
                    // Load from file
                    let result = tokio::task::spawn_blocking({
                        let file_path = file_path.clone();
                        move || -> Result<Vec<ContextItem>, Box<dyn std::error::Error + Send + Sync>> {
                            let file_content = std::fs::read_to_string(&file_path)?;
                            let context_items: Vec<ContextItem> = serde_json::from_str(&file_content)?;
                            Ok(context_items)
                        }
                    }).await;

                    let event = match result {
                        Ok(Ok(context_items)) => {
                            // Store each context item
                            let mut stored_count = 0;
                            let mut errors = Vec::new();

                            for item in &context_items {
                                let context = crate::context_store::Context::new(
                                    item.id.clone(),
                                    item.summary.clone(),
                                    item.content.clone(),
                                    "file_import".to_string(),
                                    None,
                                );

                                match components.context_repo.store_context(context).await {
                                    Ok(()) => stored_count += 1,
                                    Err(e) => errors.push(format!("Failed to store context '{}': {}", item.id, e)),
                                }
                            }

                            let success = errors.is_empty();
                            let message = if success {
                                format!("Successfully loaded {} context items from '{}'", stored_count, file_path)
                            } else {
                                format!("Loaded {} out of {} context items. Errors: {}", 
                                    stored_count, context_items.len(), errors.join("; "))
                            };

                            Event {
                                id: sub.id,
                                msg: EventMsg::LoadContextsFromFileResult(LoadContextsFromFileResultEvent {
                                    file_path: file_path.clone(),
                                    success,
                                    message,
                                    contexts_loaded: stored_count,
                                }),
                            }
                        },
                        Ok(Err(e)) => Event {
                            id: sub.id,
                            msg: EventMsg::LoadContextsFromFileResult(LoadContextsFromFileResultEvent {
                                file_path: file_path.clone(),
                                success: false,
                                message: format!("Failed to load file: {}", e),
                                contexts_loaded: 0,
                            }),
                        },
                        Err(e) => Event {
                            id: sub.id,
                            msg: EventMsg::LoadContextsFromFileResult(LoadContextsFromFileResultEvent {
                                file_path: file_path.clone(),
                                success: false,
                                message: format!("Task execution failed: {}", e),
                                contexts_loaded: 0,
                            }),
                        },
                    };
                    sess.send_event(event).await;
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::LoadContextsFromFileResult(LoadContextsFromFileResultEvent {
                            file_path: file_path.clone(),
                            success: false,
                            message: "Multi-agent functionality not enabled".to_string(),
                            contexts_loaded: 0,
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::GetSubagentStatus { task_id } => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.subagent_manager.get_task_status(&task_id).await {
                        Ok(status) => {
                            // Convert status to a simple message for now
                            let status_msg = format!("Task {} status: {:?}", task_id, status);
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::AgentMessage(AgentMessageEvent {
                                    message: status_msg,
                                }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to get task status: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            Op::ListActiveSubagents => {
                if let Some(ref components) = sess.multi_agent_components {
                    match components.subagent_manager.get_active_tasks().await {
                        Ok(tasks) => {
                            let task_list = tasks
                                .iter()
                                .map(|t| {
                                    format!("- {} ({:?}): {}", t.task_id, t.agent_type, t.title)
                                })
                                .collect::<Vec<_>>()
                                .join("\n");

                            let message = if task_list.is_empty() {
                                "No active subagent tasks".to_string()
                            } else {
                                format!("Active subagent tasks:\n{}", task_list)
                            };

                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::AgentMessage(AgentMessageEvent { message }),
                            };
                            sess.send_event(event).await;
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub.id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to list active tasks: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                } else {
                    let event = Event {
                        id: sub.id,
                        msg: EventMsg::Error(ErrorEvent {
                            message: "Multi-agent functionality not enabled".to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                }
            }
            _ => {
                // Ignore unknown ops; enum is non_exhaustive to allow extensions.
            }
        }
    }
    debug!("Agent loop exited");
}

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
async fn run_task(
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
    let tools = get_openai_tools(
        &turn_context.tools_config,
        None, // Main agent cannot use MCP tools
        crate::openai_tools::AgentType::Main,
    );

    // Detect current agent state and create state information
    let agent_state = detect_agent_state(&sess.multi_agent_components).await;
    let agent_state_info = if !agent_state.can_create_explorer() || !agent_state.can_create_coder()
    {
        format!(
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
        )
    } else {
        format!(
            "Current Agent State: {}. Subagent Creation Constraints: All subagent types are currently allowed.",
            agent_state.description()
        )
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

    let prompt = Prompt {
        input,
        tools,
        base_instructions_override: turn_context.base_instructions.clone(),
        agent_state_info: Some(agent_state_info),
    };

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

    let mut stream = turn_context.client.clone().stream(&prompt).await?;

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

async fn run_compact_task(
    sess: Arc<Session>,
    turn_context: &TurnContext,
    sub_id: String,
    input: Vec<InputItem>,
    compact_instructions: String,
) {
    let model_context_window = turn_context.client.get_model_context_window();
    let start_event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskStarted(TaskStartedEvent {
            model_context_window,
        }),
    };
    sess.send_event(start_event).await;

    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input);
    let turn_input: Vec<ResponseItem> =
        sess.turn_input_with_history(vec![initial_input_for_turn.clone().into()]);

    let prompt = Prompt {
        input: turn_input,
        tools: Vec::new(),
        base_instructions_override: Some(compact_instructions.clone()),
        agent_state_info: None, // Compact tasks don't need state info
    };

    let max_retries = turn_context.client.get_provider().stream_max_retries();
    let mut retries = 0;

    loop {
        let attempt_result = drain_to_completed(&sess, turn_context, &sub_id, &prompt).await;

        match attempt_result {
            Ok(()) => break,
            Err(CodexErr::Interrupted) => return,
            Err(e) => {
                if retries < max_retries {
                    retries += 1;
                    let delay = backoff(retries);
                    sess.notify_stream_error(
                        &sub_id,
                        format!(
                            "stream error: {e}; retrying {retries}/{max_retries} in {delay:?}…"
                        ),
                    )
                    .await;
                    tokio::time::sleep(delay).await;
                    continue;
                } else {
                    let event = Event {
                        id: sub_id.clone(),
                        msg: EventMsg::Error(ErrorEvent {
                            message: e.to_string(),
                        }),
                    };
                    sess.send_event(event).await;
                    return;
                }
            }
        }
    }

    sess.remove_task(&sub_id);

    {
        let mut state = sess.state.lock_unchecked();
        state.history.keep_last_messages(1);
    }

    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::AgentMessage(AgentMessageEvent {
            message: "Compact task completed".to_string(),
        }),
    };
    sess.send_event(event).await;
    let event = Event {
        id: sub_id.clone(),
        msg: EventMsg::TaskComplete(TaskCompleteEvent {
            last_agent_message: None,
        }),
    };
    sess.send_event(event).await;
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

async fn handle_create_subagent_task(
    sess: &Session,
    arguments: String,
    sub_id: String,
    call_id: String,
) -> ResponseInputItem {
    #[derive(Deserialize)]
    struct CreateSubagentTaskArgs {
        agent_type: String,
        title: String,
        description: String,
        #[serde(default)]
        context_refs: Vec<String>,
        #[serde(default)]
        bootstrap_paths: Vec<BootstrapPath>,
        #[serde(default = "default_auto_launch")]
        auto_launch: bool,
    }

    fn default_auto_launch() -> bool {
        true
    }

    let args = match serde_json::from_str::<CreateSubagentTaskArgs>(&arguments) {
        Ok(args) => args,
        Err(err) => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!("Failed to parse create_subagent_task arguments: {err}"),
                    success: Some(false),
                },
            };
        }
    };

    // CRITICAL: Check for forced completion blocking before proceeding
    // This check is now unified with the consecutive forced completion logic below

    // Validate agent_type
    let agent_type = match args.agent_type.as_str() {
        "explorer" => SubagentType::Explorer,
        "coder" => SubagentType::Coder,
        _ => {
            return ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: format!(
                        "Invalid agent_type '{}'. Must be 'explorer' or 'coder'",
                        args.agent_type
                    ),
                    success: Some(false),
                },
            };
        }
    };

    // Check if multi-agent components are available
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

    // Check current agent state - if we're in AgentTaskCreated state, allow any subagent creation
    let current_agent_state = detect_agent_state(&sess.multi_agent_components).await;
    if matches!(current_agent_state, AgentState::AgentTaskCreated) {
        tracing::info!(
            "🆕 AGENT TASK CREATED STATE: Allowing {} subagent creation due to new user task",
            args.agent_type
        );

        // Skip all blocking logic and proceed directly to task creation
        let selected_context_refs = if !args.context_refs.is_empty() {
            args.context_refs
        } else {
            // Try to get some relevant contexts intelligently
            match multi_agent_components
                .context_repo
                .query_contexts(&crate::context_store::ContextQuery {
                    ids: None,
                    tags: None,
                    created_by: None,
                    limit: Some(5), // Limit to 5 contexts
                })
                .await
            {
                Ok(contexts) => {
                    let refs: Vec<String> = contexts.iter().map(|ctx| ctx.id.clone()).collect();
                    tracing::info!(
                        "Selected {} available contexts for subagent '{}': {:?}",
                        refs.len(),
                        args.title,
                        refs
                    );
                    refs
                }
                Err(e) => {
                    tracing::warn!(
                        "Failed to query contexts for subagent '{}', using empty context_refs: {}",
                        args.title,
                        e
                    );
                    Vec::new()
                }
            }
        };

        // Create the subagent task specification
        let task_spec = SubagentTaskSpec {
            agent_type,
            title: args.title.clone(),
            description: args.description.clone(),
            context_refs: selected_context_refs,
            bootstrap_paths: args.bootstrap_paths,
            max_turns: Some(30),       // Default to 100 turns
            timeout_ms: Some(1800000), // Default to 30 minutes timeout
        };

        // Create the task using the subagent manager
        match multi_agent_components
            .subagent_manager
            .create_task(task_spec)
            .await
        {
            Ok(task_id) => {
                tracing::debug!(
                    "Creating subagent task: task_id={task_id}, type={}, title={}",
                    args.agent_type,
                    args.title
                );
                tracing::info!(
                    "Creating subagent task: task_id={}, type={}, title={}",
                    task_id,
                    args.agent_type,
                    args.title
                );

                let mut response = format!(
                    "Successfully created {} subagent task '{}' with ID: {}",
                    args.agent_type, args.title, task_id
                );

                // Auto-launch if requested
                if args.auto_launch {
                    match multi_agent_components
                        .subagent_manager
                        .launch_subagent(&task_id)
                        .await
                    {
                        Ok(_handle) => {
                            tracing::info!("Auto-launched subagent: task_id={}", task_id);
                            response.push_str("\nSubagent launched and executing task...");

                            // Reset the flag when subagent is successfully launched
                            multi_agent_components
                                .new_user_task_created
                                .store(false, std::sync::atomic::Ordering::Relaxed);
                        }
                        Err(e) => {
                            let event = Event {
                                id: sub_id,
                                msg: EventMsg::Error(ErrorEvent {
                                    message: format!("Failed to launch subagent: {}", e),
                                }),
                            };
                            sess.send_event(event).await;
                        }
                    }
                }

                return ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content: response,
                        success: Some(true),
                    },
                };
            }
            Err(e) => {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(ErrorEvent {
                        message: format!("Failed to create subagent task: {}", e),
                    }),
                };
                sess.send_event(event).await;

                return ResponseInputItem::FunctionCallOutput {
                    call_id,
                    output: FunctionCallOutputPayload {
                        content: format!("Failed to create subagent task: {}", e),
                        success: Some(false),
                    },
                };
            }
        }
    }

    // Continue with normal state checking logic for non-AgentTaskCreated states
    // Intelligent subagent type checking: Prevent creating same-type subagents if the last one completed normally
    // Also prevent creating if there are too many consecutive forced completions
    match multi_agent_components
        .subagent_manager
        .get_recently_completed_tasks(10) // Get more tasks to check consecutive patterns
        .await
    {
        Ok(recent_tasks) => {
            // Count consecutive forced completions of the same type
            let mut consecutive_forced_completions = 0;

            for task in &recent_tasks {
                if task.agent_type == agent_type {
                    if let crate::subagent_manager::TaskStatus::Completed { result } = &task.status
                    {
                        let was_forced_completion =
                            result.metadata.reached_max_turns || result.metadata.force_completed;

                        if was_forced_completion {
                            consecutive_forced_completions += 1;
                        } else {
                            // Found a normal completion, stop counting
                            break;
                        }
                    }
                } else {
                    // Different type task breaks the consecutive pattern
                    break;
                }
            }

            // Check for consecutive forced completion limit (0 for both Explorer and Coder - block immediately after first forced completion)
            let max_consecutive_forced = match agent_type {
                SubagentType::Explorer => 0,
                SubagentType::Coder => 0,
            };

            if consecutive_forced_completions > max_consecutive_forced {
                tracing::warn!(
                    "Blocking creation of {} subagent '{}' due to {} consecutive forced completions (limit: {}). Will inject available contexts for summarization.",
                    args.agent_type,
                    args.title,
                    consecutive_forced_completions,
                    max_consecutive_forced
                );

                // When hitting consecutive limit, gather all available contexts and inject them for summarization
                let all_contexts =
                    match get_all_available_contexts(&multi_agent_components.context_repo).await {
                        Ok(context_ids) => {
                            let mut contexts = Vec::new();
                            for context_id in context_ids {
                                if let Ok(Some(context)) = multi_agent_components
                                    .context_repo
                                    .get_context(&context_id)
                                    .await
                                {
                                    contexts.push(context);
                                }
                            }
                            contexts
                        }
                        Err(e) => {
                            tracing::warn!("Failed to retrieve contexts for summarization: {}", e);
                            Vec::new()
                        }
                    };

                if !all_contexts.is_empty() {
                    tracing::info!(
                        "Injecting {} available contexts for summarization due to consecutive forced completion limit",
                        all_contexts.len()
                    );

                    // Create a comprehensive context summary message
                    let mut context_summary = format!(
                        "🚫 **Subagent Creation Blocked**: Cannot create another '{}' subagent because the previous '{}' subagent was forced to complete (reached turn limit).\n\n📊 **Analysis Pattern**: The previous '{}' subagent reached its turn limit without completing successfully, indicating this approach may not be effective for this task.\n\n📋 **Available Context Summary**: Here are all the findings and analysis results gathered so far:\n\n",
                        args.agent_type, args.agent_type, args.agent_type
                    );

                    for (i, context) in all_contexts.iter().enumerate() {
                        context_summary.push_str(&format!(
                            "## Context {}: {}\n\n**Summary:** {}\n\n**Details:**\n{}\n\n---\n\n",
                            i + 1,
                            context.id,
                            context.summary,
                            context.content
                        ));
                    }

                    let alternative_suggestion = match agent_type {
                        SubagentType::Explorer => {
                            "Consider creating a 'coder' subagent to implement changes based on the analysis above, or provide a comprehensive summary of the findings."
                        }
                        SubagentType::Coder => {
                            "Consider creating an 'explorer' subagent to gather more information, or provide a comprehensive summary of the implementation progress."
                        }
                    };

                    context_summary.push_str(&format!(
                        "🔄 **Recommended Next Steps:**\n{}\n\n💡 **Instructions**: Please provide a comprehensive summary and analysis of the above findings. Focus on:\n1. Key insights and discoveries\n2. Important patterns or issues identified\n3. Recommendations for next steps\n4. Overall assessment of the current progress\n\nThis summary will help determine the best path forward without repeating ineffective approaches.",
                        alternative_suggestion
                    ));

                    tracing::info!(
                        "📋 CONTEXT SUMMARY: Generated summary with {} contexts, total length: {} chars",
                        all_contexts.len(),
                        context_summary.len()
                    );
                    tracing::debug!("📋 FULL CONTEXT SUMMARY: {}", context_summary);

                    // Detect current state for the injected message
                    let current_state = detect_agent_state(&sess.multi_agent_components).await;
                    let state_info = if !current_state.can_create_explorer()
                        || !current_state.can_create_coder()
                    {
                        format!(
                            "**Current Agent State**: {}\n\n\
                            **Subagent Creation Constraints**:\n\
                            - Explorer Subagent: {}\n\
                            - Coder Subagent: {}\n\n\
                            **Note**: {}\n\n\
                            Please work with available information or consider alternative approaches instead of creating blocked subagent types.",
                            current_state.description(),
                            if current_state.can_create_explorer() {
                                "Allowed"
                            } else {
                                "Blocked"
                            },
                            if current_state.can_create_coder() {
                                "Allowed"
                            } else {
                                "Blocked"
                            },
                            if !current_state.can_create_explorer()
                                && !current_state.can_create_coder()
                            {
                                "All subagent creation is currently blocked"
                            } else if !current_state.can_create_explorer() {
                                "Explorer subagent creation is blocked due to previous forced completion"
                            } else {
                                "Coder subagent creation is blocked due to previous forced completion"
                            }
                        )
                    } else {
                        format!(
                            "**Current Agent State**: {}\n\
                            **Subagent Creation Constraints**: All subagent types are currently allowed",
                            current_state.description()
                        )
                    };

                    tracing::info!(
                        "🔍 STATE INJECTION: Injecting state information to LLM - Current State: {:?}, Explorer: {}, Coder: {}",
                        current_state,
                        if current_state.can_create_explorer() {
                            "Allowed"
                        } else {
                            "BLOCKED"
                        },
                        if current_state.can_create_coder() {
                            "Allowed"
                        } else {
                            "BLOCKED"
                        }
                    );

                    // Prepare the injection message
                    let injection_message = format!(
                        "🚫 **CRITICAL: ANALYSIS COMPLETE - SUMMARY REQUIRED**\n\nThe previous '{}' subagent was forced to complete, and you now have comprehensive analysis results above.\n\n{}\n\n**MANDATORY INSTRUCTION**: You must provide a final summary based on the context items above. Do NOT create any new subagents or use any tools. Your response should be a comprehensive text-only summary that synthesizes all the analysis provided.\n\n**IMPORTANT**: Due to the forced completion, creating another '{}' subagent is now BLOCKED. You must work with the available information and provide a summary instead of attempting further subagent creation.\n\nThis is a completion mode where your task is to summarize, not to delegate further work.",
                        args.agent_type, state_info, args.agent_type
                    );

                    tracing::info!(
                        "📝 INJECTING MESSAGE TO LLM: {} contexts available, message length: {} chars",
                        all_contexts.len(),
                        injection_message.len()
                    );
                    tracing::debug!("📝 FULL INJECTION MESSAGE: {}", injection_message);

                    // Inject the context into the session for the LLM to process
                    if let Err(contexts) = sess.inject_input(vec![InputItem::Text {
                        text: injection_message,
                    }]) {
                        tracing::warn!(
                            "Failed to inject context summary for LLM analysis: {:?}",
                            contexts
                        );
                    }

                    // Return a simple blocking message - the real content will be processed in the next turn with proper state info
                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!(
                                "🚫 **SUBAGENT CREATION BLOCKED** 🚫\n\nThe previous '{}' subagent was forced to complete. Comprehensive analysis results are being prepared and will be provided shortly.\n\n⚠️ **IMPORTANT**: Creating another '{}' subagent is currently blocked due to the forced completion. Please wait for the analysis summary.",
                                args.agent_type, args.agent_type
                            ),
                            success: Some(false),
                        },
                    };
                } else {
                    // No contexts available, return simple blocking message
                    let alternative_suggestion = match agent_type {
                        SubagentType::Explorer => {
                            "Consider creating a 'coder' subagent to implement changes, or try a different analytical approach."
                        }
                        SubagentType::Coder => {
                            "Consider creating an 'explorer' subagent to gather more information, or try a different implementation approach."
                        }
                    };

                    return ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!(
                                "🚫 Cannot create another '{}' subagent because the previous '{}' subagent was forced to complete (reached turn limit).\n\n📊 **Analysis Pattern:** The previous '{}' subagent reached its turn limit without completing successfully, indicating this approach may not be effective for this task.\n\n🔄 **Suggested alternatives:**\n{}\n\n💡 **Tip:** This limit prevents infinite loops. Try a different approach or request a summary of current progress.",
                                args.agent_type,
                                args.agent_type,
                                args.agent_type,
                                alternative_suggestion
                            ),
                            success: Some(false),
                        },
                    };
                }
            }

            // Original logic: Check if the last completed task was of the same type and completed normally
            if let Some(last_completed) = recent_tasks.first() {
                if last_completed.agent_type == agent_type {
                    if let crate::subagent_manager::TaskStatus::Completed { result } =
                        &last_completed.status
                    {
                        let was_forced_completion =
                            result.metadata.reached_max_turns || result.metadata.force_completed;

                        if !was_forced_completion {
                            // Last subagent of same type completed normally - suggest alternatives
                            let suggestion = match agent_type {
                                SubagentType::Explorer => {
                                    "Consider creating a 'coder' subagent to implement changes based on the exploration results, or request a summary of the existing analysis."
                                }
                                SubagentType::Coder => {
                                    "Consider creating an 'explorer' subagent to analyze additional files or areas, or request a summary of the existing implementation."
                                }
                            };

                            tracing::info!(
                                "Blocking creation of {} subagent '{}' because the last {} subagent '{}' completed normally. Suggesting alternatives.",
                                args.agent_type,
                                args.title,
                                args.agent_type,
                                last_completed.title
                            );

                            return ResponseInputItem::FunctionCallOutput {
                                call_id,
                                output: FunctionCallOutputPayload {
                                    content: format!(
                                        "⚠️ Cannot create another '{}' subagent because the previous '{}' subagent '{}' completed successfully.\n\n🔄 **Suggested alternatives:**\n{}\n\n💡 **Tip:** If you need to continue the same type of work, please provide more specific requirements or different focus areas.",
                                        args.agent_type,
                                        args.agent_type,
                                        last_completed.title,
                                        suggestion
                                    ),
                                    success: Some(false),
                                },
                            };
                        } else {
                            tracing::info!(
                                "Allowing creation of {} subagent '{}' because the last {} subagent '{}' completed normally (reached_max_turns: {}, force_completed: {})",
                                args.agent_type,
                                args.title,
                                args.agent_type,
                                last_completed.title,
                                result.metadata.reached_max_turns,
                                result.metadata.force_completed
                            );
                        }
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!("Failed to check recent tasks for type validation: {}", e);
            // Continue with creation if we can't check - don't block due to errors
        }
    }

    // Intelligent context selection: Let LLM choose relevant contexts for the new subagent
    // For now, we'll skip the LLM call and return empty context list
    // TODO: Implement proper LLM call with access to TurnContext
    let selected_context_refs =
        match get_all_available_contexts(&multi_agent_components.context_repo).await {
            Ok(refs) => {
                tracing::info!(
                    "Selected {} relevant contexts for subagent '{}': {:?}",
                    refs.len(),
                    args.title,
                    refs
                );
                refs
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to select contexts for subagent '{}', using provided context_refs: {}",
                    args.title,
                    e
                );
                args.context_refs
            }
        };

    // Create the subagent task specification
    let task_spec = SubagentTaskSpec {
        agent_type,
        title: args.title.clone(),
        description: args.description.clone(),
        context_refs: selected_context_refs,
        bootstrap_paths: args.bootstrap_paths,
        max_turns: Some(30),       // Default to 100 turns
        timeout_ms: Some(1800000), // Default to 30 minutes timeout
    };

    // Create the task using the subagent manager
    match multi_agent_components
        .subagent_manager
        .create_task(task_spec)
        .await
    {
        Ok(task_id) => {
            tracing::debug!(
                "Creating subagent task: task_id={task_id}, type={}, title={}",
                args.agent_type,
                args.title
            );
            tracing::info!(
                "Creating subagent task: task_id={}, type={}, title={}",
                task_id,
                args.agent_type,
                args.title
            );

            let mut response = format!(
                "Successfully created {} subagent task '{}' with ID: {}",
                args.agent_type, args.title, task_id
            );

            // Auto-launch if requested
            if args.auto_launch {
                match multi_agent_components
                    .subagent_manager
                    .launch_subagent(&task_id)
                    .await
                {
                    Ok(_handle) => {
                        tracing::info!("Auto-launched subagent: task_id={}", task_id);
                        response.push_str("\nSubagent launched and executing task...");

                        // Wait for subagent to complete
                        tracing::info!("Waiting for subagent to complete: task_id={}", task_id);

                        // Poll for completion with timeout and forced completion
                        //
                        // Timeout Strategy:
                        // 1. Poll every 4 seconds for up to 60 attempts (4 minutes total)
                        // 2. If timeout occurs, force complete the subagent to get partial results
                        // 3. Return whatever results are available at that point
                        // 4. Subagent process is terminated to free resources
                        //
                        // This approach ensures:
                        // - User gets immediate feedback (no hanging)
                        // - Partial work is not lost
                        // - Resources are properly cleaned up
                        // - Clear indication of timeout vs normal completion
                        //
                        // TODO: Future Enhancement - Async Notification System
                        // - Implement event-driven completion notifications
                        // - Allow subagents to complete in background and notify user later
                        // - Add persistent task tracking across sessions
                        // - Consider WebSocket or similar for real-time updates
                        let mut attempts = 0;
                        let max_attempts = 60; // 4 minutes with 4-second intervals
                        let poll_interval_ms = 4000; // 4 seconds between checks

                        tracing::info!(
                            "Starting subagent polling: max_attempts={}, interval={}ms, total_timeout={}s",
                            max_attempts,
                            poll_interval_ms,
                            (max_attempts * poll_interval_ms) / 1000
                        );

                        loop {
                            tokio::time::sleep(tokio::time::Duration::from_millis(
                                poll_interval_ms,
                            ))
                            .await;
                            attempts += 1;

                            tracing::debug!(
                                "Polling subagent status: attempt {}/{}, task_id={}",
                                attempts,
                                max_attempts,
                                task_id
                            );

                            match multi_agent_components
                                .subagent_manager
                                .get_task_status(&task_id)
                                .await
                            {
                                Ok(status) => {
                                    match status {
                                        crate::subagent_manager::TaskStatus::Completed {
                                            result,
                                        } => {
                                            tracing::info!(
                                                "Subagent completed successfully: task_id={}",
                                                task_id
                                            );

                                            // Log detailed context information
                                            if !result.contexts.is_empty() {
                                                tracing::info!(
                                                    "Subagent generated {} context items:",
                                                    result.contexts.len()
                                                );
                                                for (i, context) in
                                                    result.contexts.iter().enumerate()
                                                {
                                                    tracing::info!(
                                                        "Context #{}: id={}, summary={}",
                                                        i + 1,
                                                        context.id,
                                                        context.summary
                                                    );
                                                    tracing::info!(
                                                        "Context #{} content (length={}): {}",
                                                        i + 1,
                                                        context.content.len(),
                                                        if context.content.len() > 500 {
                                                            format!(
                                                                "{}...[truncated]",
                                                                &context.content[..500]
                                                            )
                                                        } else {
                                                            context.content.clone()
                                                        }
                                                    );
                                                }
                                            } else {
                                                tracing::info!(
                                                    "Subagent completed without generating context items"
                                                );
                                            }

                                            // Query context repository for ALL available contexts (not just current task)
                                            // This allows main agent to see the complete analysis history
                                            let all_contexts = match multi_agent_components
                                                .context_repo
                                                .query_contexts(
                                                    &crate::context_store::ContextQuery {
                                                        ids: None,
                                                        tags: None,
                                                        created_by: None,
                                                        limit: None,
                                                    },
                                                )
                                                .await
                                            {
                                                Ok(contexts) => contexts,
                                                Err(e) => {
                                                    tracing::error!(
                                                        "Failed to query all available contexts: {}",
                                                        e
                                                    );
                                                    Vec::new()
                                                }
                                            };

                                            // Also get the current task's contexts for comparison
                                            let current_task_contexts =
                                                match query_contexts_by_task_id(
                                                    &multi_agent_components.context_repo,
                                                    &task_id,
                                                )
                                                .await
                                                {
                                                    Ok(contexts) => contexts,
                                                    Err(e) => {
                                                        tracing::error!(
                                                            "Failed to query contexts for task {}: {}",
                                                            task_id,
                                                            e
                                                        );
                                                        Vec::new()
                                                    }
                                                };

                                            // Use all contexts for injection, but prioritize current task contexts
                                            let task_contexts = if !current_task_contexts.is_empty()
                                            {
                                                // If current task has contexts, include them plus all others
                                                let mut combined_contexts =
                                                    current_task_contexts.clone();
                                                for ctx in all_contexts {
                                                    // Add contexts that are not from the current task
                                                    if !current_task_contexts
                                                        .iter()
                                                        .any(|c| c.id == ctx.id)
                                                    {
                                                        combined_contexts.push(ctx);
                                                    }
                                                }
                                                combined_contexts
                                            } else {
                                                // If current task has no contexts, use all available contexts
                                                all_contexts
                                            };

                                            // Check subagent completion status to decide how to handle contexts
                                            let (should_inject_contexts, was_forced_completion) =
                                                if !task_contexts.is_empty() {
                                                    // Check if subagent completed normally or was forced to complete due to limits
                                                    let was_forced_completion =
                                                        result.metadata.reached_max_turns
                                                            || result.metadata.force_completed;

                                                    // Calculate total content length to assess context quality
                                                    let total_content_length: usize = task_contexts
                                                        .iter()
                                                        .map(|ctx| ctx.content.len())
                                                        .sum();

                                                    if was_forced_completion {
                                                        // When subagent was forced to complete, inject contexts but allow LLM to decide next steps
                                                        tracing::info!(
                                                            "Subagent '{}' was forced to complete (reached_max_turns: {}, force_completed: {}) with {} contexts ({} chars total). Will inject contexts for analysis and summary.",
                                                            args.title,
                                                            result.metadata.reached_max_turns,
                                                            result.metadata.force_completed,
                                                            task_contexts.len(),
                                                            total_content_length
                                                        );
                                                        (true, true) // Inject contexts, was forced completion
                                                    } else {
                                                        // When subagent completed normally, inject contexts and force summarization
                                                        tracing::info!(
                                                            "Subagent '{}' completed normally with {} contexts ({} chars total). Will inject contexts for LLM summarization and block new subagent creation.",
                                                            args.title,
                                                            task_contexts.len(),
                                                            total_content_length
                                                        );
                                                        (true, false) // Inject contexts, was not forced completion
                                                    }
                                                } else {
                                                    (false, false) // No contexts to inject
                                                };

                                            if should_inject_contexts {
                                                tracing::info!(
                                                    "Injecting {} context items (including {} from current task and {} from previous tasks) into conversation for LLM analysis",
                                                    task_contexts.len(),
                                                    current_task_contexts.len(),
                                                    task_contexts.len()
                                                        - current_task_contexts.len()
                                                );

                                                // Create a context summary message for the LLM
                                                let context_summary = if was_forced_completion {
                                                    format!(
                                                        "The {} subagent '{}' was forced to complete (reached max turns) but generated {} context items. Here are ALL available analysis findings ({} total contexts, including {} from current task and {} from previous tasks):\n\n",
                                                        args.agent_type,
                                                        args.title,
                                                        current_task_contexts.len(),
                                                        task_contexts.len(),
                                                        current_task_contexts.len(),
                                                        task_contexts.len()
                                                            - current_task_contexts.len()
                                                    )
                                                } else {
                                                    format!(
                                                        "The {} subagent '{}' has completed successfully and generated {} context items. Here are ALL available analysis findings ({} total contexts, including {} from current task and {} from previous tasks):\n\n",
                                                        args.agent_type,
                                                        args.title,
                                                        current_task_contexts.len(),
                                                        task_contexts.len(),
                                                        current_task_contexts.len(),
                                                        task_contexts.len()
                                                            - current_task_contexts.len()
                                                    )
                                                };

                                                let mut full_context_content = context_summary;

                                                for (i, context) in task_contexts.iter().enumerate()
                                                {
                                                    full_context_content.push_str(&format!(
                                                        "## Context Item {}: {}\n\n**Summary:** {}\n\n**Detailed Analysis:**\n{}\n\n---\n\n",
                                                        i + 1, context.id, context.summary, context.content
                                                    ));
                                                }

                                                full_context_content.push_str(&format!(
                                                    "\n**Task Execution Summary:**\n- Agent Type: {}\n- Task: {}\n- Turns: {}\n- Success: {}\n- Comments: {}\n- Forced Completion: {}\n\n",
                                                    args.agent_type, args.title, result.metadata.num_turns, result.success, result.comments, was_forced_completion
                                                ));

                                                let prompt_instructions = if was_forced_completion {
                                                    &format!(
                                                        "**CRITICAL INSTRUCTION: COMPREHENSIVE ANALYSIS AVAILABLE - PROVIDE SUMMARY ONLY**\n\n\
                                                        The subagent was forced to complete due to reaching maximum turns, but comprehensive analysis is now available from ALL context items above ({} total contexts). Your task is to:\n\n\
                                                        **REQUIRED ACTION: SUMMARIZE AND CONCLUDE**\n\
                                                        1. **Synthesize the findings** from all context items above\n\
                                                        2. **Provide a comprehensive summary** of key insights and discoveries\n\
                                                        3. **Present conclusions** to the user based on the complete analysis\n\
                                                        4. **Optionally store synthesized insights** using store_context if valuable for future reference\n\n\
                                                        **DO NOT:**\n\
                                                        - Create new subagents (comprehensive analysis is already available)\n\
                                                        - Call create_subagent_task (sufficient information is provided above)\n\
                                                        - Request additional analysis (all necessary context items are included)\n\n\
                                                        **NOTE:** As an orchestrator, you cannot execute shell commands directly. However, with {} context items available, you have comprehensive information to provide a complete analysis.\n\n\
                                                        **Your response should be a comprehensive text summary that synthesizes all the context items above. The analysis work is complete - now provide the final summary.**\n\n",
                                                        task_contexts.len(),
                                                        task_contexts.len()
                                                    )
                                                } else {
                                                    "**CRITICAL INSTRUCTION: ANALYSIS COMPLETE - PROVIDE SUMMARY ONLY**\n\n\
                                                    The subagent work is COMPLETE. The above context items contain comprehensive analysis results. Your task now is to:\n\n\
                                                    **REQUIRED ACTION: SUMMARIZE AND CONCLUDE**\n\
                                                    1. **Synthesize the findings** from all context items above\n\
                                                    2. **Provide a comprehensive summary** of key insights and discoveries\n\
                                                    3. **Present conclusions** to the user based on the analysis\n\
                                                    4. **Optionally store synthesized insights** using store_context if valuable for future reference\n\n\
                                                    **DO NOT:**\n\
                                                    - Create new subagents (analysis is already complete)\n\
                                                    - Call create_subagent_task (the work is done)\n\
                                                    - Request additional analysis (sufficient information is provided above)\n\n\
                                                    **FOCUS YOUR SUMMARY ON:**\n\
                                                    1. Key architectural insights from the context store analysis\n\
                                                    2. Important implementation patterns and design decisions\n\
                                                    3. Overall assessment of the context store's design and functionality\n\
                                                    4. Any notable strengths or areas for potential improvement\n\
                                                    5. How this analysis addresses the original user request\n\n\
                                                    **Your response should be a comprehensive text summary that synthesizes all the context items above. The analysis work is complete - now provide the final summary.**"
                                                };

                                                // Detect current state and add state information
                                                let current_state = detect_agent_state(
                                                    &sess.multi_agent_components,
                                                )
                                                .await;
                                                let state_info = if !current_state
                                                    .can_create_explorer()
                                                    || !current_state.can_create_coder()
                                                {
                                                    format!(
                                                        "\n**Current Agent State**: {}\n\n\
                                                        **Subagent Creation Constraints**:\n\
                                                        - Explorer Subagent: {}\n\
                                                        - Coder Subagent: {}\n\n\
                                                        **Note**: {}\n\n\
                                                        Please work with available information or consider alternative approaches instead of creating blocked subagent types.\n\n",
                                                        current_state.description(),
                                                        if current_state.can_create_explorer() {
                                                            "Allowed"
                                                        } else {
                                                            "Blocked"
                                                        },
                                                        if current_state.can_create_coder() {
                                                            "Allowed"
                                                        } else {
                                                            "Blocked"
                                                        },
                                                        if !current_state.can_create_explorer()
                                                            && !current_state.can_create_coder()
                                                        {
                                                            "All subagent creation is currently blocked"
                                                        } else if !current_state
                                                            .can_create_explorer()
                                                        {
                                                            "Explorer subagent creation is blocked due to previous forced completion"
                                                        } else {
                                                            "Coder subagent creation is blocked due to previous forced completion"
                                                        }
                                                    )
                                                } else {
                                                    format!(
                                                        "\n**Current Agent State**: {}\n\
                                                        **Subagent Creation Constraints**: All subagent types are currently allowed\n\n",
                                                        current_state.description()
                                                    )
                                                };

                                                tracing::info!(
                                                    "🔍 SUBAGENT COMPLETION STATE: Current State: {:?}, Explorer: {}, Coder: {}",
                                                    current_state,
                                                    if current_state.can_create_explorer() {
                                                        "Allowed"
                                                    } else {
                                                        "BLOCKED"
                                                    },
                                                    if current_state.can_create_coder() {
                                                        "Allowed"
                                                    } else {
                                                        "BLOCKED"
                                                    }
                                                );

                                                tracing::error!(
                                                    "🔥 DEBUG: Adding prompt instructions to context injection: {}",
                                                    prompt_instructions
                                                );
                                                tracing::error!(
                                                    "🔥 DEBUG: State info will be injected via system instructions, not context content: {}",
                                                    state_info
                                                );
                                                // Do NOT add state_info to context content - it should be handled by system instructions
                                                full_context_content.push_str(prompt_instructions);

                                                // 🔥 DEBUG: Log the complete context content being sent to LLM
                                                tracing::error!(
                                                    "🔥 DEBUG: COMPLETE CONTEXT CONTENT BEING SENT TO LLM (length={}): \n{}",
                                                    full_context_content.len(),
                                                    if full_context_content.len() > 3000 {
                                                        format!(
                                                            "{}...[TRUNCATED]...{}",
                                                            &full_context_content[..1500],
                                                            &full_context_content
                                                                [full_context_content.len()
                                                                    - 1500..]
                                                        )
                                                    } else {
                                                        full_context_content.clone()
                                                    }
                                                );

                                                // 🔥 DEBUG: Specifically check if state info is in the content
                                                if full_context_content
                                                    .contains("**CURRENT AGENT STATE**")
                                                {
                                                    tracing::error!(
                                                        "✅ STATE INFO CONFIRMED: State information is included in LLM message"
                                                    );
                                                } else {
                                                    tracing::error!(
                                                        "❌ STATE INFO MISSING: State information is NOT included in LLM message"
                                                    );
                                                }

                                                // 🔥 DEBUG: Log individual context items for verification
                                                for (i, context) in task_contexts.iter().enumerate()
                                                {
                                                    tracing::error!(
                                                        "🔥 DEBUG: Context Item #{}: id='{}', summary='{}', content_length={}",
                                                        i + 1,
                                                        context.id,
                                                        context.summary,
                                                        context.content.len()
                                                    );
                                                    tracing::error!(
                                                        "🔥 DEBUG: Context Item #{} content preview: {}",
                                                        i + 1,
                                                        if context.content.len() > 500 {
                                                            format!(
                                                                "{}...[truncated]",
                                                                &context.content[..500]
                                                            )
                                                        } else {
                                                            context.content.clone()
                                                        }
                                                    );
                                                }

                                                // Inject the context summary via inject_input to trigger proper turn processing with state info
                                                if let Err(contexts) =
                                                    sess.inject_input(vec![InputItem::Text {
                                                        text: full_context_content,
                                                    }])
                                                {
                                                    tracing::warn!(
                                                        "Failed to inject context summary for LLM analysis: {:?}",
                                                        contexts
                                                    );
                                                }

                                                // Return a function call output indicating completion - the real content will be processed in the next turn
                                                return ResponseInputItem::FunctionCallOutput {
                                                    call_id,
                                                    output: FunctionCallOutputPayload {
                                                        content: format!(
                                                            "✅ Subagent '{}' completed successfully. Analysis results are being prepared and will be provided shortly with proper state constraints.",
                                                            args.title
                                                        ),
                                                        success: Some(true),
                                                    },
                                                };
                                            } else {
                                                // No contexts generated, but still provide completion summary with intelligent guidance
                                                let completion_summary = format!(
                                                    "✅ {} subagent task '{}' completed successfully!\n\n📋 **Task Summary:**\n{}\n\n📊 **Execution Metadata:**\n- Turns: {}\n- Success: {}\n\nNo specific context items were generated, but the task completed successfully.",
                                                    args.agent_type,
                                                    args.title,
                                                    result.comments,
                                                    result.metadata.num_turns,
                                                    result.success
                                                );

                                                // Add intelligent guidance for next steps
                                                let guidance = match args.agent_type.as_str() {
                                                    "explorer" => {
                                                        "🔍 **Next Steps Suggestions:**\n- Consider creating a 'coder' subagent to implement changes based on the exploration\n- Request a summary of findings if you need to understand what was discovered\n- Create another 'explorer' subagent to analyze different files or aspects"
                                                    }
                                                    "coder" => {
                                                        "🛠️ **Next Steps Suggestions:**\n- Consider creating an 'explorer' subagent to analyze related files or verify the implementation\n- Request a summary of the changes made\n- Create another 'coder' subagent to work on different aspects of the implementation"
                                                    }
                                                    _ => {
                                                        "💡 **Next Steps Suggestions:**\n- Consider what type of subagent would be most helpful next\n- Request a summary of the completed work\n- Analyze the results and plan your next actions"
                                                    }
                                                };

                                                // Detect current state and add state information
                                                let current_state = detect_agent_state(
                                                    &sess.multi_agent_components,
                                                )
                                                .await;
                                                let state_info = if !current_state
                                                    .can_create_explorer()
                                                    || !current_state.can_create_coder()
                                                {
                                                    format!(
                                                        "\n**Current Agent State**: {}\n\n\
                                                        **Subagent Creation Constraints**:\n\
                                                        - Explorer Subagent: {}\n\
                                                        - Coder Subagent: {}\n\n\
                                                        **Note**: {}\n\n\
                                                        Please work with available information or consider alternative approaches instead of creating blocked subagent types.\n\n",
                                                        current_state.description(),
                                                        if current_state.can_create_explorer() {
                                                            "Allowed"
                                                        } else {
                                                            "Blocked"
                                                        },
                                                        if current_state.can_create_coder() {
                                                            "Allowed"
                                                        } else {
                                                            "Blocked"
                                                        },
                                                        if !current_state.can_create_explorer()
                                                            && !current_state.can_create_coder()
                                                        {
                                                            "All subagent creation is currently blocked"
                                                        } else if !current_state
                                                            .can_create_explorer()
                                                        {
                                                            "Explorer subagent creation is blocked due to previous forced completion"
                                                        } else {
                                                            "Coder subagent creation is blocked due to previous forced completion"
                                                        }
                                                    )
                                                } else {
                                                    format!(
                                                        "\n**Current Agent State**: {}\n\
                                                        **Subagent Creation Constraints**: All subagent types are currently allowed\n\n",
                                                        current_state.description()
                                                    )
                                                };

                                                tracing::info!(
                                                    "🔍 SUBAGENT COMPLETION STATE (No Contexts): Current State: {:?}, Explorer: {}, Coder: {}",
                                                    current_state,
                                                    if current_state.can_create_explorer() {
                                                        "Allowed"
                                                    } else {
                                                        "BLOCKED"
                                                    },
                                                    if current_state.can_create_coder() {
                                                        "Allowed"
                                                    } else {
                                                        "BLOCKED"
                                                    }
                                                );

                                                tracing::error!(
                                                    "🔥 DEBUG: State info will be injected via system instructions, not user message content: {}",
                                                    state_info
                                                );

                                                // Do NOT add state_info to user message - it should be handled by system instructions
                                                let full_message = format!(
                                                    "{}\n\n{}",
                                                    completion_summary, guidance
                                                );

                                                // Inject the completion summary via inject_input to trigger proper turn processing with state info
                                                if let Err(contexts) =
                                                    sess.inject_input(vec![InputItem::Text {
                                                        text: full_message,
                                                    }])
                                                {
                                                    tracing::warn!(
                                                        "Failed to inject completion summary for LLM analysis: {:?}",
                                                        contexts
                                                    );
                                                }

                                                // Return a function call output indicating completion - the real content will be processed in the next turn
                                                return ResponseInputItem::FunctionCallOutput {
                                                    call_id,
                                                    output: FunctionCallOutputPayload {
                                                        content: format!(
                                                            "✅ Subagent '{}' completed successfully. Summary and guidance are being prepared and will be provided shortly with proper state constraints.",
                                                            args.title
                                                        ),
                                                        success: Some(true),
                                                    },
                                                };
                                            }
                                        }
                                        crate::subagent_manager::TaskStatus::Failed { error } => {
                                            tracing::error!(
                                                "Subagent failed: task_id={}, error={}",
                                                task_id,
                                                error
                                            );
                                            response = format!("❌ Subagent task failed: {error}");
                                            break;
                                        }
                                        crate::subagent_manager::TaskStatus::Running {
                                            current_turn,
                                            max_turns,
                                        } => {
                                            tracing::debug!(
                                                "Subagent still running: task_id={}, turn={}/{}",
                                                task_id,
                                                current_turn,
                                                max_turns
                                            );
                                            // Check for timeout and force completion to get partial results
                                            if attempts >= max_attempts {
                                                tracing::warn!(
                                                    "Subagent execution timeout reached: task_id={}, forcing completion to get partial results",
                                                    task_id
                                                );

                                                // Force complete the subagent to get whatever results are available
                                                // This ensures we don't lose any work that has been done so far
                                                match multi_agent_components
                                                    .subagent_manager
                                                    .force_complete_task(&task_id)
                                                    .await
                                                {
                                                    Ok(forced_result) => {
                                                        tracing::info!(
                                                            "Subagent force completed: task_id={}, contexts={}, turns={}",
                                                            task_id,
                                                            forced_result.contexts.len(),
                                                            forced_result.metadata.num_turns
                                                        );

                                                        // Format forced completion response with partial results
                                                        response = format!(
                                                            "⏰ Subagent task '{}' was force completed due to timeout.\n\n",
                                                            args.title
                                                        );

                                                        if !forced_result.contexts.is_empty() {
                                                            response.push_str("📋 **Partial Results Available:**\n");
                                                            response.push_str(&format!(
                                                                "The subagent generated {} context items before timeout.\n\n",
                                                                forced_result.contexts.len()
                                                            ));

                                                            // Add context items summary
                                                            response.push_str(
                                                                "Generated context items:\n",
                                                            );
                                                            for (i, context) in forced_result
                                                                .contexts
                                                                .iter()
                                                                .enumerate()
                                                            {
                                                                response.push_str(&format!(
                                                                    "{}. **{}**: {}\n",
                                                                    i + 1,
                                                                    context.id,
                                                                    context.summary
                                                                ));
                                                            }

                                                            // Inject full context items for the main agent to use
                                                            response.push_str("\n📚 **Available Context Information:**\n");
                                                            response.push_str("The following context items are now available for analysis. ");
                                                            response.push_str("**Note: These context items are provided directly in this response - ");
                                                            response.push_str("no need to search for external files or use filesystem tools.**\n\n");

                                                            for context in
                                                                forced_result.contexts.iter()
                                                            {
                                                                response.push_str(&format!(
                                                    "### Context Item: {}\n**Summary:** {}\n\n**Full Analysis:**\n```\n{}\n```\n\n---\n\n",
                                                    context.id, context.summary, context.content
                                                ));
                                                            }
                                                        } else {
                                                            response.push_str("📋 **No Context Items Generated:**\n");
                                                            response.push_str("The subagent was interrupted before generating any context items.\n\n");
                                                        }

                                                        response.push_str(&format!(
                                                            "📊 **Execution Summary:**\n- Status: Force completed due to timeout\n- Turns completed: {}\n- Comments: {}\n",
                                                            forced_result.metadata.num_turns,
                                                            forced_result.comments
                                                        ));

                                                        // TODO: Future Enhancement - Background Completion Tracking
                                                        // - Implement event-driven completion notifications
                                                        // - Allow subagents to complete in background and notify user later
                                                        // - Add persistent task tracking across sessions
                                                        // - Consider WebSocket or similar for real-time updates
                                                        // - Store task ID for later completion checking
                                                        // - Implement notification when task actually completes
                                                        // - Allow user to query completion status later
                                                        response.push_str("\n💡 **Note:** This task was force completed due to timeout. ");
                                                        response.push_str("In the future, we plan to implement background completion tracking ");
                                                        response.push_str("so you can be notified when long-running tasks finish.");
                                                    }
                                                    Err(force_err) => {
                                                        tracing::error!(
                                                            "Failed to force complete subagent: task_id={}, error={}",
                                                            task_id,
                                                            force_err
                                                        );
                                                        response = format!(
                                                            "⏰ Subagent task timeout and force completion failed: {}",
                                                            force_err
                                                        );
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                        _ => {
                                            // Handle other states (Created, Cancelled, etc.)
                                            if attempts >= max_attempts {
                                                tracing::warn!(
                                                    "Subagent timeout in unexpected state: task_id={}, attempts={}",
                                                    task_id,
                                                    attempts
                                                );

                                                // Try to force complete even in unexpected states
                                                match multi_agent_components
                                                    .subagent_manager
                                                    .force_complete_task(&task_id)
                                                    .await
                                                {
                                                    Ok(forced_result) => {
                                                        tracing::info!(
                                                            "Subagent force completed from unexpected state: task_id={}, contexts={}",
                                                            task_id,
                                                            forced_result.contexts.len()
                                                        );
                                                        response = format!(
                                                            "⏰ Subagent task timeout (unexpected state) - force completed with {} context items",
                                                            forced_result.contexts.len()
                                                        );
                                                    }
                                                    Err(_) => {
                                                        response = format!(
                                                            "⏰ Subagent task timeout after {} attempts (unexpected state)",
                                                            max_attempts
                                                        );
                                                    }
                                                }
                                                break;
                                            }
                                        }
                                    }
                                }
                                Err(err) => {
                                    tracing::error!(
                                        "Failed to get subagent status: task_id={}, error={}",
                                        task_id,
                                        err
                                    );
                                    response =
                                        format!("❌ Failed to monitor subagent status: {err}");
                                    break;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        response.push_str(&format!("\n❌ Failed to auto-launch subagent: {err}"));
                        tracing::error!(
                            "Failed to auto-launch subagent: task_id={}, error={}",
                            task_id,
                            err
                        );
                    }
                }
            }

            ResponseInputItem::FunctionCallOutput {
                call_id,
                output: FunctionCallOutputPayload {
                    content: response,
                    success: Some(true),
                },
            }
        }
        Err(err) => ResponseInputItem::FunctionCallOutput {
            call_id,
            output: FunctionCallOutputPayload {
                content: format!("Failed to create subagent task: {err}"),
                success: Some(false),
            },
        },
    }
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
    let allowed_tools = get_openai_tools(
        &turn_context.tools_config,
        None, // Main agent cannot use MCP tools
        crate::openai_tools::AgentType::Main,
    );

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
            handle_create_subagent_task(sess, arguments, sub_id, call_id).await
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
                    // Unknown function: reply with structured failure so the model can adapt.
                    ResponseInputItem::FunctionCallOutput {
                        call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("unsupported call: {name}"),
                            success: None,
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
        // Use shlex::try_join to properly quote arguments containing special characters
        let command_string = shlex::try_join(params.command.iter().map(|s| s.as_str()))
            .unwrap_or_else(|_| params.command.join(" ")); // fallback to simple join if shlex fails
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
fn contains_shell_operators(command: &[String]) -> bool {
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
    if matches!(error, SandboxErr::Timeout) {
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

fn format_exec_output_str(exec_output: &ExecToolCallOutput) -> String {
    let ExecToolCallOutput {
        aggregated_output, ..
    } = exec_output;

    // Head+tail truncation for the model: show the beginning and end with an elision.
    // Clients still receive full streams; only this formatted summary is capped.

    let s = aggregated_output.text.as_str();
    let total_lines = s.lines().count();
    if s.len() <= MODEL_FORMAT_MAX_BYTES && total_lines <= MODEL_FORMAT_MAX_LINES {
        return s.to_string();
    }

    let lines: Vec<&str> = s.lines().collect();
    let head_take = MODEL_FORMAT_HEAD_LINES.min(lines.len());
    let tail_take = MODEL_FORMAT_TAIL_LINES.min(lines.len().saturating_sub(head_take));
    let omitted = lines.len().saturating_sub(head_take + tail_take);

    // Join head and tail blocks (lines() strips newlines; reinsert them)
    let head_block = lines
        .iter()
        .take(head_take)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let tail_block = if tail_take > 0 {
        lines[lines.len() - tail_take..].join("\n")
    } else {
        String::new()
    };
    let marker = format!("\n[... omitted {omitted} of {total_lines} lines ...]\n\n");

    // Byte budgets for head/tail around the marker
    let mut head_budget = MODEL_FORMAT_HEAD_BYTES.min(MODEL_FORMAT_MAX_BYTES);
    let tail_budget = MODEL_FORMAT_MAX_BYTES.saturating_sub(head_budget + marker.len());
    if tail_budget == 0 && marker.len() >= MODEL_FORMAT_MAX_BYTES {
        // Degenerate case: marker alone exceeds budget; return a clipped marker
        return take_bytes_at_char_boundary(&marker, MODEL_FORMAT_MAX_BYTES).to_string();
    }
    if tail_budget == 0 {
        // Make room for the marker by shrinking head
        head_budget = MODEL_FORMAT_MAX_BYTES.saturating_sub(marker.len());
    }

    // Enforce line-count cap by trimming head/tail lines
    let head_lines_text = head_block;
    let tail_lines_text = tail_block;
    // Build final string respecting byte budgets
    let head_part = take_bytes_at_char_boundary(&head_lines_text, head_budget);
    let mut result = String::with_capacity(MODEL_FORMAT_MAX_BYTES.min(s.len()));
    result.push_str(head_part);
    result.push_str(&marker);

    let remaining = MODEL_FORMAT_MAX_BYTES.saturating_sub(result.len());
    let tail_budget_final = remaining;
    let tail_part = take_last_bytes_at_char_boundary(&tail_lines_text, tail_budget_final);
    result.push_str(tail_part);

    result
}

// Truncate a &str to a byte budget at a char boundary (prefix)
#[inline]
fn take_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut last_ok = 0;
    for (i, ch) in s.char_indices() {
        let nb = i + ch.len_utf8();
        if nb > maxb {
            break;
        }
        last_ok = nb;
    }
    &s[..last_ok]
}

// Take a suffix of a &str within a byte budget at a char boundary
#[inline]
fn take_last_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut start = s.len();
    let mut used = 0usize;
    for (i, ch) in s.char_indices().rev() {
        let nb = ch.len_utf8();
        if used + nb > maxb {
            break;
        }
        start = i;
        used += nb;
        if start == 0 {
            break;
        }
    }
    &s[start..]
}

/// Exec output is a pre-serialized JSON payload
fn format_exec_output(exec_output: &ExecToolCallOutput) -> String {
    let ExecToolCallOutput {
        exit_code,
        duration,
        ..
    } = exec_output;

    #[derive(Serialize)]
    struct ExecMetadata {
        exit_code: i32,
        duration_seconds: f32,
    }

    #[derive(Serialize)]
    struct ExecOutput<'a> {
        output: &'a str,
        metadata: ExecMetadata,
    }

    // round to 1 decimal place
    let duration_seconds = ((duration.as_secs_f32()) * 10.0).round() / 10.0;

    let formatted_output = format_exec_output_str(exec_output);

    let payload = ExecOutput {
        output: &formatted_output,
        metadata: ExecMetadata {
            exit_code: *exit_code,
            duration_seconds,
        },
    };

    #[expect(clippy::expect_used)]
    serde_json::to_string(&payload).expect("serialize ExecOutput")
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

async fn drain_to_completed(
    sess: &Session,
    turn_context: &TurnContext,
    sub_id: &str,
    prompt: &Prompt,
) -> CodexResult<()> {
    let mut stream = turn_context.client.clone().stream(prompt).await?;
    loop {
        let maybe_event = stream.next().await;
        let Some(event) = maybe_event else {
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => {
                // Record only to in-memory conversation history; avoid state snapshot.
                let mut state = sess.state.lock_unchecked();
                state.history.record_items(std::slice::from_ref(&item));
            }
            Ok(ResponseEvent::Completed {
                response_id: _,
                token_usage,
            }) => {
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

                sess.tx_event
                    .send(Event {
                        id: sub_id.to_string(),
                        msg: EventMsg::TokenCount(crate::protocol::TokenCountEvent { info }),
                    })
                    .await
                    .ok();

                return Ok(());
            }
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }
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
}
