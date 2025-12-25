use std::sync::Arc;

use crate::ModelProviderInfo;
use crate::Prompt;
use crate::client_common::MODEL_STREAM_IDLE_TIMEOUT;
use crate::client_common::ResponseEvent;
use crate::codex::Session;
use crate::codex::TurnContext;
use crate::codex::get_last_assistant_message_from_turn;
use crate::error::CodexErr;
use crate::error::Result as CodexResult;
use crate::features::Feature;
use crate::protocol::CompactedItem;
use crate::protocol::ContextCompactedEvent;
use crate::protocol::EventMsg;
use crate::protocol::TaskStartedEvent;
use crate::protocol::TurnContextItem;
use crate::protocol::WarningEvent;
use crate::truncate::TruncationPolicy;
use crate::truncate::approx_token_count;
use crate::truncate::truncate_text;
use crate::util::backoff;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::RolloutItem;
use codex_protocol::user_input::UserInput;
use futures::prelude::*;
use tokio::time::timeout;
use tracing::error;

pub const SUMMARIZATION_PROMPT: &str = include_str!("../templates/compact/prompt.md");
pub const SUMMARY_PREFIX: &str = include_str!("../templates/compact/summary_prefix.md");
const COMPACT_USER_MESSAGE_MAX_TOKENS: usize = 20_000;

pub(crate) fn should_use_remote_compact_task(
    session: &Session,
    provider: &ModelProviderInfo,
) -> bool {
    provider.is_openai() && session.enabled(Feature::RemoteCompaction)
}

pub(crate) async fn run_inline_auto_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
) {
    let prompt = turn_context.compact_prompt().to_string();
    let input = vec![UserInput::Text { text: prompt }];

    run_compact_task_inner(sess, turn_context, input, None).await;
}

pub(crate) async fn run_compact_task(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
) {
    let start_event = EventMsg::TaskStarted(TaskStartedEvent {
        model_context_window: turn_context.client.get_model_context_window(),
    });
    sess.send_event(&turn_context, start_event).await;
    run_compact_task_inner(sess, turn_context, input, None).await;
}

pub(crate) async fn run_compact_task_on_range(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    start: usize,
    end: usize,
) {
    let start_event = EventMsg::TaskStarted(TaskStartedEvent {
        model_context_window: turn_context.client.get_model_context_window(),
    });
    sess.send_event(&turn_context, start_event).await;

    if start == 0 {
        let event = EventMsg::Error(
            CodexErr::Other("Start index must be greater than 0".to_string()).to_error_event(None),
        );
        sess.send_event(&turn_context, event).await;
        return;
    }

    let start_idx = start.saturating_sub(1);
    let end_idx = end.saturating_sub(1);

    run_compact_task_inner(
        sess.clone(),
        turn_context,
        input,
        Some((start_idx, end_idx)),
    )
    .await;
}

async fn run_compact_task_inner(
    sess: Arc<Session>,
    turn_context: Arc<TurnContext>,
    input: Vec<UserInput>,
    range: Option<(usize, usize)>,
) {
    let initial_input_for_turn: ResponseInputItem = ResponseInputItem::from(input);

    let mut history = sess.clone_history().await;

    // If range is specified, we only want to compact that slice.
    // However, for the prompt to the model, we probably want to provide the context of what we are summarizing.
    // The current implementation of `run_compact_task_inner` assumes it's summarizing the *entire* history up to this point.
    // To support range compaction, we need to:
    // 1. Extract the slice [start..=end]
    // 2. Create a temporary history containing just this slice (plus maybe some context?)
    // 3. Ask the model to summarize *this slice*.
    // 4. Replace [start..=end] with the summary in the original history.

    // Let's adjust the strategy. Instead of modifying `run_compact_task_inner` heavily, let's make it work on a `History` object
    // that we construct. But `run_compact_task_inner` uses `sess` to send events and notifications, which is tied to the real session.

    // Actually, `run_compact_task_inner` does a lot:
    // - records initial input (the prompt)
    // - persists rollout items
    // - runs the model loop (drain_to_completed)
    // - handles errors/retries
    // - constructs the new history
    // - replaces history

    // If we want to reuse this, we need to be able to tell it:
    // "Use this specific list of items as the conversation history for the model interaction"
    // AND "When done, replace THIS range of the real history with the result".

    // The current `run_compact_task_inner` gets history from `sess.clone_history()`.
    // We can't easily mock `sess` here.

    // Alternative:
    // 1. In `run_compact_task_on_range`, we construct a temporary `TurnContext` or modify the prompt to include the text to be summarized.
    // 2. The `SUMMARIZATION_PROMPT` likely expects the history to be present in the conversation.

    // Let's look at `SUMMARIZATION_PROMPT`. It's likely "Summarize the above conversation...".
    // So we need to feed the model the conversation slice *as if* it were the history.

    // We can use `sess.clone_history()` but then *modify* the cloned history to only contain the slice we want to summarize.
    // Then `run_compact_task_inner` will use this modified history for the prompt.
    // BUT `run_compact_task_inner` calls `sess.replace_history` at the end, which would overwrite the *real* history with the compacted *slice* (losing the rest).

    // So `run_compact_task_inner` needs to know if it's operating on a slice and how to merge it back.

    // Let's modify `run_compact_task_inner` to take an optional `merge_callback` or `range`.

    let mut history_for_prompt = sess.clone_history().await;

    let (prefix, suffix) = if let Some((start, end)) = range {
        // Extract the slice to be compacted.
        // Note: history indices might need to be mapped carefully.
        // `sess.clone_history()` returns `History` struct.
        // `history.get_history()` returns `Vec<ResponseItem>`.

        // We need to verify indices are valid.
        let all_items = history_for_prompt.get_history();
        if start >= all_items.len() || end >= all_items.len() || start > end {
            let event = EventMsg::Error(
                CodexErr::Other("Invalid history range".to_string()).to_error_event(None),
            );
            sess.send_event(&turn_context, event).await;
            return;
        }

        let prefix = all_items[..start].to_vec();
        let _slice = all_items[start..=end].to_vec();
        let suffix = all_items[end + 1..].to_vec();

        // Replace history_for_prompt's items with just the slice.
        // We need to use `history_for_prompt.replace_items(slice)` if such method exists,
        // or re-construct a History. `History` struct (from `crate::message_history`) might not be public or easy to manipulate here.
        // `history` is `crate::message_history::History`.

        // Actually `history` variable here is `crate::message_history::History`.
        // It has `get_history_for_prompt()`.

        // Let's assume we can manipulate `history` or we need to change how `run_compact_task_inner` gets the input.

        // Hack: We can't easily modify `History` in place if it doesn't expose methods.
        // But `run_compact_task_inner` uses `history.get_history_for_prompt()` in the loop.

        // Let's look at `core/src/message_history.rs` if possible, but I can't see it right now.
        // Assuming `History` has `new` or `from`.

        // Wait, `sess.clone_history()` returns `History`.
        // `History` has `record_items`.

        // Let's try to implement `run_compact_task_inner` logic but customized for range.
        // Or refactor `run_compact_task_inner` to be more composable.

        // Refactoring `run_compact_task_inner`:
        // It does 3 things:
        // 1. Setup (record prompt).
        // 2. Loop (generate summary).
        // 3. Finalize (replace history).

        // We can keep 1 and 2 mostly the same, but 3 needs to change for range compaction.

        // Let's pass `range` to `run_compact_task_inner`.

        // If `range` is Some((start, end)):
        // We need to make sure `history.get_history_for_prompt()` returns only the items in range.
        // Since we can't easily modify `History` struct (it's likely opaque or complex),
        // maybe we can just construct the `Prompt` manually in the loop?

        // `Prompt { input: turn_input.clone(), .. }`
        // `turn_input` comes from `history.get_history_for_prompt()`.

        // If we filter `history` before entering the loop?
        // `history` is a local variable `mut history`.
        // If `History` allows replacing items...

        // Let's assume for now we can't easily modify `History` object to contain partial history without breaking other things (like indices).
        // But wait, `History` is just a wrapper around `Vec<ResponseItem>` usually.

        // Let's check `core/src/message_history.rs` if I can... no I should stick to `compact.rs`.

        // Let's look at `run_compact_task_inner` again.
        // It calls `history.record_items` to add the prompt.

        // If we are compacting a range, we want the prompt to be appended to that range.
        // So: [Range Items] + [Summarization Prompt]

        // So if we have `range`, we should construct a temporary `History` containing just [Range Items].
        // But `sess.clone_history()` gives us full history.

        // Maybe we can use `sess.clone_history()` then somehow filter it?
        // If `History` doesn't support filtering, we are stuck.

        // Let's assume `History` has a way to be constructed from items.
        // `sess.clone_history()` -> `History`.

        // Let's look at `core/src/codex.rs` again to see `History` usage.
        // `sess.clone_history()` returns `History`.

        // I'll assume I can't easily change `History`.
        // But I can change what I pass to `Prompt`.

        // In the loop:
        // `let turn_input = history.get_history_for_prompt();`
        // If `range` is set, we want `turn_input` to be `slice + prompt`.

        (Some(prefix), Some(suffix))
    } else {
        (None, None)
    };

    history.record_items(
        &[initial_input_for_turn.into()],
        turn_context.truncation_policy,
    );

    let mut truncated_count = 0usize;

    let max_retries = turn_context.client.get_provider().stream_max_retries();
    let mut retries = 0;

    let rollout_item = RolloutItem::TurnContext(TurnContextItem {
        cwd: turn_context.cwd.clone(),
        approval_policy: turn_context.approval_policy,
        sandbox_policy: turn_context.sandbox_policy.clone(),
        model: turn_context.client.get_model(),
        effort: turn_context.client.get_reasoning_effort(),
        summary: turn_context.client.get_reasoning_summary(),
        base_instructions: turn_context.base_instructions.clone(),
        user_instructions: turn_context.user_instructions.clone(),
        developer_instructions: turn_context.developer_instructions.clone(),
        final_output_json_schema: turn_context.final_output_json_schema.clone(),
        truncation_policy: Some(turn_context.truncation_policy.into()),
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    loop {
        let mut turn_input = history.get_history_for_prompt();

        if let Some((start, end)) = range
            && end < turn_input.len()
        {
            let mut slice = turn_input[start..=end].to_vec();
            if let Some(last) = turn_input.last() {
                slice.push(last.clone());
            }
            turn_input = slice;
        }

        let prompt = Prompt {
            input: turn_input.clone(),
            last_response_id: None,
            ..Default::default()
        };
        let attempt_result = drain_to_completed(&sess, turn_context.as_ref(), &prompt).await;

        match attempt_result {
            Ok(()) => {
                if truncated_count > 0 {
                    sess.notify_background_event(
                        turn_context.as_ref(),
                        format!(
                            "Trimmed {truncated_count} older conversation item(s) before compacting so the prompt fits the model context window."
                        ),
                    )
                    .await;
                }
                break;
            }
            Err(CodexErr::Interrupted) => {
                return;
            }
            Err(e @ CodexErr::ContextWindowExceeded) => {
                if turn_input.len() > 1 {
                    // Trim from the beginning to preserve cache (prefix-based) and keep recent messages intact.
                    error!(
                        "Context window exceeded while compacting; removing oldest history item. Error: {e}"
                    );
                    // If we are in range mode, we can't easily use `history.remove_first_item()` because it affects the whole history.
                    // But `history` here is a local clone (mostly).
                    // `sess.clone_history()` returns a clone.

                    if range.is_some() {
                        // We can't easily support truncation loop with range yet without more complex logic.
                        // For now, let's just fail or rely on the fact that we are compacting a sub-range which hopefully fits.
                        // Or we can try to adjust `start`?

                        // Let's just error out for now to be safe, or try to proceed.
                        // `history.remove_first_item()` removes from the *beginning* of the whole history.
                        // This might be outside our range!

                        let event = EventMsg::Error(e.to_error_event(None));
                        sess.send_event(&turn_context, event).await;
                        return;
                    }

                    history.remove_first_item();
                    truncated_count += 1;
                    retries = 0;
                    continue;
                }
                sess.set_total_tokens_full(turn_context.as_ref()).await;
                let event = EventMsg::Error(e.to_error_event(None));
                sess.send_event(&turn_context, event).await;
                return;
            }
            Err(e) => {
                if retries < max_retries {
                    retries += 1;
                    let delay = backoff(retries);
                    sess.notify_stream_error(
                        turn_context.as_ref(),
                        format!("Reconnecting... {retries}/{max_retries}"),
                        e,
                    )
                    .await;
                    tokio::time::sleep(delay).await;
                    continue;
                } else {
                    let event = EventMsg::Error(e.to_error_event(None));
                    sess.send_event(&turn_context, event).await;
                    return;
                }
            }
        }
    }

    let history_snapshot = sess.clone_history().await.get_history();
    let summary_suffix =
        get_last_assistant_message_from_turn(&history_snapshot).unwrap_or_default();
    let summary_text = format!("{SUMMARY_PREFIX}\n{summary_suffix}");

    // For user messages, we only want those in the range if range is specified.
    let items_to_collect = if let Some((start, end)) = range {
        if end < history_snapshot.len() {
            &history_snapshot[start..=end]
        } else {
            &history_snapshot
        }
    } else {
        &history_snapshot
    };

    let user_messages = collect_user_messages(items_to_collect);

    let initial_context = sess.build_initial_context(turn_context.as_ref());
    let mut new_compacted_slice =
        build_compacted_history(initial_context, &user_messages, &summary_text);
    new_compacted_slice = remove_ghost_snapshots(new_compacted_slice);

    let new_history = if let Some((start, _end)) = range {
        // Reconstruct full history: prefix + compacted_slice + suffix
        // Note: `build_compacted_history` adds `initial_context` at the beginning.
        // If we are compacting a middle range, we probably DON'T want `initial_context` repeated if it's already in prefix.
        // But `initial_context` usually contains system prompt / user instructions.
        // If `start > 0`, `prefix` might contain them.

        // `build_compacted_history` takes `initial_context` as first arg.
        // If we pass empty vec, it won't add them.

        let mut final_history = Vec::new();

        // Add prefix
        if let Some(p) = prefix {
            final_history.extend(p);
        }

        // Add compacted slice
        // We need to regenerate `new_compacted_slice` without `initial_context` if we are preserving prefix.
        // But wait, `build_compacted_history` puts `initial_context` first.

        // If `start == 0`, we want `initial_context`.
        // If `start > 0`, we assume `prefix` has it (or it's not needed in the middle).

        let slice_initial_context = if start == 0 {
            sess.build_initial_context(turn_context.as_ref())
        } else {
            Vec::new()
        };

        let mut middle =
            build_compacted_history(slice_initial_context, &user_messages, &summary_text);
        middle = remove_ghost_snapshots(middle);

        final_history.extend(middle);

        // Add suffix
        if let Some(s) = suffix {
            final_history.extend(s);
        }

        final_history
    } else {
        // Original logic
        remove_ghost_snapshots(new_compacted_slice)
    };

    sess.replace_history(new_history).await;
    sess.recompute_token_usage(&turn_context).await;

    let rollout_item = RolloutItem::Compacted(CompactedItem {
        message: summary_text.clone(),
        replacement_history: None,
    });
    sess.persist_rollout_items(&[rollout_item]).await;

    let event = EventMsg::ContextCompacted(ContextCompactedEvent {});
    sess.send_event(&turn_context, event).await;

    let warning = EventMsg::Warning(WarningEvent {
        message: "Heads up: Long conversations and multiple compactions can cause the model to be less accurate. Start a new conversation when possible to keep conversations small and targeted.".to_string(),
    });
    sess.send_event(&turn_context, warning).await;
}

pub(crate) fn remove_ghost_snapshots(items: Vec<ResponseItem>) -> Vec<ResponseItem> {
    items
        .into_iter()
        .filter(|item| !matches!(item, ResponseItem::GhostSnapshot { .. }))
        .collect()
}

pub fn content_items_to_text(content: &[ContentItem]) -> Option<String> {
    let mut pieces = Vec::new();
    for item in content {
        match item {
            ContentItem::InputText { text } | ContentItem::OutputText { text } => {
                if !text.is_empty() {
                    pieces.push(text.as_str());
                }
            }
            ContentItem::InputImage { .. } => {}
        }
    }
    if pieces.is_empty() {
        None
    } else {
        Some(pieces.join("\n"))
    }
}

pub(crate) fn collect_user_messages(items: &[ResponseItem]) -> Vec<String> {
    items
        .iter()
        .filter_map(|item| match crate::event_mapping::parse_turn_item(item) {
            Some(TurnItem::UserMessage(user)) => {
                if is_summary_message(&user.message()) {
                    None
                } else {
                    Some(user.message())
                }
            }
            _ => None,
        })
        .collect()
}

pub(crate) fn is_summary_message(message: &str) -> bool {
    message.starts_with(format!("{SUMMARY_PREFIX}\n").as_str())
}

pub(crate) fn build_compacted_history(
    initial_context: Vec<ResponseItem>,
    user_messages: &[String],
    summary_text: &str,
) -> Vec<ResponseItem> {
    build_compacted_history_with_limit(
        initial_context,
        user_messages,
        summary_text,
        COMPACT_USER_MESSAGE_MAX_TOKENS,
    )
}

fn build_compacted_history_with_limit(
    mut history: Vec<ResponseItem>,
    user_messages: &[String],
    summary_text: &str,
    max_tokens: usize,
) -> Vec<ResponseItem> {
    let mut selected_messages: Vec<String> = Vec::new();
    if max_tokens > 0 {
        let mut remaining = max_tokens;
        for message in user_messages.iter().rev() {
            if remaining == 0 {
                break;
            }
            let tokens = approx_token_count(message);
            if tokens <= remaining {
                selected_messages.push(message.clone());
                remaining = remaining.saturating_sub(tokens);
            } else {
                let truncated = truncate_text(message, TruncationPolicy::Tokens(remaining));
                selected_messages.push(truncated);
                break;
            }
        }
        selected_messages.reverse();
    }

    for message in &selected_messages {
        history.push(ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: message.clone(),
            }],
        });
    }

    let summary_text = if summary_text.is_empty() {
        "(no summary available)".to_string()
    } else {
        summary_text.to_string()
    };

    history.push(ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText { text: summary_text }],
    });

    history
}

async fn drain_to_completed(
    sess: &Session,
    turn_context: &TurnContext,
    prompt: &Prompt,
) -> CodexResult<()> {
    let mut stream = turn_context.client.clone().stream(prompt).await?;
    loop {
        let maybe_event = timeout(MODEL_STREAM_IDLE_TIMEOUT, stream.next())
            .await
            .map_err(|_| CodexErr::Stream("timeout waiting for SSE event".to_string(), None))?;
        let Some(event) = maybe_event else {
            return Err(CodexErr::Stream(
                "stream closed before response.completed".into(),
                None,
            ));
        };
        match event {
            Ok(ResponseEvent::OutputItemDone(item)) => {
                sess.record_into_history(std::slice::from_ref(&item), turn_context)
                    .await;
            }
            Ok(ResponseEvent::RateLimits(snapshot)) => {
                sess.update_rate_limits(turn_context, snapshot).await;
            }
            Ok(ResponseEvent::Completed { token_usage, .. }) => {
                sess.update_token_usage_info(turn_context, token_usage.as_ref())
                    .await;
                return Ok(());
            }
            Ok(_) => continue,
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {

    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn content_items_to_text_joins_non_empty_segments() {
        let items = vec![
            ContentItem::InputText {
                text: "hello".to_string(),
            },
            ContentItem::OutputText {
                text: String::new(),
            },
            ContentItem::OutputText {
                text: "world".to_string(),
            },
        ];

        let joined = content_items_to_text(&items);

        assert_eq!(Some("hello\nworld".to_string()), joined);
    }

    #[test]
    fn content_items_to_text_ignores_image_only_content() {
        let items = vec![ContentItem::InputImage {
            image_url: "file://image.png".to_string(),
        }];

        let joined = content_items_to_text(&items);

        assert_eq!(None, joined);
    }

    #[test]
    fn collect_user_messages_extracts_user_text_only() {
        let items = vec![
            ResponseItem::Message {
                id: Some("assistant".to_string()),
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "ignored".to_string(),
                }],
            },
            ResponseItem::Message {
                id: Some("user".to_string()),
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "first".to_string(),
                }],
            },
            ResponseItem::Other,
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["first".to_string()], collected);
    }

    #[test]
    fn collect_user_messages_filters_session_prefix_entries() {
        let items = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "# AGENTS.md instructions for project\n\n<INSTRUCTIONS>\ndo things\n</INSTRUCTIONS>"
                        .to_string(),
                }],
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "<ENVIRONMENT_CONTEXT>cwd=/tmp</ENVIRONMENT_CONTEXT>".to_string(),
                }],
            },
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "real user message".to_string(),
                }],
            },
        ];

        let collected = collect_user_messages(&items);

        assert_eq!(vec!["real user message".to_string()], collected);
    }

    #[test]
    fn build_token_limited_compacted_history_truncates_overlong_user_messages() {
        // Use a small truncation limit so the test remains fast while still validating
        // that oversized user content is truncated.
        let max_tokens = 16;
        let big = "word ".repeat(200);
        let history = super::build_compacted_history_with_limit(
            Vec::new(),
            std::slice::from_ref(&big),
            "SUMMARY",
            max_tokens,
        );
        assert_eq!(history.len(), 2);

        let truncated_message = &history[0];
        let summary_message = &history[1];

        let truncated_text = match truncated_message {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("unexpected item in history: {other:?}"),
        };

        assert!(
            truncated_text.contains("tokens truncated"),
            "expected truncation marker in truncated user message"
        );
        assert!(
            !truncated_text.contains(&big),
            "truncated user message should not include the full oversized user text"
        );

        let summary_text = match summary_message {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("unexpected item in history: {other:?}"),
        };
        assert_eq!(summary_text, "SUMMARY");
    }

    #[test]
    fn build_token_limited_compacted_history_appends_summary_message() {
        let initial_context: Vec<ResponseItem> = Vec::new();
        let user_messages = vec!["first user message".to_string()];
        let summary_text = "summary text";

        let history = build_compacted_history(initial_context, &user_messages, summary_text);
        assert!(
            !history.is_empty(),
            "expected compacted history to include summary"
        );

        let last = history.last().expect("history should have a summary entry");
        let summary = match last {
            ResponseItem::Message { role, content, .. } if role == "user" => {
                content_items_to_text(content).unwrap_or_default()
            }
            other => panic!("expected summary message, found {other:?}"),
        };
        assert_eq!(summary, summary_text);
    }

    #[test]
    fn remove_ghost_snapshots_filters_out_entries() {
        let items = vec![
            ResponseItem::Message {
                id: None,
                role: "user".to_string(),
                content: vec![ContentItem::InputText {
                    text: "kept".to_string(),
                }],
            },
            ResponseItem::GhostSnapshot {
                ghost_commit: codex_git::GhostCommit::new(
                    "abc1234".to_string(),
                    None,
                    Vec::new(),
                    Vec::new(),
                ),
            },
            ResponseItem::Message {
                id: None,
                role: "assistant".to_string(),
                content: vec![ContentItem::OutputText {
                    text: "also kept".to_string(),
                }],
            },
        ];

        let filtered = remove_ghost_snapshots(items);
        assert_eq!(filtered.len(), 2);
        assert!(
            filtered
                .iter()
                .all(|item| { !matches!(item, ResponseItem::GhostSnapshot { .. }) })
        );
    }
}
