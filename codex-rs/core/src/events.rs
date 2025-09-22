use serde::{Serialize, Deserialize};
use std::collections::HashMap;

use crate::exec::ExecToolCallOutput;
use codex_protocol::protocol::{InputItem, SubagentType};

/// Agent orchestration events for the event-driven architecture
/// 
/// These events drive the MainAgent's state machine and enable loose coupling
/// between the MainAgent and SubAgents as described in the orchestration design.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AgentEvent {
    /// Signals a new request for the MainAgent
    UserInputReceived {
        /// The user input items
        input: Vec<InputItem>,
        /// Submission ID for tracking
        sub_id: String,
    },

    /// Fired when a SubAgent successfully finishes its task
    SubagentCompleted {
        /// ID of the completed subagent
        subagent_id: String,
        /// Type of the subagent that completed
        agent_type: SubagentType,
        /// Results and any new context items from the subagent
        results: SubagentCompletionResult,
    },

    /// Fired if a SubAgent encounters an error
    SubagentFailed {
        /// ID of the failed subagent
        subagent_id: String,
        /// Type of the subagent that failed
        agent_type: SubagentType,
        /// Error message describing the failure
        error: String,
        /// Partial results if any were produced before failure
        partial_results: Option<SubagentCompletionResult>,
    },

    /// Fired when a SubAgent is cancelled
    SubagentCancelled {
        /// ID of the cancelled subagent
        subagent_id: String,
        /// Type of the subagent that was cancelled
        agent_type: SubagentType,
        /// Reason for cancellation
        reason: String,
    },

    /// Internal event for MainAgent to transition states
    StateTransitionRequested {
        /// The target state to transition to
        target_state: crate::types::AgentState,
        /// Reason for the transition
        reason: String,
    },

    /// Event fired when summarization is requested
    SummarizationRequested {
        /// Context items to include in summarization
        context_items: Vec<String>,
        /// Whether this is the final summarization
        is_final: bool,
    },
}

/// Results from a completed subagent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentCompletionResult {
    /// Context items created by the subagent
    pub context_items: Vec<String>,
    /// Summary of work performed
    pub summary: String,
    /// Key findings or outputs
    pub outputs: HashMap<String, String>,
    /// Whether the subagent completed successfully
    pub success: bool,
    /// Number of turns used
    pub turns_used: u32,
    /// Whether the subagent reached its turn limit
    pub reached_turn_limit: bool,
}

impl AgentEvent {
    /// Create a UserInputReceived event
    pub fn user_input_received(input: Vec<InputItem>, sub_id: String) -> Self {
        Self::UserInputReceived { input, sub_id }
    }

    /// Create a SubagentCompleted event
    pub fn subagent_completed(
        subagent_id: String,
        agent_type: SubagentType,
        results: SubagentCompletionResult,
    ) -> Self {
        Self::SubagentCompleted {
            subagent_id,
            agent_type,
            results,
        }
    }

    /// Create a SubagentFailed event
    pub fn subagent_failed(
        subagent_id: String,
        agent_type: SubagentType,
        error: String,
        partial_results: Option<SubagentCompletionResult>,
    ) -> Self {
        Self::SubagentFailed {
            subagent_id,
            agent_type,
            error,
            partial_results,
        }
    }

    /// Create a SubagentCancelled event
    pub fn subagent_cancelled(
        subagent_id: String,
        agent_type: SubagentType,
        reason: String,
    ) -> Self {
        Self::SubagentCancelled {
            subagent_id,
            agent_type,
            reason,
        }
    }

    /// Create a StateTransitionRequested event
    pub fn state_transition_requested(
        target_state: crate::types::AgentState,
        reason: String,
    ) -> Self {
        Self::StateTransitionRequested {
            target_state,
            reason,
        }
    }

    /// Create a SummarizationRequested event
    pub fn summarization_requested(context_items: Vec<String>, is_final: bool) -> Self {
        Self::SummarizationRequested {
            context_items,
            is_final,
        }
    }

    /// Get the event type as a string for logging
    pub fn event_type(&self) -> &'static str {
        match self {
            AgentEvent::UserInputReceived { .. } => "UserInputReceived",
            AgentEvent::SubagentCompleted { .. } => "SubagentCompleted",
            AgentEvent::SubagentFailed { .. } => "SubagentFailed",
            AgentEvent::SubagentCancelled { .. } => "SubagentCancelled",
            AgentEvent::StateTransitionRequested { .. } => "StateTransitionRequested",
            AgentEvent::SummarizationRequested { .. } => "SummarizationRequested",
        }
    }
}

// Constants for output formatting
pub(crate) const MODEL_FORMAT_MAX_BYTES: usize = 10 * 1024; // 10 KiB
pub(crate) const MODEL_FORMAT_MAX_LINES: usize = 256; // lines
pub(crate) const MODEL_FORMAT_HEAD_LINES: usize = MODEL_FORMAT_MAX_LINES / 2;
pub(crate) const MODEL_FORMAT_TAIL_LINES: usize = MODEL_FORMAT_MAX_LINES - MODEL_FORMAT_HEAD_LINES; // 128
pub(crate) const MODEL_FORMAT_HEAD_BYTES: usize = MODEL_FORMAT_MAX_BYTES / 2;

/// Format exec output for display to the model with truncation
pub(crate) fn format_exec_output_str(exec_output: &ExecToolCallOutput) -> String {
    let ExecToolCallOutput {
        aggregated_output, ..
    } = exec_output;

    // Head+tail truncation for the model: show the beginning and end with an elision.
    // Clients still receive full streams; only this formatted summary is capped.

    let s = aggregated_output.text.as_str();
    let total_lines = s.lines().count();
    if s.len() <= MODEL_FORMAT_MAX_BYTES && total_lines <= MODEL_FORMAT_MAX_LINES {
        return s.to_string();
    }

    let lines: Vec<&str> = s.lines().collect();
    let head_take = MODEL_FORMAT_HEAD_LINES.min(lines.len());
    let tail_take = MODEL_FORMAT_TAIL_LINES.min(lines.len().saturating_sub(head_take));
    let omitted = lines.len().saturating_sub(head_take + tail_take);

    // Join head and tail blocks (lines() strips newlines; reinsert them)
    let head_block = lines
        .iter()
        .take(head_take)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let tail_block = if tail_take > 0 {
        lines[lines.len() - tail_take..].join("\n")
    } else {
        String::new()
    };
    let marker = format!("\n[... omitted {omitted} of {total_lines} lines ...]\n\n");

    // Byte budgets for head/tail around the marker
    let mut head_budget = MODEL_FORMAT_HEAD_BYTES.min(MODEL_FORMAT_MAX_BYTES);
    let tail_budget = MODEL_FORMAT_MAX_BYTES.saturating_sub(head_budget + marker.len());
    if tail_budget == 0 && marker.len() >= MODEL_FORMAT_MAX_BYTES {
        // Degenerate case: marker alone exceeds budget; return a clipped marker
        return take_bytes_at_char_boundary(&marker, MODEL_FORMAT_MAX_BYTES).to_string();
    }
    if tail_budget == 0 {
        // Make room for the marker by shrinking head
        head_budget = MODEL_FORMAT_MAX_BYTES.saturating_sub(marker.len());
    }

    // Enforce line-count cap by trimming head/tail lines
    let head_lines_text = head_block;
    let tail_lines_text = tail_block;
    // Build final string respecting byte budgets
    let head_part = take_bytes_at_char_boundary(&head_lines_text, head_budget);
    let mut result = String::with_capacity(MODEL_FORMAT_MAX_BYTES.min(s.len()));
    result.push_str(head_part);
    result.push_str(&marker);

    let remaining = MODEL_FORMAT_MAX_BYTES.saturating_sub(result.len());
    let tail_budget_final = remaining;
    let tail_part = take_last_bytes_at_char_boundary(&tail_lines_text, tail_budget_final);
    result.push_str(tail_part);

    result
}

// Truncate a &str to a byte budget at a char boundary (prefix)
#[inline]
fn take_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut last_ok = 0;
    for (i, ch) in s.char_indices() {
        let nb = i + ch.len_utf8();
        if nb > maxb {
            break;
        }
        last_ok = nb;
    }
    &s[..last_ok]
}

// Take a suffix of a &str within a byte budget at a char boundary
#[inline]
fn take_last_bytes_at_char_boundary(s: &str, maxb: usize) -> &str {
    if s.len() <= maxb {
        return s;
    }
    let mut start = s.len();
    let mut used = 0usize;
    for (i, ch) in s.char_indices().rev() {
        let nb = ch.len_utf8();
        if used + nb > maxb {
            break;
        }
        start = i;
        used += nb;
        if start == 0 {
            break;
        }
    }
    &s[start..]
}

/// Exec output is a pre-serialized JSON payload
pub(crate) fn format_exec_output(exec_output: &ExecToolCallOutput) -> String {
    let ExecToolCallOutput {
        exit_code,
        duration,
        ..
    } = exec_output;

    #[derive(Serialize)]
    struct ExecMetadata {
        exit_code: i32,
        duration_seconds: f32,
    }

    #[derive(Serialize)]
    struct ExecOutput<'a> {
        output: &'a str,
        metadata: ExecMetadata,
    }

    // round to 1 decimal place
    let duration_seconds = ((duration.as_secs_f32()) * 10.0).round() / 10.0;

    let formatted_output = format_exec_output_str(exec_output);

    let payload = ExecOutput {
        output: &formatted_output,
        metadata: ExecMetadata {
            exit_code: *exit_code,
            duration_seconds,
        },
    };

    #[expect(clippy::expect_used)]
    serde_json::to_string(&payload).expect("serialize ExecOutput")
}