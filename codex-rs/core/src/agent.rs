use crate::agent_task::AgentTask;
use crate::turn_context::TurnContext;
use crate::error::CodexErr;
use codex_protocol::protocol::{InputItem, Submission};

pub trait Agent {
    /// Handles an input submission and returns a new AgentTask.
    fn handle_submission(&self, submission: Submission) -> Result<AgentTask, anyhow::Error>;

    /// Runs a single turn of the agent's logic.
    async fn run_turn(&self, turn_context: &TurnContext, input: Vec<InputItem>) -> Result<(), CodexErr>;
}