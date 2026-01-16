use crate::common::Reasoning;
use crate::common::TextControls;
use crate::error::ApiError;
use crate::provider::Provider;
use crate::requests::headers::build_conversation_headers;
use crate::requests::headers::insert_header;
use crate::requests::headers::subagent_header;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::SessionSource;
use http::HeaderMap;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Compression {
    #[default]
    None,
    Zstd,
}

/// Assembled request body plus headers for a Responses stream request.
pub struct ResponsesRequest {
    pub body: Value,
    pub headers: HeaderMap,
    pub compression: Compression,
}

#[derive(Default)]
pub struct ResponsesRequestBuilder<'a> {
    model: Option<&'a str>,
    instructions: Option<&'a str>,
    input: Option<&'a [ResponseItem]>,
    tools: Option<&'a [Value]>,
    parallel_tool_calls: bool,
    reasoning: Option<Reasoning>,
    include: Vec<String>,
    prompt_cache_key: Option<String>,
    text: Option<TextControls>,
    max_output_tokens: Option<i64>,
    conversation_id: Option<String>,
    session_source: Option<SessionSource>,
    store_override: Option<bool>,
    headers: HeaderMap,
    previous_response_id: Option<String>,
    caching: Option<crate::common::Caching>,
    compression: Compression,
}

impl<'a> ResponsesRequestBuilder<'a> {
    pub fn new(model: &'a str, instructions: Option<&'a str>, input: &'a [ResponseItem]) -> Self {
        Self {
            model: Some(model),
            instructions,
            input: Some(input),
            ..Default::default()
        }
    }

    pub fn tools(mut self, tools: &'a [Value]) -> Self {
        self.tools = Some(tools);
        self
    }

    pub fn parallel_tool_calls(mut self, enabled: bool) -> Self {
        self.parallel_tool_calls = enabled;
        self
    }

    pub fn reasoning(mut self, reasoning: Option<Reasoning>) -> Self {
        self.reasoning = reasoning;
        self
    }

    pub fn include(mut self, include: Vec<String>) -> Self {
        self.include = include;
        self
    }

    pub fn prompt_cache_key(mut self, key: Option<String>) -> Self {
        self.prompt_cache_key = key;
        self
    }

    pub fn text(mut self, text: Option<TextControls>) -> Self {
        self.text = text;
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: Option<i64>) -> Self {
        self.max_output_tokens = max_output_tokens;
        self
    }

    pub fn conversation(mut self, conversation_id: Option<String>) -> Self {
        self.conversation_id = conversation_id;
        self
    }

    pub fn session_source(mut self, source: Option<SessionSource>) -> Self {
        self.session_source = source;
        self
    }

    pub fn store_override(mut self, store: Option<bool>) -> Self {
        self.store_override = store;
        self
    }

    pub fn previous_response_id(mut self, id: Option<String>) -> Self {
        self.previous_response_id = id;
        self
    }

    pub fn caching(mut self, caching: Option<crate::common::Caching>) -> Self {
        self.caching = caching;
        self
    }

    pub fn extra_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub fn compression(mut self, compression: Compression) -> Self {
        self.compression = compression;
        self
    }

    pub fn build(self, provider: &Provider) -> Result<ResponsesRequest, ApiError> {
        let model = self
            .model
            .ok_or_else(|| ApiError::Stream("missing model for responses request".into()))?;
        let instructions = self.instructions;
        let original_input = self
            .input
            .ok_or_else(|| ApiError::Stream("missing input for responses request".into()))?;
        let tools = if self.previous_response_id.is_some() {
            None
        } else {
            Some(self.tools.unwrap_or_default())
        };

        let store = self
            .store_override
            .unwrap_or_else(|| provider.is_azure_responses_endpoint());

        let coalesced_input = coalesce_input(
            original_input,
            store,
            provider.is_azure_responses_endpoint(),
        );

        let req = MergedResponsesApiRequest {
            model,
            instructions,
            input: &coalesced_input,
            tools,
            tool_choice: if self.previous_response_id.is_some() {
                None
            } else {
                Some("auto")
            },
            parallel_tool_calls: if self.previous_response_id.is_some() {
                None
            } else {
                Some(self.parallel_tool_calls)
            },
            reasoning: self.reasoning,
            store,
            stream: true,
            include: self.include,
            prompt_cache_key: self.prompt_cache_key,
            text: self.text,
            max_output_tokens: self.max_output_tokens,
            previous_response_id: self.previous_response_id,
            caching: self.caching,
        };

        let body = serde_json::to_value(&req)
            .map_err(|e| ApiError::Stream(format!("failed to encode responses request: {e}")))?;

        let mut headers = self.headers;
        headers.extend(build_conversation_headers(self.conversation_id));
        if let Some(subagent) = subagent_header(&self.session_source) {
            insert_header(&mut headers, "x-openai-subagent", &subagent);
        }

        Ok(ResponsesRequest {
            body,
            headers,
            compression: self.compression,
        })
    }
}

fn get_response_item_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::Message { id: Some(id), .. }
        | ResponseItem::Reasoning { id, .. }
        | ResponseItem::WebSearchCall { id: Some(id), .. }
        | ResponseItem::FunctionCall { id: Some(id), .. }
        | ResponseItem::LocalShellCall { id: Some(id), .. }
        | ResponseItem::CustomToolCall { id: Some(id), .. } => {
            if !id.is_empty() {
                Some(id.as_str())
            } else {
                None
            }
        }
        _ => None,
    }
}

// Mirrors ResponsesApiRequest but allows arbitrary JSON Values for input items
#[derive(serde::Serialize)]
struct MergedResponsesApiRequest<'a> {
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    instructions: Option<&'a str>,
    input: &'a [Value],
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'a [serde_json::Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    parallel_tool_calls: Option<bool>,
    reasoning: Option<Reasoning>,
    store: bool,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    include: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<TextControls>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    previous_response_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caching: Option<crate::common::Caching>,
}

fn coalesce_input(items: &[ResponseItem], store: bool, is_azure: bool) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    let mut current_merge: Option<serde_json::Map<String, Value>> = None;
    let merge_assistant_items = !(store && is_azure);

    for item in items {
        let mut item_value = match serde_json::to_value(item) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Attach ID before merging if needed
        if store
            && is_azure
            && let Some(id) = get_response_item_id(item)
            && let Some(obj) = item_value.as_object_mut()
        {
            obj.insert("id".to_string(), Value::String(id.to_string()));
        }

        if merge_assistant_items && is_mergeable_assistant_item(item) {
            if let Some(mut existing) = current_merge.take() {
                merge_assistant_objects(&mut existing, item_value);
                current_merge = Some(existing);
            } else if let Value::Object(obj) = item_value {
                current_merge = Some(obj);
            } else {
                merged.push(item_value);
            }
        } else {
            // Flush any pending merge
            if let Some(existing) = current_merge.take() {
                merged.push(Value::Object(existing));
            }
            merged.push(item_value);
        }
    }

    if merge_assistant_items {
        if let Some(existing) = current_merge {
            merged.push(Value::Object(existing));
        }
    }

    merged
}

fn is_mergeable_assistant_item(item: &ResponseItem) -> bool {
    match item {
        ResponseItem::Message { role, .. } => role == "assistant",
        _ => false,
    }
}

fn merge_assistant_objects(target: &mut serde_json::Map<String, Value>, source: Value) {
    let Value::Object(mut source_obj) = source else {
        return;
    };

    // Merge content
    if let Some(source_content) = source_obj.remove("content")
        && !source_content.is_null()
        && (!target.contains_key("content") || target["content"].is_null())
    {
        target.insert("content".to_string(), source_content);
    }
    // Else: target has content, source has content. We assume target (first) is valid preamble.

    // Merge tool_calls
    if let Some(Value::Array(source_tools)) = source_obj.remove("tool_calls") {
        if let Some(Value::Array(target_tools)) = target.get_mut("tool_calls") {
            target_tools.extend(source_tools);
        } else {
            target.insert("tool_calls".to_string(), Value::Array(source_tools));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::RetryConfig;
    use crate::provider::WireApi;
    use codex_protocol::protocol::SubAgentSource;
    use http::HeaderValue;
    use pretty_assertions::assert_eq;
    use std::time::Duration;

    fn provider(name: &str, base_url: &str) -> Provider {
        Provider {
            name: name.to_string(),
            base_url: base_url.to_string(),
            query_params: None,
            wire: WireApi::Responses,
            headers: HeaderMap::new(),
            retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(50),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_retry: RetryConfig {
                max_attempts: 1,
                base_delay: Duration::from_millis(50),
                retry_429: false,
                retry_5xx: true,
                retry_transport: true,
            },
            stream_idle_timeout: Duration::from_secs(5),
            base_url_suffix: None,
        }
    }

    #[test]
    fn azure_default_store_attaches_ids_and_headers() {
        let provider = provider("azure", "https://example.openai.azure.com/v1");
        let input = vec![
            ResponseItem::Message {
                id: Some("m1".into()),
                role: "assistant".into(),
                content: Vec::new(),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".into(),
                content: Vec::new(),
            },
        ];

        let request = ResponsesRequestBuilder::new("gpt-test", Some("inst"), &input)
            .conversation(Some("conv-1".into()))
            .session_source(Some(SessionSource::SubAgent(SubAgentSource::Review)))
            .build(&provider)
            .expect("request");

        assert_eq!(request.body.get("store"), Some(&Value::Bool(true)));

        let ids: Vec<Option<String>> = request
            .body
            .get("input")
            .and_then(|v| v.as_array())
            .into_iter()
            .flatten()
            .map(|item| item.get("id").and_then(|v| v.as_str().map(str::to_string)))
            .collect();
        assert_eq!(ids, vec![Some("m1".to_string()), None]);

        assert_eq!(
            request.headers.get("session_id"),
            Some(&HeaderValue::from_static("conv-1"))
        );
        assert_eq!(
            request.headers.get("x-openai-subagent"),
            Some(&HeaderValue::from_static("review"))
        );
        assert_eq!(
            request.headers.get("extra"),
            Some(&HeaderValue::from_static(r#"{"session_id":"conv-1"}"#))
        );
    }

    #[test]
    fn omits_tools_when_previous_response_id_is_present() {
        let provider = provider("azure", "https://example.openai.azure.com/v1");
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: Vec::new(),
        }];
        let tools = vec![serde_json::json!({"type": "function", "function": {"name": "foo"}})];

        // Case 1: No previous_response_id -> tools should be present
        let req1 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .tools(&tools)
            .build(&provider)
            .expect("request");
        assert!(req1.body.get("tools").is_some());

        // Case 2: With previous_response_id -> tools should be absent
        let req2 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .tools(&tools)
            .previous_response_id(Some("resp_123".into()))
            .build(&provider)
            .expect("request");
        assert!(req2.body.get("tools").is_none());
    }

    #[test]
    fn omits_tool_choice_when_previous_response_id_is_present() {
        let provider = provider("azure", "https://example.openai.azure.com/v1");
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: Vec::new(),
        }];

        // Case 1: No previous_response_id -> tool_choice should be present
        let req1 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .build(&provider)
            .expect("request");
        assert!(req1.body.get("tool_choice").is_some());

        // Case 2: With previous_response_id -> tool_choice should be absent
        let req2 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .previous_response_id(Some("resp_123".into()))
            .build(&provider)
            .expect("request");
        assert!(req2.body.get("tool_choice").is_none());
    }

    #[test]
    fn omits_parallel_tool_calls_when_previous_response_id_is_present() {
        let provider = provider("azure", "https://example.openai.azure.com/v1");
        let input = vec![ResponseItem::Message {
            id: None,
            role: "user".into(),
            content: Vec::new(),
        }];

        // Case 1: No previous_response_id -> parallel_tool_calls should be present
        let req1 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .parallel_tool_calls(true)
            .build(&provider)
            .expect("request");
        assert!(req1.body.get("parallel_tool_calls").is_some());

        // Case 2: With previous_response_id -> parallel_tool_calls should be absent
        let req2 = ResponsesRequestBuilder::new("gpt-4", None, &input)
            .parallel_tool_calls(true)
            .previous_response_id(Some("resp_123".into()))
            .build(&provider)
            .expect("request");
        assert!(req2.body.get("parallel_tool_calls").is_none());
    }
}
