use std::sync::Arc;
use tokio::time::Duration;

use codex_core::AuthManager;
use codex_core::CodexConversation;
use codex_core::ConversationManager;
use codex_core::ModelProviderInfo;
use codex_core::NewConversation;
use codex_core::built_in_model_providers;
use codex_core::protocol::{EventMsg, InitialHistory, SandboxPolicy};
use codex_core::session::ConfigureSession;
use codex_core::tool_config::UnifiedToolConfig;
use codex_core::config::{Config, ConfigOverrides};
use codex_core::config_types::McpServerConfig;
use core_test_support::load_default_config_for_test;
use core_test_support::responses::{ev_completed, ev_function_call, mount_sse_once, sse};
use core_test_support::wait_for_event_with_timeout;
use codex_core::ConversationManager;
use codex_core::built_in_model_providers;
use codex_core::protocol::EventMsg;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::Duration;
use wiremock::matchers::header_regex;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_connect_list_call_events_flow() {
    // Prepare config with MCP server enabled (codex-mcp-server)
    let codex_home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&codex_home);
    // Use a local mock model provider base URL that won't be contacted in this test
    config.model_provider = built_in_model_providers()["openai"].clone();
    config.approval_policy = codex_core::protocol::AskForApproval::Never;
    config.sandbox_policy = SandboxPolicy::new_read_only_policy();

    // Configure MCP servers
    let mut servers = std::collections::HashMap::new();
    servers.insert(
        "codex".to_string(),
        McpServerConfig {
            command: "codex-mcp-server".to_string(),
            args: Vec::new(),
            env: None,
            startup_timeout_ms: Some(10_000),
        },
    );
    config.mcp_servers = servers;

    // Create session via ConversationManager
    let auth_manager = AuthManager::shared(config.codex_home.clone());
    let convo_mgr = ConversationManager::new(auth_manager);

    let provider = ModelProviderInfo { ..config.model_provider.clone() };

    let (session, _turn_ctx) = codex_core::session::Session::new(
        ConfigureSession {
            provider: provider,
            model: config.model.clone(),
            model_reasoning_effort: None,
            model_reasoning_summary: codex_protocol::config_types::ReasoningSummary::default(),
            user_instructions: None,
            base_instructions: None,
            approval_policy: config.approval_policy.clone(),
            sandbox_policy: config.sandbox_policy.clone(),
            notify: None,
            cwd: std::env::current_dir().unwrap(),
        },
        Arc::new(config.clone()),
        auth_manager.clone(),
        convo_mgr.event_sender_for_tests(),
        InitialHistory::New,
    )
    .await
    .expect("session");

    // After session creation, tools should be listed and cached globally
    let mcp_tools = session.get_mcp_connection_manager().list_all_tools();
    assert!(mcp_tools.contains_key("codex__codex"));
    assert!(mcp_tools.contains_key("codex__codex-reply"));

    // Trigger a tool call via handle_mcp_tool_call and observe begin/end events
    let sub_id = "test-sub".to_string();
    let call_id = "call-1".to_string();
    let args = serde_json::json!({
        "prompt": "Hello from test",
        "include_plan_tool": true
    });

    let _ = codex_core::mcp_tool_call::handle_mcp_tool_call(
        &session,
        &sub_id,
        call_id.clone(),
        "codex".to_string(),
        "codex".to_string(),
        serde_json::to_string(&args).unwrap(),
        Some(Duration::from_secs(5)),
    )
    .await;

    // Assert begin event
    let ev_begin = wait_for_event_with_timeout(
        &CodexConversation::new(codex_core::Codex::new_for_tests(session.clone(), Arc::new(Config::load_from_base_config_with_overrides(
            Config::load_config_as_toml(&config.codex_home).unwrap().into(),
            ConfigOverrides::default(),
            config.codex_home.clone(),
        ).unwrap()))),
        |ev| matches!(ev, EventMsg::McpToolCallBegin(_)),
        Duration::from_secs(3),
    )
    .await;
    match ev_begin {
        EventMsg::McpToolCallBegin(begin) => {
            assert_eq!(begin.call_id, call_id);
            assert_eq!(begin.invocation.server, "codex");
            assert_eq!(begin.invocation.tool, "codex");
        }
        _ => unreachable!(),
    }

    // Assert end event
    let ev_end = wait_for_event_with_timeout(
        &CodexConversation::new(codex_core::Codex::new_for_tests(session.clone(), Arc::new(config.clone()))),
        |ev| matches!(ev, EventMsg::McpToolCallEnd(_)),
        Duration::from_secs(5),
    )
    .await;
    match ev_end {
        EventMsg::McpToolCallEnd(end) => {
            assert_eq!(end.call_id, call_id);
            assert_eq!(end.invocation.server, "codex");
            assert_eq!(end.invocation.tool, "codex");
            assert!(end.duration >= Duration::from_millis(1));
            assert!(end.result.is_ok(), "tool call should succeed: {:?}", end.result);
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_codex_tool_success_emits_begin_and_end_events() {
    // Mock upstream model returning a function_call for MCP codex tool
    let server = core_test_support::start_mock_server().await;
    let call_id = "mcp-1";
    let args = json!({
        "prompt": "Hello from test"
    });
    let body = sse(vec![
        ev_function_call(call_id, "codex__codex", &args.to_string()),
        ev_completed("resp-1"),
    ]);
    mount_sse_once(&server, header_regex("authorization", ".*"), body).await;

    // Configure Codex
    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();

    // Create conversation
    let manager = ConversationManager::new(codex_core::AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager.new_conversation(config).await.expect("new conversation");
    let conv = new_conv.conversation;

    // Send a user prompt to trigger the mocked response
    conv.submit(codex_core::protocol::Op::UserInput {
        items: vec![codex_core::protocol::InputItem::Text {
            text: "trigger".into(),
        }],
    })
    .await
    .expect("send user message");

    // Assert MCP begin event
    let ev_begin = wait_for_event_with_timeout(&conv, |e| matches!(e, EventMsg::McpToolCallBegin(_)), Duration::from_secs(3)).await;
    match ev_begin {
        EventMsg::McpToolCallBegin(begin) => {
            assert_eq!(begin.call_id, call_id);
            assert_eq!(begin.invocation.server, "codex");
            assert_eq!(begin.invocation.tool, "codex");
        }
        _ => unreachable!(),
    }

    // Assert MCP end event
    let ev_end = wait_for_event_with_timeout(&conv, |e| matches!(e, EventMsg::McpToolCallEnd(_)), Duration::from_secs(5)).await;
    match ev_end {
        EventMsg::McpToolCallEnd(end) => {
            assert_eq!(end.call_id, call_id);
            assert_eq!(end.invocation.server, "codex");
            assert_eq!(end.invocation.tool, "codex");
            assert!(end.duration >= Duration::from_millis(1));
            assert!(end.result.is_ok(), "tool call should succeed: {:?}", end.result);
        }
        _ => unreachable!(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mcp_codex_tool_argument_parse_error_maps_to_function_output_error() {
    let server = core_test_support::start_mock_server().await;
    let call_id = "mcp-err";
    // Provide a non-JSON arguments string to trigger parse error inside handle_mcp_tool_call
    let bad_args = "not-json";
    let body = sse(vec![
        ev_function_call(call_id, "codex__codex", bad_args),
        ev_completed("resp-2"),
    ]);
    mount_sse_once(&server, header_regex("authorization", ".*"), body).await;

    let codex_home = TempDir::new().unwrap();
    let mut config = load_default_config_for_test(&codex_home);
    let mut provider = built_in_model_providers()["openai"].clone();
    provider.base_url = Some(format!("{}/v1", server.uri()));
    config.model_provider = provider.clone();

    let manager = ConversationManager::new(codex_core::AuthManager::shared(config.codex_home.clone()));
    let new_conv = manager.new_conversation(config).await.expect("new conversation");
    let conv = new_conv.conversation;

    conv.submit(codex_core::protocol::Op::UserInput {
        items: vec![codex_core::protocol::InputItem::Text {
            text: "trigger".into(),
        }],
    })
    .await
    .expect("send user message");

    // Expect a FunctionCallOutput with error content
    let ev = core_test_support::wait_for_event(&conv, |e| matches!(e, EventMsg::FunctionCallOutput(_))).await;
    match ev {
        EventMsg::FunctionCallOutput(out) => {
            assert_eq!(out.call_id, call_id);
            assert!(out.output.content.contains("err:"));
            assert_eq!(out.output.success, Some(false));
        }
        _ => unreachable!(),
    }
}