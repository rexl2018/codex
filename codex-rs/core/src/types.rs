use std::collections::HashMap;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use crate::subagent_manager::SubagentTaskSpec;
use crate::subagent_manager::ISubagentManager;
use codex_protocol::protocol::FileChange;
use codex_protocol::protocol::SubagentType;

/// Agent state based on recent subagent execution history
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum AgentState {
    /// No subagent execution history exists
    Initialization,
    /// A new user instruction has been received and a new agent task has been created.
    AgentTaskCreated,
    /// An Explorer subagent has been created and is currently running
    ExplorerCreated,
    /// A Coder subagent has been created and is currently running
    CoderCreated,
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
            AgentState::ExplorerCreated => {
                "Explorer Created - An Explorer subagent has been created and is currently running"
            }
            AgentState::CoderCreated => {
                "Coder Created - A Coder subagent has been created and is currently running"
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
            AgentState::ExplorerCreated => false,
            AgentState::CoderCreated => false,
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
            AgentState::ExplorerCreated => false,
            AgentState::CoderCreated => false,
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

/// Detect current agent state based on recent subagent execution history
pub async fn detect_agent_state(multi_agent_components: &Option<crate::session::MultiAgentComponents>) -> AgentState {
    let Some(components) = multi_agent_components else {
        return AgentState::Initialization;
    };

    // First check for currently running subagents (highest priority)
    match ISubagentManager::get_active_tasks(components.subagent_manager.as_ref()).await {
        Ok(active_tasks) => {
            if let Some(active_task) = active_tasks.first() {
                // If there's an active task, return the appropriate Created state
                match active_task.agent_type {
                    SubagentType::Explorer => return AgentState::ExplorerCreated,
                    SubagentType::Coder => return AgentState::CoderCreated,
                }
            }
        }
        Err(_) => {
            // Continue to check other states if we can't get active tasks
        }
    }

    // Then check if a new user task has been created (only if no active subagents)
    if components
        .new_user_task_created
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return AgentState::AgentTaskCreated;
    }

    match ISubagentManager::get_recently_completed_tasks(components.subagent_manager.as_ref(), 1)
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