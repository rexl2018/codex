#![allow(clippy::expect_used)]
use codex_core::CodexAuth;
use codex_core::AuthManager;
use codex_core::ConversationManager;
use codex_core::ModelProviderInfo;
use codex_core::NewConversation;
use codex_core::built_in_model_providers;
use codex_core::compact::SUMMARIZATION_PROMPT;
use codex_core::protocol::EventMsg;
use codex_core::protocol::HistoryAction;
use codex_core::protocol::Op;
use codex_core::protocol::SessionSource;
use codex_protocol::user_input::UserInput;
use core_test_support::load_default_config_for_test;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::mount_sse_sequence;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn compact_range_middle() {
    skip_if_no_network!();

    // Set up a mock server
    let server = start_mock_server().await;

    // 1. Initial conversation: User "msg1", Assistant "reply1", User "msg2", Assistant "reply2", User "msg3", Assistant "reply3"
    // We will simulate this by submitting user inputs and having mock responses.

    // Mock responses for initial turns
    let sse1 = sse(vec![
        ev_assistant_message("m1", "reply1"),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", "reply2"),
        ev_completed("r2"),
    ]);
    let sse3 = sse(vec![
        ev_assistant_message("m3", "reply3"),
        ev_completed("r3"),
    ]);

    // Mock response for compaction (summarizing msg2 and reply2)
    let summary_text = "Summary of msg2 and reply2";
    let sse_compact = sse(vec![
        ev_assistant_message("m_summary", summary_text),
        ev_completed("r_compact"),
    ]);

    // Mount responses
    mount_sse_sequence(&server, vec![sse1, sse2, sse3, sse_compact]).await;

    // Setup Codex
    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    let home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&home);
    config.model_provider = model_provider;
    config.compact_prompt = Some(SUMMARIZATION_PROMPT.to_string());

    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let conversation_manager = ConversationManager::new(auth_manager, SessionSource::Exec);
    let NewConversation {
        conversation: codex,
        ..
    } = conversation_manager.new_conversation(config).await.unwrap();

    // 1. Submit "msg1"
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "msg1".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // 2. Submit "msg2"
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "msg2".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // 3. Submit "msg3"
    codex
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: "msg3".into(),
            }],
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Current History (indices are 1-based for users, but 0-based internal):
    // 0: msg1
    // 1: reply1
    // 2: msg2
    // 3: reply2
    // 4: msg3
    // 5: reply3

    // We want to compact "msg2" and "reply2" (indices 2 and 3).
    // Indices in `HistoryAction` are 1-based usually?
    // Let's check `parse_hist_args` in `chatwidget.rs`. It parses `usize`.
    // `HistoryAction::DeleteRange` uses 1-based indices in `history.rs` logic: `self.items.drain((start - 1)..end);`.
    // So `start` is 1-based, `end` is 1-based inclusive.

    // So we want to compact items #3 and #4 (msg2 is #3, reply2 is #4).
    // Wait, items are:
    // 1. msg1
    // 2. reply1
    // 3. msg2
    // 4. reply2
    // 5. msg3
    // 6. reply3

    // So start=5, end=6.

    codex
        .submit(Op::ManageHistory {
            action: HistoryAction::Compact { start: 5, end: 6 },
        })
        .await
        .unwrap();
    wait_for_event(&codex, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;

    // Verify requests
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 4);

    // Check compaction request (last one)
    let compact_req = requests.last().unwrap();
    let body: serde_json::Value = compact_req.body_json().unwrap();
    let input = body["input"].as_array().unwrap();

    // Input should contain:
    // - msg2
    // - reply2
    // - summarization prompt

    // It should NOT contain msg1, reply1, msg3, reply3.
    // Wait, `msg2` and `reply2` are items #3 and #4.
    // The `input` to model usually includes system prompt (if configured).
    // But `run_compact_task_on_range` logic:
    // `let (prefix, suffix) = ...`
    // `history.record_items(prompt)`
    // `turn_input = history.get_history_for_prompt()`
    // `if let Some((start, end)) = range { ... slice ... }`

    // If we passed start=3, end=4 (indices in `all_items`).
    // `all_items` has 6 items.
    // `start` and `end` in `run_compact_task_on_range` are passed directly from `HistoryAction`.
    // `HistoryAction` indices are 1-based?
    // In `codex.rs`, we pass `start` and `end` directly to `CompactTask`.
    // In `compact.rs`, we use them to slice `all_items`.
    // `all_items` is 0-indexed.
    // So if user passes 1-based indices, we need to adjust them?
    // `HistoryAction::DeleteRange` logic in `history.rs`: `self.items.drain((start - 1)..end);`.
    // So `start` is 1-based.

    // I need to check if I adjusted indices in `compact.rs`.
    // I did NOT adjust indices in `compact.rs`!
    // `let slice = all_items[start..=end].to_vec();`
    // If `start` comes from `HistoryAction` (1-based), this will be off by 1 or panic.

    // I MUST fix `compact.rs` to handle 1-based indices if that's what `HistoryAction` uses.
    // `parse_hist_args` parses numbers.
    // `HistoryAction::DeleteRange` implementation handles `start-1`.

    // So `HistoryAction` carries 1-based indices (semantic).
    // My `CompactTask` receives them.
    // `run_compact_task_on_range` receives them.
    // `run_compact_task_inner` receives them.
    // And I used them directly as slice indices.

    // I need to fix `compact.rs` to subtract 1 from start, and maybe end?
    // `DeleteRange` uses `(start - 1)..end`. `drain` is exclusive of end.
    // So `start-1` (inclusive) to `end` (exclusive).
    // So items are `start-1, start, ..., end-1`.
    // i.e. items from index `start` to `end` (1-based).

    // My `compact.rs` uses `start..=end`.
    // If I want to include item at `end` (1-based), I should use `end-1` (0-based).
    // So range `start..end` (1-based) maps to `start-1..=end-1` (0-based).

    // I need to update `compact.rs` to adjust indices.
    // `let start_idx = start.saturating_sub(1);`
    // `let end_idx = end.saturating_sub(1);`

    // I will finish writing the test file first, assuming I will fix the code.
    // In test, I will use 1-based indices: 3 and 4.

    let msg2_text = "msg2";
    let reply2_text = "reply2";

    let has_msg2 = input
        .iter()
        .any(|item| item["content"][0]["text"].as_str() == Some(msg2_text));
    let has_reply2 = input
        .iter()
        .any(|item| item["content"][0]["text"].as_str() == Some(reply2_text));
    let has_msg1 = input
        .iter()
        .any(|item| item["content"][0]["text"].as_str() == Some("msg1"));

    assert!(has_msg2, "Compaction input should contain msg2");
    assert!(has_reply2, "Compaction input should contain reply2");
    assert!(!has_msg1, "Compaction input should NOT contain msg1");

    // Verify history after compaction
    // We can't easily inspect internal history state via public API except by `ViewAll` or `HistoryAction`.
    // But `HistoryAction::ViewAll` returns a string.

    // Let's use `Op::ManageHistory { action: HistoryAction::ViewAll }`.
    // But `submit` returns `Result<TaskResult, ...>`.
    // `TaskResult` for `ManageHistory` is `EventMsg::HistoryView`.
    // Wait, `manage_history` sends an event.

    codex
        .submit(Op::ManageHistory {
            action: HistoryAction::ViewAll,
        })
        .await
        .unwrap();
    let view_event = wait_for_event(&codex, |ev| matches!(ev, EventMsg::HistoryView(_))).await;
    let EventMsg::HistoryView(view) = view_event else {
        panic!("expected history view")
    };

    // Expected items:
    // 1. msg1
    // 2. reply1
    // 3. [CompactionSummary] (or user message with summary)
    // 4. msg3
    // 5. reply3

    // Note: `build_compacted_history` creates a `User` message with summary text?
    // Or `ResponseItem::CompactionSummary`?
    // `build_compacted_history` in `compact.rs` creates:
    // `ResponseItem::Message { role: "user", content: ... summary ... }`
    // AND `ResponseItem::Message { role: "assistant", content: ... "I have summarized..." ... }`?
    // No, `build_compacted_history` logic:
    // It creates a `User` message with `SUMMARY_PREFIX \n summary`.

    // So we expect:
    // 1. msg1
    // 2. reply1
    // 3. User message with summary
    // 4. msg3
    // 5. reply3

    println!("History View:\n{}", view.content);

    assert!(view.content.contains("msg1"));
    assert!(view.content.contains("reply1"));
    assert!(view.content.contains("msg3"));
    assert!(view.content.contains("reply3"));
    assert!(view.content.contains("Another language model"));
    assert!(view.content.contains("msg2")); // User message is preserved
    assert!(!view.content.contains("reply2")); // Assistant reply is summarized
}
