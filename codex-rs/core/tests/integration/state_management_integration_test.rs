//! Integration test for agent state management
//! 
//! This test verifies that the unified state management system correctly handles
//! subagent creation constraints based on current state.

use codex_core::AgentState;

/// Test case that validates unified state management
/// 
/// Test Scenario:
/// 1. Start with Idle state (all subagents allowed)
/// 2. Transition to WaitingForSubagent (no new subagents allowed)
/// 3. Transition to DecidingNextStep (all subagents allowed again)
/// 4. Test Summarizing state (no new subagents allowed)
#[test]
fn test_unified_state_management() {
    println!("🧪 Starting unified state management test...");
    
    // Phase 1: Idle state verification
    println!("\n📋 Phase 1: Idle State Verification");
    let idle_state = AgentState::Idle;
    assert!(idle_state.can_create_explorer());
    assert!(idle_state.can_create_coder());
    assert!(!idle_state.is_busy());
    println!("✅ Idle state: Both Explorer and Coder creation allowed");

    // Phase 2: DecidingNextStep state verification
    println!("\n📋 Phase 2: DecidingNextStep State Verification");
    let deciding_state = AgentState::DecidingNextStep;
    assert!(deciding_state.can_create_explorer());
    assert!(deciding_state.can_create_coder());
    assert!(deciding_state.can_summarize());
    assert!(deciding_state.is_busy());
    println!("✅ DecidingNextStep state: Both Explorer and Coder creation allowed");

    // Phase 3: WaitingForSubagent state verification
    println!("\n📋 Phase 3: WaitingForSubagent State Verification");
    let waiting_state = AgentState::WaitingForSubagent { 
        subagent_id: "test_subagent_123".to_string() 
    };
    assert!(!waiting_state.can_create_explorer());
    assert!(!waiting_state.can_create_coder());
    assert!(!waiting_state.can_summarize());
    assert!(waiting_state.is_busy());
    println!("✅ WaitingForSubagent state: Both Explorer and Coder creation blocked");

    // Phase 4: Summarizing state verification
    println!("\n📋 Phase 4: Summarizing State Verification");
    let summarizing_state = AgentState::Summarizing;
    assert!(!summarizing_state.can_create_explorer());
    assert!(!summarizing_state.can_create_coder());
    assert!(!summarizing_state.can_summarize());
    assert!(summarizing_state.is_busy());
    println!("✅ Summarizing state: Both Explorer and Coder creation blocked");

    println!("\n🎉 Unified state management test completed successfully!");
}

#[test]
fn test_state_descriptions() {
    println!("🧪 Testing state descriptions...");
    
    let idle_state = AgentState::Idle;
    assert_eq!(idle_state.description(), "Idle - The agent is waiting for new user input");
    
    let deciding_state = AgentState::DecidingNextStep;
    assert_eq!(deciding_state.description(), "Deciding Next Step - The agent is actively deciding the next action");
    
    let waiting_state = AgentState::WaitingForSubagent { 
        subagent_id: "test_123".to_string() 
    };
    assert_eq!(waiting_state.description(), "Waiting for Subagent - The agent has dispatched a sub-agent and is waiting for completion");
    
    let summarizing_state = AgentState::Summarizing;
    assert_eq!(summarizing_state.description(), "Summarizing - The agent is compiling the final result");
    
    println!("✅ All state descriptions are correct");
}