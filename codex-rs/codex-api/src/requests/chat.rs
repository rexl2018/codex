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
    msg.get("role").and_then(|r| r.as_str()) == Some("assistant")
}

fn merge_json_objects(target: &mut Value, source: Value) {
    if let (Some(target_obj), Some(source_obj)) = (target.as_object_mut(), source.as_object()) {
        // Merge content
        if let Some(source_content) = source_obj.get("content")
            && !source_content.is_null()
            && (!target_obj.contains_key("content") || target_obj["content"].is_null())
        {
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
            && let Some(reasoning) = source_obj.get("reasoning")
        {
            target_obj.insert("reasoning".to_string(), reasoning.clone());
        }
    }
}

fn push_tool_call_message(messages: &mut Vec<Value>, tool_call: Value, reasoning: Option<&str>) {
    // Chat Completions requires that tool calls are grouped into a single assistant message
    // (with `tool_calls: [...]`) followed by tool role responses.
    if let Some(Value::Object(obj)) = messages.last_mut()
        && obj.get("role").and_then(Value::as_str) == Some("assistant")
        && obj.get("content").is_some_and(Value::is_null)
        && let Some(tool_calls) = obj.get_mut("tool_calls").and_then(Value::as_array_mut)
    {
        tool_calls.push(tool_call);
        if let Some(reasoning) = reasoning {
            if let Some(Value::String(existing)) = obj.get_mut("reasoning") {
                if !existing.is_empty() {
                    existing.push('\n');
                }
                existing.push_str(reasoning);
            } else {
                obj.insert(
                    "reasoning".to_string(),
                    Value::String(reasoning.to_string()),
                );
            }
        }
        return;
    }

    let mut msg = json!({
        "role": "assistant",
        "content": null,
        "tool_calls": [tool_call],
    });
    if let Some(reasoning) = reasoning
        && let Some(obj) = msg.as_object_mut()
    {
        obj.insert("reasoning".to_string(), json!(reasoning));
    }
    messages.push(msg);
}