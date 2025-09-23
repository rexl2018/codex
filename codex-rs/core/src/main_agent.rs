use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio::time::timeout;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::agent::Agent;
use crate::agent_task::AgentTask;
use crate::agent_task_factory::AgentTaskFactory;
use crate::error::CodexErr;
use crate::events::AgentEvent;
use crate::events::SubagentCompletionResult;
use crate::session::Session;
use crate::state::AgentStateManager;
use crate::turn_context::TurnContext;
use crate::types::AgentState;
use codex_protocol::protocol::InputItem;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ReviewDecision;
use codex_protocol::protocol::SubagentType;
use codex_protocol::protocol::Submission;

/// MainAgent - The orchestrator that manages the entire lifecycle of user requests
///
/// This agent runs as a long-running service and operates as a state machine to manage
/// the workflow between user input, subagent creation, and result summarization.
pub struct MainAgent {
    session: Arc<Session>,
    state_manager: Arc<AgentStateManager>,
    event_receiver: mpsc::UnboundedReceiver<AgentEvent>,
    event_sender: mpsc::UnboundedSender<AgentEvent>,
    shutdown_receiver: Option<oneshot::Receiver<()>>,
    is_running: bool,
}

impl MainAgent {
    /// Create a new MainAgent with event channels
    pub fn new(session: Arc<Session>) -> (Self, mpsc::UnboundedSender<AgentEvent>) {
        let (event_sender, event_receiver) = mpsc::unbounded_channel();
        let state_manager = Arc::new(AgentStateManager::new());

        let agent = Self {
            session,
            state_manager,
            event_receiver,
            event_sender: event_sender.clone(),
            shutdown_receiver: None,
            is_running: false,
        };

        (agent, event_sender)
    }

    /// Create a new MainAgent with a shutdown channel
    pub fn with_shutdown(
        session: Arc<Session>,
        shutdown_receiver: oneshot::Receiver<()>,
    ) -> (Self, mpsc::UnboundedSender<AgentEvent>) {
        let (mut agent, event_sender) = Self::new(session);
        agent.shutdown_receiver = Some(shutdown_receiver);
        (agent, event_sender)
    }

    /// Get the current state manager
    pub fn state_manager(&self) -> Arc<AgentStateManager> {
        self.state_manager.clone()
    }

    /// Get the event sender for external components to send events
    pub fn event_sender(&self) -> mpsc::UnboundedSender<AgentEvent> {
        self.event_sender.clone()
    }

    /// Run the MainAgent's event loop
    ///
    /// This is the main orchestration loop that processes events and manages state transitions.
    /// It runs until a shutdown signal is received or an unrecoverable error occurs.
    pub async fn run(&mut self) -> Result<(), CodexErr> {
        info!("MainAgent starting event loop");
        self.is_running = true;
        self.state_manager.transition_to_idle();

        loop {
            tokio::select! {
                // Handle shutdown signal
                _ = async {
                    if let Some(ref mut shutdown_rx) = self.shutdown_receiver {
                        shutdown_rx.await.ok()
                    } else {
                        std::future::pending().await
                    }
                } => {
                    info!("MainAgent received shutdown signal");
                    break;
                }

                // Handle incoming events
                event = self.event_receiver.recv() => {
                    match event {
                        Some(event) => {
                            if let Err(e) = self.handle_event(event).await {
                                error!("Error handling event: {}", e);
                                // Continue processing other events unless it's a critical error
                            }
                        }
                        None => {
                            warn!("Event channel closed, shutting down MainAgent");
                            break;
                        }
                    }
                }

                // Periodic state check (every 30 seconds)
                _ = tokio::time::sleep(Duration::from_secs(30)) => {
                    self.periodic_state_check().await;
                }
            }
        }

        self.is_running = false;
        info!("MainAgent event loop terminated");
        Ok(())
    }

    /// Handle an incoming agent event
    async fn handle_event(&self, event: AgentEvent) -> Result<(), CodexErr> {
        debug!("MainAgent handling event: {}", event.event_type());

        match event {
            AgentEvent::UserInputReceived { input, sub_id } => {
                self.handle_user_input_event(input, sub_id).await
            }
            AgentEvent::SubagentCompleted {
                subagent_id,
                agent_type,
                results,
            } => {
                self.handle_subagent_completed(subagent_id, agent_type, results)
                    .await
            }
            AgentEvent::SubagentFailed {
                subagent_id,
                agent_type,
                error,
                partial_results,
            } => {
                self.handle_subagent_failed(subagent_id, agent_type, error, partial_results)
                    .await
            }
            AgentEvent::SubagentCancelled {
                subagent_id,
                agent_type,
                reason,
            } => {
                self.handle_subagent_cancelled(subagent_id, agent_type, reason)
                    .await
            }
            AgentEvent::StateTransitionRequested {
                target_state,
                reason,
            } => {
                self.handle_state_transition_request(target_state, reason)
                    .await
            }
            AgentEvent::SummarizationRequested {
                context_items,
                is_final,
            } => {
                self.handle_summarization_request(context_items, is_final)
                    .await
            }
        }
    }

    /// Handle user input received event
    async fn handle_user_input_event(
        &self,
        input: Vec<InputItem>,
        sub_id: String,
    ) -> Result<(), CodexErr> {
        info!("MainAgent processing user input: sub_id={}", sub_id);

        // Transition to DecidingNextStep state
        self.state_manager.transition_to_deciding_next_step();

        // Process the user input and decide next steps
        // This would typically involve LLM interaction to determine the appropriate action
        self.decide_next_step_for_user_input(input, sub_id).await
    }

    /// Handle subagent completion
    async fn handle_subagent_completed(
        &self,
        subagent_id: String,
        agent_type: SubagentType,
        results: SubagentCompletionResult,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent handling subagent completion: id={}, type={:?}",
            subagent_id, agent_type
        );

        // Determine if we should summarize or continue with more subagents
        let should_summarize = self.should_start_summarization(&results).await;

        // Handle the completion and transition to appropriate state
        self.state_manager
            .handle_subagent_completion(&subagent_id, should_summarize);

        if should_summarize {
            // Start summarization process
            self.start_summarization(results.context_items).await
        } else {
            // The actual decision making happens in the main task loop via run_turn
            // when the state transitions to DecidingNextStep
            info!(
                "Subagent completed, state transitioned to DecidingNextStep. LLM will be called in next turn to decide next steps: id={}, type={:?}",
                subagent_id, agent_type
            );
            Ok(())
        }
    }

    /// Handle subagent failure
    async fn handle_subagent_failed(
        &self,
        subagent_id: String,
        agent_type: SubagentType,
        error: String,
        partial_results: Option<SubagentCompletionResult>,
    ) -> Result<(), CodexErr> {
        warn!(
            "MainAgent handling subagent failure: id={}, type={:?}, error={}",
            subagent_id, agent_type, error
        );

        // Transition back to DecidingNextStep to handle the failure
        self.state_manager.transition_to_deciding_next_step();

        // Decide how to handle the failure (retry, alternative approach, etc.)
        self.handle_failure_recovery(subagent_id, agent_type, error, partial_results)
            .await
    }

    /// Handle subagent cancellation
    async fn handle_subagent_cancelled(
        &self,
        subagent_id: String,
        agent_type: SubagentType,
        reason: String,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent handling subagent cancellation: id={}, type={:?}, reason={}",
            subagent_id, agent_type, reason
        );

        // Transition back to DecidingNextStep
        self.state_manager.transition_to_deciding_next_step();

        // Decide next steps after cancellation
        self.decide_next_step_after_cancellation(subagent_id, agent_type, reason)
            .await
    }

    /// Handle state transition request
    async fn handle_state_transition_request(
        &self,
        target_state: AgentState,
        reason: String,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent handling state transition request: target={:?}, reason={}",
            target_state, reason
        );

        // Validate and perform the state transition
        self.state_manager.set_state(target_state);

        Ok(())
    }

    /// Handle summarization request
    async fn handle_summarization_request(
        &self,
        context_items: Vec<String>,
        is_final: bool,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent handling summarization request: contexts={}, final={}",
            context_items.len(),
            is_final
        );

        if is_final {
            self.state_manager.transition_to_summarizing();
        }

        // Perform summarization
        self.perform_summarization(context_items, is_final).await
    }

    /// Periodic state check to ensure consistency
    async fn periodic_state_check(&self) {
        let current_state = self.state_manager.get_state();
        debug!("MainAgent periodic state check: {:?}", current_state);

        // Check for any inconsistencies or stuck states
        // This could include checking if subagents are still running, etc.
    }

    /// Decide next steps for user input (placeholder for LLM interaction)
    async fn decide_next_step_for_user_input(
        &self,
        input: Vec<InputItem>,
        sub_id: String,
    ) -> Result<(), CodexErr> {
        // This is where the MainAgent would interact with an LLM to decide
        // whether to create a subagent, handle the request directly, etc.

        // For now, this is a placeholder implementation
        info!(
            "MainAgent deciding next step for user input: sub_id={}",
            sub_id
        );

        // Example: Create a task to handle the input
        // In the full implementation, this would involve LLM decision making
        Ok(())
    }

    /// Determine if summarization should start based on results
    async fn should_start_summarization(&self, results: &SubagentCompletionResult) -> bool {
        // This logic would determine if all necessary work is complete
        // and summarization should begin

        // For now, simple heuristic: if the subagent indicates completion
        results.success && !results.reached_turn_limit
    }

    /// Start the summarization process
    async fn start_summarization(&self, context_items: Vec<String>) -> Result<(), CodexErr> {
        info!(
            "MainAgent starting summarization with {} context items",
            context_items.len()
        );

        // This would involve creating a summarization task or calling LLM
        // to compile final results

        // After summarization is complete, transition back to idle
        self.state_manager.transition_to_idle();

        Ok(())
    }



    /// Handle failure recovery
    async fn handle_failure_recovery(
        &self,
        subagent_id: String,
        agent_type: SubagentType,
        error: String,
        partial_results: Option<SubagentCompletionResult>,
    ) -> Result<(), CodexErr> {
        warn!(
            "MainAgent handling failure recovery for subagent {}: {}",
            subagent_id, error
        );

        // This would involve deciding whether to retry, try a different approach,
        // or escalate the failure

        Ok(())
    }

    /// Decide next steps after cancellation
    async fn decide_next_step_after_cancellation(
        &self,
        subagent_id: String,
        agent_type: SubagentType,
        reason: String,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent deciding next step after cancellation: id={}, reason={}",
            subagent_id, reason
        );

        // This would involve determining how to proceed after a cancellation

        Ok(())
    }

    /// Perform summarization
    async fn perform_summarization(
        &self,
        context_items: Vec<String>,
        is_final: bool,
    ) -> Result<(), CodexErr> {
        info!(
            "MainAgent performing summarization: contexts={}, final={}",
            context_items.len(),
            is_final
        );

        // This would involve the actual summarization logic

        if is_final {
            // After final summarization, transition back to idle
            self.state_manager.transition_to_idle();
        }

        Ok(())
    }

    /// Check if the MainAgent is currently running
    pub fn is_running(&self) -> bool {
        self.is_running
    }

    /// Get the current state description
    pub fn get_state_description(&self) -> String {
        self.state_manager.get_state_description()
    }

    /// Legacy method: Handle UserInput operations
    pub fn handle_user_input(
        &self,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        items: Vec<InputItem>,
    ) -> Result<AgentTask, anyhow::Error> {
        // attempt to inject input into current task
        if let Err(items) = self.session.inject_input(items) {
            // no current task, spawn a new one
            tracing::debug!("Creating agent task for UserInput: sub_id={}", sub_id);
            tracing::info!("Creating agent task for UserInput: sub_id={}", sub_id);

            let task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context.clone(),
                sub_id.clone(),
                items,
            );

            // Set the task in the session
            self.session.set_task(task);

            // Return a new task for the caller
            let new_task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context,
                sub_id,
                vec![], // Empty input since the task is already set
            );
            Ok(new_task)
        } else {
            // Input was injected into existing task, no new task needed
            Err(anyhow::anyhow!("Input injected into existing task"))
        }
    }

    /// Handle UserTurn operations  
    pub fn handle_user_turn(
        &self,
        turn_context: Arc<TurnContext>,
        sub_id: String,
        items: Vec<InputItem>,
    ) -> Result<AgentTask, anyhow::Error> {
        // attempt to inject input into current task
        if let Err(items) = self.session.inject_input(items) {
            let task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context.clone(),
                sub_id.clone(),
                items,
            );

            // Set the task in the session
            self.session.set_task(task);

            // Return a new task for the caller
            let new_task = AgentTaskFactory::create_regular_task(
                self.session.clone(),
                turn_context,
                sub_id,
                vec![], // Empty input since the task is already set
            );
            Ok(new_task)
        } else {
            // Input was injected into existing task, no new task needed
            Err(anyhow::anyhow!("Input injected into existing task"))
        }
    }

    /// Handle session-level operations that don't require agent delegation
    pub fn handle_session_operation(&self, submission: Submission) -> bool {
        match submission.op {
            Op::Interrupt => {
                self.session.interrupt_task();
                true
            }
            Op::ExecApproval { id, decision } => {
                match decision {
                    ReviewDecision::Abort => {
                        self.session.interrupt_task();
                    }
                    other => {
                        self.session.notify_approval(&id, other);
                    }
                }
                true
            }
            Op::PatchApproval { id, decision } => {
                match decision {
                    ReviewDecision::Abort => {
                        self.session.interrupt_task();
                    }
                    other => {
                        self.session.notify_approval(&id, other);
                    }
                }
                true
            }
            Op::Shutdown => {
                // This should be handled by the session itself
                false
            }
            _ => false,
        }
    }
}

impl Agent for MainAgent {
    fn handle_submission(&self, submission: Submission) -> Result<AgentTask, anyhow::Error> {
        match submission.op {
            Op::UserInput { items } => {
                // Convert to event and send to the event loop
                let event = AgentEvent::user_input_received(items, submission.id.clone());
                if let Err(e) = self.event_sender.send(event) {
                    error!("Failed to send UserInput event: {}", e);
                    return Err(anyhow::anyhow!("Failed to send UserInput event: {}", e));
                }

                // Return a placeholder task - the actual processing happens in the event loop
                // This maintains compatibility with the existing interface
                Err(anyhow::anyhow!(
                    "UserInput converted to event - processing in MainAgent event loop"
                ))
            }
            Op::UserTurn { items, .. } => {
                // Convert to event and send to the event loop
                let event = AgentEvent::user_input_received(items, submission.id.clone());
                if let Err(e) = self.event_sender.send(event) {
                    error!("Failed to send UserTurn event: {}", e);
                    return Err(anyhow::anyhow!("Failed to send UserTurn event: {}", e));
                }

                // Return a placeholder task - the actual processing happens in the event loop
                Err(anyhow::anyhow!(
                    "UserTurn converted to event - processing in MainAgent event loop"
                ))
            }
            _ => {
                // Other operations are handled by session-level operations
                if self.handle_session_operation(submission) {
                    Err(anyhow::anyhow!("Operation handled at session level"))
                } else {
                    Err(anyhow::anyhow!("Operation not handled by MainAgent"))
                }
            }
        }
    }

    async fn run_turn(
        &self,
        _turn_context: &TurnContext,
        _input: Vec<InputItem>,
    ) -> Result<(), CodexErr> {
        // The MainAgent doesn't run individual turns - it orchestrates through events
        // The actual processing happens in the run() method's event loop
        warn!("MainAgent::run_turn called - this should not happen in the new architecture");
        Ok(())
    }
}
