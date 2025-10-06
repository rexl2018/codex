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
async fn apply_patch_success_emits_patch_apply_events() {
    let server = start_mock_server().await;

    let call_id = "patch-1";
    let patch_content = r#"--- a/test.txt
+++ b/test.txt
@@ -1 +1 @@
-old content
+new content"#;
    let arguments = json!({
        "input": patch_content
    });
    let body = sse(vec![
        ev_function_call(call_id, "apply_patch", &arguments.to_string()),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, wiremock::matchers::header_regex("authorization", ".*"), body).await;

    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();
    config.include_apply_patch_tool = true;

    // Create a test file to patch
    let test_file = config.cwd.join("test.txt");
    std::fs::write(&test_file, "old content").unwrap();

    let manager = ConversationManager::new(AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager
        .new_conversation(config)
        .await
        .expect("create new conversation");
    let conv = new_conv.conversation;

    conv
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "apply the patch".into(),
            }],
        })
        .await
        .unwrap();

    // Wait for PatchApplyBegin event
    let ev_begin = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::PatchApplyBegin(_))).await;
    match ev_begin {
        EventMsg::PatchApplyBegin(begin) => {
            assert_eq!(begin.call_id, call_id);
            assert!(begin.patch_content.contains("old content"));
            assert!(begin.patch_content.contains("new content"));
        }
        _ => unreachable!(),
    }

    // Wait for PatchApplyEnd event
    let ev_end = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::PatchApplyEnd(_))).await;
    match ev_end {
        EventMsg::PatchApplyEnd(end) => {
            assert_eq!(end.call_id, call_id);
            assert!(end.success, "patch should apply successfully");
        }
        _ => unreachable!(),
    }

    // Verify file was actually patched
    let content = std::fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "new content");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn apply_patch_invalid_format_returns_error() {
    let server = start_mock_server().await;

    let call_id = "patch-err";
    let bad_patch = "not a valid patch format";
    let arguments = json!({
        "input": bad_patch
    });
    let body = sse(vec![
        ev_function_call(call_id, "apply_patch", &arguments.to_string()),
        ev_completed("resp-2"),
    ]);
    mount_sse_once(&server, wiremock::matchers::header_regex("authorization", ".*"), body).await;

    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();
    config.include_apply_patch_tool = true;

    let manager = ConversationManager::new(AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager
        .new_conversation(config)
        .await
        .expect("create new conversation");
    let conv = new_conv.conversation;

    conv
        .submit(Op::UserInput {
            items: vec![InputItem::Text {
                text: "apply bad patch".into(),
            }],
        })
        .await
        .unwrap();

    // Expect a function call output with error
    let ev = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::FunctionCallOutput(_))).await;
    match ev {
        EventMsg::FunctionCallOutput(out) => {
            assert_eq!(out.call_id, call_id);
            assert!(out.output.content.contains("not contain a valid apply_patch command") || 
                    out.output.content.contains("CorrectnessError"));
            assert_eq!(out.output.success, Some(false));
        }
        _ => unreachable!(),
    }
}