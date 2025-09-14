use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tokio::task::AbortHandle;
use uuid::Uuid;

use crate::client::ModelClient;
use crate::context_store::Context;
use crate::context_store::IContextRepository;
use crate::context_store::InMemoryContextRepository;
use crate::llm_subagent_executor::LLMSubagentExecutor;
use crate::mock_subagent_executor::MockSubagentExecutor;
use codex_protocol::protocol::BootstrapPath;
use codex_protocol::protocol::ContextItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SubagentCancelledEvent;
use codex_protocol::protocol::SubagentCompletedEvent;
use codex_protocol::protocol::SubagentForceCompletedEvent;
use codex_protocol::protocol::SubagentMetadata;
use codex_protocol::protocol::SubagentProgressEvent;
use codex_protocol::protocol::SubagentStartedEvent;
use codex_protocol::protocol::SubagentTaskCreatedEvent;
use codex_protocol::protocol::SubagentType;

/// Subagent manager abstract interface
#[async_trait]
pub trait ISubagentManager: Send + Sync {
    /// Create subagent task (using complete task specification)
    async fn create_task(&self, spec: SubagentTaskSpec) -> Result<String, SubagentError>;

    /// Get complete task information by task ID
    async fn get_task(&self, task_id: &str) -> Result<SubagentTask, SubagentError>;

    /// Launch subagent execution
    async fn launch_subagent(&self, task_id: &str) -> Result<SubagentHandle, SubagentError>;

    /// Get task status
    async fn get_task_status(&self, task_id: &str) -> Result<TaskStatus, SubagentError>;

    /// Cancel task execution
    async fn cancel_task(&self, task_id: &str) -> Result<(), SubagentError>;

    /// Force complete task (trigger force completion mechanism)
    async fn force_complete_task(&self, task_id: &str) -> Result<SubagentReport, SubagentError>;

    /// Get all active tasks
    async fn get_active_tasks(&self) -> Result<Vec<TaskSummary>, SubagentError>;

    /// Get task execution report
    async fn get_task_report(&self, task_id: &str)
    -> Result<Option<SubagentReport>, SubagentError>;

    /// Get recently completed tasks (sorted by completion time, most recent first)
    async fn get_recently_completed_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<TaskSummary>, SubagentError>;
}

/// Subagent task specification
#[derive(Debug, Clone)]
pub struct SubagentTaskSpec {
    pub agent_type: SubagentType,
    pub title: String,
    pub description: String,
    pub context_refs: Vec<String>,
    pub bootstrap_paths: Vec<BootstrapPath>,
    pub max_turns: Option<u32>,
    pub timeout_ms: Option<u64>,
}

/// Subagent handle
pub struct SubagentHandle {
    pub task_id: String,
    pub agent_type: SubagentType,
    pub abort_handle: tokio::task::AbortHandle,
}

/// Task status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskStatus {
    Created,
    Running { current_turn: u32, max_turns: u32 },
    Completed { result: SubagentReport },
    Failed { error: String },
    Cancelled,
}

/// Task summary
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSummary {
    pub task_id: String,
    pub agent_type: SubagentType,
    pub title: String,
    pub status: TaskStatus,
    pub created_at: SystemTime,
}

/// Subagent error types
#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("Task not found: {task_id}")]
    TaskNotFound { task_id: String },
    #[error("Invalid task specification: {message}")]
    InvalidTaskSpec { message: String },
    #[error("Execution error: {message}")]
    ExecutionError { message: String },
    #[error("Timeout error: task {task_id} exceeded {timeout_ms}ms")]
    TimeoutError { task_id: String, timeout_ms: u64 },
    #[error("Context error: {0}")]
    ContextError(#[from] crate::context_store::ContextError),
}

/// Subagent task (complete definition)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentTask {
    /// Task unique identifier
    pub task_id: String,
    /// Agent type
    pub agent_type: SubagentType,
    /// Task title
    pub title: String,
    /// Task description
    pub description: String,
    /// Context content resolved from context store
    pub ctx_store_contexts: HashMap<String, String>,
    /// Bootstrap context list (file/directory content)
    pub bootstrap_contexts: Vec<BootstrapContext>,
    /// Task status
    pub status: TaskStatus,
    /// Maximum execution turns
    pub max_turns: u32,
    /// Creation time
    pub created_at: SystemTime,
    /// Start time
    pub started_at: Option<SystemTime>,
    /// Completion time
    pub completed_at: Option<SystemTime>,
}

/// Bootstrap context content
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BootstrapContext {
    /// File or directory path
    pub path: PathBuf,
    /// Content
    pub content: String,
    /// Reason for inclusion
    pub reason: String,
}

/// Subagent execution report (complete definition)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentReport {
    /// Task ID
    pub task_id: String,
    /// Newly discovered context list
    pub contexts: Vec<ContextItem>,
    /// Execution summary and comments
    pub comments: String,
    /// Whether execution was successful
    pub success: bool,
    /// Execution metadata
    pub metadata: SubagentMetadata,
    /// Complete message trajectory (optional, for debugging)
    pub trajectory: Option<Vec<MessageEntry>>,
}

/// Message trajectory entry
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageEntry {
    /// Message role
    pub role: String,
    /// Message content
    pub content: String,
    /// Timestamp
    pub timestamp: SystemTime,
}

/// Executor type configuration
#[derive(Debug, Clone)]
pub enum ExecutorType {
    /// Simple mock executor for testing
    Mock,
    /// LLM-driven executor for production
    LLM { 
        model_client: Arc<ModelClient>,
        mcp_tools: Option<HashMap<String, mcp_types::Tool>>,
    },
}

/// In-memory subagent manager implementation
pub struct InMemorySubagentManager {
    tasks: Arc<RwLock<HashMap<String, SubagentTask>>>,
    reports: Arc<RwLock<HashMap<String, SubagentReport>>>,
    context_repo: Arc<InMemoryContextRepository>,
    event_sender: tokio::sync::mpsc::UnboundedSender<Event>,
    executor_type: ExecutorType,
}

impl InMemorySubagentManager {
    pub fn new(
        context_repo: Arc<InMemoryContextRepository>,
        event_sender: tokio::sync::mpsc::UnboundedSender<Event>,
        executor_type: ExecutorType,
    ) -> Self {
        Self {
            tasks: Arc::new(RwLock::new(HashMap::new())),
            reports: Arc::new(RwLock::new(HashMap::new())),
            context_repo,
            event_sender,
            executor_type,
        }
    }

    async fn load_bootstrap_contexts(
        &self,
        paths: &[BootstrapPath],
    ) -> Result<Vec<BootstrapContext>, SubagentError> {
        let mut contexts = Vec::new();

        for bootstrap_path in paths {
            let path = &bootstrap_path.path;

            tracing::info!(
                "Processing bootstrap path: {} (exists: {}, is_file: {}, is_dir: {})",
                path.display(),
                path.exists(),
                path.is_file(),
                path.is_dir()
            );

            let content = if path.is_file() {
                // Read file content
                match tokio::fs::read_to_string(path).await {
                    Ok(content) => content,
                    Err(e) => {
                        tracing::warn!("Failed to read file {}: {}", path.display(), e);
                        format!("Error reading file {}: {}", path.display(), e)
                    }
                }
            } else if path.is_dir() {
                // List directory contents
                match tokio::fs::read_dir(path).await {
                    Ok(mut entries) => {
                        let mut dir_content =
                            format!("Directory listing for {}:\n", path.display());
                        while let Ok(Some(entry)) = entries.next_entry().await {
                            let entry_path = entry.path();
                            let entry_name = entry_path
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("?");

                            if entry_path.is_dir() {
                                dir_content.push_str(&format!("  {entry_name}/\n"));
                            } else {
                                dir_content.push_str(&format!("  {entry_name}\n"));
                            }
                        }
                        dir_content
                    }
                    Err(e) => {
                        tracing::warn!("Failed to read directory {}: {}", path.display(), e);
                        format!("Error reading directory {}: {}", path.display(), e)
                    }
                }
            } else {
                format!(
                    "Path {} does not exist or is not accessible",
                    path.display()
                )
            };

            tracing::info!(
                "Loaded bootstrap context: {} ({} chars)",
                path.display(),
                content.len()
            );

            contexts.push(BootstrapContext {
                path: bootstrap_path.path.clone(),
                content,
                reason: bootstrap_path.reason.clone(),
            });
        }

        Ok(contexts)
    }

    async fn send_event(&self, event: Event) {
        if let Err(e) = self.event_sender.send(event) {
            tracing::error!("Failed to send subagent event: {}", e);
        }
    }
}

#[async_trait]
impl ISubagentManager for InMemorySubagentManager {
    async fn create_task(&self, spec: SubagentTaskSpec) -> Result<String, SubagentError> {
        let task_id = Uuid::new_v4().to_string();

        tracing::info!(
            "Creating subagent task: id={}, type={:?}, title='{}', contexts={}, bootstrap_paths={}",
            task_id,
            spec.agent_type,
            spec.title,
            spec.context_refs.len(),
            spec.bootstrap_paths.len()
        );

        // Resolve context references
        let contexts = self.context_repo.get_contexts(&spec.context_refs).await?;

        let mut ctx_store_contexts = HashMap::new();
        for context in contexts {
            ctx_store_contexts.insert(context.id, context.content);
        }

        // Load bootstrap contexts
        let bootstrap_contexts = self.load_bootstrap_contexts(&spec.bootstrap_paths).await?;

        let task = SubagentTask {
            task_id: task_id.clone(),
            agent_type: spec.agent_type.clone(),
            title: spec.title.clone(),
            description: spec.description,
            ctx_store_contexts,
            bootstrap_contexts,
            status: TaskStatus::Created,
            max_turns: spec.max_turns.unwrap_or(10),
            created_at: SystemTime::now(),
            started_at: None,
            completed_at: None,
        };

        // Store task
        {
            let mut tasks = self.tasks.write().await;
            tasks.insert(task_id.clone(), task);
        }

        // Send event
        let event = Event {
            id: task_id.clone(),
            msg: EventMsg::SubagentTaskCreated(SubagentTaskCreatedEvent {
                task_id: task_id.clone(),
                agent_type: spec.agent_type,
                title: spec.title,
                context_refs_count: spec.context_refs.len(),
                bootstrap_paths_count: spec.bootstrap_paths.len(),
            }),
        };

        self.send_event(event).await;

        tracing::info!("Subagent task created successfully: id={}", task_id);

        Ok(task_id)
    }

    async fn get_task(&self, task_id: &str) -> Result<SubagentTask, SubagentError> {
        let tasks = self.tasks.read().await;
        tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| SubagentError::TaskNotFound {
                task_id: task_id.to_string(),
            })
    }

    async fn launch_subagent(&self, task_id: &str) -> Result<SubagentHandle, SubagentError> {
        tracing::debug!("Launching subagent execution: task_id={task_id}");
        tracing::info!("Launching subagent execution: task_id={task_id}");

        let mut tasks = self.tasks.write().await;

        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| SubagentError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;

        // Check task status
        match task.status {
            TaskStatus::Created => {}
            _ => {
                return Err(SubagentError::ExecutionError {
                    message: format!("Task {task_id} is not in Created state"),
                });
            }
        }

        // Update task status
        task.status = TaskStatus::Running {
            current_turn: 0,
            max_turns: task.max_turns,
        };
        task.started_at = Some(SystemTime::now());

        let agent_type = task.agent_type.clone();
        let title = task.title.clone();

        // Send started event
        let event = Event {
            id: task_id.to_string(),
            msg: EventMsg::SubagentStarted(SubagentStartedEvent {
                task_id: task_id.to_string(),
                agent_type: agent_type.clone(),
                title,
            }),
        };

        self.send_event(event).await;

        tracing::info!(
            "Subagent execution started: task_id={}, type={:?}, max_turns={}",
            task_id,
            agent_type,
            task.max_turns
        );

        // Spawn the actual subagent execution
        let task_clone = task.clone();
        let task_id_clone = task_id.to_string();
        let event_sender = self.event_sender.clone();
        let tasks_clone = self.tasks.clone();
        let reports_clone = self.reports.clone();
        let context_repo = self.context_repo.clone();
        let executor_type = self.executor_type.clone();

        let abort_handle = tokio::spawn(async move {
            // Execute the task based on executor type
            tracing::info!("Starting subagent execution with executor type: {:?}", 
                match &executor_type {
                    ExecutorType::Mock => "Mock",
                    ExecutorType::LLM { .. } => "LLM",
                }
            );
            
            let report = match executor_type {
                ExecutorType::Mock => {
                    // Use the mock executor
                    tracing::info!("Using MockSubagentExecutor for task: {}", task_clone.task_id);
                    let executor = MockSubagentExecutor::new(context_repo);
                    executor.execute_task(&task_clone).await
                }
                ExecutorType::LLM { model_client, mcp_tools } => {
                    // Use the LLM executor
                    tracing::info!("Using LLMSubagentExecutor for task: {}, MCP tools available: {}", 
                        task_clone.task_id, 
                        mcp_tools.as_ref().map(|t| t.len()).unwrap_or(0)
                    );
                    let executor =
                        LLMSubagentExecutor::new(context_repo, model_client, task_clone.max_turns, mcp_tools, event_sender.clone());
                    executor.execute_task(&task_clone).await
                }
            };

            // Update task status
            {
                let mut tasks = tasks_clone.write().await;
                if let Some(task) = tasks.get_mut(&task_id_clone) {
                    task.status = TaskStatus::Completed {
                        result: report.clone(),
                    };
                    task.completed_at = Some(SystemTime::now());
                }
            }

            // Store report
            {
                let mut reports = reports_clone.write().await;
                reports.insert(task_id_clone.clone(), report.clone());
            }

            // Send completion event
            let event = Event {
                id: task_id_clone.clone(),
                msg: EventMsg::SubagentCompleted(SubagentCompletedEvent {
                    task_id: task_id_clone,
                    success: report.success,
                    contexts_created: report.contexts.len(),
                    comments: report.comments,
                    metadata: report.metadata,
                }),
            };

            let _ = event_sender.send(event);
        })
        .abort_handle();

        tracing::info!(
            "Subagent execution launched successfully: task_id={}",
            task_id
        );

        Ok(SubagentHandle {
            task_id: task_id.to_string(),
            agent_type,
            abort_handle,
        })
    }

    async fn get_task_status(&self, task_id: &str) -> Result<TaskStatus, SubagentError> {
        let tasks = self.tasks.read().await;
        let task = tasks
            .get(task_id)
            .ok_or_else(|| SubagentError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;

        Ok(task.status.clone())
    }

    async fn cancel_task(&self, task_id: &str) -> Result<(), SubagentError> {
        let mut tasks = self.tasks.write().await;
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| SubagentError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;

        let current_turn = match &task.status {
            TaskStatus::Running { current_turn, .. } => *current_turn,
            _ => {
                return Err(SubagentError::ExecutionError {
                    message: format!("Task {task_id} is not running"),
                });
            }
        };

        task.status = TaskStatus::Cancelled;
        task.completed_at = Some(SystemTime::now());

        // Send cancellation event
        let event = Event {
            id: task_id.to_string(),
            msg: EventMsg::SubagentCancelled(SubagentCancelledEvent {
                task_id: task_id.to_string(),
                agent_type: task.agent_type.clone(),
                title: task.title.clone(),
                reason: "User requested cancellation".to_string(),
                cancelled_at_turn: current_turn,
            }),
        };

        self.send_event(event).await;
        Ok(())
    }

    async fn force_complete_task(&self, task_id: &str) -> Result<SubagentReport, SubagentError> {
        let mut tasks = self.tasks.write().await;
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| SubagentError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;

        let (current_turn, max_turns) = match &task.status {
            TaskStatus::Running {
                current_turn,
                max_turns,
            } => (*current_turn, *max_turns),
            _ => {
                return Err(SubagentError::ExecutionError {
                    message: format!("Task {task_id} is not running"),
                });
            }
        };

        // Create force completion report
        let report = SubagentReport {
            task_id: task_id.to_string(),
            contexts: Vec::new(),
            comments: "Task was force completed by user request".to_string(),
            success: false,
            metadata: SubagentMetadata {
                num_turns: current_turn,
                max_turns,
                input_tokens: 500,
                output_tokens: 200,
                duration_ms: 1000,
                reached_max_turns: false,
                force_completed: true,
                error_message: Some("Force completed".to_string()),
            },
            trajectory: None,
        };

        task.status = TaskStatus::Completed {
            result: report.clone(),
        };
        task.completed_at = Some(SystemTime::now());

        // Store report
        {
            let mut reports = self.reports.write().await;
            reports.insert(task_id.to_string(), report.clone());
        }

        // Send force completion event
        let event = Event {
            id: task_id.to_string(),
            msg: EventMsg::SubagentForceCompleted(SubagentForceCompletedEvent {
                task_id: task_id.to_string(),
                agent_type: task.agent_type.clone(),
                title: task.title.clone(),
                num_turns: current_turn,
                max_turns,
                contexts_created: 0,
                comments: report.comments.clone(),
                metadata: report.metadata.clone(),
            }),
        };

        self.send_event(event).await;

        Ok(report)
    }

    async fn get_active_tasks(&self) -> Result<Vec<TaskSummary>, SubagentError> {
        let tasks = self.tasks.read().await;
        let mut summaries = Vec::new();

        for task in tasks.values() {
            if matches!(
                task.status,
                TaskStatus::Created | TaskStatus::Running { .. }
            ) {
                summaries.push(TaskSummary {
                    task_id: task.task_id.clone(),
                    agent_type: task.agent_type.clone(),
                    title: task.title.clone(),
                    status: task.status.clone(),
                    created_at: task.created_at,
                });
            }
        }

        Ok(summaries)
    }

    async fn get_task_report(
        &self,
        task_id: &str,
    ) -> Result<Option<SubagentReport>, SubagentError> {
        let reports = self.reports.read().await;
        Ok(reports.get(task_id).cloned())
    }

    async fn get_recently_completed_tasks(
        &self,
        limit: usize,
    ) -> Result<Vec<TaskSummary>, SubagentError> {
        let tasks = self.tasks.read().await;
        let mut completed_tasks = Vec::new();

        for task in tasks.values() {
            if let TaskStatus::Completed { .. } = task.status {
                completed_tasks.push(TaskSummary {
                    task_id: task.task_id.clone(),
                    agent_type: task.agent_type.clone(),
                    title: task.title.clone(),
                    status: task.status.clone(),
                    created_at: task.created_at,
                });
            }
        }

        // Sort by completion time (most recent first)
        // Since we don't have completed_at in TaskSummary, we'll use created_at as approximation
        // In a real implementation, we might want to add completed_at to TaskSummary
        completed_tasks.sort_by(|a, b| b.created_at.cmp(&a.created_at));

        // Limit the results
        completed_tasks.truncate(limit);

        Ok(completed_tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_store::InMemoryContextRepository;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_create_and_get_task() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let (event_sender, _) = mpsc::unbounded_channel();
        let manager = InMemorySubagentManager::new(context_repo, event_sender, ExecutorType::Mock);

        let spec = SubagentTaskSpec {
            agent_type: SubagentType::Explorer,
            title: "Test Task".to_string(),
            description: "Test Description".to_string(),
            context_refs: Vec::new(),
            bootstrap_paths: Vec::new(),
            max_turns: Some(5),
            timeout_ms: None,
        };

        let task_id = manager.create_task(spec).await.unwrap();
        let task = manager.get_task(&task_id).await.unwrap();

        assert_eq!(task.title, "Test Task");
        assert_eq!(task.max_turns, 5);
        assert!(matches!(task.status, TaskStatus::Created));
    }

    #[tokio::test]
    async fn test_launch_subagent() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let manager = InMemorySubagentManager::new(context_repo, event_sender, ExecutorType::Mock);

        let spec = SubagentTaskSpec {
            agent_type: SubagentType::Coder,
            title: "Test Task".to_string(),
            description: "Test Description".to_string(),
            context_refs: Vec::new(),
            bootstrap_paths: Vec::new(),
            max_turns: Some(10),
            timeout_ms: None,
        };

        let task_id = manager.create_task(spec).await.unwrap();
        let handle = manager.launch_subagent(&task_id).await.unwrap();

        assert_eq!(handle.task_id, task_id);
        assert!(matches!(handle.agent_type, SubagentType::Coder));

        // Check that we received the started event
        let event = event_receiver.recv().await.unwrap();
        assert!(matches!(event.msg, EventMsg::SubagentTaskCreated(_)));

        let event = event_receiver.recv().await.unwrap();
        assert!(matches!(event.msg, EventMsg::SubagentStarted(_)));
    }

    #[tokio::test]
    async fn test_get_active_tasks() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let (event_sender, _) = mpsc::unbounded_channel();
        let manager = InMemorySubagentManager::new(context_repo, event_sender, ExecutorType::Mock);

        let spec = SubagentTaskSpec {
            agent_type: SubagentType::Explorer,
            title: "Active Task".to_string(),
            description: "Test Description".to_string(),
            context_refs: Vec::new(),
            bootstrap_paths: Vec::new(),
            max_turns: Some(5),
            timeout_ms: None,
        };

        let task_id = manager.create_task(spec).await.unwrap();
        let active_tasks = manager.get_active_tasks().await.unwrap();

        assert_eq!(active_tasks.len(), 1);
        assert_eq!(active_tasks[0].task_id, task_id);
        assert_eq!(active_tasks[0].title, "Active Task");
    }
}
