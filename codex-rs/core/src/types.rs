use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::subagent_manager::SubagentTaskSpec;
use crate::subagent_manager::ISubagentManager;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::SubagentType;

/// Unified agent state that combines execution status and orchestration workflow
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    /// The agent is idle and waiting for new user input.
    Idle,

    /// The agent is actively deciding the next step, likely involving an LLM call.
    DecidingNextStep,

    /// The agent has dispatched a sub-agent and is waiting for it to complete.
    WaitingForSubagent { subagent_id: String },

    /// All tasks are complete, and the agent is summarizing the final result.
    Summarizing,
}

impl AgentState {
    /// Get a human-readable description of the current state
    pub fn description(&self) -> &'static str {
        match self {
            AgentState::Idle => "Idle - The agent is waiting for new user input",
            AgentState::DecidingNextStep => "Deciding Next Step - The agent is actively deciding the next action",
            AgentState::WaitingForSubagent { .. } => "Waiting for Subagent - The agent has dispatched a sub-agent and is waiting for completion",
            AgentState::Summarizing => "Summarizing - The agent is compiling the final result",
        }
    }



    /// Check if creating an Explorer subagent is allowed in this state
    pub fn can_create_explorer(&self) -> bool {
        match self {
            AgentState::Idle | AgentState::DecidingNextStep => true,
            AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
        }
    }

    /// Check if creating a Coder subagent is allowed in this state
    pub fn can_create_coder(&self) -> bool {
        match self {
            AgentState::Idle | AgentState::DecidingNextStep => true,
            AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
        }
    }

    /// Check if the agent can transition to the summarizing state
    pub fn can_summarize(&self) -> bool {
        match self {
            AgentState::DecidingNextStep => true,
            AgentState::Idle | AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
        }
    }

    /// Check if the agent is currently busy (not idle)
    pub fn is_busy(&self) -> bool {
        !matches!(self, AgentState::Idle)
    }


}

/// Detect current agent state based on the unified state manager
pub async fn detect_agent_state(multi_agent_components: &Option<crate::session::MultiAgentComponents>) -> AgentState {
    let Some(components) = multi_agent_components else {
        return AgentState::Idle;
    };

    // Use the unified state manager
    let current_state = components.state_manager.get_state();
    
    match current_state {
        AgentState::Idle | AgentState::DecidingNextStep | AgentState::Summarizing => {
            current_state
        }
        AgentState::WaitingForSubagent { ref subagent_id } => {
            // Verify the specific subagent is still running
            match ISubagentManager::get_active_tasks(components.subagent_manager.as_ref()).await {
                Ok(active_tasks) => {
                    // Check if the specific subagent we're waiting for is still active
                    let subagent_still_active = active_tasks.iter().any(|task| &task.task_id == subagent_id);
                    
                    if subagent_still_active {
                        tracing::debug!("Subagent {} is still active, maintaining WaitingForSubagent state", subagent_id);
                        current_state
                    } else {
                        // The specific subagent we were waiting for is no longer active
                        tracing::info!("Subagent {} is no longer active, transitioning to DecidingNextStep", subagent_id);
                        components.state_manager.transition_to_deciding_next_step();
                        AgentState::DecidingNextStep
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to get active tasks: {}, assuming subagent completed", e);
                    // Error checking tasks, assume we need to decide next step
                    components.state_manager.transition_to_deciding_next_step();
                    AgentState::DecidingNextStep
                }
            }
        }
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