use std::sync::Arc;
use crate::agent::Agent;
use crate::codex::{AgentTask, TurnContext, Session};
use crate::error::CodexErr;
use crate::error_handling::{SubagentErrorHandler, SubagentError, ErrorSeverity, RecoveryStrategy};
use crate::performance::{PerformanceOptimizer, OptimizedEventSender, TaskPool};
use crate::context_store::{IContextRepository, Context};
use crate::subagent_manager::{ISubagentManager, SubagentTaskSpec};
use codex_protocol::protocol::{
    InputItem, Submission, Op, SubagentType, BootstrapPath, Event, EventMsg,
    SubagentTaskCreatedEvent, SubagentStartedEvent
};

pub struct SubAgent {
    session: Arc<Session>,
    error_handler: SubagentErrorHandler,
    performance_optimizer: Arc<PerformanceOptimizer>,
    task_pool: Arc<TaskPool>,
    event_sender: OptimizedEventSender,
}

impl SubAgent {
    pub fn new(session: Arc<Session>) -> Self {
        let performance_optimizer = Arc::new(PerformanceOptimizer::new(10)); // Max 10 concurrent operations
        let task_pool = Arc::new(TaskPool::new(5)); // Max 5 concurrent tasks
        let event_sender = OptimizedEventSender::new(session.clone(), performance_optimizer.clone());
        
        Self { 
            session,
            error_handler: SubagentErrorHandler::default(),
            performance_optimizer,
            task_pool,
            event_sender,
        }
    }

    pub fn with_error_handler(session: Arc<Session>, error_handler: SubagentErrorHandler) -> Self {
        let performance_optimizer = Arc::new(PerformanceOptimizer::new(10));
        let task_pool = Arc::new(TaskPool::new(5));
        let event_sender = OptimizedEventSender::new(session.clone(), performance_optimizer.clone());
        
        Self {
            session,
            error_handler,
            performance_optimizer,
            task_pool,
            event_sender,
        }
    }

    pub fn with_performance_config(
        session: Arc<Session>, 
        error_handler: SubagentErrorHandler,
        max_concurrent_ops: usize,
        max_concurrent_tasks: usize,
    ) -> Self {
        let performance_optimizer = Arc::new(PerformanceOptimizer::new(max_concurrent_ops));
        let task_pool = Arc::new(TaskPool::new(max_concurrent_tasks));
        let event_sender = OptimizedEventSender::new(session.clone(), performance_optimizer.clone());
        
        Self {
            session,
            error_handler,
            performance_optimizer,
            task_pool,
            event_sender,
        }
    }

    /// Handle subagent-specific operations
    pub fn handle_subagent_operation(&self, submission: Submission) -> bool {
        match submission.op {
            Op::CreateSubagentTask {
                agent_type,
                title,
                description,
                context_refs,
                bootstrap_paths,
            } => {
                self.handle_create_subagent_task(
                    submission.id,
                    agent_type,
                    title,
                    description,
                    context_refs,
                    bootstrap_paths,
                );
                true
            }
            Op::LaunchSubagent { task_id } => {
                self.handle_launch_subagent(submission.id, task_id);
                true
            }
            Op::SubagentReport {
                task_id,
                contexts,
                comments,
                metadata,
            } => {
                self.handle_subagent_report(submission.id, task_id, contexts, comments, metadata);
                true
            }
            Op::ForceCompleteSubagent { task_id } => {
                self.handle_force_complete_subagent(submission.id, task_id);
                true
            }
            Op::CancelSubagent { task_id } => {
                self.handle_cancel_subagent(submission.id, task_id);
                true
            }
            Op::GetSubagentStatus { task_id } => {
                self.handle_get_subagent_status(submission.id, task_id);
                true
            }
            Op::GetSubagentReport { task_id } => {
                self.handle_get_subagent_report(submission.id, task_id);
                true
            }
            Op::ListActiveSubagents => {
                self.handle_list_active_subagents(submission.id);
                true
            }
            _ => false,
        }
    }

    fn handle_create_subagent_task(
        &self,
        sub_id: String,
        agent_type: SubagentType,
        title: String,
        description: String,
        context_refs: Vec<String>,
        bootstrap_paths: Vec<BootstrapPath>,
    ) {
        tracing::info!(
            "Creating subagent task: type={:?}, title={}, description={}, context_refs={:?}, bootstrap_paths={:?}",
            agent_type, title, description, context_refs, bootstrap_paths
        );
        
        // Get the subagent manager from multi-agent components
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let session = self.session.clone();
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            
            // Spawn async task to create subagent
            tokio::spawn(async move {
                let context_refs_count = context_refs.len();
                let bootstrap_paths_count = bootstrap_paths.len();
                
                let spec = SubagentTaskSpec {
                    agent_type: agent_type.clone(),
                    title: title.clone(),
                    description,
                    context_refs,
                    bootstrap_paths: bootstrap_paths.clone(),
                    max_turns: Some(50), // Default max turns
                    timeout_ms: Some(300_000), // 5 minutes default timeout
                    network_access: None,
                };
                
                match subagent_manager.create_task(spec).await {
                    Ok(task_id) => {
                        tracing::info!("Successfully created subagent task: {}", task_id);
                        
                        // Send task created event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::SubagentTaskCreated(SubagentTaskCreatedEvent {
                                task_id,
                                agent_type,
                                title,
                                context_refs_count,
                                bootstrap_paths_count,
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Err(e) => {
                        tracing::error!("Failed to create subagent task: {}", e);
                    }
                }
            });
        } else {
            tracing::warn!("No multi-agent components available");
        }
    }

    fn handle_launch_subagent(&self, sub_id: String, task_id: String) {
        tracing::info!("Launching subagent: task_id={}", task_id);
        
        // Get the subagent manager from multi-agent components
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let session = self.session.clone();
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let task_id_clone = task_id.clone();
            let error_handler = self.error_handler.clone();
            
            // Spawn async task to launch subagent with enhanced error handling
            tokio::spawn(async move {
                let operation_name = format!("launch_subagent_{}", task_id_clone);
                
                let result = error_handler.execute_with_retry(&operation_name, || {
                    let manager = subagent_manager.clone();
                    let task_id = task_id_clone.clone();
                    Box::pin(async move {
                        manager.launch_subagent(&task_id).await
                    })
                }).await;
                
                match result {
                    Ok(handle) => {
                        tracing::info!("Successfully launched subagent: {}", task_id_clone);
                        
                        // Send subagent started event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::SubagentStarted(SubagentStartedEvent {
                                task_id: handle.task_id,
                                agent_type: handle.agent_type,
                                title: "Subagent Task".to_string(), // We'd need to get this from the task
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Err(subagent_error) => {
                        tracing::error!("Failed to launch subagent {}: {}", task_id_clone, subagent_error);
                        
                        // Send detailed error event with recovery suggestions
                        let recovery_strategy = subagent_error.recovery_strategy();
                        let user_message = subagent_error.user_message();
                        
                        let error_message = match recovery_strategy {
                            RecoveryStrategy::Retry => {
                                format!("{} (Will retry automatically)", user_message)
                            }
                            RecoveryStrategy::Reset => {
                                format!("{} (Task will be reset)", user_message)
                            }
                            RecoveryStrategy::Fallback => {
                                format!("{} (Trying alternative approach)", user_message)
                            }
                            RecoveryStrategy::Escalate => {
                                format!("{} (Manual intervention required)", user_message)
                            }
                            RecoveryStrategy::Ignore => {
                                format!("{} (Continuing with other tasks)", user_message)
                            }
                        };
                        
                        let event = Event {
                            id: sub_id.clone(),
                            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                                message: error_message,
                            }),
                        };
                        session.send_event(event).await;
                        
                        // If it's a critical error, send additional warning
                        if subagent_error.is_critical() {
                            let warning_event = Event {
                                id: sub_id,
                                msg: EventMsg::BackgroundEvent(codex_protocol::protocol::BackgroundEventEvent {
                                    message: "Critical error detected. Please check system configuration.".to_string(),
                                }),
                            };
                            session.send_event(warning_event).await;
                        }
                    }
                }
            });
        } else {
            tracing::warn!("No multi-agent components available");
            
            // Send configuration error
            let session = self.session.clone();
            tokio::spawn(async move {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                        message: "Multi-agent functionality not enabled (Configuration error)".to_string(),
                    }),
                };
                session.send_event(event).await;
            });
        }
    }

    fn handle_subagent_report(
        &self,
        _sub_id: String,
        task_id: String,
        contexts: Vec<codex_protocol::protocol::ContextItem>,
        comments: String,
        _metadata: codex_protocol::protocol::SubagentMetadata,
    ) {
        tracing::info!(
            "Subagent report: task_id={}, contexts_count={}, comments={}",
            task_id, contexts.len(), comments
        );
        
        // Store the contexts in the context repository if available
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let context_repo = multi_agent_components.context_repo.clone();
            let task_id_for_context = task_id.clone();
            tokio::spawn(async move {
                for context_item in contexts {
                    let now = std::time::SystemTime::now();
                    let context = Context {
                        id: context_item.id.clone(),
                        summary: context_item.summary,
                        content: context_item.content,
                        created_by: "subagent".to_string(),
                        task_id: Some(task_id_for_context.clone()),
                        created_at: now,
                        updated_at: now,
                        tags: vec![],
                        metadata: std::collections::HashMap::new(),
                    };
                    if let Err(e) = context_repo.store_context(context).await {
                        tracing::error!("Failed to store context {}: {}", context_item.id, e);
                    }
                }
            });
        }
        
        // Log the report for now - in a full implementation, this would be stored
        tracing::info!("Subagent {} report: {}", task_id, comments);
    }

    fn handle_force_complete_subagent(&self, sub_id: String, task_id: String) {
        tracing::info!("Force completing subagent: task_id={}", task_id);
        
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let task_id_clone = task_id.clone();
            let session = self.session.clone();
            
            tokio::spawn(async move {
                match subagent_manager.force_complete_task(&task_id_clone).await {
                    Ok(report) => {
                        tracing::info!("Successfully force completed subagent: {}", task_id_clone);
                        
                        // Get task information for the event
                        let (agent_type, title) = if let Ok(task) = subagent_manager.get_task(&task_id_clone).await {
                            (task.agent_type, task.title)
                        } else {
                            (codex_protocol::protocol::SubagentType::Explorer, "Unknown Task".to_string())
                        };
                        
                        // Send force completed event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::SubagentForceCompleted(codex_protocol::protocol::SubagentForceCompletedEvent {
                                task_id: task_id_clone,
                                agent_type,
                                title,
                                num_turns: report.metadata.num_turns,
                                max_turns: report.metadata.max_turns,
                                contexts_created: report.contexts.len(),
                                comments: report.comments,
                                metadata: report.metadata,
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Err(e) => {
                        tracing::error!("Failed to force complete subagent {}: {}", task_id_clone, e);
                        
                        // Send error event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                                message: format!("Failed to force complete subagent: {}", e),
                            }),
                        };
                        session.send_event(event).await;
                    }
                }
            });
        } else {
            // Send error event if multi-agent components not available
            let session = self.session.clone();
            tokio::spawn(async move {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                        message: "Multi-agent functionality not enabled".to_string(),
                    }),
                };
                session.send_event(event).await;
            });
        }
    }

    fn handle_cancel_subagent(&self, sub_id: String, task_id: String) {
        tracing::info!("Cancelling subagent: task_id={}", task_id);
        
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let task_id_clone = task_id.clone();
            let session = self.session.clone();
            
            tokio::spawn(async move {
                // First get task info before cancelling
                let task_info = subagent_manager.get_task_status(&task_id_clone).await;
                
                match subagent_manager.cancel_task(&task_id_clone).await {
                    Ok(()) => {
                        tracing::info!("Successfully cancelled subagent: {}", task_id_clone);
                        
                        // Get task information for the event
                        let (agent_type, title, cancelled_at_turn) = if let Ok(task) = subagent_manager.get_task(&task_id_clone).await {
                            (task.agent_type, task.title, 0) // TODO: track current turn properly
                        } else {
                            (codex_protocol::protocol::SubagentType::Explorer, "Unknown Task".to_string(), 0)
                        };
                        
                        // Send cancelled event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::SubagentCancelled(codex_protocol::protocol::SubagentCancelledEvent {
                                task_id: task_id_clone,
                                agent_type,
                                title,
                                reason: "User requested cancellation".to_string(),
                                cancelled_at_turn,
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Err(e) => {
                        tracing::error!("Failed to cancel subagent {}: {}", task_id_clone, e);
                        
                        // Send error event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                                message: format!("Failed to cancel subagent: {}", e),
                            }),
                        };
                        session.send_event(event).await;
                    }
                }
            });
        } else {
            // Send error event if multi-agent components not available
            let session = self.session.clone();
            tokio::spawn(async move {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                        message: "Multi-agent functionality not enabled".to_string(),
                    }),
                };
                session.send_event(event).await;
            });
        }
    }

    fn handle_get_subagent_status(&self, sub_id: String, task_id: String) {
        tracing::info!("Getting subagent status: task_id={}", task_id);
        
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let task_id_clone = task_id.clone();
            let event_sender = self.event_sender.clone();
            let task_pool = self.task_pool.clone();
            let performance_optimizer = self.performance_optimizer.clone();
            let performance_optimizer_for_cache = self.performance_optimizer.clone();
            
            // Use task pool for better concurrency management
            let task_pool_clone = task_pool.clone();
            tokio::spawn(async move {
                task_pool_clone.execute(async move {
                    // Use performance optimizer for rate limiting
                    performance_optimizer.execute_with_rate_limit(async move {
                        // Get task status directly (no caching due to Clone trait issues)
                        let status_result = subagent_manager.get_task_status(&task_id_clone).await;
                        
                        match status_result {
                            Ok(status) => {
                                tracing::info!("Subagent {} status: {:?}", task_id_clone, status);
                                
                                // Extract information from TaskStatus enum
                                let (current_turn, max_turns, status_message) = match &status {
                                    crate::subagent_manager::TaskStatus::Running { current_turn, max_turns } => {
                                        (*current_turn, *max_turns, "Running".to_string())
                                    }
                                    crate::subagent_manager::TaskStatus::Created => {
                                        (0, 50, "Created".to_string())
                                    }
                                    crate::subagent_manager::TaskStatus::Completed { .. } => {
                                        (0, 50, "Completed".to_string())
                                    }
                                    crate::subagent_manager::TaskStatus::Failed { error } => {
                                        (0, 50, format!("Failed: {}", error))
                                    }
                                    crate::subagent_manager::TaskStatus::Cancelled => {
                                        (0, 50, "Cancelled".to_string())
                                    }
                                };
                                
                                // Send status event using optimized sender
                                let event = Event {
                                    id: sub_id,
                                    msg: EventMsg::SubagentProgress(codex_protocol::protocol::SubagentProgressEvent {
                                        task_id: task_id_clone,
                                        current_turn,
                                        max_turns,
                                        status_message,
                                    }),
                                };
                                event_sender.send_event(event).await;
                            }
                            Err(e) => {
                                tracing::error!("Failed to get subagent {} status: {}", task_id_clone, e);
                                
                                // Send error event using optimized sender
                                event_sender.send_error(
                                    sub_id,
                                    format!("Failed to get subagent status: {}", e)
                                ).await;
                            }
                        }
                    }).await
                }).await;
            });
        } else {
            // Send error event if multi-agent components not available
            let event_sender = self.event_sender.clone();
            tokio::spawn(async move {
                event_sender.send_error(
                    sub_id,
                    "Multi-agent functionality not enabled".to_string()
                ).await;
            });
        }
    }

    fn handle_get_subagent_report(&self, sub_id: String, task_id: String) {
        tracing::info!("Getting subagent report: task_id={}", task_id);
        
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let task_id_clone = task_id.clone();
            let session = self.session.clone();
            
            tokio::spawn(async move {
                match subagent_manager.get_task_report(&task_id_clone).await {
                    Ok(Some(report)) => {
                        tracing::info!("Retrieved subagent {} report", task_id_clone);
                        
                        // Send subagent fallback report event with the retrieved report
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::SubagentFallbackReport(codex_protocol::protocol::SubagentFallbackReportEvent {
                                task_id: task_id_clone.clone(),
                                agent_type: if let Ok(task) = subagent_manager.get_task(&task_id_clone).await { task.agent_type } else { codex_protocol::protocol::SubagentType::Explorer },
                                title: if let Ok(task) = subagent_manager.get_task(&task_id_clone).await { task.title } else { "Unknown Task".to_string() },
                                reason: "Report retrieved".to_string(),
                                contexts_created: report.contexts.len(),
                                metadata: report.metadata,
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Ok(None) => {
                        tracing::info!("No report available for subagent {}", task_id_clone);
                        
                        // Send background event indicating no report available
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::BackgroundEvent(codex_protocol::protocol::BackgroundEventEvent {
                                message: format!("No report available for subagent {}", task_id_clone),
                            }),
                        };
                        session.send_event(event).await;
                    }
                    Err(e) => {
                        tracing::error!("Failed to get subagent {} report: {}", task_id_clone, e);
                        
                        // Send error event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                                message: format!("Failed to get subagent report: {}", e),
                            }),
                        };
                        session.send_event(event).await;
                    }
                }
            });
        } else {
            // Send error event if multi-agent components not available
            let session = self.session.clone();
            tokio::spawn(async move {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                        message: "Multi-agent functionality not enabled".to_string(),
                    }),
                };
                session.send_event(event).await;
            });
        }
    }

    fn handle_list_active_subagents(&self, sub_id: String) {
        tracing::info!("Listing active subagents");
        
        if let Some(multi_agent_components) = self.session.get_multi_agent_components() {
            let subagent_manager = multi_agent_components.subagent_manager.clone();
            let session = self.session.clone();
            
            tokio::spawn(async move {
                match subagent_manager.get_active_tasks().await {
                    Ok(tasks) => {
                        tracing::info!("Found {} active subagents", tasks.len());
                        
                        // Send multi-agent status event
                        let event = Event {
                            id: sub_id.clone(),
                            msg: EventMsg::MultiAgentStatus(codex_protocol::protocol::MultiAgentStatusEvent {
                                active_tasks: tasks.len(),
                                completed_tasks: 0, // We'd need to track this separately
                                total_contexts: 0, // We'd need to query the context repository
                                status: format!("{} active subagent tasks", tasks.len()),
                            }),
                        };
                        session.send_event(event).await;
                        
                        // Send background event with task details
                        if !tasks.is_empty() {
                            let task_list = tasks
                                .iter()
                                .map(|t| format!("- {} ({:?}): {}", t.task_id, t.agent_type, t.title))
                                .collect::<Vec<_>>()
                                .join("\n");
                            
                            let event = Event {
                                id: sub_id.clone(),
                                msg: EventMsg::BackgroundEvent(codex_protocol::protocol::BackgroundEventEvent {
                                    message: format!("Active subagent tasks:\n{}", task_list),
                                }),
                            };
                            session.send_event(event).await;
                        } else {
                            let event = Event {
                                id: sub_id,
                                msg: EventMsg::BackgroundEvent(codex_protocol::protocol::BackgroundEventEvent {
                                    message: "No active subagent tasks".to_string(),
                                }),
                            };
                            session.send_event(event).await;
                        }
                    }
                    Err(e) => {
                        tracing::error!("Failed to list active subagents: {}", e);
                        
                        // Send error event
                        let event = Event {
                            id: sub_id,
                            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                                message: format!("Failed to list active subagents: {}", e),
                            }),
                        };
                        session.send_event(event).await;
                    }
                }
            });
        } else {
            // Send error event if multi-agent components not available
            let session = self.session.clone();
            tokio::spawn(async move {
                let event = Event {
                    id: sub_id,
                    msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent {
                        message: "Multi-agent functionality not enabled".to_string(),
                    }),
                };
                session.send_event(event).await;
            });
        }
    }
}

impl Agent for SubAgent {
    fn handle_submission(&self, submission: Submission) -> Result<AgentTask, anyhow::Error> {
        // SubAgent primarily handles subagent-specific operations
        // For other operations, it would delegate to appropriate handlers
        match submission.op {
            Op::CreateSubagentTask { .. } |
            Op::LaunchSubagent { .. } |
            Op::SubagentReport { .. } |
            Op::ForceCompleteSubagent { .. } |
            Op::CancelSubagent { .. } |
            Op::GetSubagentStatus { .. } |
            Op::GetSubagentReport { .. } |
            Op::ListActiveSubagents => {
                // These operations don't create new tasks, they manage existing ones
                Err(anyhow::anyhow!("Subagent operations don't create new tasks"))
            }
            _ => {
                // For other operations, SubAgent doesn't handle them
                Err(anyhow::anyhow!("Operation not handled by SubAgent: {:?}", submission.op))
            }
        }
    }

    async fn run_turn(&self, _turn_context: &TurnContext, _input: Vec<InputItem>) -> Result<(), CodexErr> {
        // SubAgent doesn't run turns in the traditional sense
        // It manages subagent tasks and coordinates between them
        // The actual turn execution would be handled by the specific subagent implementations
        Ok(())
    }
}