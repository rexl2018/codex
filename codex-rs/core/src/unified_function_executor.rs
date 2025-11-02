use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::BootstrapPath;
use codex_protocol::protocol::SubagentType;
use serde_json;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::context_store::IContextRepository;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::subagent_completion_tracker::SubagentCompletionStatus;
use crate::subagent_completion_tracker::SubagentCompletionTracker;
use crate::subagent_manager::ISubagentManager;
use crate::subagent_manager::SubagentTaskSpec;
use crate::unified_error_handler::GLOBAL_ERROR_HANDLER;
use crate::unified_error_handler::handle_error;
use crate::unified_error_types::ContextErrorCode;
use crate::unified_error_types::ContextOperation;
use crate::unified_error_types::ErrorContext;
use crate::unified_error_types::FileSystemErrorCode;
use crate::unified_error_types::FileSystemOperation;
use crate::unified_error_types::FunctionCallErrorCode;
use crate::unified_error_types::ShellErrorCode;
use crate::unified_error_types::UnifiedError;
use crate::unified_error_types::UnifiedResult;
use crate::unified_function_handler::UniversalFunctionCallContext;
use crate::unified_function_handler::UniversalFunctionExecutor;
use serde::Deserialize;
use std::time::Duration;

/// Arguments for create_subagent_task function call
#[derive(Debug, Deserialize, Clone)]
struct CreateSubagentTaskArgs {
    agent_type: String,
    title: String,
    description: String,
    #[serde(default)]
    context_refs: Vec<String>,
    #[serde(default)]
    bootstrap_paths: Vec<BootstrapPath>,
    #[serde(default = "default_auto_launch")]
    auto_launch: bool,
}

fn default_auto_launch() -> bool {
    true
}

/// Concrete implementation of UniversalFunctionExecutor that integrates with existing codex systems
pub struct CodexFunctionExecutor {
    /// Context repository for storing and retrieving contexts
    context_repository: Arc<dyn IContextRepository>,
    /// Optional MCP connection manager for external tool integration
    mcp_connection_manager: Option<Arc<McpConnectionManager>>,
    /// Optional subagent manager for multi-agent coordination
    subagent_manager: Option<Arc<dyn ISubagentManager>>,
    /// Optional completion tracker for clearing completed status on resume
    subagent_completion_tracker: Option<Arc<SubagentCompletionTracker>>,
    /// Working directory for file operations
    working_directory: PathBuf,
}

impl CodexFunctionExecutor {
    /// Create a new CodexFunctionExecutor
    pub fn new(
        context_repository: Arc<dyn IContextRepository>,
        mcp_connection_manager: Option<Arc<McpConnectionManager>>,
        subagent_manager: Option<Arc<dyn ISubagentManager>>,
        subagent_completion_tracker: Option<Arc<SubagentCompletionTracker>>,
        working_directory: PathBuf,
    ) -> Self {
        Self {
            context_repository,
            mcp_connection_manager,
            subagent_manager,
            subagent_completion_tracker,
            working_directory,
        }
    }

    /// Resolve file path relative to working directory
    fn resolve_path(&self, file_path: &str) -> PathBuf {
        if std::path::Path::new(file_path).is_absolute() {
            PathBuf::from(file_path)
        } else {
            self.working_directory.join(file_path)
        }
    }

    /// Validate file path for security
    fn validate_file_path(&self, file_path: &str) -> UnifiedResult<PathBuf> {
        let path = self.resolve_path(file_path);

        // Check for path traversal attempts
        if file_path.contains("..") {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                "Path traversal not allowed",
                FileSystemErrorCode::InvalidPath,
            ));
        }

        // Check for restricted paths
        let restricted_paths = [
            "/etc/passwd",
            "/etc/shadow",
            "/etc/hosts",
            "/proc/",
            "/sys/",
        ];

        let path_str = path.to_string_lossy();
        for restricted in &restricted_paths {
            if path_str.starts_with(restricted) {
                return Err(UnifiedError::file_system(
                    FileSystemOperation::Read,
                    file_path,
                    "Access to restricted path denied",
                    FileSystemErrorCode::PermissionDenied,
                ));
            }
        }

        Ok(path)
    }

    /// Execute shell command using existing codex shell infrastructure
    async fn execute_shell_command(
        &self,
        command: &str,
        context: &UniversalFunctionCallContext,
    ) -> UnifiedResult<String> {
        debug!("Executing shell command: {}", command);

        // Try to parse as JSON array first (from unified handler), then fall back to shlex parsing
        let args = if command.starts_with('[') && command.ends_with(']') {
            // Parse as JSON array
            match serde_json::from_str::<Vec<String>>(command) {
                Ok(args) => args,
                Err(_) => {
                    return Err(UnifiedError::shell_execution(
                        command,
                        "Failed to parse shell command JSON array",
                        None,
                        ShellErrorCode::InvalidCommand,
                    ));
                }
            }
        } else {
            // Parse as shell command string
            match shlex::split(command) {
                Some(args) => args,
                None => {
                    return Err(UnifiedError::shell_execution(
                        command,
                        "Failed to parse shell command",
                        None,
                        ShellErrorCode::InvalidCommand,
                    ));
                }
            }
        };

        if args.is_empty() {
            return Err(UnifiedError::shell_execution(
                command,
                "Empty command",
                None,
                ShellErrorCode::InvalidCommand,
            ));
        }

        // Use the same shell operator detection logic as the main agent
        let command_string = args.join(" ");
        let has_shell_operators = crate::codex::contains_shell_operators(&command_string);
        
        let final_command = if has_shell_operators {
            // Convert the command array to a shell command string and run it with bash -c
            // Don't use shlex::try_join as it quotes shell operators, use simple join instead
            let command_string = args.join(" ");
            vec!["bash".to_string(), "-c".to_string(), command_string]
        } else {
            args
        };

        // Create exec params
        let exec_params = crate::exec::ExecParams {
            command: final_command,
            cwd: context.cwd.clone(),
            timeout_ms: Some(300000), // 5 minutes default
            env: std::collections::HashMap::new(),
            with_escalated_permissions: None,
            justification: None,
            arg0: None,
        };

        // Execute using existing exec infrastructure
        match crate::exec::process_exec_tool_call(
            exec_params,
            crate::exec::SandboxType::None,
            &crate::protocol::SandboxPolicy::ReadOnly,
            &context.cwd,
            &None,
            None,
        )
        .await
        {
            Ok(output) => {
                debug!("Shell command completed successfully");
                Ok(format!(
                    "Command executed successfully.\n\nOutput:\n{}",
                    output.aggregated_output.text
                ))
            }
            Err(e) => {
                error!("Shell command failed: {}", e);
                Err(UnifiedError::shell_execution(
                    command,
                    format!("Command execution failed: {}", e),
                    None,
                    ShellErrorCode::ExecutionFailed,
                ))
            }
        }
    }

    /// Read file content with line range support
    async fn read_file_content_with_range(
        &self,
        file_path: &str,
        line_offset: Option<usize>,
        line_num: Option<usize>,
    ) -> UnifiedResult<(String, usize, usize, usize)> {
        let path = self.validate_file_path(file_path)?;

        // Check if file exists
        if !path.exists() {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                "File not found",
                FileSystemErrorCode::FileNotFound,
            ));
        }

        // Check if it's a file (not a directory)
        if !path.is_file() {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                "Path is not a file",
                FileSystemErrorCode::NotAFile,
            ));
        }

        // Check file size
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) => {
                return Err(UnifiedError::file_system(
                    FileSystemOperation::Read,
                    file_path,
                    format!("Failed to read file metadata: {}", e),
                    FileSystemErrorCode::IoError,
                ));
            }
        };

        const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
        if metadata.len() > MAX_FILE_SIZE {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                format!(
                    "File too large: {} bytes (max: {} bytes)",
                    metadata.len(),
                    MAX_FILE_SIZE
                ),
                FileSystemErrorCode::FileTooLarge,
            ));
        }

        // Read file content
        let full_content = match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(e) => {
                error!("Failed to read file {}: {}", file_path, e);
                return Err(UnifiedError::file_system(
                    FileSystemOperation::Read,
                    file_path,
                    format!("Failed to read file: {}", e),
                    FileSystemErrorCode::IoError,
                ));
            }
        };

        // Split content into lines
        let lines: Vec<&str> = full_content.lines().collect();
        let total_lines = lines.len();

        // Apply defaults for line_offset and line_num
        let offset = line_offset.unwrap_or(0);
        let num_lines = line_num.unwrap_or(1000);

        // Validate offset
        if offset >= total_lines {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                format!("Line offset {} exceeds file length {} lines", offset, total_lines),
                FileSystemErrorCode::InvalidPath,
            ));
        }

        // Calculate actual range
        let start_line = offset + 1; // Convert to 1-based for display
        let end_index = std::cmp::min(offset + num_lines, total_lines);
        let end_line = end_index; // 1-based for display

        // Extract the requested lines
        let selected_lines = &lines[offset..end_index];
        let content = selected_lines.join("\n");

        debug!(
            "Successfully read file: {} (lines {}-{} of {} total, {} bytes)",
            file_path,
            start_line,
            end_line,
            total_lines,
            content.len()
        );

        Ok((content, total_lines, start_line, end_line))
    }

    /// Read file content with size limits and validation (legacy method)
    async fn read_file_content(&self, file_path: &str) -> UnifiedResult<String> {
        let path = self.validate_file_path(file_path)?;

        // Check if file exists
        if !path.exists() {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                "File not found",
                FileSystemErrorCode::FileNotFound,
            ));
        }

        // Check if it's a file (not a directory)
        if !path.is_file() {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                "Path is not a file",
                FileSystemErrorCode::NotAFile,
            ));
        }

        // Check file size
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(e) => {
                return Err(UnifiedError::file_system(
                    FileSystemOperation::Read,
                    file_path,
                    format!("Failed to read file metadata: {}", e),
                    FileSystemErrorCode::IoError,
                ));
            }
        };

        const MAX_FILE_SIZE: u64 = 10 * 1024 * 1024; // 10MB
        if metadata.len() > MAX_FILE_SIZE {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Read,
                file_path,
                format!(
                    "File too large: {} bytes (max: {} bytes)",
                    metadata.len(),
                    MAX_FILE_SIZE
                ),
                FileSystemErrorCode::FileTooLarge,
            ));
        }

        // Read file content
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                debug!(
                    "Successfully read file: {} ({} bytes)",
                    file_path,
                    content.len()
                );
                Ok(content)
            }
            Err(e) => {
                error!("Failed to read file {}: {}", file_path, e);
                Err(UnifiedError::file_system(
                    FileSystemOperation::Read,
                    file_path,
                    format!("Failed to read file: {}", e),
                    FileSystemErrorCode::IoError,
                ))
            }
        }
    }

    /// Write file content with validation and backup
    async fn write_file_content(&self, file_path: &str, content: &str) -> UnifiedResult<String> {
        let path = self.validate_file_path(file_path)?;

        // Check content size
        const MAX_CONTENT_SIZE: usize = 10 * 1024 * 1024; // 10MB
        if content.len() > MAX_CONTENT_SIZE {
            return Err(UnifiedError::file_system(
                FileSystemOperation::Write,
                file_path,
                format!(
                    "Content too large: {} bytes (max: {} bytes)",
                    content.len(),
                    MAX_CONTENT_SIZE
                ),
                FileSystemErrorCode::FileTooLarge,
            ));
        }

        // Create parent directories if they don't exist
        if let Some(parent) = path.parent() {
            if !parent.exists() {
                if let Err(e) = std::fs::create_dir_all(parent) {
                    return Err(UnifiedError::file_system(
                        FileSystemOperation::Write,
                        file_path,
                        format!("Failed to create parent directories: {}", e),
                        FileSystemErrorCode::IoError,
                    ));
                }
            }
        }

        // Write file content
        match std::fs::write(&path, content) {
            Ok(()) => {
                info!(
                    "Successfully wrote file: {} ({} bytes)",
                    file_path,
                    content.len()
                );
                Ok(format!(
                    "File written successfully: {} ({} bytes)",
                    file_path,
                    content.len()
                ))
            }
            Err(e) => {
                error!("Failed to write file {}: {}", file_path, e);
                Err(UnifiedError::file_system(
                    FileSystemOperation::Write,
                    file_path,
                    format!("Failed to write file: {}", e),
                    FileSystemErrorCode::IoError,
                ))
            }
        }
    }

    /// Update context using the context repository
    async fn update_context_item(
        &self,
        id: &str,
        content: &str,
        reason: &str,
    ) -> UnifiedResult<String> {
        // Validate context ID
        if id.is_empty() {
            return Err(UnifiedError::context(
                ContextOperation::Update,
                Some(id.to_string()),
                "Context ID cannot be empty",
                ContextErrorCode::InvalidContextId,
            ));
        }

        // Validate content size
        const MAX_CONTEXT_SIZE: usize = 1024 * 1024; // 1MB
        if content.len() > MAX_CONTEXT_SIZE {
            return Err(UnifiedError::context(
                ContextOperation::Update,
                Some(id.to_string()),
                format!(
                    "Context content too large: {} bytes (max: {} bytes)",
                    content.len(),
                    MAX_CONTEXT_SIZE
                ),
                ContextErrorCode::ContentTooLarge,
            ));
        }

        // Update context
        match self.context_repository.update_context(id, content.to_string(), reason.to_string()).await {
            Ok(()) => {
                info!("Successfully updated context: {} - {}", id, reason);
                Ok(format!("Context updated successfully: {} - {}", id, reason))
            }
            Err(e) => {
                error!("Failed to update context {}: {}", id, e);
                Err(UnifiedError::context(
                    ContextOperation::Update,
                    Some(id.to_string()),
                    format!("Failed to update context: {}", e),
                    ContextErrorCode::ContextNotFound,
                ))
            }
        }
    }

    /// Store context using the context repository
    async fn store_context_item(
        &self,
        id: &str,
        summary: &str,
        content: &str,
        context: &UniversalFunctionCallContext,
    ) -> UnifiedResult<String> {
        // Validate context ID
        if id.is_empty() {
            return Err(UnifiedError::context(
                ContextOperation::Store,
                Some(id.to_string()),
                "Context ID cannot be empty",
                ContextErrorCode::InvalidContextId,
            ));
        }

        // Validate content size
        const MAX_CONTEXT_SIZE: usize = 1024 * 1024; // 1MB
        if content.len() > MAX_CONTEXT_SIZE {
            return Err(UnifiedError::context(
                ContextOperation::Store,
                Some(id.to_string()),
                format!(
                    "Context content too large: {} bytes (max: {} bytes)",
                    content.len(),
                    MAX_CONTEXT_SIZE
                ),
                ContextErrorCode::ContentTooLarge,
            ));
        }

        // Determine the created_by value based on the agent type and sub_id
        let created_by = match context.agent_type {
            crate::unified_function_handler::AgentType::Main => "main_agent".to_string(),
            crate::unified_function_handler::AgentType::Explorer | 
            crate::unified_function_handler::AgentType::Coder => {
                // For subagents, use the sub_id as the created_by value
                context.sub_id.clone()
            }
        };

        // Create context item using the Context::new constructor
        let context_item = crate::context_store::Context::new(
            id.to_string(),
            summary.to_string(),
            content.to_string(),
            created_by, // Use the determined created_by value
            None,       // task_id
        );

        // Store context
        match self.context_repository.store_context(context_item).await {
            Ok(()) => {
                info!("Successfully stored context: {} - {}", id, summary);
                Ok(format!("Context stored successfully: {} - {}", id, summary))
            }
            Err(e) => {
                error!("Failed to store context {}: {}", id, e);
                Err(UnifiedError::context(
                    ContextOperation::Store,
                    Some(id.to_string()),
                    format!("Failed to store context: {}", e),
                    ContextErrorCode::StorageFull,
                ))
            }
        }
    }

    async fn retrieve_context_data(&self, id: &str) -> UnifiedResult<String> {
        // Validate context ID
        if id.is_empty() {
            return Err(UnifiedError::context(
                ContextOperation::Retrieve,
                Some(id.to_string()),
                "Context ID cannot be empty",
                ContextErrorCode::InvalidContextId,
            ));
        }

        // Retrieve context
        match self.context_repository.get_context(id).await {
            Ok(Some(context_item)) => {
                info!("Successfully retrieved context: {}", id);
                Ok(serde_json::to_string_pretty(&context_item).unwrap_or_else(|_| {
                    format!("Context: {} - {}\nContent: {}", 
                        context_item.id, 
                        context_item.summary, 
                        context_item.content)
                }))
            }
            Ok(None) => {
                Err(UnifiedError::context(
                    ContextOperation::Retrieve,
                    Some(id.to_string()),
                    "Context not found",
                    ContextErrorCode::ContextNotFound,
                ))
            }
            Err(e) => {
                error!("Failed to retrieve context {}: {}", id, e);
                Err(UnifiedError::context(
                    ContextOperation::Retrieve,
                    Some(id.to_string()),
                    format!("Failed to retrieve context: {}", e),
                    ContextErrorCode::ContextNotFound,
                ))
            }
        }
    }

    async fn list_context_data(&self) -> UnifiedResult<String> {
        // List all contexts using query_contexts with empty query
        let query = crate::context_store::ContextQuery {
            ids: None,
            tags: None,
            created_by: None,
            limit: None,
        };
        match self.context_repository.query_contexts(&query).await {
            Ok(contexts) => {
                info!("Successfully listed {} contexts", contexts.len());
                if contexts.is_empty() {
                    Ok("No contexts found".to_string())
                } else {
                    let context_list: Vec<String> = contexts
                        .iter()
                        .map(|ctx| format!("- {}: {}", ctx.id, ctx.summary))
                        .collect();
                    Ok(format!("Available contexts ({}):\n{}", 
                        contexts.len(), 
                        context_list.join("\n")))
                }
            }
            Err(e) => {
                error!("Failed to list contexts: {}", e);
                Err(UnifiedError::context(
                    ContextOperation::List,
                    None,
                    format!("Failed to list contexts: {}", e),
                    ContextErrorCode::ContextNotFound,
                ))
            }
        }
    }

    async fn create_subagent_task(&self, task_spec: SubagentTaskSpec) -> UnifiedResult<String> {
        if let Some(subagent_manager) = &self.subagent_manager {
            match subagent_manager.create_task(task_spec).await {
                Ok(task_id) => {
                    info!("Successfully created subagent task: {}", task_id);
                    Ok(task_id)
                }
                Err(e) => {
                    error!("Failed to create subagent task: {}", e);
                    Err(UnifiedError::function_call(
                        "create_subagent_task",
                        format!("Failed to create subagent task: {}", e),
                        FunctionCallErrorCode::ExecutionFailed,
                    ))
                }
            }
        } else {
            Err(UnifiedError::function_call(
                "create_subagent_task",
                "Subagent manager not available",
                FunctionCallErrorCode::ExecutionFailed,
            ))
        }
    }

    async fn get_subagent_status(&self, task_id: &str) -> UnifiedResult<serde_json::Value> {
        if let Some(subagent_manager) = &self.subagent_manager {
            match subagent_manager.get_task_status(task_id).await {
                Ok(status) => {
                    info!("Successfully retrieved subagent status: {}", task_id);
                    Ok(serde_json::to_value(&status).unwrap_or_else(|_| {
                        serde_json::json!({"status": format!("{:?}", status)})
                    }))
                }
                Err(e) => {
                    error!("Failed to get subagent status: {}", e);
                    Err(UnifiedError::function_call(
                        "get_subagent_status",
                        format!("Failed to get subagent status: {}", e),
                        FunctionCallErrorCode::ExecutionFailed,
                    ))
                }
            }
        } else {
            Err(UnifiedError::function_call(
                "get_subagent_status",
                "Subagent manager not available",
                FunctionCallErrorCode::ExecutionFailed,
            ))
        }
    }

    async fn execute_mcp_call_internal(
        &self,
        server_name: &str,
        method: &str,
        params: serde_json::Value,
    ) -> UnifiedResult<String> {
        if let Some(mcp_manager) = &self.mcp_connection_manager {
            match mcp_manager.call_tool(server_name, method, Some(params)).await {
                Ok(result) => {
                    info!("Successfully executed MCP call: {}::{}", server_name, method);
                    Ok(serde_json::to_string_pretty(&result).unwrap_or_else(|_| {
                        format!("MCP call result: {:?}", result)
                    }))
                }
                Err(e) => {
                    error!("Failed to execute MCP call: {}", e);
                    Err(UnifiedError::mcp(
                        server_name,
                        method,
                        format!("MCP call failed: {}", e),
                        crate::unified_error_types::McpErrorCode::RequestFailed,
                    ))
                }
            }
        } else {
            Err(UnifiedError::mcp(
                server_name,
                method,
                "MCP connection manager not available",
                crate::unified_error_types::McpErrorCode::ServerNotFound,
            ))
        }
    }

    async fn wait_for_subagent_completion(
        &self,
        task_id: &str,
        timeout: Duration,
    ) -> UnifiedResult<SubagentCompletionStatus> {
        if let Some(tracker) = &self.subagent_completion_tracker {
            match tracker.wait_for_completion(task_id, timeout).await {
                Ok(status) => {
                    info!("Subagent task {} completed with status: {:?}", task_id, status);
                    Ok(status)
                }
                Err(e) => {
                    error!("Failed to wait for subagent completion: {}", e);
                    Err(UnifiedError::function_call(
                        "wait_for_subagent_completion",
                        format!("Failed to wait for subagent completion: {}", e),
                        FunctionCallErrorCode::ExecutionFailed,
                    ))
                }
            }
        } else {
            Err(UnifiedError::function_call(
                "wait_for_subagent_completion",
                "Subagent completion tracker not available",
                FunctionCallErrorCode::ExecutionFailed,
            ))
        }
    }
}

#[async_trait::async_trait]
impl UniversalFunctionExecutor for CodexFunctionExecutor {
    async fn execute_shell(
        &self,
        command: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_shell")
            .with_info("command", &command)
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.execute_shell_command(&command, context).await {
            Ok(output) => FunctionCallOutputPayload {
            content_items: None,
                content: output,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_read_file(
        &self,
        file_path: String,
        line_offset: Option<usize>,
        line_num: Option<usize>,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_read_file")
            .with_info("file_path", &file_path)
            .with_info("line_offset", &format!("{:?}", line_offset))
            .with_info("line_num", &format!("{:?}", line_num))
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.read_file_content_with_range(&file_path, line_offset, line_num).await {
            Ok((content, total_lines, start_line, end_line)) => FunctionCallOutputPayload {
            content_items: None,
                content: format!(
                    "File content of '{}' (lines {}-{} of {} total lines):\n\n{}",
                    file_path, start_line, end_line, total_lines, content
                ),
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_write_file(
        &self,
        file_path: String,
        content: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_write_file")
            .with_info("file_path", &file_path)
            .with_info("content_length", &content.len().to_string())
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.write_file_content(&file_path, &content).await {
            Ok(result) => FunctionCallOutputPayload {
                content_items: None,
                content: result,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_store_context(
        &self,
        id: String,
        summary: String,
        content: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_store_context")
            .with_info("id", &id)
            .with_info("summary", &summary)
            .with_info("content_length", &content.len().to_string())
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.store_context_item(&id, &summary, &content, context).await {
            Ok(result) => FunctionCallOutputPayload {
                content_items: None,
                content: result,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_retrieve_context(
        &self,
        id: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_retrieve_context")
            .with_info("id", &id)
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.retrieve_context_data(&id).await {
            Ok(result) => FunctionCallOutputPayload {
                content_items: None,
                content: result,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_list_context(
        &self,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_list_context")
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.list_context_data().await {
            Ok(result) => FunctionCallOutputPayload {
                content_items: None,
                content: result,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_create_subagent_task(
        &self,
        args: serde_json::Value,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_create_subagent_task")
            .with_info("args", &args.to_string())
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        // Parse arguments
        let parsed_args: CreateSubagentTaskArgs = match serde_json::from_value(args) {
            Ok(args) => args,
            Err(e) => {
                let error = UnifiedError::function_call(
                    "execute_create_subagent_task",
                    format!("Failed to parse create_subagent_task arguments: {}", e),
                    FunctionCallErrorCode::InvalidArguments,
                );
                return GLOBAL_ERROR_HANDLER
                    .handle_error(error, None, Some(context.call_id.clone()))
                    .await;
            }
        };

        // Parse agent type
        let agent_type = match parsed_args.agent_type.as_str() {
            "Coder" => SubagentType::Coder,
            "Explorer" => SubagentType::Explorer,
            _ => {
                let error = UnifiedError::function_call(
                    "execute_create_subagent_task",
                    format!("Invalid agent type: {}", parsed_args.agent_type),
                    FunctionCallErrorCode::InvalidArguments,
                );
                return GLOBAL_ERROR_HANDLER
                    .handle_error(error, None, Some(context.call_id.clone()))
                    .await;
            }
        };

        let task_spec = SubagentTaskSpec {
            agent_type,
            title: parsed_args.title,
            description: parsed_args.description,
            context_refs: parsed_args.context_refs,
            bootstrap_paths: parsed_args.bootstrap_paths,
            max_turns: Some(50),
            timeout_ms: Some(300_000),
            network_access: None,
            injected_conversation: None,
        };

        match self.create_subagent_task(task_spec).await {
            Ok(task_id) => {
                let result = format!("Created subagent task with ID: {}", task_id);
                FunctionCallOutputPayload {
                    content_items: None,
                    content: result,
                    success: Some(true),
                }
            }
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_get_subagent_status(
        &self,
        task_id: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_get_subagent_status")
            .with_info("task_id", &task_id)
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.get_subagent_status(&task_id).await {
            Ok(status) => {
                let result = serde_json::to_string_pretty(&status).unwrap_or_else(|_| {
                    format!("Status: {:?}", status)
                });
                FunctionCallOutputPayload {
                    content_items: None,
                    content: result,
                    success: Some(true),
                }
            }
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_wait_for_subagent(
        &self,
        task_id: String,
        timeout_seconds: Option<u64>,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_wait_for_subagent")
            .with_info("task_id", &task_id)
            .with_info("timeout_seconds", &format!("{:?}", timeout_seconds))
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        let timeout = timeout_seconds.map(Duration::from_secs).unwrap_or(Duration::from_secs(300)); // Default 5 minutes

        match self.wait_for_subagent_completion(&task_id, timeout).await {
            Ok(status) => {
                let result = match &status {
                    SubagentCompletionStatus::Completed { success, contexts_created, comments } => {
                        format!("Subagent task {} completed: success={}, contexts_created={}, comments={}", 
                            task_id, success, contexts_created, comments)
                    }
                    SubagentCompletionStatus::ForceCompleted { num_turns, max_turns, contexts_created, comments } => {
                        format!("Subagent task {} force completed: {}/{} turns, contexts_created={}, comments={}", 
                            task_id, num_turns, max_turns, contexts_created, comments)
                    }
                    SubagentCompletionStatus::Failed { error } => {
                        format!("Subagent task {} failed: {}", task_id, error)
                    }
                    SubagentCompletionStatus::Cancelled { reason, cancelled_at_turn } => {
                        format!("Subagent task {} cancelled at turn {}: {}", task_id, cancelled_at_turn, reason)
                    }
                };
                FunctionCallOutputPayload {
                    content_items: None,
                    content: result,
                    success: Some(matches!(status, SubagentCompletionStatus::Completed { success: true, .. } | SubagentCompletionStatus::ForceCompleted { .. })),
                }
            }
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }

    async fn execute_mcp_call(
        &self,
        server_name: String,
        method: String,
        params: serde_json::Value,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_mcp_call")
            .with_info("server_name", &server_name)
            .with_info("method", &method)
            .with_info("params", &params.to_string())
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.execute_mcp_call_internal(&server_name, &method, params).await {
            Ok(result) => FunctionCallOutputPayload {
                content_items: None,
                content: result,
                success: Some(true),
            },
            Err(error) => {
                GLOBAL_ERROR_HANDLER
                    .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                    .await
            }
        }
    }








}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_store::InMemoryContextRepository;
    use crate::unified_function_handler::AgentType;
    use crate::unified_function_handler::FunctionPermissions;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_file_operations() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let executor = CodexFunctionExecutor::new(
            context_repo,
            None,
            None, // No subagent manager for this test
            None, // No completion tracker for this test
            PathBuf::from("/tmp"),
        );

        let context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "test".to_string(),
            call_id: "call_123".to_string(),
            agent_type: AgentType::Main,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
        };

        // Test path validation
        let result = executor.validate_file_path("../etc/passwd");
        assert!(result.is_err());

        let result = executor.validate_file_path("test.txt");
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_context_storage() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let executor = CodexFunctionExecutor::new(
            context_repo.clone(),
            None,
            None, // No subagent manager for this test
            None, // No completion tracker for this test
            PathBuf::from("/tmp"),
        );

        let context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "test".to_string(),
            call_id: "call_123".to_string(),
            agent_type: AgentType::Main,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
        };

        let result = executor
            .execute_store_context(
                "test_context".to_string(),
                "Test summary".to_string(),
                "Test content".to_string(),
                &context,
            )
            .await;

        assert_eq!(result.success, Some(true));

        // Verify context was stored
        let stored_context = context_repo.get_context("test_context").await;
        assert!(stored_context.is_ok());
        
        // Verify created_by field is set correctly for main agent
        let context = stored_context.unwrap().unwrap();
        assert_eq!(context.created_by, "main_agent");
    }

    #[tokio::test]
    async fn test_context_storage_created_by_field() {
        let context_repo = Arc::new(InMemoryContextRepository::new());
        let executor = CodexFunctionExecutor::new(
            context_repo.clone(),
            None,
            None,
            None,
            PathBuf::from("/tmp"),
        );

        // Test main agent
        let main_context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "main_agent_123".to_string(),
            call_id: "call_123".to_string(),
            agent_type: AgentType::Main,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
        };

        let result = executor
            .execute_store_context(
                "main_context".to_string(),
                "Main agent context".to_string(),
                "Content from main agent".to_string(),
                &main_context,
            )
            .await;

        assert_eq!(result.success, Some(true));
        let stored = context_repo.get_context("main_context").await.unwrap().unwrap();
        assert_eq!(stored.created_by, "main_agent");

        // Test explorer subagent
        let explorer_context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "explorer_subagent_456".to_string(),
            call_id: "call_456".to_string(),
            agent_type: AgentType::Explorer,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Explorer),
        };

        let result = executor
            .execute_store_context(
                "explorer_context".to_string(),
                "Explorer subagent context".to_string(),
                "Content from explorer subagent".to_string(),
                &explorer_context,
            )
            .await;

        assert_eq!(result.success, Some(true));
        let stored = context_repo.get_context("explorer_context").await.unwrap().unwrap();
        assert_eq!(stored.created_by, "explorer_subagent_456");

        // Test coder subagent
        let coder_context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "coder_subagent_789".to_string(),
            call_id: "call_789".to_string(),
            agent_type: AgentType::Coder,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Coder),
        };

        let result = executor
            .execute_store_context(
                "coder_context".to_string(),
                "Coder subagent context".to_string(),
                "Content from coder subagent".to_string(),
                &coder_context,
            )
            .await;

        assert_eq!(result.success, Some(true));
        let stored = context_repo.get_context("coder_context").await.unwrap().unwrap();
        assert_eq!(stored.created_by, "coder_subagent_789");
    }
}

/// Arguments for resume_subagent function call
#[derive(Debug, Deserialize, Clone)]
struct ResumeSubagentArgs {
    task_id: String,
    #[serde(default)]
    new_instruction: Option<String>,
    #[serde(default)]
    additional_context_refs: Vec<String>,
    #[serde(default)]
    additional_bootstrap_paths: Vec<BootstrapPath>,
    #[serde(default)]
    new_max_turns: Option<u32>,
    #[serde(default)]
    use_previous_trajectory: Option<bool>,
    #[serde(default = "default_auto_launch")]
    auto_launch: bool,
}
