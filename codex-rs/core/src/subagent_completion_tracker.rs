use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use codex_protocol::protocol::{
    Event, EventMsg, SubagentCompletedEvent,
};
use crate::subagent_manager::{ISubagentManager, TaskStatus};

/// Represents the completion status of a subagent task
#[derive(Debug, Clone)]
pub enum SubagentCompletionStatus {
    /// Task completed normally
    Completed {
        success: bool,
        contexts_created: usize,
        comments: String,
    },
    /// Task was force completed
    ForceCompleted {
        num_turns: u32,
        max_turns: u32,
        contexts_created: usize,
        comments: String,
    },
    /// Task was cancelled
    Cancelled {
        reason: String,
        cancelled_at_turn: u32,
    },
    /// Task failed during execution
    Failed { error: String },
}

/// Tracks subagent completion using event-driven approach with polling fallback
pub struct SubagentCompletionTracker {
    /// Map of task_id to completion status
    completed_tasks: Arc<RwLock<HashMap<String, SubagentCompletionStatus>>>,
    /// Event receiver for subagent events
    event_receiver: Arc<Mutex<mpsc::UnboundedReceiver<Event>>>,
    /// Subagent manager for polling fallback
    subagent_manager: Arc<dyn ISubagentManager>,
    /// Whether the tracker is running
    is_running: Arc<RwLock<bool>>,
}

impl SubagentCompletionTracker {
    /// Create a new SubagentCompletionTracker
    pub fn new(
        event_receiver: mpsc::UnboundedReceiver<Event>,
        subagent_manager: Arc<dyn ISubagentManager>,
    ) -> Self {
        Self {
            completed_tasks: Arc::new(RwLock::new(HashMap::new())),
            event_receiver: Arc::new(Mutex::new(event_receiver)),
            subagent_manager,
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the event listener
    pub async fn start(&self) {
        let mut is_running = self.is_running.write().await;
        if *is_running {
            warn!("SubagentCompletionTracker is already running");
            return;
        }
        *is_running = true;
        drop(is_running);

        let completed_tasks = self.completed_tasks.clone();
        let event_receiver = self.event_receiver.clone();

        tokio::spawn(async move {
            let mut receiver = event_receiver.lock().await;
            
            while let Some(event) = receiver.recv().await {
                match &event.msg {
                    EventMsg::SubagentCompleted(ev) => {
                        info!("Event received: SubagentCompleted for task {}", ev.task_id);
                        let status = SubagentCompletionStatus::Completed {
                            success: ev.success,
                            contexts_created: ev.contexts_created,
                            comments: ev.comments.clone(),
                        };
                        completed_tasks.write().await.insert(ev.task_id.clone(), status);
                    }
                    EventMsg::SubagentForceCompleted(ev) => {
                        info!("Event received: SubagentForceCompleted for task {}", ev.task_id);
                        let status = SubagentCompletionStatus::ForceCompleted {
                            num_turns: ev.num_turns,
                            max_turns: ev.max_turns,
                            contexts_created: ev.contexts_created,
                            comments: ev.comments.clone(),
                        };
                        completed_tasks.write().await.insert(ev.task_id.clone(), status);
                    }
                    EventMsg::SubagentCancelled(ev) => {
                        info!("Event received: SubagentCancelled for task {}", ev.task_id);
                        let status = SubagentCompletionStatus::Cancelled {
                            reason: ev.reason.clone(),
                            cancelled_at_turn: ev.cancelled_at_turn,
                        };
                        completed_tasks.write().await.insert(ev.task_id.clone(), status);
                    }
                    _ => {
                        // Ignore other events
                    }
                }
            }
        });
    }

    /// Wait for a subagent task to complete using event-driven approach with polling fallback
    pub async fn wait_for_completion(
        &self,
        task_id: &str,
        timeout_duration: Duration,
    ) -> Result<SubagentCompletionStatus, SubagentCompletionError> {
        let start_time = Instant::now();
        let poll_interval = Duration::from_millis(2000); // 2 seconds for fallback polling
        let event_check_interval = Duration::from_millis(100); // 100ms for event checking

        info!("Waiting for subagent completion: task_id={}, timeout={:?}", task_id, timeout_duration);

        // First, check if the task is already completed
        if let Some(status) = self.completed_tasks.read().await.get(task_id) {
            info!("Task {} already completed (from cache)", task_id);
            return Ok(status.clone());
        }

        let mut last_poll_time = Instant::now();
        let mut event_checks = 0;
        let mut poll_checks = 0;

        loop {
            // Check if timeout exceeded
            if start_time.elapsed() > timeout_duration {
                warn!("Timeout waiting for subagent completion: task_id={}", task_id);
                return Err(SubagentCompletionError::Timeout {
                    task_id: task_id.to_string(),
                    timeout_ms: timeout_duration.as_millis() as u64,
                });
            }

            // Primary: Check for event-driven completion (fast check)
            if let Some(status) = self.completed_tasks.read().await.get(task_id) {
                info!("Event-driven completion detected for task {}: {:?}", task_id, status);
                return Ok(status.clone());
            }
            event_checks += 1;

            // Fallback: Polling check (less frequent)
            if last_poll_time.elapsed() > poll_interval {
                debug!("Performing fallback polling check for task {}", task_id);
                match self.subagent_manager.get_task_status(task_id).await {
                    Ok(TaskStatus::Completed { result }) => {
                        info!("Polling detected completion for task {}", task_id);
                        let status = SubagentCompletionStatus::Completed {
                            success: result.success,
                            contexts_created: result.contexts.len(),
                            comments: result.comments,
                        };
                        // Cache the result for future queries
                        self.completed_tasks.write().await.insert(task_id.to_string(), status.clone());
                        return Ok(status);
                    }
                    Ok(TaskStatus::Cancelled) => {
                        info!("Polling detected cancellation for task {}", task_id);
                        let status = SubagentCompletionStatus::Cancelled {
                            reason: "Task was cancelled".to_string(),
                            cancelled_at_turn: 0, // We don't have this info from polling
                        };
                        self.completed_tasks.write().await.insert(task_id.to_string(), status.clone());
                        return Ok(status);
                    }
                    Ok(TaskStatus::Failed { error }) => {
                        info!("Polling detected failure for task {}", task_id);
                        let status = SubagentCompletionStatus::Failed { error };
                        self.completed_tasks.write().await.insert(task_id.to_string(), status.clone());
                        return Ok(status);
                    }
                    Ok(TaskStatus::Running { current_turn, max_turns }) => {
                        debug!("Task {} still running: turn {}/{}", task_id, current_turn, max_turns);
                    }
                    Ok(TaskStatus::Created) => {
                        debug!("Task {} still in created state", task_id);
                    }
                    Err(e) => {
                        warn!("Error polling task status for {}: {}", task_id, e);
                        return Err(SubagentCompletionError::PollingError {
                            task_id: task_id.to_string(),
                            error: e.to_string(),
                        });
                    }
                }
                last_poll_time = Instant::now();
                poll_checks += 1;
            }

            // Short sleep before next check
            tokio::time::sleep(event_check_interval).await;
        }
    }

    /// Check if a task is completed (non-blocking)
    pub async fn is_completed(&self, task_id: &str) -> bool {
        self.completed_tasks.read().await.contains_key(task_id)
    }

    /// Get completion status for a task (non-blocking)
    pub async fn get_completion_status(&self, task_id: &str) -> Option<SubagentCompletionStatus> {
        self.completed_tasks.read().await.get(task_id).cloned()
    }

    /// Remove a completed task from tracking (cleanup)
    pub async fn remove_completed_task(&self, task_id: &str) {
        self.completed_tasks.write().await.remove(task_id);
    }

    /// Stop the tracker
    pub async fn stop(&self) {
        let mut is_running = self.is_running.write().await;
        *is_running = false;
    }
}

/// Errors that can occur during subagent completion tracking
#[derive(Debug, thiserror::Error)]
pub enum SubagentCompletionError {
    #[error("Timeout waiting for subagent completion: task_id={task_id}, timeout={timeout_ms}ms")]
    Timeout { task_id: String, timeout_ms: u64 },
    
    #[error("Polling error for task {task_id}: {error}")]
    PollingError { task_id: String, error: String },
    
    #[error("Event processing error: {message}")]
    EventProcessingError { message: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_store::InMemoryContextRepository;
    use crate::subagent_manager::{InMemorySubagentManager, ExecutorType, SubagentTaskSpec};
    use codex_protocol::protocol::SubagentType;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_event_driven_completion() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let subagent_manager = Arc::new(InMemorySubagentManager::new_for_testing(
            context_repo,
            event_tx.clone(),
            ExecutorType::Mock,
        ));

        let tracker = SubagentCompletionTracker::new(event_rx, subagent_manager.clone());
        tracker.start().await;

        // Create a task
        let spec = SubagentTaskSpec {
            agent_type: SubagentType::Explorer,
            title: "Test Task".to_string(),
            description: "Test Description".to_string(),
            context_refs: Vec::new(),
            bootstrap_paths: Vec::new(),
            max_turns: Some(5),
            timeout_ms: Some(60_000),
            network_access: None,
            injected_conversation: None,
        };

        let task_id = subagent_manager.create_task(spec).await.unwrap();

        // Simulate completion event
        let completion_event = Event {
            id: task_id.clone(),
            msg: EventMsg::SubagentCompleted(SubagentCompletedEvent {
                task_id: task_id.clone(),
                success: true,
                contexts_created: 2,
                comments: "Test completed".to_string(),
                metadata: codex_protocol::protocol::SubagentMetadata {
                    num_turns: 3,
                    max_turns: 5,
                    input_tokens: 100,
                    output_tokens: 200,
                    duration_ms: 5000,
                    reached_max_turns: false,
                    force_completed: false,
                    error_message: None,
                },
            }),
        };

        // Send the event
        event_tx.send(completion_event).unwrap();

        // Wait for completion with short timeout
        let result = tracker.wait_for_completion(&task_id, Duration::from_secs(2)).await;
        assert!(result.is_ok());

        if let Ok(SubagentCompletionStatus::Completed { success, contexts_created, .. }) = result {
            assert!(success);
            assert_eq!(contexts_created, 2);
        } else {
            panic!("Expected Completed status");
        }
    }
}