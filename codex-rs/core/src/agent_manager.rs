use std::sync::Arc;
use crate::agent::Agent;
use crate::main_agent::MainAgent;
use crate::sub_agent::SubAgent;
use crate::codex::{AgentTask, TurnContext, Session};
use crate::error::CodexErr;
use codex_protocol::protocol::{InputItem, Submission, Op};

/// Unified agent manager that coordinates between different agent types
pub struct AgentManager {
    main_agent: MainAgent,
    sub_agent: SubAgent,
    session: Arc<Session>,
}

impl AgentManager {
    pub fn new(session: Arc<Session>) -> Self {
        let main_agent = MainAgent::new(session.clone());
        let sub_agent = SubAgent::new(session.clone());
        
        Self {
            main_agent,
            sub_agent,
            session,
        }
    }

    /// Route a submission to the appropriate agent
    pub fn route_submission(&self, submission: Submission) -> AgentRoutingResult {
        // First, try MainAgent for session-level operations
        if self.main_agent.handle_session_operation(submission.clone()) {
            return AgentRoutingResult::HandledByMainAgent;
        }

        // Then, try SubAgent for subagent-specific operations
        if self.sub_agent.handle_subagent_operation(submission.clone()) {
            return AgentRoutingResult::HandledBySubAgent;
        }

        // Check if this is a user input operation that should create a task
        match submission.op {
            Op::UserInput { .. } | Op::UserTurn { .. } => {
                AgentRoutingResult::RequiresTaskCreation(submission)
            }
            _ => {
                AgentRoutingResult::Unhandled(submission)
            }
        }
    }

    /// Get the main agent
    pub fn get_main_agent(&self) -> &MainAgent {
        &self.main_agent
    }

    /// Get the sub agent
    pub fn get_sub_agent(&self) -> &SubAgent {
        &self.sub_agent
    }

    /// Get the session
    pub fn get_session(&self) -> &Arc<Session> {
        &self.session
    }
}

/// Result of routing a submission to agents
#[derive(Debug)]
pub enum AgentRoutingResult {
    /// The submission was handled by the main agent
    HandledByMainAgent,
    /// The submission was handled by the sub agent
    HandledBySubAgent,
    /// The submission requires task creation (user input operations)
    RequiresTaskCreation(Submission),
    /// The submission was not handled by any agent
    Unhandled(Submission),
}

impl Agent for AgentManager {
    fn handle_submission(&self, submission: Submission) -> Result<AgentTask, anyhow::Error> {
        // AgentManager doesn't directly handle submissions, it routes them
        // This method is primarily for compatibility with the Agent trait
        match self.route_submission(submission) {
            AgentRoutingResult::RequiresTaskCreation(sub) => {
                // For user input operations, delegate to the main agent
                self.main_agent.handle_submission(sub)
            }
            _ => {
                Err(anyhow::anyhow!("Submission was routed but doesn't create a task"))
            }
        }
    }

    async fn run_turn(&self, turn_context: &TurnContext, input: Vec<InputItem>) -> Result<(), CodexErr> {
        // AgentManager doesn't run turns directly
        // Individual agents handle their own turn execution
        Ok(())
    }
}