use codex_api::common::Caching;
use codex_api::common::CachingType;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Determines how Codex should build the conversation payload that gets sent to
/// a model provider. Different providers have subtly different requirements,
/// so we centralize that logic here instead of scattering feature flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum ConversationBuildStrategy {
    /// Sends the full conversation history along with the standard
    /// `instructions` field that OpenAI's Responses API expects.
    #[default]
    FullHistory,
    /// Uses `previous_response_id` to anchor the provider's context cache and
    /// only sends the delta since that response. Instructions are moved into
    /// the first system message because some providers reject the dedicated
    /// `instructions` field when caching is enabled.
    PreviousResponseId,
}

#[derive(Debug)]
pub(crate) struct ConversationBuildInput {
    pub instructions: String,
    pub formatted_input: Vec<ResponseItem>,
    pub reasoning_requested: bool,
    pub conversation_id: String,
    pub last_response_id: Option<String>,
}

#[derive(Debug)]
pub(crate) struct ConversationBuildPlan {
    pub instructions: String,
    pub input: Vec<ResponseItem>,
    pub include: Vec<String>,
    pub prompt_cache_key: Option<String>,
    pub previous_response_id: Option<String>,
    pub caching: Option<Caching>,
    pub include_instructions_field: bool,
    pub force_store: Option<bool>,
}

impl ConversationBuildStrategy {
    pub(crate) fn build_plan(self, params: ConversationBuildInput) -> ConversationBuildPlan {
        match self {
            ConversationBuildStrategy::FullHistory => ConversationBuildPlan {
                instructions: params.instructions,
                input: params.formatted_input,
                include: include_for_reasoning(params.reasoning_requested),
                prompt_cache_key: Some(params.conversation_id),
                previous_response_id: None,
                caching: None,
                include_instructions_field: true,
                force_store: None,
            },
            ConversationBuildStrategy::PreviousResponseId => {
                // Use the stored response ID from the previous turn
                let previous_response_id = params.last_response_id;

                // Only send items after the last response
                let trimmed_input = if previous_response_id.is_some() {
                    // If we have a previous response ID, we can trim the history
                    // and only send new items since that response
                    let (_, tail) = split_after_last_item_with_id(params.formatted_input);
                    tail
                } else {
                    // First request - send all items
                    params.formatted_input
                };

                let mut input_with_instructions =
                    Vec::with_capacity(trimmed_input.len().saturating_add(1));
                if let Some(system_message) = instructions_as_system_message(&params.instructions) {
                    input_with_instructions.push(system_message);
                }
                input_with_instructions.extend(trimmed_input);

                ConversationBuildPlan {
                    instructions: params.instructions,
                    input: input_with_instructions,
                    include: Vec::new(),
                    prompt_cache_key: None,
                    previous_response_id,
                    caching: Some(Caching {
                        r#type: CachingType::Enabled,
                    }),
                    include_instructions_field: false,
                    force_store: Some(true),
                }
            }
        }
    }
}

fn include_for_reasoning(requested: bool) -> Vec<String> {
    if requested {
        vec!["reasoning.encrypted_content".to_string()]
    } else {
        Vec::new()
    }
}

fn split_after_last_item_with_id(items: Vec<ResponseItem>) -> (Option<String>, Vec<ResponseItem>) {
    if let Some((idx, item_id)) = last_item_with_id(&items) {
        let mut prefix_and_tail = items;
        let tail = if idx < prefix_and_tail.len() {
            prefix_and_tail.split_off(idx + 1)
        } else {
            Vec::new()
        };
        (Some(item_id), tail)
    } else {
        (None, items)
    }
}

fn last_item_with_id(items: &[ResponseItem]) -> Option<(usize, String)> {
    items
        .iter()
        .enumerate()
        .rev()
        .find_map(|(idx, item)| response_item_id(item).map(|id| (idx, id.to_string())))
}

fn response_item_id(item: &ResponseItem) -> Option<&str> {
    match item {
        ResponseItem::Message { id: Some(id), .. } if !id.is_empty() => Some(id),
        ResponseItem::Reasoning { id, .. } if !id.is_empty() => Some(id),
        ResponseItem::FunctionCall { id: Some(id), .. } if !id.is_empty() => Some(id),
        ResponseItem::WebSearchCall { id: Some(id), .. } if !id.is_empty() => Some(id),
        ResponseItem::LocalShellCall { id: Some(id), .. } if !id.is_empty() => Some(id),
        ResponseItem::CustomToolCall { id: Some(id), .. } if !id.is_empty() => Some(id),
        _ => None,
    }
}

fn instructions_as_system_message(instructions: &str) -> Option<ResponseItem> {
    if instructions.trim().is_empty() {
        None
    } else {
        Some(ResponseItem::Message {
            id: None,
            role: "system".to_string(),
            content: vec![ContentItem::InputText {
                text: instructions.to_string(),
            }],
            end_turn: None,
        })
    }
}
