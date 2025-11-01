use std::sync::Arc;

use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::InputItem;
use codex_protocol::protocol::TurnAbortReason;
use codex_protocol::protocol::TurnAbortedEvent;
use tokio::task::AbortHandle;

use crate::compact;
use crate::codex::run_task;
use crate::session::Session;
use crate::turn_context::TurnContext;

/// A series of Turns in response to user input.
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
            let sess = sess.clone();
            let sub_id = sub_id.clone();
            let tc = Arc::clone(&turn_context);
            tokio::spawn(async move { run_task(sess, tc.as_ref(), sub_id, input).await })
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
            let sess = sess.clone();
            let sub_id = sub_id.clone();
            let tc = Arc::clone(&turn_context);
            tokio::spawn(async move {
                crate::codex::compact::run_compact_task(sess, tc, input).await
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
