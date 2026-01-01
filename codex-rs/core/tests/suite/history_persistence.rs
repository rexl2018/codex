#![allow(clippy::expect_used)]

use std::sync::Arc;

use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::CodexConversation;
use codex_core::ConversationManager;
use codex_core::ModelProviderInfo;
use codex_core::built_in_model_providers;
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
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn deleting_history_persists_across_resume() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse1 = sse(vec![
        ev_assistant_message("m1", "assistant-one"),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", "assistant-two"),
        ev_completed("r2"),
    ]);
    mount_sse_sequence(&server, vec![sse1, sse2]).await;

    let home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&home).await;
    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    config.model_provider = model_provider;

    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let manager = ConversationManager::new(auth_manager, SessionSource::Exec);
    let conversation = manager
        .new_conversation(config.clone())
        .await
        .expect("spawn conversation")
        .conversation;
    let rollout_path = conversation.rollout_path();

    submit_user_text(&conversation, "first turn").await;
    submit_user_text(&conversation, "second turn").await;

    let history_view = fetch_history_view(&conversation).await;
    let assistant_two_index = find_index(&history_view, "assistant-two");

    delete_history_entry(&conversation, assistant_two_index).await;

    let post_delete_view = fetch_history_view(&conversation).await;
    assert!(
        !post_delete_view.contains("assistant-two"),
        "expected assistant-two to be removed before shutdown\n{post_delete_view}"
    );

    shutdown_conversation(&conversation).await;

    let resume_auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let resumed_manager = ConversationManager::new(resume_auth.clone(), SessionSource::Exec);
    let resumed = resumed_manager
        .resume_conversation_from_rollout(config.clone(), rollout_path, resume_auth)
        .await
        .expect("resume conversation")
        .conversation;

    let resumed_view = fetch_history_view(&resumed).await;
    assert!(
        resumed_view.contains("assistant-one"),
        "expected first assistant response to remain after resume\n{resumed_view}"
    );
    assert!(
        !resumed_view.contains("assistant-two"),
        "deleted assistant response should not reappear after resume\n{resumed_view}"
    );

    shutdown_conversation(&resumed).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn undo_history_range_persists_across_resume() {
    skip_if_no_network!();

    let server = start_mock_server().await;
    let sse1 = sse(vec![
        ev_assistant_message("m1", "assistant-one"),
        ev_completed("r1"),
    ]);
    let sse2 = sse(vec![
        ev_assistant_message("m2", "assistant-two"),
        ev_completed("r2"),
    ]);
    mount_sse_sequence(&server, vec![sse1, sse2]).await;

    let home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&home).await;
    let model_provider = ModelProviderInfo {
        base_url: Some(format!("{}/v1", server.uri())),
        ..built_in_model_providers()["openai"].clone()
    };
    config.model_provider = model_provider;

    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let manager = ConversationManager::new(auth_manager, SessionSource::Exec);
    let conversation = manager
        .new_conversation(config.clone())
        .await
        .expect("spawn conversation")
        .conversation;
    let rollout_path = conversation.rollout_path();

    submit_user_text(&conversation, "first turn").await;
    submit_user_text(&conversation, "second turn").await;

    let history_view = fetch_history_view(&conversation).await;
    let assistant_one_index = find_index(&history_view, "assistant-one");

    undo_history_from(&conversation, Some(assistant_one_index)).await;

    let post_undo_view = fetch_history_view(&conversation).await;
    assert!(
        !post_undo_view.contains("assistant-one"),
        "expected undo to remove assistant-one\n{post_undo_view}"
    );
    assert!(
        !post_undo_view.contains("assistant-two"),
        "expected undo to remove assistant-two\n{post_undo_view}"
    );

    shutdown_conversation(&conversation).await;

    let resume_auth = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("dummy"));
    let resumed_manager = ConversationManager::new(resume_auth.clone(), SessionSource::Exec);
    let resumed = resumed_manager
        .resume_conversation_from_rollout(config.clone(), rollout_path, resume_auth)
        .await
        .expect("resume conversation")
        .conversation;

    let resumed_view = fetch_history_view(&resumed).await;
    assert!(
        !resumed_view.contains("assistant-one"),
        "assistant-one should remain removed after resume\n{resumed_view}"
    );
    assert!(
        !resumed_view.contains("assistant-two"),
        "assistant-two should remain removed after resume\n{resumed_view}"
    );

    shutdown_conversation(&resumed).await;
}

async fn submit_user_text(conversation: &Arc<CodexConversation>, text: &str) {
    conversation
        .submit(Op::UserInput {
            items: vec![UserInput::Text {
                text: text.to_string(),
            }],
        })
        .await
        .expect("submit user input");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::TaskComplete(_))).await;
}

async fn fetch_history_view(conversation: &Arc<CodexConversation>) -> String {
    conversation
        .submit(Op::ManageHistory {
            action: HistoryAction::ViewAll,
        })
        .await
        .expect("request view");
    let view_event =
        wait_for_event(conversation, |ev| matches!(ev, EventMsg::HistoryView(_))).await;
    let EventMsg::HistoryView(view) = view_event else {
        unreachable!("predicate guaranteed a HistoryView event");
    };
    view.content
}

fn find_index(view: &str, needle: &str) -> usize {
    for line in view.lines() {
        if let Some((idx, rest)) = line.split_once(". ")
            && rest.contains(needle)
        {
            return idx
                .parse()
                .unwrap_or_else(|e| panic!("invalid index in history view: {line} ({e})"));
        }
    }
    panic!("needle {needle} not found in history view:\n{view}");
}

async fn delete_history_entry(conversation: &Arc<CodexConversation>, index: usize) {
    conversation
        .submit(Op::ManageHistory {
            action: HistoryAction::Delete { index },
        })
        .await
        .expect("delete history entry");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::HistoryView(_))).await;
}

async fn undo_history_from(conversation: &Arc<CodexConversation>, start: Option<usize>) {
    conversation
        .submit(Op::ManageHistory {
            action: HistoryAction::Undo { start },
        })
        .await
        .expect("undo history range");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::HistoryView(_))).await;
}

async fn shutdown_conversation(conversation: &Arc<CodexConversation>) {
    conversation
        .submit(Op::Shutdown)
        .await
        .expect("shutdown conversation");
    wait_for_event(conversation, |ev| matches!(ev, EventMsg::ShutdownComplete)).await;
}
