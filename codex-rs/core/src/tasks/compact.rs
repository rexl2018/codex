use std::sync::Arc;

use super::SessionTask;
use super::SessionTaskContext;
use crate::codex::TurnContext;
use crate::state::TaskKind;
use async_trait::async_trait;
use codex_protocol::user_input::UserInput;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Default)]
pub(crate) struct CompactTask {
    pub range: Option<(usize, usize)>,
}

#[async_trait]
impl SessionTask for CompactTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Compact
    }

    async fn run(
        self: Arc<Self>,
        session: Arc<SessionTaskContext>,
        ctx: Arc<TurnContext>,
        input: Vec<UserInput>,
        _cancellation_token: CancellationToken,
    ) -> Option<String> {
        let session = session.clone_session();
        if crate::compact::should_use_remote_compact_task(&session) {
            // Remote compaction does not support range yet, fallback or ignore range?
            // For now, let's assume remote compaction is full history only.
            // If range is set, we might want to force local compaction or error.
            // But the user request implies reusing logic.
            // Let's just use remote if configured (ignoring range) OR force local if range is present.
            // Given the complexity, let's force local if range is present.
            if self.range.is_some() {
                if let Some((start, end)) = self.range {
                    crate::compact::run_compact_task_on_range(session, ctx, input, start, end).await
                }
            } else {
                crate::compact_remote::run_remote_compact_task(session, ctx).await
            }
        } else if let Some((start, end)) = self.range {
            crate::compact::run_compact_task_on_range(session, ctx, input, start, end).await
        } else {
            crate::compact::run_compact_task(session, ctx, input).await
        }

        None
    }
}
