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

    // Legacy states for backward compatibility - these will be mapped to the unified states
    /// No subagent execution history exists (maps to Idle)
    #[deprecated(note = "Use Idle instead")]
    Initialization,
    /// A new user instruction has been received and a new agent task has been created (maps to DecidingNextStep)
    #[deprecated(note = "Use DecidingNextStep instead")]
    AgentTaskCreated,
    /// An Explorer subagent has been created and is currently running (maps to WaitingForSubagent)
    #[deprecated(note = "Use WaitingForSubagent instead")]
    ExplorerCreated,
    /// A Coder subagent has been created and is currently running (maps to WaitingForSubagent)
    #[deprecated(note = "Use WaitingForSubagent instead")]
    CoderCreated,
    /// Last Explorer subagent completed successfully (maps to DecidingNextStep)
    #[deprecated(note = "Use DecidingNextStep instead")]
    ExplorerNormalCompletion,
    /// Last Explorer subagent was forced to complete (maps to DecidingNextStep)
    #[deprecated(note = "Use DecidingNextStep instead")]
    ExplorerForcedCompletion,
    /// Last Coder subagent completed successfully (maps to DecidingNextStep or Summarizing)
    #[deprecated(note = "Use DecidingNextStep or Summarizing instead")]
    CoderNormalCompletion,
    /// Last Coder subagent was forced to complete (maps to DecidingNextStep or Summarizing)
    #[deprecated(note = "Use DecidingNextStep or Summarizing instead")]
    CoderForcedCompletion,
}

impl AgentState {
    /// Get a human-readable description of the current state
    pub fn description(&self) -> &'static str {
        match self {
            // Unified states
            AgentState::Idle => "Idle - The agent is waiting for new user input",
            AgentState::DecidingNextStep => "Deciding Next Step - The agent is actively deciding the next action",
            AgentState::WaitingForSubagent { subagent_id } => "Waiting for Subagent - The agent has dispatched a sub-agent and is waiting for completion",
            AgentState::Summarizing => "Summarizing - The agent is compiling the final result",
            
            // Legacy states (deprecated)
            #[allow(deprecated)]
            AgentState::Initialization => "Initialization - No subagent execution history",
            #[allow(deprecated)]
            AgentState::AgentTaskCreated => {
                "AgentTaskCreated - A new user instruction has been received and a new agent task has been created."
            }
            #[allow(deprecated)]
            AgentState::ExplorerCreated => {
                "Explorer Created - An Explorer subagent has been created and is currently running"
            }
            #[allow(deprecated)]
            AgentState::CoderCreated => {
                "Coder Created - A Coder subagent has been created and is currently running"
            }
            #[allow(deprecated)]
            AgentState::ExplorerNormalCompletion => {
                "Explorer Normal Completion - Last Explorer subagent completed successfully"
            }
            #[allow(deprecated)]
            AgentState::ExplorerForcedCompletion => {
                "Explorer Forced Completion - Last Explorer subagent reached turn limit"
            }
            #[allow(deprecated)]
            AgentState::CoderNormalCompletion => {
                "Coder Normal Completion - Last Coder subagent completed successfully"
            }
            #[allow(deprecated)]
            AgentState::CoderForcedCompletion => {
                "Coder Forced Completion - Last Coder subagent reached turn limit"
            }
        }
    }

    /// Convert legacy states to unified states
    pub fn to_unified(&self) -> AgentState {
        match self {
            // Already unified states
            AgentState::Idle | AgentState::DecidingNextStep | AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => self.clone(),
            
            // Legacy state mappings
            #[allow(deprecated)]
            AgentState::Initialization => AgentState::Idle,
            #[allow(deprecated)]
            AgentState::AgentTaskCreated => AgentState::DecidingNextStep,
            #[allow(deprecated)]
            AgentState::ExplorerCreated => AgentState::WaitingForSubagent { subagent_id: "unknown".to_string() },
            #[allow(deprecated)]
            AgentState::CoderCreated => AgentState::WaitingForSubagent { subagent_id: "unknown".to_string() },
            #[allow(deprecated)]
            AgentState::ExplorerNormalCompletion | AgentState::ExplorerForcedCompletion => AgentState::DecidingNextStep,
            #[allow(deprecated)]
            AgentState::CoderNormalCompletion | AgentState::CoderForcedCompletion => AgentState::DecidingNextStep,
        }
    }

    /// Check if creating an Explorer subagent is allowed in this state
    pub fn can_create_explorer(&self) -> bool {
        match self.to_unified() {
            AgentState::Idle | AgentState::DecidingNextStep => true,
            AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
            // These should not occur after to_unified(), but handle them for completeness
            #[allow(deprecated)]
            AgentState::Initialization | AgentState::AgentTaskCreated => true,
            #[allow(deprecated)]
            AgentState::ExplorerCreated | AgentState::CoderCreated => false,
            #[allow(deprecated)]
            AgentState::ExplorerNormalCompletion | AgentState::ExplorerForcedCompletion => false,
            #[allow(deprecated)]
            AgentState::CoderNormalCompletion | AgentState::CoderForcedCompletion => true,
        }
    }

    /// Check if creating a Coder subagent is allowed in this state
    pub fn can_create_coder(&self) -> bool {
        match self.to_unified() {
            AgentState::Idle | AgentState::DecidingNextStep => true,
            AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
            // These should not occur after to_unified(), but handle them for completeness
            #[allow(deprecated)]
            AgentState::Initialization | AgentState::AgentTaskCreated => true,
            #[allow(deprecated)]
            AgentState::ExplorerCreated | AgentState::CoderCreated => false,
            #[allow(deprecated)]
            AgentState::ExplorerNormalCompletion | AgentState::ExplorerForcedCompletion => true,
            #[allow(deprecated)]
            AgentState::CoderNormalCompletion | AgentState::CoderForcedCompletion => false,
        }
    }

    /// Check if the agent can transition to the summarizing state
    pub fn can_summarize(&self) -> bool {
        match self.to_unified() {
            AgentState::DecidingNextStep => true,
            AgentState::Idle | AgentState::WaitingForSubagent { .. } | AgentState::Summarizing => false,
            // These should not occur after to_unified(), but handle them for completeness
            #[allow(deprecated)]
            AgentState::Initialization => false,
            #[allow(deprecated)]
            AgentState::AgentTaskCreated => true,
            #[allow(deprecated)]
            AgentState::ExplorerCreated | AgentState::CoderCreated => false,
            #[allow(deprecated)]
            AgentState::ExplorerNormalCompletion | AgentState::ExplorerForcedCompletion => true,
            #[allow(deprecated)]
            AgentState::CoderNormalCompletion | AgentState::CoderForcedCompletion => true,
        }
    }

    /// Check if the agent is currently busy (not idle)
    pub fn is_busy(&self) -> bool {
        !matches!(self.to_unified(), AgentState::Idle)
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
            (
                AgentState::ExplorerCreated | AgentState::CoderCreated,
                _,
            ) => {
                "A subagent is currently running. Please wait for it to complete before creating another subagent."
            }
            _ => "No alternatives needed - action should be allowed.",
        }
    }
}

/// Detect current agent state based on the unified state manager
/// 
/// This function now primarily delegates to the unified state manager,
/// but maintains backward compatibility by checking subagent status when needed.
pub async fn detect_agent_state(multi_agent_components: &Option<crate::session::MultiAgentComponents>) -> AgentState {
    let Some(components) = multi_agent_components else {
        #[allow(deprecated)]
        return AgentState::Initialization;
    };

    // Use the unified state manager if available
    let current_state = components.state_manager.get_state();
    
    // For unified states, return them directly with validation
    match current_state {
        AgentState::Idle | AgentState::DecidingNextStep | AgentState::Summarizing => {
            return current_state;
        }
        AgentState::WaitingForSubagent { ref subagent_id } => {
            // Verify the specific subagent is still running
            match ISubagentManager::get_active_tasks(components.subagent_manager.as_ref()).await {
                Ok(active_tasks) => {
                    // Check if the specific subagent we're waiting for is still active
                    let subagent_still_active = active_tasks.iter().any(|task| &task.task_id == subagent_id);
                    
                    if subagent_still_active {
                        tracing::debug!("Subagent {} is still active, maintaining WaitingForSubagent state", subagent_id);
                        return current_state;
                    } else {
                        // The specific subagent we were waiting for is no longer active
                        tracing::info!("Subagent {} is no longer active, transitioning to DecidingNextStep", subagent_id);
                        components.state_manager.transition_to_deciding_next_step();
                        return AgentState::DecidingNextStep;
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to get active tasks: {}, assuming subagent completed", e);
                    // Error checking tasks, assume we need to decide next step
                    components.state_manager.transition_to_deciding_next_step();
                    return AgentState::DecidingNextStep;
                }
            }
        }
        // Handle legacy states by converting them to unified states
        _ => {
            let unified_state = current_state.to_unified();
            components.state_manager.set_state(unified_state.clone());
            return unified_state;
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