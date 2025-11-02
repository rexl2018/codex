use std::sync::Arc;
use crate::agent_task::AgentTask;
use crate::session::Session;
use crate::turn_context::TurnContext;
use codex_app_server_protocol::InputItem;

pub struct AgentTaskFactory;

impl AgentTaskFactory {
    pub fn create_regular_task(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
    ) -> AgentTask {
        AgentTask::spawn(sess, turn_context, sub_id, input)
    }

    pub fn create_compact_task(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
        compact_instructions: String,
    ) -> AgentTask {
        AgentTask::compact(sess, turn_context, sub_id, input, compact_instructions)
    }
}