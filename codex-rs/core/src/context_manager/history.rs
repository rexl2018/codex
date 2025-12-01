use crate::codex::TurnContext;
use crate::context_manager::normalize;
use crate::truncate::TruncationPolicy;
use crate::truncate::approx_token_count;
use crate::truncate::approx_tokens_from_byte_count;
use crate::truncate::truncate_function_output_items_with_policy;
use crate::truncate::truncate_text;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ReasoningItemReasoningSummary;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::HistoryAction;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::protocol::TokenUsageInfo;
use std::ops::Deref;

pub(crate) struct HistoryActionOutput {
    pub content: String,
    pub history_changed: bool,
}

impl HistoryActionOutput {
    fn changed(content: String) -> Self {
        Self {
            content,
            history_changed: true,
        }
    }

    fn unchanged(content: String) -> Self {
        Self {
            content,
            history_changed: false,
        }
    }
}

/// Transcript of conversation history
#[derive(Debug, Clone, Default)]
pub(crate) struct ContextManager {
    /// The oldest items are at the beginning of the vector.
    items: Vec<ResponseItem>,
    token_info: Option<TokenUsageInfo>,
}

impl ContextManager {
    pub(crate) fn new() -> Self {
        Self {
            items: Vec::new(),
            token_info: TokenUsageInfo::new_or_append(&None, &None, None),
        }
    }

    pub(crate) fn token_info(&self) -> Option<TokenUsageInfo> {
        self.token_info.clone()
    }

    pub(crate) fn set_token_info(&mut self, info: Option<TokenUsageInfo>) {
        self.token_info = info;
    }

    pub(crate) fn set_token_usage_full(&mut self, context_window: i64) {
        match &mut self.token_info {
            Some(info) => info.fill_to_context_window(context_window),
            None => {
                self.token_info = Some(TokenUsageInfo::full_context_window(context_window));
            }
        }
    }

    /// `items` is ordered from oldest to newest.
    pub(crate) fn record_items<I>(&mut self, items: I, policy: TruncationPolicy)
    where
        I: IntoIterator,
        I::Item: std::ops::Deref<Target = ResponseItem>,
    {
        for item in items {
            let item_ref = item.deref();
            let is_ghost_snapshot = matches!(item_ref, ResponseItem::GhostSnapshot { .. });
            if !is_api_message(item_ref) && !is_ghost_snapshot {
                continue;
            }

            let processed = self.process_item(item_ref, policy);
            self.items.push(processed);
        }
    }

    pub(crate) fn get_history(&mut self) -> Vec<ResponseItem> {
        self.normalize_history();
        self.contents()
    }

    // Returns the history prepared for sending to the model.
    // With extra response items filtered out and GhostCommits removed.
    pub(crate) fn get_history_for_prompt(&mut self) -> Vec<ResponseItem> {
        let mut history = self.get_history();
        Self::remove_ghost_snapshots(&mut history);
        history
    }

    // Estimate token usage using byte-based heuristics from the truncation helpers.
    // This is a coarse lower bound, not a tokenizer-accurate count.
    pub(crate) fn estimate_token_count(&self, turn_context: &TurnContext) -> Option<i64> {
        let model_family = turn_context.client.get_model_family();
        let base_tokens =
            i64::try_from(approx_token_count(model_family.base_instructions.as_str()))
                .unwrap_or(i64::MAX);

        let items_tokens = self.items.iter().fold(0i64, |acc, item| {
            acc + match item {
                ResponseItem::Reasoning {
                    encrypted_content: Some(content),
                    ..
                }
                | ResponseItem::CompactionSummary {
                    encrypted_content: content,
                } => estimate_reasoning_length(content.len()) as i64,
                item => {
                    let serialized = serde_json::to_string(item).unwrap_or_default();
                    i64::try_from(approx_token_count(&serialized)).unwrap_or(i64::MAX)
                }
            }
        });

        Some(base_tokens.saturating_add(items_tokens))
    }

    pub(crate) fn remove_first_item(&mut self) {
        if !self.items.is_empty() {
            // Remove the oldest item (front of the list). Items are ordered from
            // oldest → newest, so index 0 is the first entry recorded.
            let removed = self.items.remove(0);
            // If the removed item participates in a call/output pair, also remove
            // its corresponding counterpart to keep the invariants intact without
            // running a full normalization pass.
            normalize::remove_corresponding_for(&mut self.items, &removed);
        }
    }

    pub(crate) fn replace(&mut self, items: Vec<ResponseItem>) {
        self.items = items;
    }

    pub(crate) fn update_token_info(
        &mut self,
        usage: &TokenUsage,
        model_context_window: Option<i64>,
    ) {
        self.token_info = TokenUsageInfo::new_or_append(
            &self.token_info,
            &Some(usage.clone()),
            model_context_window,
        );
    }

    fn get_non_last_reasoning_items_tokens(&self) -> usize {
        // get reasoning items excluding all the ones after the last user message
        let Some(last_user_index) = self
            .items
            .iter()
            .rposition(|item| matches!(item, ResponseItem::Message { role, .. } if role == "user"))
        else {
            return 0usize;
        };

        let total_reasoning_bytes = self
            .items
            .iter()
            .take(last_user_index)
            .filter_map(|item| {
                if let ResponseItem::Reasoning {
                    encrypted_content: Some(content),
                    ..
                } = item
                {
                    Some(content.len())
                } else {
                    None
                }
            })
            .map(estimate_reasoning_length)
            .fold(0usize, usize::saturating_add);

        let token_estimate = approx_tokens_from_byte_count(total_reasoning_bytes);
        token_estimate as usize
    }

    pub(crate) fn get_total_token_usage(&self) -> i64 {
        self.token_info
            .as_ref()
            .map(|info| info.last_token_usage.total_tokens)
            .unwrap_or(0)
            .saturating_add(self.get_non_last_reasoning_items_tokens() as i64)
    }

    /// This function enforces a couple of invariants on the in-memory history:
    /// 1. every call (function/custom) has a corresponding output entry
    /// 2. every output has a corresponding call entry
    fn normalize_history(&mut self) {
        // all function/tool calls must have a corresponding output
        normalize::ensure_call_outputs_present(&mut self.items);

        // all outputs must have a corresponding function/tool call
        normalize::remove_orphan_outputs(&mut self.items);
    }

    /// Returns a clone of the contents in the transcript.
    fn contents(&self) -> Vec<ResponseItem> {
        self.items.clone()
    }

    fn remove_ghost_snapshots(items: &mut Vec<ResponseItem>) {
        items.retain(|item| !matches!(item, ResponseItem::GhostSnapshot { .. }));
    }

    fn process_item(&self, item: &ResponseItem, policy: TruncationPolicy) -> ResponseItem {
        let policy_with_serialization_budget = policy.mul(1.2);
        match item {
            ResponseItem::FunctionCallOutput { call_id, output } => {
                let truncated =
                    truncate_text(output.content.as_str(), policy_with_serialization_budget);
                let truncated_items = output.content_items.as_ref().map(|items| {
                    truncate_function_output_items_with_policy(
                        items,
                        policy_with_serialization_budget,
                    )
                });
                ResponseItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    output: FunctionCallOutputPayload {
                        content: truncated,
                        content_items: truncated_items,
                        success: output.success,
                    },
                }
            }
            ResponseItem::CustomToolCallOutput { call_id, output } => {
                let truncated = truncate_text(output, policy_with_serialization_budget);
                ResponseItem::CustomToolCallOutput {
                    call_id: call_id.clone(),
                    output: truncated,
                }
            }
            ResponseItem::Message { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::CompactionSummary { .. }
            | ResponseItem::GhostSnapshot { .. }
            | ResponseItem::Other => item.clone(),
        }
    }

    pub(crate) fn handle_history_action(&mut self, action: HistoryAction) -> HistoryActionOutput {
        match action {
            HistoryAction::ViewAll => {
                HistoryActionOutput::unchanged(self.view_history(0, self.items.len()))
            }
            HistoryAction::ViewLast { count } => {
                let start = self.items.len().saturating_sub(count);
                HistoryActionOutput::unchanged(self.view_history(start, self.items.len()))
            }
            HistoryAction::ViewAround { index } => {
                // index is 1-based from user
                let center = index.saturating_sub(1);
                let start = center.saturating_sub(2);
                let end = (center + 3).min(self.items.len());
                HistoryActionOutput::unchanged(self.view_history(start, end))
            }
            HistoryAction::ViewItem { index } => {
                HistoryActionOutput::unchanged(self.view_item_details(index))
            }
            HistoryAction::ViewSnapshots => HistoryActionOutput::unchanged(self.view_snapshots()),
            HistoryAction::ViewAssistant => {
                HistoryActionOutput::unchanged(self.view_assistant_messages())
            }
            HistoryAction::ViewReasoning => {
                HistoryActionOutput::unchanged(self.view_reasoning_items())
            }
            HistoryAction::ViewUser => HistoryActionOutput::unchanged(self.view_user_messages()),
            HistoryAction::Delete { index } => {
                if index > 0 && index <= self.items.len() {
                    self.items.remove(index - 1);
                    HistoryActionOutput::changed(format!("Deleted item #{index}"))
                } else {
                    HistoryActionOutput::unchanged(format!("Invalid index: {index}"))
                }
            }
            HistoryAction::DeleteRange { start, end } => {
                let len = self.items.len();
                if start == 0 {
                    return HistoryActionOutput::unchanged(format!("Invalid range: {start}-{end}"));
                }

                let normalized_end = if end == usize::MAX { len } else { end };

                if normalized_end < start || normalized_end > len {
                    HistoryActionOutput::unchanged(format!("Invalid range: {start}-{end}"))
                } else {
                    // drain is exclusive of end, but user range is inclusive
                    // start-1 to end
                    self.items.drain((start - 1)..normalized_end);
                    HistoryActionOutput::changed(format!(
                        "Deleted items #{start} to #{normalized_end}"
                    ))
                }
            }
            HistoryAction::DeleteLast { count } => {
                let len = self.items.len();
                if count > 0 && count <= len {
                    let start = len - count;
                    self.items.drain(start..);
                    HistoryActionOutput::changed(format!("Deleted last {count} items"))
                } else {
                    HistoryActionOutput::unchanged(format!("Invalid count: {count}"))
                }
            }
            HistoryAction::DeleteBefore { index } => {
                if index > 0 && index <= self.items.len() {
                    // Delete 1..=index (inclusive)
                    // 0..index
                    self.items.drain(0..index);
                    HistoryActionOutput::changed(format!(
                        "Deleted items before and including #{index}"
                    ))
                } else {
                    HistoryActionOutput::unchanged(format!("Invalid index: {index}"))
                }
            }
            HistoryAction::Undo { .. } => {
                HistoryActionOutput::unchanged("Undo must be handled by the session".to_string())
            }
            HistoryAction::Compact { .. } => {
                HistoryActionOutput::unchanged("Compaction is handled asynchronously.".to_string())
            }
        }
    }

    fn view_snapshots(&self) -> String {
        let snapshots: Vec<(usize, &codex_git::GhostCommit)> = self
            .items
            .iter()
            .enumerate()
            .filter_map(|(idx, item)| match item {
                ResponseItem::GhostSnapshot { ghost_commit } => Some((idx + 1, ghost_commit)),
                _ => None,
            })
            .collect();

        if snapshots.is_empty() {
            return "No snapshots captured yet.".to_string();
        }

        let mut output = String::new();
        for (index, ghost_commit) in snapshots {
            let short_id: String = ghost_commit.id().chars().take(7).collect();
            if let Some(parent) = ghost_commit.parent() {
                let short_parent: String = parent.chars().take(7).collect();
                output.push_str(&format!(
                    "{index}. Snapshot {short_id} (parent {short_parent})\n"
                ));
            } else {
                output.push_str(&format!("{index}. Snapshot {short_id}\n"));
            }
        }
        output
    }

    fn view_history(&self, start: usize, end: usize) -> String {
        if self.items.is_empty() {
            return "History is empty.".to_string();
        }

        let mut output = String::new();
        for (i, item) in self.items.iter().enumerate().skip(start).take(end - start) {
            let index = i + 1;
            let content = format_item_summary(item);
            output.push_str(&format!("{index}. {content}\n"));
        }
        output
    }

    fn view_item_details(&self, index: usize) -> String {
        if index == 0 || index > self.items.len() {
            return format!("Invalid index: {index}");
        }

        let item = &self.items[index - 1];
        match serde_json::to_string_pretty(item) {
            Ok(json) => format!("{index}.\n{json}\n"),
            Err(err) => format!("Failed to serialize item #{index}: {err}"),
        }
    }

    fn view_filtered_history<F>(&self, predicate: F, empty_message: &str) -> String
    where
        F: Fn(&ResponseItem) -> bool,
    {
        let mut output = String::new();
        for (i, item) in self.items.iter().enumerate() {
            if predicate(item) {
                let index = i + 1;
                let content = format_item_summary(item);
                output.push_str(&format!("{index}. {content}\n"));
            }
        }

        if output.is_empty() {
            empty_message.to_string()
        } else {
            output
        }
    }

    fn view_assistant_messages(&self) -> String {
        self.view_filtered_history(
            |item| matches!(item, ResponseItem::Message { role, .. } if role == "assistant"),
            "No assistant messages found.",
        )
    }

    fn view_user_messages(&self) -> String {
        self.view_filtered_history(
            |item| matches!(item, ResponseItem::Message { role, .. } if role == "user"),
            "No user messages found.",
        )
    }

    fn view_reasoning_items(&self) -> String {
        self.view_filtered_history(
            |item| matches!(item, ResponseItem::Reasoning { .. }),
            "No reasoning entries found.",
        )
    }
}

fn format_item_summary(item: &ResponseItem) -> String {
    match item {
        ResponseItem::Message { role, content, .. } => {
            let text = content
                .iter()
                .map(|c| match c {
                    codex_protocol::models::ContentItem::InputText { text } => text.clone(),
                    codex_protocol::models::ContentItem::OutputText { text } => text.clone(),
                    codex_protocol::models::ContentItem::InputImage { image_url } => {
                        format!("[Image: {image_url}]")
                    }
                })
                .collect::<Vec<_>>()
                .join(" ");
            let short_text = truncate_preview(&text, 100);
            format!("[{role}] {short_text}")
        }
        ResponseItem::FunctionCall {
            call_id,
            name,
            arguments,
            ..
        } => {
            let args = if arguments.len() > 100 {
                format!("{}...", &arguments[..100])
            } else {
                arguments.clone()
            };
            format!("[FunctionCall] {name}({args}) (id: {call_id})")
        }
        ResponseItem::FunctionCallOutput { call_id, .. } => {
            format!("[FunctionOutput] (id: {call_id})")
        }
        ResponseItem::Reasoning {
            summary, content, ..
        } => {
            if let Some(preview) = reasoning_preview(summary, content.as_ref()) {
                let short_text = truncate_preview(&preview, 100);
                format!("[Reasoning] {short_text}")
            } else {
                "[Reasoning]".to_string()
            }
        }
        ResponseItem::LocalShellCall { action, .. } => match action {
            codex_protocol::models::LocalShellAction::Exec(exec) => {
                format!("[Shell] {:?}", exec.command)
            }
        },
        ResponseItem::WebSearchCall { action, .. } => match action {
            codex_protocol::models::WebSearchAction::Search { query } => {
                format!("[WebSearch] {}", query.as_deref().unwrap_or("?"))
            }
            _ => "[WebSearch]".to_string(),
        },
        ResponseItem::CustomToolCall { name, .. } => {
            format!("[Tool] {name}")
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            format!("[ToolOutput] (id: {call_id})")
        }
        ResponseItem::CompactionSummary { .. } => "[CompactionSummary]".to_string(),
        ResponseItem::GhostSnapshot { .. } => "[GhostSnapshot]".to_string(),
        ResponseItem::Other => "[Other]".to_string(),
    }
}

fn truncate_preview(text: &str, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        format!("{}...", text.chars().take(max_chars).collect::<String>())
    } else {
        text.to_string()
    }
}

fn reasoning_preview(
    summary: &[ReasoningItemReasoningSummary],
    content: Option<&Vec<ReasoningItemContent>>,
) -> Option<String> {
    if let Some(text) = summary.iter().find_map(|item| match item {
        ReasoningItemReasoningSummary::SummaryText { text } if !text.is_empty() => {
            Some(text.clone())
        }
        _ => None,
    }) {
        return Some(text);
    }

    if let Some(content_items) = content {
        for item in content_items {
            if let ReasoningItemContent::ReasoningText { text } = item
                && !text.is_empty() {
                    return Some(text.clone());
                }
        }
    }

    None
}

/// API messages include every non-system item (user/assistant messages, reasoning,
/// tool calls, tool outputs, shell calls, and web-search calls).
fn is_api_message(message: &ResponseItem) -> bool {
    match message {
        ResponseItem::Message { role, .. } => role.as_str() != "system",
        ResponseItem::FunctionCallOutput { .. }
        | ResponseItem::FunctionCall { .. }
        | ResponseItem::CustomToolCall { .. }
        | ResponseItem::CustomToolCallOutput { .. }
        | ResponseItem::LocalShellCall { .. }
        | ResponseItem::Reasoning { .. }
        | ResponseItem::WebSearchCall { .. }
        | ResponseItem::CompactionSummary { .. } => true,
        ResponseItem::GhostSnapshot { .. } => false,
        ResponseItem::Other => false,
    }
}

fn estimate_reasoning_length(encoded_len: usize) -> usize {
    encoded_len
        .saturating_mul(3)
        .checked_div(4)
        .unwrap_or(0)
        .saturating_sub(650)
}

#[cfg(test)]
#[path = "history_tests.rs"]
mod tests;
