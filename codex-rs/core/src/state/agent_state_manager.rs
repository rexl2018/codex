use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::Mutex;

use codex_protocol::models::ResponseInputItem;
use codex_protocol::protocol::ReviewDecision;
use tokio::sync::oneshot;
use tracing::info;

use crate::agent_task::AgentTask;
use crate::conversation_history::ConversationHistory;
use crate::protocol::TokenUsageInfo;
use crate::types::AgentState;

/// Mutable state of the agent
#[derive(Default)]
pub(crate) struct State {
    pub(crate) approved_commands: HashSet<Vec<String>>,
    pub(crate) current_task: Option<AgentTask>,
    pub(crate) pending_approvals: HashMap<String, oneshot::Sender<ReviewDecision>>,
    pub(crate) pending_input: Vec<ResponseInputItem>,
    pub(crate) history: ConversationHistory,
    pub(crate) token_info: Option<TokenUsageInfo>,
}

/// Unified Agent State Manager - The single source of truth for agent orchestration state
///
/// This manager combines both the physical execution status and orchestration workflow state
/// into a single, unified state machine as described in the agent orchestration design.
#[derive(Debug, Clone)]
pub struct AgentStateManager {
    state: Arc<Mutex<AgentState>>,
}

impl AgentStateManager {
    /// Create a new AgentStateManager with the initial Idle state
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(AgentState::Idle)),
        }
    }

    /// Create a new AgentStateManager with a specific initial state
    pub fn with_initial_state(initial_state: AgentState) -> Self {
        Self {
            state: Arc::new(Mutex::new(initial_state)),
        }
    }

    /// Set the agent state and log the transition
    ///
    /// This is the single point for all state changes and ensures observability
    /// by logging every state transition using the info! macro as required.
    pub fn set_state(&self, new_state: AgentState) {
        let mut state = self.state.lock().unwrap();
        let old_state = state.clone();

        if *state != new_state {
            *state = new_state.clone();
            // Centralized logging for every state transition as required by the design
            info!(
                "Agent state transition: {:?} -> {:?}",
                old_state, new_state
            );
        }
    }

    /// Get the current agent state
    pub fn get_state(&self) -> AgentState {
        self.state.lock().unwrap().clone()
    }

    /// Check if the agent can create an Explorer subagent in the current state
    pub fn can_create_explorer(&self) -> bool {
        self.get_state().can_create_explorer()
    }

    /// Check if the agent can create a Coder subagent in the current state
    pub fn can_create_coder(&self) -> bool {
        self.get_state().can_create_coder()
    }

    /// Check if the agent can transition to summarizing in the current state
    pub fn can_summarize(&self) -> bool {
        self.get_state().can_summarize()
    }

    /// Check if the agent is currently busy (not idle)
    pub fn is_busy(&self) -> bool {
        self.get_state().is_busy()
    }

    /// Transition to DecidingNextStep state
    ///
    /// This is typically called when:
    /// - User input is received
    /// - A subagent completes and the main agent needs to decide next steps
    pub fn transition_to_deciding_next_step(&self) {
        self.set_state(AgentState::DecidingNextStep);
    }

    /// Transition to WaitingForSubagent state
    ///
    /// This is called when a subagent is successfully created and launched
    pub fn transition_to_waiting_for_subagent(&self, subagent_id: String) {
        self.set_state(AgentState::WaitingForSubagent { subagent_id });
    }

    /// Transition to Summarizing state
    ///
    /// This is called when all subagent work is complete and final results need to be compiled
    pub fn transition_to_summarizing(&self) {
        self.set_state(AgentState::Summarizing);
    }

    /// Transition to Idle state
    ///
    /// This is called when all work is complete and the agent is ready for new input
    pub fn transition_to_idle(&self) {
        self.set_state(AgentState::Idle);
    }

    /// Get a human-readable description of the current state
    pub fn get_state_description(&self) -> String {
        self.get_state().description().to_string()
    }

    /// Handle subagent completion and determine appropriate next state
    ///
    /// This method encapsulates the logic for determining whether to continue
    /// with more subagents or transition to summarizing
    pub fn handle_subagent_completion(&self, subagent_id: &str, should_summarize: bool) {
        let current_state = self.get_state();

        info!(
            "🔄 SUBAGENT COMPLETION: Handling completion for subagent '{}', current state: {:?}, should_summarize: {}",
            subagent_id, current_state, should_summarize
        );

        // Verify we're in the expected state
        if let AgentState::WaitingForSubagent {
            subagent_id: ref current_id,
        } = current_state
        {
            if current_id == subagent_id {
                if should_summarize {
                    info!(
                        "🔄 SUBAGENT COMPLETION: Transitioning to Summarizing for subagent '{}'",
                        subagent_id
                    );
                    self.transition_to_summarizing();
                } else {
                    info!(
                        "🔄 SUBAGENT COMPLETION: Transitioning to DecidingNextStep for subagent '{}'",
                        subagent_id
                    );
                    self.transition_to_deciding_next_step();
                }
            } else {
                info!(
                    "⚠️ SUBAGENT COMPLETION MISMATCH: Expected '{}', got '{}' in state {:?}",
                    current_id, subagent_id, current_state
                );
            }
        } else {
            info!(
                "⚠️ UNEXPECTED SUBAGENT COMPLETION: Received completion for '{}' while in state {:?}",
                subagent_id, current_state
            );
            // Force transition to DecidingNextStep to recover from inconsistent state
            info!(
                "🔄 RECOVERY: Force transitioning to DecidingNextStep to recover from inconsistent state"
            );
            self.transition_to_deciding_next_step();
        }
    }
}

impl Default for AgentStateManager {
    fn default() -> Self {
        Self::new()
    }
}
