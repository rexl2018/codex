use std::collections::{
    HashMap,
    HashSet,
};

use codex_protocol::{
    models::ResponseInputItem,
    protocol::ReviewDecision,
};
use tokio::sync::oneshot;

use crate::{
    agent_task::AgentTask,
    conversation_history::ConversationHistory,
    protocol::TokenUsageInfo,
};

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