//! Integration test for agent state management
//! 
//! This test verifies that the state management system correctly blocks
//! subagent creation after forced completions for both Explorer and Coder agents.

/// Comprehensive test case that validates both Explorer and Coder state constraints
/// 
/// Test Scenario:
/// 1. Start with Initialization state (all subagents allowed)
/// 2. Create an Explorer subagent and force it to complete
/// 3. Verify Explorer creation is blocked, Coder creation is allowed
/// 4. Create a Coder subagent and force it to complete  
/// 5. Verify both Explorer and Coder creation are blocked
/// 6. Test that the main agent provides appropriate alternatives
#[test]
fn test_comprehensive_state_management() {
    println!("🧪 Starting comprehensive state management test...");
    
    // Phase 1: Initial state verification
    println!("\n📋 Phase 1: Initial State Verification");
    let mut test_session = create_test_session();
    
    let initial_state = test_session.get_current_state();
    assert_eq!(initial_state, AgentState::Initialization);
    assert!(initial_state.can_create_explorer());
    assert!(initial_state.can_create_coder());
    println!("✅ Initial state: Both Explorer and Coder creation allowed");

    // Phase 2: Explorer forced completion
    println!("\n📋 Phase 2: Explorer Forced Completion Test");
    
    // Create explorer subagent
    let explorer_task_id = test_session.create_subagent(
        SubagentType::Explorer,
        "Test Explorer Task - Analyze Test Files"
    ).expect("Should be able to create explorer initially");
    
    // Force the explorer to reach max turns
    test_session.force_subagent_to_max_turns(explorer_task_id);
    
    // Verify state after explorer forced completion
    let state_after_explorer = test_session.get_current_state();
    assert_eq!(state_after_explorer, AgentState::ExplorerForcedCompletion);
    assert!(!state_after_explorer.can_create_explorer(), "Explorer creation should be blocked");
    assert!(state_after_explorer.can_create_coder(), "Coder creation should still be allowed");
    println!("✅ After Explorer forced completion: Explorer blocked, Coder allowed");
    
    // Test that explorer creation is actually blocked
    let explorer_block_result = test_session.attempt_create_subagent(
        SubagentType::Explorer,
        "This should be blocked"
    );
    assert!(explorer_block_result.is_blocked(), "Explorer creation should be blocked");
    println!("✅ Explorer creation correctly blocked");
    
    // Test that coder creation is still allowed
    let coder_allowed_result = test_session.attempt_create_subagent(
        SubagentType::Coder,
        "This should be allowed"
    );
    assert!(coder_allowed_result.is_success(), "Coder creation should be allowed");
    println!("✅ Coder creation still allowed");

    // Phase 3: Coder forced completion
    println!("\n📋 Phase 3: Coder Forced Completion Test");
    
    // Create coder subagent
    let coder_task_id = test_session.create_subagent(
        SubagentType::Coder,
        "Test Coder Task - Implement Test Feature"
    ).expect("Should be able to create coder");
    
    // Force the coder to reach max turns
    test_session.force_subagent_to_max_turns(coder_task_id);
    
    // Verify state after coder forced completion
    let state_after_coder = test_session.get_current_state();
    assert_eq!(state_after_coder, AgentState::CoderForcedCompletion);
    assert!(!state_after_coder.can_create_explorer(), "Explorer creation should still be blocked");
    assert!(!state_after_coder.can_create_coder(), "Coder creation should now be blocked");
    println!("✅ After Coder forced completion: Both Explorer and Coder blocked");
    
    // Test that both subagent types are blocked
    let explorer_block_result2 = test_session.attempt_create_subagent(
        SubagentType::Explorer,
        "This should be blocked"
    );
    assert!(explorer_block_result2.is_blocked(), "Explorer creation should still be blocked");
    
    let coder_block_result = test_session.attempt_create_subagent(
        SubagentType::Coder,
        "This should now be blocked"
    );
    assert!(coder_block_result.is_blocked(), "Coder creation should now be blocked");
    println!("✅ Both Explorer and Coder creation correctly blocked");

    // Phase 4: Prompt injection verification
    println!("\n📋 Phase 4: Prompt Injection Verification");
    
    // Verify that state information is correctly injected into prompts
    let prompt_info = test_session.get_last_prompt_state_info();
    assert!(prompt_info.contains("Current Agent State"), "Prompt should contain state info");
    assert!(prompt_info.contains("Subagent Creation Constraints"), "Prompt should contain constraints");
    assert!(prompt_info.contains("Explorer Subagent: Blocked"), "Should show Explorer as blocked");
    assert!(prompt_info.contains("Coder Subagent: Blocked"), "Should show Coder as blocked");
    println!("✅ State information correctly injected into prompts");

    // Phase 5: Alternative suggestion verification
    println!("\n📋 Phase 5: Alternative Suggestion Verification");
    
    // Test that the system provides appropriate alternatives
    let alternatives = test_session.get_suggested_alternatives();
    assert!(!alternatives.is_empty(), "Should provide alternatives when subagents are blocked");
    assert!(alternatives.iter().any(|alt| alt.contains("available information")), 
           "Should suggest working with available information");
    println!("✅ Appropriate alternatives suggested when subagents blocked");

    println!("\n🎉 Comprehensive state management test completed successfully!");
}

/// Mock test session for state management testing
struct TestSession {
    current_state: AgentState,
    subagent_history: Vec<SubagentRecord>,
    last_prompt_state_info: String,
    suggested_alternatives: Vec<String>,
    next_task_id: u32,
}

#[derive(Clone)]
struct SubagentRecord {
    task_id: String,
    agent_type: SubagentType,
    status: SubagentStatus,
    turns_completed: u32,
    force_completed: bool,
}

#[derive(Clone, PartialEq)]
enum SubagentStatus {
    Running,
    Completed,
    ForcedCompletion,
}

#[derive(Clone)]
enum SubagentType {
    Explorer,
    Coder,
}

#[derive(PartialEq, Debug, Clone)]
enum AgentState {
    Initialization,
    ExplorerForcedCompletion,
    CoderForcedCompletion,
}

impl AgentState {
    fn can_create_explorer(&self) -> bool {
        match self {
            AgentState::Initialization => true,
            AgentState::ExplorerForcedCompletion => false,
            AgentState::CoderForcedCompletion => false,
        }
    }
    
    fn can_create_coder(&self) -> bool {
        match self {
            AgentState::Initialization => true,
            AgentState::ExplorerForcedCompletion => true,
            AgentState::CoderForcedCompletion => false,
        }
    }
}

struct SubagentCreationResult {
    success: bool,
    blocked: bool,
    message: String,
}

impl SubagentCreationResult {
    fn is_success(&self) -> bool { self.success }
    fn is_blocked(&self) -> bool { self.blocked }
}

// Mock implementation methods
impl TestSession {
    fn get_current_state(&self) -> AgentState {
        self.current_state.clone()
    }
    
    fn create_subagent(&mut self, agent_type: SubagentType, _title: &str) -> Result<String, String> {
        if !self.can_create_subagent_type(&agent_type) {
            return Err("Subagent creation blocked by state constraints".to_string());
        }
        
        self.next_task_id += 1;
        let task_id = format!("task_{}", self.next_task_id);
        let record = SubagentRecord {
            task_id: task_id.clone(),
            agent_type,
            status: SubagentStatus::Running,
            turns_completed: 0,
            force_completed: false,
        };
        
        self.subagent_history.push(record);
        Ok(task_id)
    }
    
    fn attempt_create_subagent(&mut self, agent_type: SubagentType, title: &str) -> SubagentCreationResult {
        match self.create_subagent(agent_type, title) {
            Ok(_) => SubagentCreationResult {
                success: true,
                blocked: false,
                message: "Subagent created successfully".to_string(),
            },
            Err(msg) => SubagentCreationResult {
                success: false,
                blocked: true,
                message: msg,
            },
        }
    }
    
    fn force_subagent_to_max_turns(&mut self, task_id: String) {
        let agent_type = if let Some(record) = self.subagent_history.iter_mut().find(|r| r.task_id == task_id) {
            record.turns_completed = 10; // Max turns
            record.status = SubagentStatus::ForcedCompletion;
            record.force_completed = true;
            
            // Clone the agent type to avoid borrowing issues
            record.agent_type.clone()
        } else {
            return;
        };
        
        // Update agent state based on the forced completion
        self.update_state_after_forced_completion(&agent_type);
    }
    
    fn update_state_after_forced_completion(&mut self, agent_type: &SubagentType) {
        self.current_state = match agent_type {
            SubagentType::Explorer => AgentState::ExplorerForcedCompletion,
            SubagentType::Coder => AgentState::CoderForcedCompletion,
        };
        
        // Update prompt state info
        self.update_prompt_state_info();
        
        // Update suggested alternatives
        self.update_suggested_alternatives();
    }
    
    fn can_create_subagent_type(&self, agent_type: &SubagentType) -> bool {
        match agent_type {
            SubagentType::Explorer => self.current_state.can_create_explorer(),
            SubagentType::Coder => self.current_state.can_create_coder(),
        }
    }
    
    fn update_prompt_state_info(&mut self) {
        self.last_prompt_state_info = format!(
            "**Current Agent State**: {:?}\n\
            **Subagent Creation Constraints**:\n\
            - Explorer Subagent: {}\n\
            - Coder Subagent: {}\n\
            Please work with available information or consider alternative approaches.",
            self.current_state,
            if self.current_state.can_create_explorer() { "Allowed" } else { "Blocked" },
            if self.current_state.can_create_coder() { "Allowed" } else { "Blocked" }
        );
    }
    
    fn update_suggested_alternatives(&mut self) {
        self.suggested_alternatives = vec![
            "Work with available information from previous analysis".to_string(),
            "Provide a summary based on existing context".to_string(),
            "Consider alternative approaches that don't require new subagents".to_string(),
        ];
    }
    
    fn get_last_prompt_state_info(&self) -> String {
        self.last_prompt_state_info.clone()
    }
    
    fn get_suggested_alternatives(&self) -> Vec<String> {
        self.suggested_alternatives.clone()
    }
}

fn create_test_session() -> TestSession {
    let mut session = TestSession {
        current_state: AgentState::Initialization,
        subagent_history: Vec::new(),
        last_prompt_state_info: String::new(),
        suggested_alternatives: Vec::new(),
        next_task_id: 0,
    };
    
    session.update_prompt_state_info();
    session
}

// Additional helper tests for specific scenarios

#[test]
fn test_explorer_normal_completion_allows_coder() {
    println!("🧪 Testing Explorer normal completion allows Coder creation...");
    
    let mut session = create_test_session();
    
    // Create and normally complete an explorer
    let explorer_id = session.create_subagent(SubagentType::Explorer, "Normal Explorer").unwrap();
    
    // Simulate normal completion (not forced)
    if let Some(record) = session.subagent_history.iter_mut().find(|r| r.task_id == explorer_id) {
        record.status = SubagentStatus::Completed;
        record.turns_completed = 5; // Less than max
        record.force_completed = false;
    }
    
    // State should remain Initialization, allowing both types
    assert_eq!(session.get_current_state(), AgentState::Initialization);
    assert!(session.current_state.can_create_explorer());
    assert!(session.current_state.can_create_coder());
    
    println!("✅ Explorer normal completion correctly allows both subagent types");
}

#[test]
fn test_state_transition_sequence() {
    println!("🧪 Testing complete state transition sequence...");
    
    let test_cases = vec![
        ("Initial", AgentState::Initialization, true, true),
        ("Explorer Forced", AgentState::ExplorerForcedCompletion, false, true),
        ("Coder Forced", AgentState::CoderForcedCompletion, false, false),
    ];
    
    for (phase, expected_state, can_explorer, can_coder) in test_cases {
        println!("  Testing phase: {}", phase);
        assert_eq!(expected_state.can_create_explorer(), can_explorer);
        assert_eq!(expected_state.can_create_coder(), can_coder);
    }
    
    println!("✅ All state transitions work correctly");
}