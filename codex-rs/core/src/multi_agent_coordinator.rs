use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;
use tokio::task::AbortHandle;
use serde::{Deserialize, Serialize};

use crate::context_store::{IContextRepository, InMemoryContextRepository};
use crate::subagent_manager::{ISubagentManager, InMemorySubagentManager, SubagentHandle, SubagentTaskSpec, TaskSummary};
use codex_protocol::protocol::{Event, EventMsg, SubagentType, MultiAgentStatusEvent};

/// Execution strategy for coordinating multiple subagents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ExecutionStrategy {
    /// Execute tasks sequentially - suitable for dependent tasks
    Sequential,
    /// Execute tasks in parallel - suitable for independent tasks
    Parallel,
    /// Conditional execution - decide subsequent tasks based on previous results
    Conditional,
    /// Mixed strategy - complex execution flow
    Mixed(Vec<ExecutionStep>),
}

/// Execution step for mixed strategy
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionStep {
    /// Step ID
    pub step_id: String,
    /// Tasks to execute in this step
    pub task_ids: Vec<String>,
    /// Whether this step can run in parallel
    pub parallel: bool,
    /// Condition for executing this step (optional)
    pub condition: Option<String>,
}

/// Execution plan for coordinating multiple subagents
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Plan ID
    pub plan_id: String,
    /// Execution strategy
    pub strategy: ExecutionStrategy,
    /// Execution steps
    pub steps: Vec<ExecutionStep>,
    /// Plan creation time
    pub created_at: SystemTime,
}

/// Subtask coordinator for managing multiple subagent tasks
pub struct SubtaskCoordinator {
    /// Active subtasks
    subtasks: Arc<RwLock<HashMap<String, SubagentHandle>>>,
    /// Execution plan
    execution_plan: ExecutionPlan,
    /// Shared context between subtasks
    shared_context: SharedContext,
    /// Subagent manager
    subagent_manager: Arc<InMemorySubagentManager>,
    /// Context repository
    context_repo: Arc<InMemoryContextRepository>,
    /// Event sender
    event_sender: tokio::sync::mpsc::UnboundedSender<Event>,
}

/// Shared context between subtasks
#[derive(Debug, Clone)]
pub struct SharedContext {
    /// Shared data between subtasks
    pub data: HashMap<String, String>,
    /// Global task context
    pub global_context: String,
    /// Task dependencies
    pub dependencies: HashMap<String, Vec<String>>,
}

/// Enhanced AgentTask status for multi-agent coordination
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentTaskStatus {
    /// Planning subtasks
    Planning,
    /// Executing subtasks
    ExecutingSubtasks {
        completed: usize,
        total: usize,
        current: Option<String>, // Current executing subtask ID
    },
    /// Completed
    Completed,
    /// Failed with reason
    Failed { reason: String },
}

/// Multi-agent coordination result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationResult {
    /// Whether coordination was successful
    pub success: bool,
    /// Completed subtasks
    pub completed_subtasks: Vec<String>,
    /// Failed subtasks
    pub failed_subtasks: Vec<String>,
    /// Consolidated results
    pub consolidated_results: HashMap<String, String>,
    /// Execution summary
    pub summary: String,
}

impl SubtaskCoordinator {
    pub fn new(
        execution_plan: ExecutionPlan,
        shared_context: SharedContext,
        subagent_manager: Arc<InMemorySubagentManager>,
        context_repo: Arc<InMemoryContextRepository>,
        event_sender: tokio::sync::mpsc::UnboundedSender<Event>,
    ) -> Self {
        Self {
            subtasks: Arc::new(RwLock::new(HashMap::new())),
            execution_plan,
            shared_context,
            subagent_manager,
            context_repo,
            event_sender,
        }
    }
    
    /// Execute the coordination plan
    pub async fn execute(&self) -> Result<CoordinationResult, CoordinationError> {
        match &self.execution_plan.strategy {
            ExecutionStrategy::Sequential => self.execute_sequential().await,
            ExecutionStrategy::Parallel => self.execute_parallel().await,
            ExecutionStrategy::Conditional => self.execute_conditional().await,
            ExecutionStrategy::Mixed(steps) => self.execute_mixed(steps).await,
        }
    }
    
    /// Execute tasks sequentially
    async fn execute_sequential(&self) -> Result<CoordinationResult, CoordinationError> {
        let mut completed_subtasks = Vec::new();
        let mut failed_subtasks = Vec::new();
        let mut consolidated_results = HashMap::new();
        
        for step in &self.execution_plan.steps {
            for task_id in &step.task_ids {
                match self.execute_single_task(task_id).await {
                    Ok(result) => {
                        completed_subtasks.push(task_id.clone());
                        consolidated_results.insert(task_id.clone(), result);
                    }
                    Err(e) => {
                        failed_subtasks.push(task_id.clone());
                        tracing::error!("Task {} failed: {}", task_id, e);
                    }
                }
            }
        }
        
        let success = failed_subtasks.is_empty();
        let completed_count = completed_subtasks.len();
        let failed_count = failed_subtasks.len();
        Ok(CoordinationResult {
            success,
            completed_subtasks,
            failed_subtasks,
            consolidated_results,
            summary: format!("Sequential execution completed. {completed_count} succeeded, {failed_count} failed"),
        })
    }
    
    /// Execute tasks in parallel
    async fn execute_parallel(&self) -> Result<CoordinationResult, CoordinationError> {
        let mut handles = Vec::new();
        
        for step in &self.execution_plan.steps {
            for task_id in &step.task_ids {
                let task_id_clone = task_id.clone();
                let coordinator = self.clone_for_task();
                
                let handle = tokio::spawn(async move {
                    coordinator.execute_single_task(&task_id_clone).await
                        .map(|result| (task_id_clone, result))
                });
                handles.push(handle);
            }
        }
        
        let mut completed_subtasks = Vec::new();
        let mut failed_subtasks = Vec::new();
        let mut consolidated_results = HashMap::new();
        
        for handle in handles {
            match handle.await {
                Ok(Ok((task_id, result))) => {
                    completed_subtasks.push(task_id.clone());
                    consolidated_results.insert(task_id, result);
                }
                Ok(Err(e)) => {
                    tracing::error!("Task failed: {}", e);
                    // We don't have the task_id here, so we'll add it to failed list later
                }
                Err(e) => {
                    tracing::error!("Task join failed: {}", e);
                }
            }
        }
        
        let success = failed_subtasks.is_empty();
        let completed_count = completed_subtasks.len();
        let failed_count = failed_subtasks.len();
        Ok(CoordinationResult {
            success,
            completed_subtasks,
            failed_subtasks,
            consolidated_results,
            summary: format!("Parallel execution completed. {completed_count} succeeded, {failed_count} failed"),
        })
    }
    
    /// Execute tasks conditionally
    async fn execute_conditional(&self) -> Result<CoordinationResult, CoordinationError> {
        // For now, implement as sequential with condition checking
        // In a real implementation, this would evaluate conditions based on previous results
        self.execute_sequential().await
    }
    
    /// Execute mixed strategy
    async fn execute_mixed(&self, steps: &[ExecutionStep]) -> Result<CoordinationResult, CoordinationError> {
        let mut completed_subtasks = Vec::new();
        let mut failed_subtasks = Vec::new();
        let mut consolidated_results = HashMap::new();
        
        for step in steps {
            // Check condition if present
            if let Some(ref condition) = step.condition {
                if !self.evaluate_condition(condition, &consolidated_results).await {
                    continue;
                }
            }
            
            if step.parallel {
                // Execute tasks in this step in parallel
                let mut handles = Vec::new();
                
                for task_id in &step.task_ids {
                    let task_id_clone = task_id.clone();
                    let coordinator = self.clone_for_task();
                    
                    let handle = tokio::spawn(async move {
                        coordinator.execute_single_task(&task_id_clone).await
                            .map(|result| (task_id_clone, result))
                    });
                    handles.push(handle);
                }
                
                for handle in handles {
                    match handle.await {
                        Ok(Ok((task_id, result))) => {
                            completed_subtasks.push(task_id.clone());
                            consolidated_results.insert(task_id, result);
                        }
                        Ok(Err(e)) => {
                            tracing::error!("Task failed: {}", e);
                        }
                        Err(e) => {
                            tracing::error!("Task join failed: {}", e);
                        }
                    }
                }
            } else {
                // Execute tasks in this step sequentially
                for task_id in &step.task_ids {
                    match self.execute_single_task(task_id).await {
                        Ok(result) => {
                            completed_subtasks.push(task_id.clone());
                            consolidated_results.insert(task_id.clone(), result);
                        }
                        Err(e) => {
                            failed_subtasks.push(task_id.clone());
                            tracing::error!("Task {} failed: {}", task_id, e);
                        }
                    }
                }
            }
        }
        
        let success = failed_subtasks.is_empty();
        let completed_count = completed_subtasks.len();
        let failed_count = failed_subtasks.len();
        Ok(CoordinationResult {
            success,
            completed_subtasks,
            failed_subtasks,
            consolidated_results,
            summary: format!("Mixed execution completed. {completed_count} succeeded, {failed_count} failed"),
        })
    }
    
    /// Execute a single task
    async fn execute_single_task(&self, task_id: &str) -> Result<String, CoordinationError> {
        // In a real implementation, this would:
        // 1. Get the task specification
        // 2. Launch the subagent
        // 3. Wait for completion
        // 4. Return the result
        
        // For now, return a mock result
        Ok(format!("Mock result for task {task_id}"))
    }
    
    /// Evaluate a condition for conditional execution
    async fn evaluate_condition(&self, condition: &str, results: &HashMap<String, String>) -> bool {
        // In a real implementation, this would parse and evaluate the condition
        // For now, always return true
        true
    }
    
    /// Clone coordinator for task execution (simplified for parallel execution)
    fn clone_for_task(&self) -> Self {
        Self {
            subtasks: self.subtasks.clone(),
            execution_plan: self.execution_plan.clone(),
            shared_context: self.shared_context.clone(),
            subagent_manager: self.subagent_manager.clone(),
            context_repo: self.context_repo.clone(),
            event_sender: self.event_sender.clone(),
        }
    }
    
    /// Get coordination status
    pub async fn get_status(&self) -> CoordinationStatus {
        let subtasks = self.subtasks.read().await;
        let active_tasks = subtasks.len();
        
        // In a real implementation, this would get more detailed status
        CoordinationStatus {
            active_tasks,
            completed_tasks: 0,
            total_contexts: 0,
            status: "Running".to_string(),
        }
    }
    
    /// Send coordination status event
    async fn send_status_event(&self) {
        let status = self.get_status().await;
        let event = Event {
            id: self.execution_plan.plan_id.clone(),
            msg: EventMsg::MultiAgentStatus(MultiAgentStatusEvent {
                active_tasks: status.active_tasks,
                completed_tasks: status.completed_tasks,
                total_contexts: status.total_contexts,
                status: status.status,
            }),
        };
        
        if let Err(e) = self.event_sender.send(event) {
            tracing::error!("Failed to send coordination status event: {}", e);
        }
    }
}

/// Coordination status
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoordinationStatus {
    pub active_tasks: usize,
    pub completed_tasks: usize,
    pub total_contexts: usize,
    pub status: String,
}

/// Coordination error types
#[derive(Debug, thiserror::Error)]
pub enum CoordinationError {
    #[error("Task execution failed: {message}")]
    TaskExecutionFailed { message: String },
    #[error("Planning failed: {message}")]
    PlanningFailed { message: String },
    #[error("Condition evaluation failed: {message}")]
    ConditionEvaluationFailed { message: String },
    #[error("Subagent error: {0}")]
    SubagentError(#[from] crate::subagent_manager::SubagentError),
    #[error("Context error: {0}")]
    ContextError(#[from] crate::context_store::ContextError),
}

/// Task planner for generating execution plans
pub struct TaskPlanner {
    context_repo: Arc<InMemoryContextRepository>,
}

impl TaskPlanner {
    pub fn new(context_repo: Arc<InMemoryContextRepository>) -> Self {
        Self { context_repo }
    }
    
    /// Generate execution plan from user request
    pub async fn generate_execution_plan(&self, user_request: &str) -> Result<ExecutionPlan, CoordinationError> {
        // In a real implementation, this would:
        // 1. Analyze the user request
        // 2. Determine required subtasks
        // 3. Identify dependencies
        // 4. Generate optimal execution strategy
        
        // For now, return a simple plan
        let plan_id = uuid::Uuid::new_v4().to_string();
        
        Ok(ExecutionPlan {
            plan_id,
            strategy: ExecutionStrategy::Sequential,
            steps: vec![
                ExecutionStep {
                    step_id: "step-1".to_string(),
                    task_ids: vec!["task-1".to_string()],
                    parallel: false,
                    condition: None,
                }
            ],
            created_at: SystemTime::now(),
        })
    }
    
    /// Analyze user request to identify required subtasks
    async fn analyze_request(&self, request: &str) -> Vec<SubtaskSpec> {
        // In a real implementation, this would use NLP or pattern matching
        // to identify required subtasks
        vec![
            SubtaskSpec {
                agent_type: SubagentType::Explorer,
                title: "Analyze request".to_string(),
                description: format!("Analyze the user request: {request}"),
                priority: TaskPriority::High,
            }
        ]
    }
}

/// Subtask specification for planning
#[derive(Debug, Clone)]
pub struct SubtaskSpec {
    pub agent_type: SubagentType,
    pub title: String,
    pub description: String,
    pub priority: TaskPriority,
}

/// Task priority
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TaskPriority {
    Low,
    Medium,
    High,
    Critical,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_store::InMemoryContextRepository;
    use crate::subagent_manager::InMemorySubagentManager;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn test_task_planner() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let planner = TaskPlanner::new(context_repo);
        
        let plan = planner.generate_execution_plan("Analyze this project and optimize performance").await.unwrap();
        
        assert!(!plan.plan_id.is_empty());
        assert!(matches!(plan.strategy, ExecutionStrategy::Sequential));
        assert_eq!(plan.steps.len(), 1);
    }
    
    #[tokio::test]
    async fn test_subtask_coordinator_creation() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let (event_sender, _) = mpsc::unbounded_channel();
        let subagent_manager = Arc::new(InMemorySubagentManager::new(context_repo.clone(), event_sender.clone()));
        
        let plan = ExecutionPlan {
            plan_id: "test-plan".to_string(),
            strategy: ExecutionStrategy::Sequential,
            steps: Vec::new(),
            created_at: SystemTime::now(),
        };
        
        let shared_context = SharedContext {
            data: HashMap::new(),
            global_context: "Test context".to_string(),
            dependencies: HashMap::new(),
        };
        
        let coordinator = SubtaskCoordinator::new(
            plan,
            shared_context,
            subagent_manager,
            context_repo,
            event_sender,
        );
        
        let status = coordinator.get_status().await;
        assert_eq!(status.active_tasks, 0);
    }
}