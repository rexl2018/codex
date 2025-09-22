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
    /// Working directory for file operations
    working_directory: PathBuf,
}

impl CodexFunctionExecutor {
    /// Create a new CodexFunctionExecutor
    pub fn new(
        context_repository: Arc<dyn IContextRepository>,
        mcp_connection_manager: Option<Arc<McpConnectionManager>>,
        subagent_manager: Option<Arc<dyn ISubagentManager>>,
        working_directory: PathBuf,
    ) -> Self {
        Self {
            context_repository,
            mcp_connection_manager,
            subagent_manager,
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

        // Parse command into arguments
        let args = match shlex::split(command) {
            Some(args) => args,
            None => {
                return Err(UnifiedError::shell_execution(
                    command,
                    "Failed to parse shell command",
                    None,
                    ShellErrorCode::InvalidCommand,
                ));
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

        // Create exec params
        let exec_params = crate::exec::ExecParams {
            command: args,
            cwd: context.cwd.clone(),
            timeout_ms: Some(300000), // 5 minutes default
            env: std::collections::HashMap::new(),
            with_escalated_permissions: None,
            justification: None,
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

    /// Read file content with size limits and validation
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

    /// Store context using the context repository
    async fn store_context_item(
        &self,
        id: &str,
        summary: &str,
        content: &str,
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

        // Create context item using the Context::new constructor
        let context_item = crate::context_store::Context::new(
            id.to_string(),
            summary.to_string(),
            content.to_string(),
            "unified_executor".to_string(), // created_by
            None,                           // task_id
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
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_read_file")
            .with_info("file_path", &file_path)
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.read_file_content(&file_path).await {
            Ok(content) => FunctionCallOutputPayload {
                content: format!("File content of '{}':\n\n{}", file_path, content),
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
            .with_info("context_id", &id)
            .with_info("summary", &summary)
            .with_info("content_length", &content.len().to_string())
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        match self.store_context_item(&id, &summary, &content).await {
            Ok(result) => FunctionCallOutputPayload {
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
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        info!(
            "🚀 [UNIFIED_EXECUTOR] Creating subagent task with arguments: {}",
            arguments
        );
        info!(
            "🔧 [UNIFIED_EXECUTOR] Context: sub_id={}, call_id={}, agent_type={:?}",
            context.sub_id, context.call_id, context.agent_type
        );

        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_create_subagent_task")
            .with_info("call_id", &context.call_id);

        // Check if subagent manager is available
        let subagent_manager = match &self.subagent_manager {
            Some(manager) => manager,
            None => {
                let error_msg =
                    "Subagent manager not available. Multi-agent functionality may not be enabled.";
                warn!("{}", error_msg);
                return FunctionCallOutputPayload {
                    content: error_msg.to_string(),
                    success: Some(false),
                };
            }
        };

        // Parse arguments
        let args: CreateSubagentTaskArgs = match serde_json::from_str(&arguments) {
            Ok(args) => args,
            Err(e) => {
                let error_msg = format!("Failed to parse create_subagent_task arguments: {}", e);
                error!("{}", error_msg);
                return FunctionCallOutputPayload {
                    content: error_msg,
                    success: Some(false),
                };
            }
        };

        // Parse agent type
        let agent_type = match args.agent_type.to_lowercase().as_str() {
            "explorer" => SubagentType::Explorer,
            "coder" => SubagentType::Coder,
            _ => {
                let error_msg = format!(
                    "Invalid agent_type '{}'. Must be 'explorer' or 'coder'",
                    args.agent_type
                );
                error!("{}", error_msg);
                return FunctionCallOutputPayload {
                    content: error_msg,
                    success: Some(false),
                };
            }
        };

        // Create subagent task specification
        let spec = SubagentTaskSpec {
            agent_type: agent_type.clone(),
            title: args.title.clone(),
            description: args.description,
            context_refs: args.context_refs,
            bootstrap_paths: args.bootstrap_paths,
            max_turns: Some(50),       // Default max turns
            timeout_ms: Some(300_000), // 5 minutes default timeout
            network_access: None,
        };

        // Create the task
        match subagent_manager.create_task(spec).await {
            Ok(task_id) => {
                info!(
                    "Successfully created subagent task '{}' with ID: {}",
                    args.title, task_id
                );

                let mut response = format!(
                    "Successfully created {:?} subagent task '{}' with ID: {}",
                    agent_type, args.title, task_id
                );

                // Auto-launch if requested
                if args.auto_launch {
                    match subagent_manager.launch_subagent(&task_id).await {
                        Ok(_handle) => {
                            info!("Auto-launched subagent: task_id={}", task_id);

                            // Return a special response that includes a function call to keep main agent running
                            return FunctionCallOutputPayload {
                                content: format!(
                                    "🚀 Subagent launched and executing task...\n📋 Task ID: {}\n⏳ The subagent will run in the background.\n\n**IMPORTANT**: You should now wait for the subagent to complete, then use `list_contexts` to check for new context items and provide a comprehensive summary of the subagent's findings.",
                                    task_id
                                ),
                                success: Some(true),
                            };
                        }
                        Err(e) => {
                            let error_msg = format!("Failed to auto-launch subagent: {}", e);
                            warn!("{}", error_msg);
                            response.push_str(&format!("\nWarning: {}", error_msg));
                        }
                    }
                }

                FunctionCallOutputPayload {
                    content: response,
                    success: Some(true),
                }
            }
            Err(e) => {
                let error_msg = format!("Failed to create subagent task: {}", e);
                error!("{}", error_msg);

                GLOBAL_ERROR_HANDLER
                    .handle_error(
                        UnifiedError::function_call(
                            "create_subagent_task",
                            error_msg.clone(),
                            FunctionCallErrorCode::ExecutionFailed,
                        ),
                        Some(error_context),
                        Some(context.call_id.clone()),
                    )
                    .await
            }
        }
    }

    async fn execute_mcp_tool(
        &self,
        tool_name: String,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
        let error_context = ErrorContext::new("CodexFunctionExecutor")
            .with_function("execute_mcp_tool")
            .with_info("tool_name", &tool_name)
            .with_info("agent_type", &format!("{:?}", context.agent_type));

        if let Some(mcp_manager) = &self.mcp_connection_manager {
            // Parse server name and tool name using MCP delimiter "__"
            let (server_name, actual_tool_name) = if tool_name.contains("__") {
                let parts: Vec<&str> = tool_name.splitn(2, "__").collect();
                (parts[0].to_string(), parts[1].to_string())
            } else {
                ("default".to_string(), tool_name.clone())
            };

            // Parse arguments
            let args_value: serde_json::Value = match serde_json::from_str(&arguments) {
                Ok(value) => value,
                Err(e) => {
                    let error = UnifiedError::mcp(
                        &server_name,
                        &actual_tool_name,
                        format!("Failed to parse arguments: {}", e),
                        crate::unified_error_types::McpErrorCode::RequestFailed,
                    );
                    return GLOBAL_ERROR_HANDLER
                        .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                        .await;
                }
            };

            // Execute MCP tool
            match mcp_manager
                .call_tool(&server_name, &actual_tool_name, Some(args_value), None)
                .await
            {
                Ok(result) => {
                    // Format result for display
                    let mut output = String::new();
                    for content_block in &result.content {
                        match content_block {
                            mcp_types::ContentBlock::TextContent(text_content) => {
                                output.push_str(&text_content.text);
                                output.push('\n');
                            }
                            mcp_types::ContentBlock::ImageContent(_) => {
                                output.push_str("[Image content]\n");
                            }
                            mcp_types::ContentBlock::AudioContent(_) => {
                                output.push_str("[Audio content]\n");
                            }
                            mcp_types::ContentBlock::ResourceLink(_) => {
                                output.push_str("[Resource link]\n");
                            }
                            mcp_types::ContentBlock::EmbeddedResource(_) => {
                                output.push_str("[Embedded resource]\n");
                            }
                        }
                    }

                    if output.is_empty() {
                        output = "MCP tool executed successfully (no output)".to_string();
                    }

                    FunctionCallOutputPayload {
                        content: output.trim().to_string(),
                        success: Some(!result.is_error.unwrap_or(false)),
                    }
                }
                Err(e) => {
                    let error = UnifiedError::mcp(
                        &server_name,
                        &actual_tool_name,
                        format!("MCP tool execution failed: {}", e),
                        crate::unified_error_types::McpErrorCode::RequestFailed,
                    );
                    GLOBAL_ERROR_HANDLER
                        .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                        .await
                }
            }
        } else {
            let error = UnifiedError::mcp(
                "unknown",
                &tool_name,
                "MCP connection manager not available",
                crate::unified_error_types::McpErrorCode::ServerNotFound,
            );
            GLOBAL_ERROR_HANDLER
                .handle_error(error, Some(error_context), Some(context.call_id.clone()))
                .await
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
    }
}
