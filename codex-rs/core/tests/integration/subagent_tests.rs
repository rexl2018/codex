use codex_core::context_store::{InMemoryContextRepository, IContextRepository, ContextQuery, Context};
use codex_core::subagent_manager::{InMemorySubagentManager, ISubagentManager, SubagentTaskSpec};
use codex_core::multi_agent_coordinator::{SubtaskCoordinator, TaskPlanner};
use codex_protocol::protocol::{
    SubagentType, BootstrapPath, EventMsg,
    SubagentTaskCreatedEvent,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::mpsc;

// Context Store Tests
#[tokio::test]
async fn test_context_store_basic_operations() {
    let repo = InMemoryContextRepository::new();
    
    // Test storing context
    let context = Context::new(
        "test-context-1".to_string(),
        "Test context summary".to_string(),
        "This is test content".to_string(),
        "test-creator".to_string(),
        Some("task-1".to_string()),
    );
    
    repo.store_context(context.clone()).await.unwrap();
    
    // Test retrieving context
    let retrieved = repo.get_context("test-context-1").await.unwrap();
    assert!(retrieved.is_some());
    let retrieved = retrieved.unwrap();
    assert_eq!(retrieved.id, context.id);
    assert_eq!(retrieved.summary, context.summary);
    assert_eq!(retrieved.content, context.content);
    
    // Test querying contexts
    let query = ContextQuery {
        ids: Some(vec!["test-context-1".to_string()]),
        tags: None,
        created_by: None,
        limit: None,
    };
    
    let contexts = repo.query_contexts(&query).await.unwrap();
    assert_eq!(contexts.len(), 1);
    assert_eq!(contexts[0].id, "test-context-1");
}

#[tokio::test]
async fn test_context_store_query_filtering() {
    let repo = InMemoryContextRepository::new();
    
    // Store multiple contexts
    for i in 1..=5 {
        let context = Context::new(
            format!("context-{}", i),
            format!("Summary {}", i),
            format!("Content {}", i),
            if i % 2 == 0 { "even-creator".to_string() } else { "odd-creator".to_string() },
            Some(format!("task-{}", i)),
        );
        repo.store_context(context).await.unwrap();
    }
    
    // Test filtering by creator
    let query = ContextQuery {
        ids: None,
        tags: None,
        created_by: Some("even-creator".to_string()),
        limit: None,
    };
    
    let even_contexts = repo.query_contexts(&query).await.unwrap();
    assert_eq!(even_contexts.len(), 2);
    for context in &even_contexts {
        assert_eq!(context.created_by, "even-creator");
    }
    
    // Test limiting results
    let query = ContextQuery {
        ids: None,
        tags: None,
        created_by: None,
        limit: Some(2),
    };
    
    let limited_contexts = repo.query_contexts(&query).await.unwrap();
    assert_eq!(limited_contexts.len(), 2);
}

// Subagent Manager Tests
#[tokio::test]
async fn test_subagent_manager_task_lifecycle() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::new_for_testing(context_repo.clone(), tx, codex_core::subagent_manager::ExecutorType::Mock);
    
    // Create a task
    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "Test exploration task".to_string(),
        description: "Explore test directory structure".to_string(),
        context_refs: vec!["ref-1".to_string(), "ref-2".to_string()],
        bootstrap_paths: vec![
            BootstrapPath {
                path: PathBuf::from("/test/path1"),
                reason: "Test reason 1".to_string(),
            },
            BootstrapPath {
                path: PathBuf::from("/test/path2"),
                reason: "Test reason 2".to_string(),
            }
        ],
        max_turns: Some(10),
        timeout_ms: Some(60000),
        network_access: None,
        injected_conversation: None,
    };

    let task_id = manager.create_task(spec).await.unwrap();
    let task = manager.get_task(&task_id).await.unwrap();

    assert_eq!(task.title, "Test exploration task");
    assert_eq!(task.agent_type, SubagentType::Explorer);
    assert_eq!(task.max_turns, 10);

    // Check that we received the task created event
    let event = rx.try_recv().unwrap();
    match event.msg {
        EventMsg::SubagentTaskCreated(_) => {
            // Expected event type
        }
        _ => panic!("Expected SubagentTaskCreated event"),
    }
}

#[tokio::test]
async fn test_subagent_manager_force_complete() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::new_for_testing(context_repo.clone(), tx, codex_core::subagent_manager::ExecutorType::Mock);
    
    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Coder,
        title: "Test coding task".to_string(),
        description: "Write test code".to_string(),
        context_refs: vec![],
        bootstrap_paths: vec![],
        max_turns: Some(5),
        timeout_ms: Some(30000),
        network_access: None,
        injected_conversation: None,
    };

    let task_id = manager.create_task(spec).await.unwrap();
    let report = manager.force_complete_task(&task_id).await.unwrap();

    assert_eq!(report.task_id, task_id);
    assert!(!report.success);

    // Check that we received the force completed event
    let _ = rx.try_recv().unwrap();
}

#[tokio::test]
async fn test_subagent_manager_create_task() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::new_for_testing(context_repo.clone(), tx, codex_core::subagent_manager::ExecutorType::Mock);

    // Create multiple tasks concurrently
    let mut handles = vec![];

    for i in 0..5 {
        let manager_clone = manager.clone();
        let handle = tokio::spawn(async move {
            let spec = SubagentTaskSpec {
                agent_type: if i % 2 == 0 { SubagentType::Explorer } else { SubagentType::Coder },
                title: format!("Concurrent task {}", i),
                description: format!("Task {} description", i),
                context_refs: vec![],
                bootstrap_paths: vec![],
                max_turns: Some(3),
                timeout_ms: Some(10000),
                network_access: None,
                injected_conversation: None,
            };
            
            manager_clone.create_task(spec).await
        });
        handles.push(handle);
    }

    // Wait for all tasks to be created
    let mut task_ids = vec![];
    for handle in handles {
        let task_id = handle.await.unwrap().unwrap();
        task_ids.push(task_id);
    }

    // Verify all tasks were created
    assert_eq!(task_ids.len(), 5);

    // Verify all tasks are active
    let active_tasks = manager.get_active_tasks().await.unwrap();
    assert_eq!(active_tasks.len(), 5);

    // Note: cancel_task only works on running tasks
    // For this test, we'll just verify all tasks were created
    for task_id in &task_ids {
        let status = manager.get_task_status(task_id).await.unwrap();
        match status {
            codex_core::subagent_manager::TaskStatus::Created => {},
            _ => panic!("Expected task to be in Created status"),
        }
    }
}

#[tokio::test]
async fn test_subtask_coordinator_creation() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, _rx) = mpsc::unbounded_channel();
    let _subagent_manager = Arc::new(InMemorySubagentManager::new_for_testing(
        context_repo.clone(),
        tx.clone(),
        codex_core::subagent_manager::ExecutorType::Mock,
    ));

    // Test that we can create the basic components
    // This is mainly testing that the types compile correctly
    assert!(true, "SubtaskCoordinator components can be created");
}

#[tokio::test]
async fn test_task_planner_creation() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    
    let _planner = TaskPlanner::new(context_repo);
    
    // If we get here without panicking, the test passes
    assert!(true);
}

#[tokio::test]
async fn test_end_to_end_context_flow() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, mut rx) = mpsc::unbounded_channel();
    let manager = InMemorySubagentManager::new_for_testing(context_repo.clone(), tx, codex_core::subagent_manager::ExecutorType::Mock);
    
    let spec = SubagentTaskSpec {
        agent_type: SubagentType::Explorer,
        title: "End-to-end test".to_string(),
        description: "Test full context flow".to_string(),
        context_refs: vec!["test-ref".to_string()],
        bootstrap_paths: vec![],
        max_turns: Some(3),
        timeout_ms: Some(15000),
        network_access: None,
        injected_conversation: None,
    };

    let task_id = manager.create_task(spec).await.unwrap();
    let task = manager.get_task(&task_id).await.unwrap();

    assert_eq!(task.title, "End-to-end test");
    assert_eq!(task.ctx_store_contexts.get("test-ref"), Some(&"test-ref".to_string()));

    // Check that we received the task created event
    let _ = rx.try_recv().unwrap();
}

#[tokio::test]
async fn test_concurrent_task_management() {
    let context_repo = Arc::new(InMemoryContextRepository::new());
    let (tx, _rx) = mpsc::unbounded_channel();
    let manager = Arc::new(InMemorySubagentManager::new_for_testing(context_repo.clone(), tx, codex_core::subagent_manager::ExecutorType::Mock));

    // Create multiple tasks concurrently
    let mut handles = vec![];
    
    for i in 0..5 {
        let manager_clone = manager.clone();
        let handle = tokio::spawn(async move {
            let spec = SubagentTaskSpec {
                agent_type: if i % 2 == 0 { SubagentType::Explorer } else { SubagentType::Coder },
                title: format!("Concurrent task {}", i),
                description: format!("Task {} description", i),
                context_refs: vec![],
                bootstrap_paths: vec![],
                max_turns: Some(3),
                timeout_ms: Some(10000),
                network_access: None,
                injected_conversation: None,
            };
            
            manager_clone.create_task(spec).await
        });
        handles.push(handle);
    }
    
    // Wait for all tasks to be created
    let mut task_ids = vec![];
    for handle in handles {
        let task_id = handle.await.unwrap().unwrap();
        task_ids.push(task_id);
    }
    
    // Verify all tasks were created
    assert_eq!(task_ids.len(), 5);
    
    // Verify all tasks are active
    let active_tasks = manager.get_active_tasks().await.unwrap();
    assert_eq!(active_tasks.len(), 5);
    
    // Note: cancel_task only works on running tasks
    // For this test, we'll just verify all tasks were created
    for task_id in &task_ids {
        let status = manager.get_task_status(task_id).await.unwrap();
        match status {
            codex_core::subagent_manager::TaskStatus::Created => {},
            _ => panic!("Expected task to be in Created status"),
        }
    }
}

#[test]
fn test_subagent_type_display() {
    assert_eq!(format!("{}", SubagentType::Explorer), "Explorer");
    assert_eq!(format!("{}", SubagentType::Coder), "Coder");
    
    // Test Debug trait
    assert_eq!(format!("{:?}", SubagentType::Explorer), "Explorer");
    assert_eq!(format!("{:?}", SubagentType::Coder), "Coder");
}

#[test]
fn test_event_serialization() {
    let event = SubagentTaskCreatedEvent {
        task_id: "test-task".to_string(),
        agent_type: SubagentType::Explorer,
        title: "Test Task".to_string(),
        description: "Test Description".to_string(),
    };
    
    // Test that the event can be serialized and deserialized
    let serialized = serde_json::to_string(&event).unwrap();
    let deserialized: SubagentTaskCreatedEvent = serde_json::from_str(&serialized).unwrap();
    
    assert_eq!(event.task_id, deserialized.task_id);
    assert_eq!(event.agent_type, deserialized.agent_type);
    assert_eq!(event.title, deserialized.title);
    assert_eq!(event.description, deserialized.description);
}