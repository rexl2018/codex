use core_test_support::load_default_config_for_test;
use core_test_support::start_mock_server;
use core_test_support::responses::{ev_completed, ev_function_call, mount_sse_once, sse};
use codex_core::ConversationManager;
use codex_core::AuthManager;
use codex_core::built_in_model_providers;
use codex_core::protocol::{EventMsg, Op, InputItem};
use serde_json::json;
use tempfile::TempDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_emits_plan_update_event_and_succeeds() {
    let server = start_mock_server().await;

    let call_id = "req-123";
    let arguments = json!({
        "explanation": "Starting work",
        "plan": [
            {"step":"Analyze codebase","status":"in_progress"},
            {"step":"Add tests","status":"pending"}
        ]
    });
    let body = sse(vec![
        ev_function_call(call_id, "update_plan", &arguments.to_string()),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, wiremock::matchers::header_regex("authorization", ".*"), body).await;

    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();
    config.include_plan_tool = true;

    let manager = ConversationManager::new(AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager
        .new_conversation(config)
        .await
        .expect("create new conversation");
    let conv = new_conv.conversation;

    conv
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "please plan".into(),
            }],
        })
        .await
        .unwrap();

    let ev = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::PlanUpdate(_))).await;
    match ev {
        EventMsg::PlanUpdate(update) => {
            assert_eq!(update.explanation, Some("Starting work".to_string()));
            assert_eq!(update.plan.len(), 2);
            let in_progress = update
                .plan
                .iter()
                .filter(|p| p.status == codex_core::plan_tool::StepStatus::InProgress)
                .count();
            assert_eq!(in_progress, 1);
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn update_plan_invalid_args_returns_function_error() {
    let server = start_mock_server().await;

    let call_id = "req-err";
    let bad_arguments = json!({"explanation":"oops"});
    let body = sse(vec![
        ev_function_call(call_id, "update_plan", &bad_arguments.to_string()),
        ev_completed("resp-2"),
    ]);
    mount_sse_once(&server, wiremock::matchers::header_regex("authorization", ".*"), body).await;

    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();
    config.include_plan_tool = true;

    let manager = ConversationManager::new(AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager
        .new_conversation(config)
        .await
        .expect("create new conversation");
    let conv = new_conv.conversation;

    conv
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "bad plan".into(),
            }],
        })
        .await
        .unwrap();

    let ev = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::FunctionCallOutput(_))).await;
    match ev {
        EventMsg::FunctionCallOutput(out) => {
            assert_eq!(out.call_id, call_id);
            assert!(out.output.content.contains("failed to parse function arguments"));
            assert_eq!(out.output.success, None);
        }
        _ => unreachable!(),
    }
}