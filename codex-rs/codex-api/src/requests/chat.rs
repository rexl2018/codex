use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::ReasoningItemContent;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use serde_json::Value;
use serde_json::json;
use std::collections::HashMap;
use std::collections::VecDeque;
use tracing::warn;

/// Assembled request body plus headers for Chat Completions streaming calls.
pub struct ChatRequest {
    pub body: Value,
    pub headers: HeaderMap,
}

pub struct ChatRequestBuilder<'a> {
    model: &'a str,
    instructions: &'a str,
    input: &'a [ResponseItem],
    tools: &'a [Value],
    conversation_id: Option<String>,
    session_source: Option<SessionSource>,
}

impl<'a> ChatRequestBuilder<'a> {
    pub fn new(
        model: &'a str,
        instructions: &'a str,
        input: &'a [ResponseItem],
        tools: &'a [Value],
    ) -> Self {
        Self {
            model,
            instructions,
            input,
            tools,
            conversation_id: None,
            session_source: None,
        }
    }

    pub fn conversation_id(mut self, id: Option<String>) -> Self {
        self.conversation_id = id;
        self
    }

    pub fn session_source(mut self, source: Option<SessionSource>) -> Self {
        self.session_source = source;
        self
    }

    pub fn build(self, _provider: &Provider) -> Result<ChatRequest, ApiError> {
        let mut messages = Vec::<Value>::new();
        messages.push(json!({"role": "system", "content": self.instructions}));
        let mut tool_call_id_overrides: HashMap<String, String> = HashMap::new();

        let input = self.input;
        let mut reasoning_by_anchor_index: HashMap<usize, String> = HashMap::new();
        let mut last_emitted_role: Option<&str> = None;
        for item in input {
            match item {
                ResponseItem::Message { role, .. } => last_emitted_role = Some(role.as_str()),
                ResponseItem::FunctionCall { .. } | ResponseItem::LocalShellCall { .. } => {
                    last_emitted_role = Some("assistant")
                }
                ResponseItem::FunctionCallOutput { .. } => last_emitted_role = Some("tool"),
                ResponseItem::Reasoning { .. } | ResponseItem::Other => {}
                ResponseItem::CustomToolCall { .. } => {}
                ResponseItem::CustomToolCallOutput { .. } => {}
                ResponseItem::WebSearchCall { .. } => {}
                ResponseItem::GhostSnapshot { .. } => {}
                ResponseItem::CompactionSummary { .. } => {}
            }
        }

        let mut last_user_index: Option<usize> = None;
        for (idx, item) in input.iter().enumerate() {
            if let ResponseItem::Message { role, .. } = item
                && role == "user"
            {
                last_user_index = Some(idx);
            }
        }

        if !matches!(last_emitted_role, Some("user")) {
            for (idx, item) in input.iter().enumerate() {
                if let Some(u_idx) = last_user_index
                    && idx <= u_idx
                {
                    continue;
                }

                if let ResponseItem::Reasoning {
                    content: Some(items),
                    ..
                } = item
                {
                    let mut text = String::new();
                    for entry in items {
                        match entry {
                            ReasoningItemContent::ReasoningText { text: segment }
                            | ReasoningItemContent::Text { text: segment } => {
                                text.push_str(segment)
                            }
                        }
                    }
                    if text.trim().is_empty() {
                        continue;
                    }

                    let mut attached = false;
                    if idx > 0
                        && let ResponseItem::Message { role, .. } = &input[idx - 1]
                        && role == "assistant"
                    {
                        reasoning_by_anchor_index
                            .entry(idx - 1)
                            .and_modify(|v| v.push_str(&text))
                            .or_insert(text.clone());
                        attached = true;
                    }

                    if !attached && idx + 1 < input.len() {
                        match &input[idx + 1] {
                            ResponseItem::FunctionCall { .. }
                            | ResponseItem::LocalShellCall { .. } => {
                                reasoning_by_anchor_index
                                    .entry(idx + 1)
                                    .and_modify(|v| v.push_str(&text))
                                    .or_insert(text.clone());
                            }
                            ResponseItem::Message { role, .. } if role == "assistant" => {
                                reasoning_by_anchor_index
                                    .entry(idx + 1)
                                    .and_modify(|v| v.push_str(&text))
                                    .or_insert(text.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        let mut last_assistant_text: Option<String> = None;

        for (idx, item) in input.iter().enumerate() {
            match item {
                ResponseItem::Message { role, content, .. } => {
                    let mut text = String::new();
                    let mut items: Vec<Value> = Vec::new();
                    let mut saw_image = false;

                    for c in content {
                        match c {
                            ContentItem::InputText { text: t }
                            | ContentItem::OutputText { text: t } => {
                                text.push_str(t);
                                items.push(json!({"type":"text","text": t}));
                            }
                            ContentItem::InputImage { image_url } => {
                                saw_image = true;
                                items.push(
                                    json!({"type":"image_url","image_url": {"url": image_url}}),
                                );
                            }
                        }
                    }

                    if role == "assistant" {
                        if let Some(prev) = &last_assistant_text
                            && prev == &text
                        {
                            continue;
                        }
                        last_assistant_text = Some(text.clone());
                    }

                    let content_value = if role == "assistant" {
                        json!(text)
                    } else if saw_image {
                        json!(items)
                    } else {
                        json!(text)
                    };

                    let mut msg = json!({"role": role, "content": content_value});
                    if role == "assistant"
                        && let Some(reasoning) = reasoning_by_anchor_index.get(&idx)
                        && let Some(obj) = msg.as_object_mut()
                    {
                        obj.insert("reasoning".to_string(), json!(reasoning));
                    }
                    messages.push(msg);
                }
                ResponseItem::FunctionCall {
                    id,
                    name,
                    arguments,
                    call_id,
                    thought_signature,
                    ..
                } => {
                    if let Some(provider_id) = id.as_ref() {
                        tool_call_id_overrides.insert(call_id.clone(), provider_id.clone());
                    }
                    if self.model.contains("gemini") && thought_signature.is_none() {
                        warn!(
                            model = self.model,
                            call_id,
                            "Gemini function call missing thought_signature before serialization"
                        );
                    }
                    let request_tool_id = tool_call_id_overrides
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| call_id.clone());
                    let mut tool_call = json!({
                        "id": request_tool_id,
                        "type": "function",
                        "function": {
                            "name": name,
                            "arguments": arguments,
                        }
                    });

                    // Include thought_signature in extra_content.google if present (for Gemini)
                    if let Some(sig) = thought_signature
                        && let Some(obj) = tool_call.as_object_mut()
                    {
                        obj.insert("signature".to_string(), json!(sig));
                        obj.insert(
                            "extra_content".to_string(),
                            json!({
                                "google": {
                                    "thought_signature": sig
                                }
                            }),
                        );
                    }

                    let mut msg = json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [tool_call]
                    });
                    if let Some(reasoning) = reasoning_by_anchor_index.get(&idx)
                        && let Some(obj) = msg.as_object_mut()
                    {
                        obj.insert("reasoning".to_string(), json!(reasoning));
                    }
                    messages.push(msg);
                }
                ResponseItem::LocalShellCall {
                    id,
                    call_id,
                    status,
                    action,
                } => {
                    if let (Some(original_call_id), Some(provider_id)) =
                        (call_id.as_ref(), id.as_ref())
                    {
                        tool_call_id_overrides
                            .insert(original_call_id.clone(), provider_id.clone());
                    }
                    let resolved_id = call_id
                        .as_ref()
                        .and_then(|original| tool_call_id_overrides.get(original))
                        .cloned()
                        .or_else(|| id.clone())
                        .unwrap_or_default();
                    let mut msg = json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": resolved_id,
                            "type": "local_shell_call",
                            "status": status,
                            "action": action,
                        }]
                    });
                    if let Some(reasoning) = reasoning_by_anchor_index.get(&idx)
                        && let Some(obj) = msg.as_object_mut()
                    {
                        obj.insert("reasoning".to_string(), json!(reasoning));
                    }
                    messages.push(msg);
                }
                ResponseItem::FunctionCallOutput { call_id, output } => {
                    let content_value = if let Some(items) = &output.content_items {
                        let mapped: Vec<Value> = items
                            .iter()
                            .map(|it| match it {
                                FunctionCallOutputContentItem::InputText { text } => {
                                    json!({"type":"text","text": text})
                                }
                                FunctionCallOutputContentItem::InputImage { image_url } => {
                                    json!({"type":"image_url","image_url": {"url": image_url}})
                                }
                            })
                            .collect();
                        json!(mapped)
                    } else {
                        json!(output.content)
                    };

                    let response_call_id = tool_call_id_overrides
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| call_id.clone());
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": response_call_id,
                        "content": content_value,
                    }));
                }
                ResponseItem::CustomToolCall {
                    id,
                    call_id,
                    name,
                    input,
                    status: _,
                } => {
                    if let Some(provider_id) = id.as_ref() {
                        tool_call_id_overrides.insert(call_id.clone(), provider_id.clone());
                    }
                    let request_tool_id = tool_call_id_overrides
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| call_id.clone());
                    messages.push(json!({
                        "role": "assistant",
                        "content": null,
                        "tool_calls": [{
                            "id": request_tool_id,
                            "type": "custom",
                            "custom": {
                                "name": name,
                                "input": input,
                            }
                        }]
                    }));
                }
                ResponseItem::CustomToolCallOutput { call_id, output } => {
                    let response_call_id = tool_call_id_overrides
                        .get(call_id)
                        .cloned()
                        .unwrap_or_else(|| call_id.clone());
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": response_call_id,
                        "content": output,
                    }));
                }
                ResponseItem::GhostSnapshot { .. } => {
                    continue;
                }
                ResponseItem::Reasoning { .. }
                | ResponseItem::WebSearchCall { .. }
                | ResponseItem::Other
                | ResponseItem::CompactionSummary { .. } => {
                    continue;
                }
            }
        }

        ensure_tool_results_balanced(&messages, self.model);

        let messages = coalesce_messages(messages);

        let payload = json!({
            "model": self.model,
            "messages": messages,
            "stream": true,
            "tools": self.tools,
        });

        let mut headers = build_conversation_headers(self.conversation_id);
        if let Some(subagent) = subagent_header(&self.session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        Ok(ChatRequest {
            body: payload,
            headers,
        })
    }
}

fn ensure_tool_results_balanced(messages: &[Value], model: &str) {
    if !model.contains("gemini") {
        return;
    }

    let mut pending: VecDeque<String> = VecDeque::new();

    for (idx, message) in messages.iter().enumerate() {
        let role = message
            .get("role")
            .and_then(|r| r.as_str())
            .unwrap_or_default();

        match role {
            "assistant" => {
                if let Some(tool_calls) = message.get("tool_calls").and_then(|v| v.as_array()) {
                    for (tool_idx, call) in tool_calls.iter().enumerate() {
                        let id = call
                            .get("id")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string();

                        if id.is_empty() {
                            warn!(
                                model,
                                assistant_index = idx,
                                tool_index = tool_idx,
                                "Gemini tool call missing id"
                            );
                        }

                        pending.push_back(id);
                    }
                }
            }
            "tool" => {
                let call_id = message
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();

                if call_id.is_empty() {
                    warn!(
                        model,
                        message_index = idx,
                        "Gemini tool result missing tool_call_id"
                    );
                    continue;
                }

                if let Some(pos) = pending.iter().position(|pending_id| pending_id == &call_id) {
                    pending.remove(pos);
                } else {
                    warn!(
                        model,
                        message_index = idx,
                        tool_call_id = call_id,
                        "Gemini tool result without matching pending call"
                    );
                }
            }
            "user" | "system" => {
                if !pending.is_empty() {
                    warn!(
                        model,
                        message_index = idx,
                        pending = ?pending,
                        "Encountered message while Gemini tool calls are still pending"
                    );
                }
            }
            _ => {}
        }
    }

    if !pending.is_empty() {
        warn!(
            model,
            pending = ?pending,
            "Conversation ended with unmatched Gemini tool calls"
        );
    }
}

fn coalesce_messages(messages: Vec<Value>) -> Vec<Value> {
    let mut merged = Vec::with_capacity(messages.len());
    let mut iter = messages.into_iter();

    if let Some(mut current) = iter.next() {
        for next in iter {
            if is_assistant(&current) && is_assistant(&next) {
                merge_json_objects(&mut current, next);
            } else {
                merged.push(current);
                current = next;
            }
        }
        merged.push(current);
    }

    merged
}

fn is_assistant(msg: &Value) -> bool {
    msg.get("role")
        .and_then(|r| r.as_str()) == Some("assistant")
}

fn merge_json_objects(target: &mut Value, source: Value) {
    if let (Some(target_obj), Some(source_obj)) = (target.as_object_mut(), source.as_object()) {
        // Merge content
        if let Some(source_content) = source_obj.get("content")
            && !source_content.is_null()
                && (!target_obj.contains_key("content") || target_obj["content"].is_null()) {
                    target_obj.insert("content".to_string(), source_content.clone());
                }
                // If both have content, we keep target's (first) content, similar to the strategy in responses.rs
                // to avoid overwriting preamble with potentially conflicting content or duplicating.
                // For "Preamble" + "ToolCall", target has content, source (ToolCall) usually has null content.

        // Merge tool_calls
        if let Some(source_tools) = source_obj.get("tool_calls").and_then(|v| v.as_array()) {
            if let Some(target_tools) = target_obj
                .get_mut("tool_calls")
                .and_then(|v| v.as_array_mut())
            {
                target_tools.extend(source_tools.clone());
            } else {
                target_obj.insert("tool_calls".to_string(), Value::Array(source_tools.clone()));
            }
        }

        // Merge reasoning?
        // If target has reasoning, keep it. If not, take source.
        if !target_obj.contains_key("reasoning")
            && let Some(reasoning) = source_obj.get("reasoning") {
                target_obj.insert("reasoning".to_string(), reasoning.clone());
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use crate::provider::WireApi;
    use codex_protocol::protocol::SessionSource;
    use codex_protocol::protocol::SubAgentSource;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn provider() -> Provider {
        Provider {
            name: "openai".to_string(),
            base_url: "https://api.openai.com/v1".to_string(),
            query_params: None,
            wire: WireApi::Chat,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(10),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(10),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(1),
            base_url_suffix: None,
        }
    }

    #[test]
    fn attaches_conversation_and_subagent_headers() {
        let prompt_input = vec![ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "hi".to_string(),
            }],
        }];
        let req = ChatRequestBuilder::new("gpt-test", "inst", &prompt_input, &[])
            .conversation_id(Some("conv-1".into()))
            .session_source(Some(SessionSource::SubAgent(SubAgentSource::Review)))
            .build(&provider())
            .expect("request");

        assert_eq!(
            req.headers.get("conversation_id"),
            Some(&HeaderValue::from_static("conv-1"))
        );
        assert_eq!(
            req.headers.get("session_id"),
            Some(&HeaderValue::from_static("conv-1"))
        );
        assert_eq!(
            req.headers.get("x-openai-subagent"),
            Some(&HeaderValue::from_static("review"))
        );
        assert_eq!(
            req.headers.get("extra"),
            Some(&HeaderValue::from_static(r#"{"session_id":"conv-1"}"#))
        );
    }
}
