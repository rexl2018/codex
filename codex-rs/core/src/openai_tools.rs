use serde::Deserialize;
use serde::Serialize;
use serde_json::Value as JsonValue;
use serde_json::json;
use std::collections::BTreeMap;


/// Agent type for tool configuration
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum AgentType {
    /// Main agent - only has access to store_context tool
    Main,
    /// Explorer agent - read-only operations, no file writing
    Explorer,
    /// Coder agent - full access including file writing
    Coder,
}

impl From<codex_protocol::protocol::SubagentType> for AgentType {
    fn from(subagent_type: codex_protocol::protocol::SubagentType) -> Self {
        match subagent_type {
            codex_protocol::protocol::SubagentType::Explorer => AgentType::Explorer,
            codex_protocol::protocol::SubagentType::Coder => AgentType::Coder,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResponsesApiTool {
    pub(crate) name: String,
    pub(crate) description: String,
    /// TODO: Validation. When strict is set to true, the JSON schema,
    /// `required` and `additional_properties` must be present. All fields in
    /// `properties` must be present in `required`.
    pub(crate) strict: bool,
    pub(crate) parameters: JsonSchema,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FreeformTool {
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) format: FreeformToolFormat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FreeformToolFormat {
    pub(crate) r#type: String,
    pub(crate) syntax: String,
    pub(crate) definition: String,
}

/// When serialized as JSON, this produces a valid "Tool" in the OpenAI
/// Responses API.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "type")]
pub(crate) enum OpenAiTool {
    #[serde(rename = "function")]
    Function(ResponsesApiTool),
    #[serde(rename = "local_shell")]
    LocalShell {},
    // TODO: Understand why we get an error on web_search although the API docs say it's supported.
    // https://platform.openai.com/docs/guides/tools-web-search?api-mode=responses#:~:text=%7B%20type%3A%20%22web_search%22%20%7D%2C
    #[serde(rename = "web_search")]
    WebSearch {},
    #[serde(rename = "custom")]
    Freeform(FreeformTool),
}







/// Generic JSON‑Schema subset needed for our tool definitions
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum JsonSchema {
    Boolean {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    String {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// MCP schema allows "number" | "integer" for Number
    #[serde(alias = "integer")]
    Number {
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Array {
        items: Box<JsonSchema>,

        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    Object {
        properties: BTreeMap<String, JsonSchema>,
        #[serde(skip_serializing_if = "Option::is_none")]
        required: Option<Vec<String>>,
        #[serde(
            rename = "additionalProperties",
            skip_serializing_if = "Option::is_none"
        )]
        additional_properties: Option<bool>,
    },
}

fn create_unified_exec_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "input".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "When no session_id is provided, treat the array as the command and arguments \
                 to launch. When session_id is set, concatenate the strings (in order) and write \
                 them to the session's stdin."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "session_id".to_string(),
        JsonSchema::String {
            description: Some(
                "Identifier for an existing interactive session. If omitted, a new command \
                 is spawned."
                    .to_string(),
            ),
        },
    );
    properties.insert(
        "timeout_ms".to_string(),
        JsonSchema::Number {
            description: Some(
                "Maximum time in milliseconds to wait for output after writing the input."
                    .to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "unified_exec".to_string(),
        description:
            "Runs a command in a PTY. Provide a session_id to reuse an existing interactive session.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["input".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_shell_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "command".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some("The command to execute".to_string()),
        },
    );
    properties.insert(
        "workdir".to_string(),
        JsonSchema::String {
            description: Some("The working directory to execute the command in".to_string()),
        },
    );
    properties.insert(
        "timeout_ms".to_string(),
        JsonSchema::Number {
            description: Some("The timeout for the command in milliseconds".to_string()),
        },
    );

    properties.insert(
        "with_escalated_permissions".to_string(),
        JsonSchema::Boolean {
            description: Some("Whether to request escalated permissions. Set to true if command needs to be run without sandbox restrictions".to_string()),
        },
    );
    properties.insert(
        "justification".to_string(),
        JsonSchema::String {
            description: Some("Only set if with_escalated_permissions is true. 1-sentence explanation of why we want to run this command.".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "shell".to_string(),
        description: "Runs a shell command and returns its output.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["command".to_string()]),
            additional_properties: Some(false),
        },
    })
}

fn create_view_image_tool() -> OpenAiTool {
    // Support only local filesystem path.
    let mut properties = BTreeMap::new();
    properties.insert(
        "path".to_string(),
        JsonSchema::String {
            description: Some("Local filesystem path to an image file".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "view_image".to_string(),
        description:
            "Attach a local image (by filesystem path) to the conversation context for this turn."
                .to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["path".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_subagent_task_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();

    properties.insert(
        "agent_type".to_string(),
        JsonSchema::String {
            description: Some("Type of subagent to create: 'explorer' for investigation and analysis, 'coder' for implementation and modification tasks".to_string()),
        },
    );

    properties.insert(
        "title".to_string(),
        JsonSchema::String {
            description: Some(
                "A concise title for the subagent task (max 50 characters)".to_string(),
            ),
        },
    );

    properties.insert(
        "description".to_string(),
        JsonSchema::String {
            description: Some("Detailed description of what the subagent should accomplish. Be specific about the expected outcomes and deliverables.".to_string()),
        },
    );

    properties.insert(
        "context_refs".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String { description: None }),
            description: Some(
                "List of context reference IDs that the subagent should have access to".to_string(),
            ),
        },
    );

    properties.insert(
        "bootstrap_paths".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::Object {
                properties: {
                    let mut bootstrap_props = BTreeMap::new();
                    bootstrap_props.insert(
                        "path".to_string(),
                        JsonSchema::String {
                            description: Some(
                                "File or directory path to include in subagent context".to_string(),
                            ),
                        },
                    );
                    bootstrap_props.insert(
                        "reason".to_string(),
                        JsonSchema::String {
                            description: Some(
                                "Explanation of why this path is relevant to the task".to_string(),
                            ),
                        },
                    );
                    bootstrap_props
                },
                required: Some(vec!["path".to_string(), "reason".to_string()]),
                additional_properties: Some(false),
            }),
            description: Some(
                "List of files or directories to bootstrap the subagent with relevant context"
                    .to_string(),
            ),
        },
    );

    properties.insert(
        "auto_launch".to_string(),
        JsonSchema::Boolean {
            description: Some(
                "When true, automatically launches the subagent after creation. Default: true"
                    .to_string(),
            ),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "create_subagent_task".to_string(),
        description: "Create a specialized subagent task for complex analysis or implementation work. Use 'explorer' agents for investigation, understanding codebases, and verification. Use 'coder' agents for implementation, bug fixes, and code modifications.".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec![
                "agent_type".to_string(),
                "title".to_string(),
                "description".to_string(),
            ]),
            additional_properties: Some(false),
        },
    })
}
/// TODO(dylan): deprecate once we get rid of json tool
#[derive(Serialize, Deserialize)]
pub(crate) struct ApplyPatchToolArgs {
    pub(crate) input: String,
}

/// Returns JSON values that are compatible with Function Calling in the
/// Responses API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=responses
pub fn create_tools_json_for_responses_api(
    tools: &[OpenAiTool],
) -> crate::error::Result<Vec<serde_json::Value>> {
    let mut tools_json = Vec::new();

    for tool in tools {
        let json = serde_json::to_value(tool)?;
        tools_json.push(json);
    }

    Ok(tools_json)
}
/// Returns JSON values that are compatible with Function Calling in the
/// Chat Completions API:
/// https://platform.openai.com/docs/guides/function-calling?api-mode=chat
pub(crate) fn create_tools_json_for_chat_completions_api(
    tools: &[OpenAiTool],
) -> crate::error::Result<Vec<serde_json::Value>> {
    // We start with the JSON for the Responses API and than rewrite it to match
    // the chat completions tool call format.
    let responses_api_tools_json = create_tools_json_for_responses_api(tools)?;
    let tools_json = responses_api_tools_json
        .into_iter()
        .filter_map(|mut tool| {
            if tool.get("type") != Some(&serde_json::Value::String("function".to_string())) {
                return None;
            }

            if let Some(map) = tool.as_object_mut() {
                // Remove "type" field as it is not needed in chat completions.
                map.remove("type");
                Some(json!({
                    "type": "function",
                    "function": map,
                }))
            } else {
                None
            }
        })
        .collect::<Vec<serde_json::Value>>();
    Ok(tools_json)
}

pub(crate) fn mcp_tool_to_openai_tool(
    fully_qualified_name: String,
    tool: mcp_types::Tool,
) -> Result<ResponsesApiTool, serde_json::Error> {
    let mcp_types::Tool {
        description,
        mut input_schema,
        ..
    } = tool;

    // OpenAI models mandate the "properties" field in the schema. The Agents
    // SDK fixed this by inserting an empty object for "properties" if it is not
    // already present https://github.com/openai/openai-agents-python/issues/449
    // so here we do the same.
    if input_schema.properties.is_none() {
        input_schema.properties = Some(serde_json::Value::Object(serde_json::Map::new()));
    }

    // Serialize to a raw JSON value so we can sanitize schemas coming from MCP
    // servers. Some servers omit the top-level or nested `type` in JSON
    // Schemas (e.g. using enum/anyOf), or use unsupported variants like
    // `integer`. Our internal JsonSchema is a small subset and requires
    // `type`, so we coerce/sanitize here for compatibility.
    let mut serialized_input_schema = serde_json::to_value(input_schema)?;
    sanitize_json_schema(&mut serialized_input_schema);
    let input_schema = serde_json::from_value::<JsonSchema>(serialized_input_schema)?;

    Ok(ResponsesApiTool {
        name: fully_qualified_name,
        description: description.unwrap_or_default(),
        strict: false,
        parameters: input_schema,
    })
}

/// Sanitize a JSON Schema (as serde_json::Value) so it can fit our limited
/// JsonSchema enum. This function:
/// - Ensures every schema object has a "type". If missing, infers it from
///   common keywords (properties => object, items => array, enum/const/format => string)
///   and otherwise defaults to "string".
/// - Fills required child fields (e.g. array items, object properties) with
///   permissive defaults when absent.
fn sanitize_json_schema(value: &mut JsonValue) {
    match value {
        JsonValue::Bool(_) => {
            // JSON Schema boolean form: true/false. Coerce to an accept-all string.
            *value = json!({ "type": "string" });
        }
        JsonValue::Array(arr) => {
            for v in arr.iter_mut() {
                sanitize_json_schema(v);
            }
        }
        JsonValue::Object(map) => {
            // First, recursively sanitize known nested schema holders
            if let Some(props) = map.get_mut("properties")
                && let Some(props_map) = props.as_object_mut()
            {
                for (_k, v) in props_map.iter_mut() {
                    sanitize_json_schema(v);
                }
            }
            if let Some(items) = map.get_mut("items") {
                sanitize_json_schema(items);
            }
            // Some schemas use oneOf/anyOf/allOf - sanitize their entries
            for combiner in ["oneOf", "anyOf", "allOf", "prefixItems"] {
                if let Some(v) = map.get_mut(combiner) {
                    sanitize_json_schema(v);
                }
            }

            // Normalize/ensure type
            let mut ty = map
                .get("type")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());

            // If type is an array (union), pick first supported; else leave to inference
            if ty.is_none()
                && let Some(JsonValue::Array(types)) = map.get("type")
            {
                for t in types {
                    if let Some(tt) = t.as_str()
                        && matches!(
                            tt,
                            "object" | "array" | "string" | "number" | "integer" | "boolean"
                        )
                    {
                        ty = Some(tt.to_string());
                        break;
                    }
                }
            }

            // Infer type if still missing
            if ty.is_none() {
                if map.contains_key("properties")
                    || map.contains_key("required")
                    || map.contains_key("additionalProperties")
                {
                    ty = Some("object".to_string());
                } else if map.contains_key("items") || map.contains_key("prefixItems") {
                    ty = Some("array".to_string());
                } else if map.contains_key("enum")
                    || map.contains_key("const")
                    || map.contains_key("format")
                {
                    ty = Some("string".to_string());
                } else if map.contains_key("minimum")
                    || map.contains_key("maximum")
                    || map.contains_key("exclusiveMinimum")
                    || map.contains_key("exclusiveMaximum")
                    || map.contains_key("multipleOf")
                {
                    ty = Some("number".to_string());
                }
            }
            // If we still couldn't infer, default to string
            let ty = ty.unwrap_or_else(|| "string".to_string());
            map.insert("type".to_string(), JsonValue::String(ty.to_string()));

            // Ensure object schemas have properties map
            if ty == "object" {
                if !map.contains_key("properties") {
                    map.insert(
                        "properties".to_string(),
                        JsonValue::Object(serde_json::Map::new()),
                    );
                }
                // If additionalProperties is an object schema, sanitize it too.
                // Leave booleans as-is, since JSON Schema allows boolean here.
                if let Some(ap) = map.get_mut("additionalProperties") {
                    let is_bool = matches!(ap, JsonValue::Bool(_));
                    if !is_bool {
                        sanitize_json_schema(ap);
                    }
                }
            }

            // Ensure array schemas have items
            if ty == "array" && !map.contains_key("items") {
                map.insert("items".to_string(), json!({ "type": "string" }));
            }
        }
        _ => {}
    }
}

/// Returns a list of OpenAiTools based on the provided config and MCP tools.
/// Note that the keys of mcp_tools should be fully qualified names. See
/// [`McpConnectionManager`] for more details.
///
/// Different agent types have different tool access:
/// - Main: `list_contexts`, `multi_retrieve_contexts`, and `create_subagent_task`.
/// - Explorer: All tools except file_write, plus MCP tools
/// - Coder: All tools including file_write, plus MCP tools


// Additional tool creation functions for the unified system
pub(crate) fn create_read_file_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "file_path".to_string(),
        JsonSchema::String {
            description: Some("Path to the file to read".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "read_file".to_string(),
        description: "Read the contents of a file".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["file_path".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_write_file_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "file_path".to_string(),
        JsonSchema::String {
            description: Some("Path to the file to write".to_string()),
        },
    );
    properties.insert(
        "content".to_string(),
        JsonSchema::String {
            description: Some("Content to write to the file".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "write_file".to_string(),
        description: "Write content to a file".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["file_path".to_string(), "content".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_store_context_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "id".to_string(),
        JsonSchema::String {
            description: Some("Unique identifier for the context".to_string()),
        },
    );
    properties.insert(
        "summary".to_string(),
        JsonSchema::String {
            description: Some("Brief summary of the context".to_string()),
        },
    );
    properties.insert(
        "content".to_string(),
        JsonSchema::String {
            description: Some("Detailed content of the context".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "store_context".to_string(),
        description: "Store context information for later retrieval".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["id".to_string(), "summary".to_string(), "content".to_string()]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_list_contexts_tool() -> OpenAiTool {
    OpenAiTool::Function(ResponsesApiTool {
        name: "list_contexts".to_string(),
        description: "List all available context items".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties: BTreeMap::new(),
            required: Some(vec![]),
            additional_properties: Some(false),
        },
    })
}

pub(crate) fn create_multi_retrieve_contexts_tool() -> OpenAiTool {
    let mut properties = BTreeMap::new();
    properties.insert(
        "context_ids".to_string(),
        JsonSchema::Array {
            items: Box::new(JsonSchema::String {
                description: Some("Context ID".to_string()),
            }),
            description: Some("List of context IDs to retrieve".to_string()),
        },
    );

    OpenAiTool::Function(ResponsesApiTool {
        name: "multi_retrieve_contexts".to_string(),
        description: "Retrieve multiple context items by their IDs".to_string(),
        strict: false,
        parameters: JsonSchema::Object {
            properties,
            required: Some(vec!["context_ids".to_string()]),
            additional_properties: Some(false),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_tool() {
        let tool = super::create_shell_tool();
        let OpenAiTool::Function(ResponsesApiTool {
            description, name, ..
        }) = &tool
        else {
            panic!("expected function tool");
        };
        assert_eq!(name, "shell");

        let expected = "Runs a shell command and returns its output.";
        assert_eq!(description, expected);
    }
}
