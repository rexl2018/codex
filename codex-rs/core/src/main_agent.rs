use std::sync::Arc;
use crate::agent::Agent;
use crate::agent_task_factory::AgentTaskFactory;
use crate::agent_task::AgentTask;
use crate::session::Session;
use crate::turn_context::TurnContext;
use crate::error::CodexErr;
use codex_protocol::protocol::{InputItem, Submission, Op, ReviewDecision};

pub struct MainAgent {
    session: Arc<Session>,
}

impl MainAgent {
    pub fn new(session: Arc<Session>) -> Self {
        Self { session }
    }

    /// Handle UserInput operations
    pub fn handle_user_input(&self, turn_context: Arc<TurnContext>, sub_id: String, items: Vec<InputItem>) -> Result<AgentTask, anyhow::Error> {
        // attempt to inject input into current task
        if let Err(items) = self.session.inject_input(items) {
            // no current task, spawn a new one
            tracing::debug!("Creating agent task for UserInput: sub_id={}", sub_id);
            tracing::info!("Creating agent task for UserInput: sub_id={}", sub_id);

            let task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context.clone(),
                sub_id.clone(),
                items,
            );
            
            // Set the task in the session
            self.session.set_task(task);
            
            // Return a new task for the caller
            let new_task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context,
                sub_id,
                vec![], // Empty input since the task is already set
            );
            Ok(new_task)
        } else {
            // Input was injected into existing task, no new task needed
            Err(anyhow::anyhow!("Input injected into existing task"))
        }
    }

    /// Handle UserTurn operations  
    pub fn handle_user_turn(
        &self,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        items: Vec<InputItem>,
    ) -> Result<AgentTask, anyhow::Error> {
        // attempt to inject input into current task
        if let Err(items) = self.session.inject_input(items) {
            let task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context.clone(),
                sub_id.clone(),
                items,
            );
            
            // Set the task in the session
            self.session.set_task(task);
            
            // Return a new task for the caller
            let new_task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context,
                sub_id,
                vec![], // Empty input since the task is already set
            );
            Ok(new_task)
        } else {
            // Input was injected into existing task, no new task needed
            Err(anyhow::anyhow!("Input injected into existing task"))
        }
    }

    /// Handle session-level operations that don't require agent delegation
    pub fn handle_session_operation(&self, submission: Submission) -> bool {
        match submission.op {
            Op::Interrupt => {
                self.session.interrupt_task();
                true
            }
            Op::ExecApproval { id, decision } => {
                match decision {
                    ReviewDecision::Abort => {
                        self.session.interrupt_task();
                    }
                    other => {
                        self.session.notify_approval(&id, other);
                    }
                }
                true
            }
            Op::PatchApproval { id, decision } => {
                match decision {
                    ReviewDecision::Abort => {
                        self.session.interrupt_task();
                    }
                    other => {
                        self.session.notify_approval(&id, other);
                    }
                }
                true
            }
            Op::Shutdown => {
                // This should be handled by the session itself
                false
            }
            _ => false
        }
    }
}

impl Agent for MainAgent {
    fn handle_submission(&self, submission: Submission) -> Result<AgentTask, anyhow::Error> {
        match submission.op {
            Op::UserInput { items: _ } => {
                // For now, we need a turn context. This will be provided by the session.
                // This is a simplified implementation - in the full refactor, the turn context
                // would be managed properly.
                Err(anyhow::anyhow!("UserInput handling requires turn context - to be implemented in full refactor"))
            }
            Op::UserTurn { items: _, .. } => {
                // Similar to UserInput, needs proper turn context management
                Err(anyhow::anyhow!("UserTurn handling requires turn context - to be implemented in full refactor"))
            }
            _ => {
                // Other operations are not handled by the main agent
                Err(anyhow::anyhow!("Operation not handled by MainAgent: {:?}", submission.op))
            }
        }
    }

    async fn run_turn(&self, _turn_context: &TurnContext, _input: Vec<InputItem>) -> Result<(), CodexErr> {
        // The main agent doesn't run turns directly - it orchestrates sub-agents
        // This method would be used for the main agent's own processing if needed
        Ok(())
    }
}