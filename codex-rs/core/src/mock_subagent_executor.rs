use std::path::PathBuf;
use std::time::SystemTime;
use uuid::Uuid;

use crate::context_store::{Context, IContextRepository, InMemoryContextRepository};
use crate::subagent_manager::{SubagentTask, SubagentReport, MessageEntry};
use crate::subagent_system_messages::{get_explorer_system_message, get_coder_system_message};
use codex_protocol::protocol::{ContextItem, SubagentMetadata, SubagentType};

/// Mock subagent executor for testing and demonstration purposes
pub struct MockSubagentExecutor {
    context_repo: std::sync::Arc<InMemoryContextRepository>,
}

impl MockSubagentExecutor {
    pub fn new(context_repo: std::sync::Arc<InMemoryContextRepository>) -> Self {
        Self { context_repo }
    }

    /// Execute a subagent task and return a report
    pub async fn execute_task(&self, task: &SubagentTask) -> SubagentReport {
        tracing::info!(
            "Starting subagent execution: task_id={}, type={:?}, title='{}'",
            task.task_id,
            task.agent_type,
            task.title
        );

        // Simulate some processing time
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        // Create contexts based on the task type
        let contexts = match task.agent_type {
            SubagentType::Explorer => self.execute_explorer_task(task).await,
            SubagentType::Coder => self.execute_coder_task(task).await,
        };

        // Store contexts in the context repository
        tracing::info!(
            "Subagent generated {} context items for task: {}",
            contexts.len(),
            task.task_id
        );
        
        for (i, context) in contexts.iter().enumerate() {
            tracing::info!(
                "Context item {}/{}: id='{}', summary='{}'",
                i + 1,
                contexts.len(),
                context.id,
                context.summary
            );
            
            // Print the full content for debugging
            tracing::info!(
                "Context item {} full content:\n--- START CONTENT ---\n{}\n--- END CONTENT ---",
                context.id,
                context.content
            );
            
            let ctx = Context::new(
                context.id.clone(),
                context.summary.clone(),
                context.content.clone(),
                "subagent".to_string(),
                Some(task.task_id.clone()),
            );
            
            match self.context_repo.store_context(ctx).await {
                Ok(()) => {
                    tracing::info!("Successfully stored context item: {}", context.id);
                }
                Err(e) => {
                    tracing::error!("Failed to store context {}: {}", context.id, e);
                }
            }
        }

        let success = !contexts.is_empty();
        let comments = if success {
            format!("Successfully completed {} task: {}", 
                match task.agent_type {
                    SubagentType::Explorer => "exploration",
                    SubagentType::Coder => "implementation",
                },
                task.title
            )
        } else {
            "Task completed but no contexts were generated".to_string()
        };

        tracing::info!(
            "Subagent execution completed: task_id={}, contexts={}, success={}",
            task.task_id,
            contexts.len(),
            success
        );

        let comments_clone = comments.clone();

        SubagentReport {
            task_id: task.task_id.clone(),
            contexts,
            comments,
            success,
            metadata: SubagentMetadata {
                num_turns: 1,
                max_turns: task.max_turns,
                input_tokens: 500,
                output_tokens: 300,
                duration_ms: 500,
                reached_max_turns: false,
                force_completed: false,
                error_message: None,
            },
            trajectory: Some(vec![
                MessageEntry {
                    role: "system".to_string(),
                    content: format!("Executing {} task: {}", 
                        match task.agent_type {
                            SubagentType::Explorer => "explorer",
                            SubagentType::Coder => "coder",
                        },
                        task.title
                    ),
                    timestamp: SystemTime::now(),
                },
                MessageEntry {
                    role: "assistant".to_string(),
                    content: comments_clone,
                    timestamp: SystemTime::now(),
                },
            ]),
        }
    }

    /// Execute an explorer task - analyze files and create contexts
    async fn execute_explorer_task(&self, task: &SubagentTask) -> Vec<ContextItem> {
        let mut contexts = Vec::new();

        // Build task prompt similar to reference implementation
        let task_prompt = self.build_task_prompt(task);
        
        tracing::info!("Explorer task prompt:\n{}", task_prompt);

        // Analyze bootstrap contexts (files provided to the subagent)
        for bootstrap in &task.bootstrap_contexts {
            let uuid_str = Uuid::new_v4().to_string();
            let context_id = format!("analysis_{}", &uuid_str[..8]);
            
            let analysis = if bootstrap.path.extension().and_then(|s| s.to_str()) == Some("rs") {
                self.analyze_rust_file(&bootstrap.path, &bootstrap.content, &task.description).await
            } else {
                self.analyze_general_file(&bootstrap.path, &bootstrap.content, &task.description).await
            };

            contexts.push(ContextItem {
                id: context_id,
                summary: format!("Analysis of {}", bootstrap.path.display()),
                content: analysis,
            });
        }

        // If no bootstrap contexts, create a general analysis context based on description
        if contexts.is_empty() {
            let uuid_str = Uuid::new_v4().to_string();
            contexts.push(ContextItem {
                id: format!("general_analysis_{}", &uuid_str[..8]),
                summary: "Task-guided analysis".to_string(),
                content: format!("Task: {}\n\nDescription: {}\n\nAnalysis: Based on the task description, this appears to be a {} task focused on {}.", 
                    task.title, 
                    task.description,
                    match task.agent_type {
                        SubagentType::Explorer => "exploration and analysis",
                        SubagentType::Coder => "implementation and coding",
                    },
                    task.title.to_lowercase()
                ),
            });
        }

        contexts
    }

    /// Execute a coder task - implement features and create contexts
    async fn execute_coder_task(&self, task: &SubagentTask) -> Vec<ContextItem> {
        let mut contexts = Vec::new();

        // Create implementation context
        let uuid_str = Uuid::new_v4().to_string();
        let context_id = format!("implementation_{}", &uuid_str[..8]);
        
        let implementation_details = format!(
            "Implementation Plan for: {}\n\n\
            Description: {}\n\n\
            Proposed Changes:\n\
            1. Analyze existing code structure\n\
            2. Implement required functionality\n\
            3. Add appropriate error handling\n\
            4. Update tests if necessary\n\n\
            Note: This is a simulated implementation result.",
            task.title,
            task.description
        );

        contexts.push(ContextItem {
            id: context_id,
            summary: format!("Implementation plan for {}", task.title),
            content: implementation_details,
        });

        contexts
    }

    /// Build task prompt similar to reference implementation
    fn build_task_prompt(&self, task: &SubagentTask) -> String {
        let mut sections = Vec::new();
        
        // Add system message based on agent type
        let system_message = match task.agent_type {
            SubagentType::Explorer => get_explorer_system_message(),
            SubagentType::Coder => get_coder_system_message(),
        };
        sections.push(system_message.to_string());
        
        // Task description
        sections.push(format!("# Task: {}", task.title));
        sections.push(task.description.clone());
        
        // Include bootstrap files/dirs
        if !task.bootstrap_contexts.is_empty() {
            sections.push("## Relevant Files/Directories".to_string());
            for item in &task.bootstrap_contexts {
                sections.push(format!("- {}: {}", item.path.display(), item.reason));
            }
        }
        
        sections.push("Begin your investigation/implementation now.".to_string());
        
        sections.join("\n\n")
    }

    /// Analyze a Rust file
    async fn analyze_rust_file(&self, path: &PathBuf, content: &str, task_description: &str) -> String {
        let mut analysis = format!("Rust File Analysis: {}\n\n", path.display());
        
        // Determine analysis type based on task description
        let is_bug_analysis = task_description.to_lowercase().contains("bug") || 
                             task_description.to_lowercase().contains("issue") ||
                             task_description.to_lowercase().contains("error") ||
                             task_description.to_lowercase().contains("problem");
        
        let is_security_analysis = task_description.to_lowercase().contains("security") ||
                                  task_description.to_lowercase().contains("safety") ||
                                  task_description.to_lowercase().contains("vulnerability");
        
        let is_performance_analysis = task_description.to_lowercase().contains("performance") ||
                                     task_description.to_lowercase().contains("optimization") ||
                                     task_description.to_lowercase().contains("bottleneck");
        
        if is_bug_analysis {
            analysis.push_str("## Bug Analysis Report\n\n");
            analysis.push_str("### Potential Issues Found:\n\n");
            
            // Analyze for common Rust issues
            self.analyze_for_bugs(&mut analysis, content);
            
        } else if is_security_analysis {
            analysis.push_str("## Security Analysis Report\n\n");
            analysis.push_str("### Security Assessment:\n\n");
            
            self.analyze_for_security(&mut analysis, content);
            
        } else if is_performance_analysis {
            analysis.push_str("## Performance Analysis Report\n\n");
            analysis.push_str("### Performance Assessment:\n\n");
            
            self.analyze_for_performance(&mut analysis, content);
            
        } else {
            // Default general analysis
            self.analyze_general_structure(&mut analysis, content);
        }

        analysis
    }
    
    /// Analyze for potential bugs and issues
    fn analyze_for_bugs(&self, analysis: &mut String, content: &str) {
        let mut issues_found = 0;
        
        // Check for unwrap() usage
        if content.contains(".unwrap()") {
            issues_found += 1;
            analysis.push_str("🚨 **Potential Panic Risk**: Found `.unwrap()` calls which can cause panics\n");
            analysis.push_str("   - Consider using `.expect()` with descriptive messages or proper error handling\n\n");
        }
        
        // Check for expect() usage
        if content.contains(".expect(") {
            analysis.push_str("⚠️  **Error Handling**: Found `.expect()` calls\n");
            analysis.push_str("   - Review if these are appropriate or if Result propagation would be better\n\n");
        }
        
        // Check for unsafe blocks
        if content.contains("unsafe ") {
            issues_found += 1;
            analysis.push_str("🔥 **Unsafe Code**: Found `unsafe` blocks\n");
            analysis.push_str("   - Ensure memory safety invariants are maintained\n");
            analysis.push_str("   - Consider if safer alternatives exist\n\n");
        }
        
        // Check for TODO/FIXME comments
        if content.contains("TODO") || content.contains("FIXME") {
            issues_found += 1;
            analysis.push_str("📝 **Incomplete Code**: Found TODO/FIXME comments\n");
            analysis.push_str("   - Review and address pending work items\n\n");
        }
        
        // Check for potential race conditions with Arc/Mutex
        if content.contains("Arc<") && content.contains("Mutex<") {
            analysis.push_str("🔒 **Concurrency**: Using Arc<Mutex<T>> pattern\n");
            analysis.push_str("   - Verify proper lock ordering to avoid deadlocks\n");
            analysis.push_str("   - Consider if RwLock would be more appropriate for read-heavy workloads\n\n");
        }
        
        // Check for clone() usage that might be inefficient
        if content.matches(".clone()").count() > 5 {
            analysis.push_str("📋 **Performance**: High number of `.clone()` calls\n");
            analysis.push_str("   - Review if borrowing or references could be used instead\n\n");
        }
        
        if issues_found == 0 {
            analysis.push_str("✅ **No Critical Issues Found**: The code appears to follow good Rust practices\n");
            analysis.push_str("   - No obvious bugs or anti-patterns detected\n");
            analysis.push_str("   - Error handling appears appropriate\n\n");
        }
        
        analysis.push_str(&format!("### Summary: {issues_found} potential issues identified\n"));
    }
    
    /// Analyze for security issues
    fn analyze_for_security(&self, analysis: &mut String, content: &str) {
        analysis.push_str("🔐 **Memory Safety**: Rust's ownership system provides memory safety by default\n");
        analysis.push_str("🛡️  **Thread Safety**: Send + Sync traits ensure safe concurrent access\n");
        
        if content.contains("unsafe ") {
            analysis.push_str("⚠️  **Unsafe Code**: Manual review required for unsafe blocks\n");
        } else {
            analysis.push_str("✅ **No Unsafe Code**: All code uses safe Rust constructs\n");
        }
    }
    
    /// Analyze for performance issues
    fn analyze_for_performance(&self, analysis: &mut String, content: &str) {
        if content.contains("Vec<") {
            analysis.push_str("📊 **Collections**: Using Vec - consider pre-allocation if size is known\n");
        }
        
        if content.contains("HashMap<") {
            analysis.push_str("🗂️  **Hash Maps**: Using HashMap - good for key-value lookups\n");
        }
        
        if content.contains("async ") {
            analysis.push_str("⚡ **Async Code**: Asynchronous operations for better concurrency\n");
        }
    }
    
    /// General structure analysis (fallback)
    fn analyze_general_structure(&self, analysis: &mut String, content: &str) {
        let lines = content.lines().count();
        let has_tests = content.contains("#[test]") || content.contains("#[cfg(test)]");
        let has_async = content.contains("async ");
        let has_traits = content.contains("trait ");
        let has_structs = content.contains("struct ");
        let has_enums = content.contains("enum ");
        
        analysis.push_str("File Statistics:\n");
        analysis.push_str(&format!("- Lines of code: {lines}\n"));
        analysis.push_str(&format!("- Contains tests: {has_tests}\n"));
        analysis.push_str(&format!("- Uses async: {has_async}\n"));
        analysis.push_str(&format!("- Defines traits: {has_traits}\n"));
        analysis.push_str(&format!("- Defines structs: {has_structs}\n"));
        analysis.push_str(&format!("- Defines enums: {has_enums}\n"));
        
        // Extract key components
        analysis.push_str("\nKey Components:\n");
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("pub struct ") || trimmed.starts_with("struct ") {
                analysis.push_str(&format!("- Struct: {trimmed}\n"));
            } else if trimmed.starts_with("pub enum ") || trimmed.starts_with("enum ") {
                analysis.push_str(&format!("- Enum: {trimmed}\n"));
            } else if trimmed.starts_with("pub trait ") || trimmed.starts_with("trait ") {
                analysis.push_str(&format!("- Trait: {trimmed}\n"));
            } else if trimmed.starts_with("pub fn ") || trimmed.starts_with("fn ") {
                analysis.push_str(&format!("- Function: {trimmed}\n"));
            }
        }
    }

    /// Analyze a general file
    async fn analyze_general_file(&self, path: &PathBuf, content: &str, task_description: &str) -> String {
        let mut analysis = format!("File Analysis: {}\n\n", path.display());
        
        let lines = content.lines().count();
        let chars = content.chars().count();
        let words = content.split_whitespace().count();

        analysis.push_str("File Statistics:\n");
        analysis.push_str(&format!("- Lines: {lines}\n"));
        analysis.push_str(&format!("- Characters: {chars}\n"));
        analysis.push_str(&format!("- Words: {words}\n"));

        if let Some(extension) = path.extension().and_then(|s| s.to_str()) {
            analysis.push_str(&format!("- File type: {extension}\n"));
        }

        // Show first few lines as sample
        analysis.push_str("\nContent Preview:\n");
        for (i, line) in content.lines().take(5).enumerate() {
            let line_num = i + 1;
            analysis.push_str(&format!("{line_num}: {line}\n"));
        }

        if lines > 5 {
            let more_lines = lines - 5;
            analysis.push_str(&format!("... ({more_lines} more lines)\n"));
        }

        // Note: task_description is used to guide the analysis approach,
        // but we don't include it directly in the output

        analysis
    }
}