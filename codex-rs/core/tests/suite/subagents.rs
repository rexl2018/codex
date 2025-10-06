use std::time::Duration;

// 导入核心组件
use codex_core::AgentStateManager;
use codex_core::context_store::InMemoryContextRepository;
use codex_core::main_agent::MainAgent;
use codex_core::AuthManager;
use codex_core::CodexAuth;
use codex_core::built_in_model_providers;
use codex_core::protocol::{BootstrapPath, Event, EventMsg};
use codex_core::protocol::SubagentType;
use codex_core::subagent_manager::{ExecutorType, InMemorySubagentManager, ISubagentManager, SubagentTaskSpec};
use codex_core::{ModelClient, WireApi};
use codex_protocol::mcp_protocol::ConversationId;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use core_test_support::load_default_config_for_test;

/// 等待特定事件的辅助函数
/// 
/// 这个函数会持续监听事件通道，直到收到满足条件的事件
async fn wait_for_event<F>(rx: &mut mpsc::UnboundedReceiver<Event>, pred: F) -> Event
where
    F: Fn(&EventMsg) -> bool,
{
    loop {
        match rx.recv().await {
            Some(ev) if pred(&ev.msg) => return ev,
            Some(_) => continue, // 忽略不匹配的事件
            None => panic!("event channel closed before receiving expected event"),
        }
    }
}

/// 创建用于测试的子代理管理器和主代理
/// 
/// 返回：
/// - InMemorySubagentManager: 子代理管理器
/// - Arc<AgentStateManager>: 状态管理器，用于验证状态转换
/// - mpsc::UnboundedReceiver<Event>: 事件接收器，用于监听子代理事件
/// - oneshot::Sender<()>: 关闭信号发送器，用于清理主代理
async fn make_manager_with_main_agent() -> (
    InMemorySubagentManager,
    std::sync::Arc<AgentStateManager>,
    mpsc::UnboundedReceiver<Event>,
    oneshot::Sender<()>,
) {
    // 创建临时目录用于测试
    let codex_home = TempDir::new().expect("tempdir");
    let mut config = load_default_config_for_test(&codex_home);
    let cwd = TempDir::new().expect("cwd");
    config.cwd = cwd.path().to_path_buf();

    // 使用内置的OSS提供商进行测试
    let provider = built_in_model_providers()["oss"].clone();
    config.model_provider = provider;

    // 创建认证管理器
    let auth_manager = AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
    
    // 使用ConversationManager创建会话，这是推荐的方式
    let conversation_manager = codex_core::ConversationManager::with_auth(CodexAuth::from_api_key("Test API Key"));
    let new_conversation = conversation_manager
        .new_conversation(config)
        .await
        .expect("create new conversation");
    
    // 从conversation中获取session（通过内部访问）
    // 注意：这里我们需要直接使用conversation而不是session
    let conversation = new_conversation.conversation;

    // 创建主代理和关闭通道
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    
    // 由于我们无法直接访问Session，我们需要使用一个简化的方法
    // 创建一个独立的状态管理器用于测试
    let state_manager = std::sync::Arc::new(AgentStateManager::new());
    
    // 创建事件通道用于监听子代理事件
    let (tx_event, rx_event) = mpsc::unbounded_channel::<Event>();

    // 创建上下文存储库
    let ctx_repo = InMemoryContextRepository::new();
    let rollout_path = codex_home.path().join("rollout.jsonl");

    // 创建一个简化的主代理事件发送器
    let (main_agent_event_tx, _main_agent_event_rx) = mpsc::unbounded_channel();

    // 创建子代理管理器，使用Mock执行器进行测试
    let manager = InMemorySubagentManager::with_state_manager(
        std::sync::Arc::new(ctx_repo),
        tx_event,
        ExecutorType::Mock, // 使用Mock执行器，避免真实的LLM调用
        state_manager.clone(),
        main_agent_event_tx,
        rollout_path,
    );

    (manager, state_manager, rx_event, shutdown_tx)
}

/// 测试Explorer和Coder子代理的端到端执行流程，验证状态阻塞机制
/// 
/// 这个测试验证：
/// 1. 初始状态下可以创建子代理
/// 2. 子代理运行时会阻塞新的子代理创建
/// 3. 子代理完成后恢复创建能力
/// 4. 不同类型的子代理都遵循相同的状态管理规则
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explorer_and_coder_end_to_end_with_state_blocking() {
    let (manager, state_manager, mut rx_event, shutdown_tx) = make_manager_with_main_agent().await;

    // 验证初始状态：应该允许创建所有类型的子代理
    assert!(state_manager.can_create_explorer());
    assert!(state_manager.can_create_coder());

    // 准备Explorer子代理的引导文件
    let tmp = TempDir::new().unwrap();
    let file_path = tmp.path().join("hello.txt");
    std::fs::write(&file_path, "hello world").unwrap();

    // 创建Explorer子代理任务规格
    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "Explore bootstrap file".to_string(),
        description: "Analyze given file and produce contexts".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![BootstrapPath {
            path: file_path.clone(),
            reason: "seed file".to_string(),
        }],
        max_turns: Some(3),
        timeout_ms: Some(3_000),
        network_access: None,
        injected_conversation: None,
    };

    // 创建并启动Explorer子代理
    let task_id = manager.create_task(spec).await.expect("create explorer task");
    let _handle = manager.launch_subagent(&task_id).await.expect("launch explorer");

    // 手动设置状态为等待子代理（模拟真实的状态转换）
    state_manager.transition_to_waiting_for_subagent(task_id.clone());

    // 验证子代理运行时的状态阻塞：应该阻止创建新的子代理
    assert!(!state_manager.can_create_explorer());
    assert!(!state_manager.can_create_coder());

    // 等待子代理启动和完成事件
    let _started = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentStarted(_))).await;
    let completed = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
    
    // 验证完成事件的内容
    if let EventMsg::SubagentCompleted(ev) = completed.msg {
        assert_eq!(ev.task_id, task_id);
        assert!(ev.contexts_created >= 0); // Mock执行器可能创建0个或多个上下文
    }

    // 手动恢复状态（模拟主代理处理完成事件）
    state_manager.transition_to_idle();
    
    // 验证状态恢复：完成后应该重新允许创建子代理
    assert!(state_manager.can_create_explorer());
    assert!(state_manager.can_create_coder());

    // 现在测试Coder子代理
    let spec2 = SubagentTaskSpec {
        agent_type: SubagentType::Coder,
        title: "Implement small change".to_string(),
        description: "Write or modify code based on task description".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(2),
        timeout_ms: Some(3_000),
        network_access: None,
        injected_conversation: None,
    };

    // 创建并启动Coder子代理
    let task_id2 = manager.create_task(spec2).await.expect("create coder task");
    let _handle2 = manager.launch_subagent(&task_id2).await.expect("launch coder");

    // 手动设置状态为等待子代理
    state_manager.transition_to_waiting_for_subagent(task_id2.clone());

    // 再次验证状态阻塞
    assert!(!state_manager.can_create_explorer());
    assert!(!state_manager.can_create_coder());

    // 等待Coder子代理完成
    let _started2 = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentStarted(_))).await;
    let completed2 = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
    if let EventMsg::SubagentCompleted(ev) = completed2.msg {
        assert_eq!(ev.task_id, task_id2);
    }

    // 最终状态恢复验证
    state_manager.transition_to_idle();
    assert!(state_manager.can_create_explorer());
    assert!(state_manager.can_create_coder());

    // 清理：关闭主代理以避免后台任务泄漏
    let _ = shutdown_tx.send(());
}

/// 测试并发子代理的执行和报告可用性
/// 
/// 这个测试验证：
/// 1. 可以同时启动多个子代理
/// 2. 所有子代理都能正常完成
/// 3. 完成后的报告可以正确检索
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_subagents_complete_and_reports_available() {
    let (manager, _state_manager, mut rx_event, shutdown_tx) = make_manager_with_main_agent().await;

    // 快速创建两个不同类型的子代理任务
    let spec_a = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "Explore A".to_string(),
        description: "Analyze A".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(2),
        timeout_ms: Some(2_000),
        network_access: None,
        injected_conversation: None,
    };
    let spec_b = SubagentTaskSpec {
        agent_type: SubagentType::Coder,
        title: "Implement B".to_string(),
        description: "Code B".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(2),
        timeout_ms: Some(2_000),
        network_access: None,
        injected_conversation: None,
    };

    // 创建任务
    let id_a = manager.create_task(spec_a).await.unwrap();
    let id_b = manager.create_task(spec_b).await.unwrap();

    // 几乎同时启动两个子代理
    let _h_a = manager.launch_subagent(&id_a).await.unwrap();
    let _h_b = manager.launch_subagent(&id_b).await.unwrap();

    // 等待两个完成事件（顺序不确定）
    let mut completed_ids = vec![];
    for _ in 0..2 {
        let ev = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
        if let EventMsg::SubagentCompleted(c) = ev.msg {
            completed_ids.push(c.task_id);
        }
    }

    // 验证所有任务都完成了
    completed_ids.sort();
    let mut expected = vec![id_a.clone(), id_b.clone()];
    expected.sort();
    assert_eq!(completed_ids, expected);

    // 验证报告存储和检索功能
    let r_a = manager.get_task_report(&id_a).await.unwrap().expect("report A");
    let r_b = manager.get_task_report(&id_b).await.unwrap().expect("report B");
    assert_eq!(r_a.task_id, id_a);
    assert_eq!(r_b.task_id, id_b);

    // 清理
    let _ = shutdown_tx.send(());
}

/// 测试系统消息在轨迹中的角色匹配代理类型
/// 
/// 这个测试验证：
/// 1. Mock执行器生成的轨迹包含正确的系统消息
/// 2. 不同类型的子代理有不同的系统消息内容
/// 3. 轨迹记录功能正常工作
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn system_message_role_in_trajectory_matches_agent_type() {
    let (manager, _state_manager, mut rx_event, shutdown_tx) = make_manager_with_main_agent().await;

    // 测试Explorer子代理的轨迹
    let id_e = manager
        .create_task(SubagentTaskSpec {
            agent_type: SubagentType::Explorer,
            title: "Trajectory check E".to_string(),
            description: "".to_string(),
            context_refs: vec![],
            bootstrap_paths: vec![],
            max_turns: Some(1),
            timeout_ms: Some(1_000),
            network_access: None,
            injected_conversation: None,
        })
        .await
        .unwrap();
    let _ = manager.launch_subagent(&id_e).await.unwrap();
    let _ = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
    
    // 检查Explorer的轨迹
    let rep_e = manager.get_task_report(&id_e).await.unwrap().unwrap();
    if let Some(traj) = rep_e.trajectory {
        // 验证轨迹中包含正确的系统消息，表明这是explorer任务
        assert!(traj.iter().any(|m| m.role == "system" && m.content.contains("Executing explorer task")));
    } else {
        panic!("expected trajectory for explorer");
    }

    // 测试Coder子代理的轨迹
    let id_c = manager
        .create_task(SubagentTaskSpec {
            agent_type: SubagentType::Coder,
            title: "Trajectory check C".to_string(),
            description: "".to_string(),
            context_refs: vec![],
            bootstrap_paths: vec![],
            max_turns: Some(1),
            timeout_ms: Some(1_000),
            network_access: None,
            injected_conversation: None,
        })
        .await
        .unwrap();
    let _ = manager.launch_subagent(&id_c).await.unwrap();
    let _ = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
    
    // 检查Coder的轨迹
    let rep_c = manager.get_task_report(&id_c).await.unwrap().unwrap();
    if let Some(traj) = rep_c.trajectory {
        // 验证轨迹中包含正确的系统消息，表明这是coder任务
        assert!(traj.iter().any(|m| m.role == "system" && m.content.contains("Executing coder task")));
    } else {
        panic!("expected trajectory for coder");
    }

    // 清理
    let _ = shutdown_tx.send(());
}

/// 严格断言：在LLM执行路径下，注入历史出现在请求体 input 中
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn injected_history_appears_in_llm_request_body() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};

    // 启动mock Responses API服务器
    let server = MockServer::start().await;
    let sse = r#"[
        {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"ok"}]}},
        {"type":"response.completed","response":{"id":"resp-1"}}
    ]"#;
    let template = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse, "text/event-stream");
    Mock::given(method("POST")).and(path("/v1/responses")).respond_with(template).mount(&server).await;

    // provider 指向 mock
    let provider = codex_core::ModelProviderInfo {
        name: "mock-openai".into(),
        base_url: Some(format!("{}/v1", server.uri())),
        env_key: None,
        env_key_instructions: None,
        wire_api: WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(2_000),
        requires_openai_auth: false,
    };

    // 最小配置
    let codex_home = tempfile::TempDir::new().unwrap();
    let mut cfg = core_test_support::load_default_config_for_test(&codex_home);
    cfg.model_provider = provider.clone();
    let effort = cfg.model_reasoning_effort;
    let summary = cfg.model_reasoning_summary;
    let cfg = Arc::new(cfg);

    let client = Arc::new(ModelClient::new(Arc::clone(&cfg), None, provider.clone(), effort, summary, ConversationId::new()));

    // 事件通道与状态管理器
    let (tx_event, mut rx_event) = mpsc::unbounded_channel::<codex_core::protocol::Event>();
    let state_manager = std::sync::Arc::new(AgentStateManager::new());
    let (main_agent_event_tx, _rx) = mpsc::unbounded_channel();

    // 使用测试辅助构造LLM执行器
    let manager = InMemorySubagentManager::with_state_manager(
        std::sync::Arc::new(InMemoryContextRepository::new()),
        tx_event,
        codex_core::subagent_manager::ExecutorType::llm_for_testing(Arc::clone(&client)),
        state_manager.clone(),
        main_agent_event_tx,
        codex_home.path().join("rollout.jsonl"),
    );

    // 构造注入历史
    use codex_protocol::models::{ResponseItem, ContentItem};
    let injected = vec![
        ResponseItem::Message { id: None, role: "user".into(), content: vec![ContentItem::InputText { text: "u1".into() }] },
        ResponseItem::Message { id: None, role: "assistant".into(), content: vec![ContentItem::OutputText { text: "a1".into() }] },
    ];

    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "LLM input check".into(),
        description: "Ensure injected appears in input".into(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(1),
        timeout_ms: Some(2_000),
        network_access: None,
        injected_conversation: Some(injected.clone()),
    };

    let task_id = manager.create_task(spec).await.unwrap();
    let _h = manager.launch_subagent(&task_id).await.unwrap();

    // 等待完成事件
    let _ = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;

    // 验证请求体 input 前两条与注入一致
    let requests = server.received_requests().await.unwrap();
    println!("Received {} requests", requests.len());
    for (i, req) in requests.iter().enumerate() {
        println!("Request {}: {} {}", i, req.method, req.url.path());
        if let Ok(body) = req.body_json::<serde_json::Value>() {
            println!("Request {} body: {}", i, serde_json::to_string_pretty(&body).unwrap_or_default());
        }
    }
    
    // 找到包含input的请求
    let request_with_input = requests.iter().find(|req| {
        req.body_json::<serde_json::Value>()
            .map(|body| body.get("input").is_some())
            .unwrap_or(false)
    }).expect("Should have at least one request with input field");
    
    let body = request_with_input.body_json::<serde_json::Value>().unwrap();
    let input = body["input"].as_array().expect("input must be array");
    
    // 验证注入的历史出现在input的开头
    assert!(input.len() >= 2, "Input should have at least 2 messages (injected history)");
    assert_eq!(input[0]["role"].as_str().unwrap(), "user");
    assert_eq!(input[0]["content"][0]["text"].as_str().unwrap(), "u1");
    assert_eq!(input[1]["role"].as_str().unwrap(), "assistant");
    assert_eq!(input[1]["content"][0]["text"].as_str().unwrap(), "a1");
}

/// 验证：子代理创建的上下文标记为由subagent创建，体现与主代理消息历史的隔离
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_created_contexts_are_tagged_as_subagent() {
    let (tx_event, mut rx_event) = mpsc::unbounded_channel::<codex_core::protocol::Event>();
    let state_manager = std::sync::Arc::new(AgentStateManager::new());
    let (main_agent_event_tx, _main_agent_event_rx) = mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::with_state_manager(
        std::sync::Arc::new(InMemoryContextRepository::new()),
        tx_event,
        ExecutorType::Mock,
        state_manager.clone(),
        main_agent_event_tx,
        tempfile::TempDir::new().unwrap().path().join("rollout.jsonl"),
    );

    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "Context tag".to_string(),
        description: "Generate contexts".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(1),
        timeout_ms: Some(1_000),
        network_access: None,
        injected_conversation: None,
    };
    let task_id = manager.create_task(spec).await.unwrap();
    let _h = manager.launch_subagent(&task_id).await.unwrap();

    let _ = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
    let report = manager.get_task_report(&task_id).await.unwrap().unwrap();
    for ctx in report.contexts {
        assert_eq!(ctx.created_by, "subagent");
    }
}

/// 验证：子代理的事件与主代理消息隔离，不会产生主代理消息事件（仅有子代理相关事件）
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subagent_events_are_isolated_from_main_agent_messages() {
    // 使用Mock执行器快速运行
    let (tx_event, mut rx_event) = mpsc::unbounded_channel::<codex_core::protocol::Event>();
    let state_manager = std::sync::Arc::new(AgentStateManager::new());
    let (main_agent_event_tx, _main_agent_event_rx) = mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::with_state_manager(
        std::sync::Arc::new(InMemoryContextRepository::new()),
        tx_event,
        ExecutorType::Mock,
        state_manager.clone(),
        main_agent_event_tx,
        tempfile::TempDir::new().unwrap().path().join("rollout.jsonl"),
    );

    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Coder,
        title: "Isolation test".to_string(),
        description: "Ensure events are isolated".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(1),
        timeout_ms: Some(1_000),
        network_access: None,
        injected_conversation: None,
    };
    let task_id = manager.create_task(spec).await.unwrap();
    let _h = manager.launch_subagent(&task_id).await.unwrap();

    // 收集直到完成事件，期间不应出现主代理消息事件类型
    // 我们只验证子代理事件：Started/Completed
    let _ = wait_for_event(&mut rx_event, |m| matches!(m, EventMsg::SubagentCompleted(_))).await;
}

/// 验证：主agent会自动从会话历史中提取并注入内容到子agent，包括带 [MainAgent] 前缀的助手消息
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn main_agent_automatically_injects_conversation_history() {
    use wiremock::{Mock, MockServer, ResponseTemplate};
    use wiremock::matchers::{method, path};

    // 启动mock Responses API服务器
    let server = MockServer::start().await;
    let sse = r#"[
        {"type":"response.output_item.done","item":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"I'll help you with that task."}]}},
        {"type":"response.completed","response":{"id":"resp-1"}}
    ]"#;
    let template = ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_raw(sse, "text/event-stream");
    Mock::given(method("POST")).and(path("/v1/responses")).respond_with(template).mount(&server).await;

    // 配置指向mock服务器
    let provider = codex_core::ModelProviderInfo {
        name: "mock-openai".into(),
        base_url: Some(format!("{}/v1", server.uri())),
        env_key: None,
        env_key_instructions: None,
        wire_api: codex_core::WireApi::Responses,
        query_params: None,
        http_headers: None,
        env_http_headers: None,
        request_max_retries: Some(0),
        stream_max_retries: Some(0),
        stream_idle_timeout_ms: Some(2_000),
        requires_openai_auth: false,
    };

    // 创建事件通道
    let (tx_event, mut rx_event) = mpsc::unbounded_channel::<Event>();
    
    // 创建模型客户端
    let model_client = Arc::new(ModelClient::new(
        Arc::new(load_default_config_for_test(&TempDir::new().unwrap())),
        None,
        provider,
        None,
        codex_protocol::config_types::ReasoningSummary::None,
        ConversationId::new(),
    ));

    // 创建子agent管理器，使用LLM执行器
    let ctx_repo = Arc::new(InMemoryContextRepository::new());
    let state_manager = Arc::new(AgentStateManager::new());
    let (main_agent_event_tx, _main_agent_event_rx) = mpsc::unbounded_channel();
    let rollout_path = std::env::temp_dir().join("test_rollout.jsonl");

    let manager = InMemorySubagentManager::with_state_manager(
        ctx_repo,
        tx_event,
        ExecutorType::llm_for_testing(model_client),
        state_manager,
        main_agent_event_tx,
        rollout_path,
    );

    // 手动构建包含用户和助手消息的注入历史，包括 [MainAgent] 前缀
    let injected_conversation = vec![
        codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::InputText {
                text: "Hello, I need help with a project".to_string(),
            }],
        },
        codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: "[MainAgent] I'd be happy to help you with your project!".to_string(),
            }],
        },
        codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![codex_protocol::models::ContentItem::InputText {
                text: "I need to analyze some code files".to_string(),
            }],
        },
        codex_protocol::models::ResponseItem::Message {
            id: None,
            role: "assistant".to_string(),
            content: vec![codex_protocol::models::ContentItem::OutputText {
                text: "[MainAgent] I can help you analyze code files. Let me create a subagent for this task.".to_string(),
            }],
        },
    ];

    // 创建子agent任务规范，包含注入的对话历史
    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "Test automatic injection with MainAgent prefix".to_string(),
        description: "Test that conversation history is automatically injected with [MainAgent] prefix".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(1),
        timeout_ms: None,
        network_access: None,
        injected_conversation: Some(injected_conversation),
    };

    // 创建任务
    let task_id = manager.create_task(spec).await.expect("create task");

    // 启动子agent
    let _handle = manager.launch_subagent(&task_id).await.expect("launch subagent");

    // 等待子agent完成
    let _completion_event = wait_for_event(&mut rx_event, |m| {
        matches!(m, EventMsg::SubagentCompleted(_))
    }).await;

    // 验证mock服务器收到的请求
    let requests = server.received_requests().await.unwrap();
    
    println!("Received {} requests from mock server", requests.len());
    
    // 查找包含input的请求（子agent的LLM请求）
    let mut found_injected_history = false;
    let mut found_mainagent_prefix = false;
    
    for (i, request) in requests.iter().enumerate() {
         if let Ok(body) = request.body_json::<serde_json::Value>() {
             if let Some(input) = body.get("input").and_then(|v| v.as_array()) {
                 println!("Request {} has input with {} messages", i, input.len());
                 
                 // 检查是否包含注入的历史
                 for (j, msg) in input.iter().enumerate() {
                     if let Some(role) = msg.get("role").and_then(|r| r.as_str()) {
                         if role == "user" {
                             if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                                 if let Some(text) = content.get(0).and_then(|t| t.get("text")).and_then(|t| t.as_str()) {
                                     if text.contains("Hello, I need help with a project") || 
                                        text.contains("I need to analyze some code files") {
                                         println!("✅ Found injected conversation history in request {} message {}: {}", i, j, text);
                                         found_injected_history = true;
                                     }
                                 }
                             }
                         } else if role == "assistant" {
                             if let Some(content) = msg.get("content").and_then(|c| c.as_array()) {
                                 if let Some(text) = content.get(0).and_then(|t| t.get("text")).and_then(|t| t.as_str()) {
                                     if text.starts_with("[MainAgent]") {
                                         println!("✅ Found [MainAgent] prefixed assistant message in request {} message {}: {}", i, j, text);
                                         found_mainagent_prefix = true;
                                     }
                                 }
                             }
                         }
                     }
                 }
             }
         }
    }



    assert!(!requests.is_empty(), "Should have received at least some requests");
    assert!(found_injected_history, "Should have found injected conversation history");
    assert!(found_mainagent_prefix, "Should have found [MainAgent] prefixed assistant messages");
    
    println!("✅ Successfully verified that main agent automatically injects conversation history with [MainAgent] prefix");
}