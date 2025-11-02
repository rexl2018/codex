use std::sync::Arc;

use codex_protocol::protocol::Event;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;
use codex_protocol::protocol::EventMsg;
use codex_app_server_protocol::InputItem;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use tokio::task::AbortHandle;

use crate::compact;
use crate::codex::run_task;
use crate::session::Session;
use crate::turn_context::TurnContext;

/// A series of Turns in response to user input.
#[derive(Debug)]
pub struct AgentTask {
    sess: Arc<Session>,
    sub_id: String,
    handle: AbortHandle,
}

impl AgentTask {
    pub fn spawn(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
    ) -> Self {
        let handle = {
            let _sess = sess.clone();
            let _sub_id = sub_id.clone();
            let _tc = Arc::clone(&turn_context);
            let _input = input;
            
            // TODO: Implement proper task execution
            // For now, create a placeholder task that does nothing
            tokio::spawn(async move {
                // Placeholder implementation
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            })
            .abort_handle()
        };
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    pub fn compact(
        sess: Arc<Session>,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        input: Vec<InputItem>,
        compact_instructions: String,
    ) -> Self {
        let handle = {
            let _sess = sess.clone();
            let _sub_id = sub_id.clone();
            let _tc = Arc::clone(&turn_context);
            let _input = input;
            let _compact_instructions = compact_instructions;
            
            // TODO: Implement proper compact task execution
            // For now, create a placeholder task that does nothing
            tokio::spawn(async move {
                // Placeholder implementation
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
            })
            .abort_handle()
        };
        Self {
            sess,
            sub_id,
            handle,
        }
    }

    pub fn get_sub_id(&self) -> &str {
        &self.sub_id
    }

    pub(crate) fn abort(self, reason: TurnAbortReason) {
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
