/// Universal Function Call Handler
///
/// This module provides a unified interface for handling function calls
/// that can be used by both the main agent and subagents.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use serde::Deserialize;
use serde_json;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::exec::ExecParams;

/// Context required for function call execution
pub struct FunctionCallContext {
    pub cwd: PathBuf,
    pub sub_id: String,
    pub call_id: String,
}

/// Trait for executing different types of function calls
#[async_trait::async_trait]
pub trait FunctionExecutor {
    /// Execute a shell command
    async fn execute_shell(
        &self,
        command: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload;

    /// Read a file
    async fn execute_read_file(
        &self,
        file_path: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload;

    /// Write a file
    async fn execute_write_file(
        &self,
        file_path: String,
        content: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload;

    /// Store a context item
    async fn execute_store_context(
        &self,
        id: String,
        summary: String,
        content: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload;
}

/// Universal function call handler
pub struct FunctionCallHandler<E: FunctionExecutor> {
    executor: Arc<E>,
}

impl<E: FunctionExecutor> FunctionCallHandler<E> {
    pub fn new(executor: Arc<E>) -> Self {
        Self { executor }
    }

    /// Handle a function call and return the appropriate response
    pub async fn handle_function_call(
        &self,
        name: String,
        arguments: String,
        context: FunctionCallContext,
    ) -> ResponseInputItem {
        info!("FunctionCall: {}({})", name, arguments);

        let output = match name.as_str() {
            "shell" => self.handle_shell_call(arguments, &context).await,
            "read_file" => self.handle_read_file_call(arguments, &context).await,
            "write_file" => self.handle_write_file_call(arguments, &context).await,
            "store_context" => self.handle_store_context_call(arguments, &context).await,
            "create_subagent_task" => {
                warn!("Subagent attempted to call create_subagent_task: {name}");
                FunctionCallOutputPayload {
                    content: "Error: Subagents cannot create other subagents. Only the main agent can use the create_subagent_task function. Please complete your current task and report your findings instead.".to_string(),
                    success: Some(false),
                }
            }
            _ => {
                warn!("Unsupported function call: {name}");
                FunctionCallOutputPayload {
                    content: format!("unsupported call: {name}"),
                    success: Some(false),
                }
            }
        };

        ResponseInputItem::FunctionCallOutput {
            call_id: context.call_id,
            output,
        }
    }

    async fn handle_shell_call(
        &self,
        arguments: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        #[derive(Deserialize)]
        struct ShellArgs {
            command: Vec<String>,
        }

        let args = match serde_json::from_str::<ShellArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse shell arguments: {e}");
                return FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: Some(false),
                };
            }
        };

        let command_str = args.command.join(" ");
        debug!("Executing shell command: {}", command_str);
        self.executor.execute_shell(command_str, context).await
    }

    async fn handle_read_file_call(
        &self,
        arguments: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        #[derive(Deserialize)]
        struct ReadFileArgs {
            file_path: String,
        }

        let args = match serde_json::from_str::<ReadFileArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse read_file arguments: {e}");
                return FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: Some(false),
                };
            }
        };

        debug!("Reading file: {}", args.file_path);
        self.executor
            .execute_read_file(args.file_path, context)
            .await
    }

    async fn handle_write_file_call(
        &self,
        arguments: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        #[derive(Deserialize)]
        struct WriteFileArgs {
            file_path: String,
            content: String,
        }

        let args = match serde_json::from_str::<WriteFileArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse write_file arguments: {e}");
                return FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: Some(false),
                };
            }
        };

        debug!(
            "Writing file: {} (content length: {})",
            args.file_path,
            args.content.len()
        );
        self.executor
            .execute_write_file(args.file_path, args.content, context)
            .await
    }

    async fn handle_store_context_call(
        &self,
        arguments: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        #[derive(Deserialize)]
        struct StoreContextArgs {
            id: String,
            summary: String,
            content: String,
        }

        let args = match serde_json::from_str::<StoreContextArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse store_context arguments: {e}");
                return FunctionCallOutputPayload {
                    content: format!("failed to parse function arguments: {e}"),
                    success: Some(false),
                };
            }
        };

        debug!(
            "Storing context: id='{}', summary='{}'",
            args.id, args.summary
        );
        self.executor
            .execute_store_context(args.id, args.summary, args.content, context)
            .await
    }
}

/// Simple executor for subagents (simulated execution)
pub struct SubagentExecutor;

#[async_trait::async_trait]
impl FunctionExecutor for SubagentExecutor {
    async fn execute_shell(
        &self,
        command: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        // For subagents, provide meaningful responses for common commands
        let output = match command.trim() {
            "pwd" => {
                // Return the actual current working directory
                context.cwd.to_string_lossy().to_string()
            }
            cmd if cmd.starts_with("ls") || cmd.starts_with("dir") => {
                format!("Shell command '{}' is not available for subagents.\nTo list directory contents, use the filesystem MCP tool: filesystem.list_directory", cmd)
            }
            cmd if cmd.starts_with("cd ") => {
                "Shell command 'cd' is not available for subagents.\nSubagents operate in a fixed working directory. Use filesystem MCP tools with absolute paths.".to_string()
            }
            _ => {
                format!("Shell command '{}' is not available for subagents.\nSubagents have limited shell access. Use MCP tools for file operations.", command)
            }
        };

        FunctionCallOutputPayload {
            content: output,
            success: Some(true),
        }
    }

    async fn execute_read_file(
        &self,
        file_path: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        // For subagents, we simulate file reading
        let abs_path = if file_path.starts_with('/') {
            PathBuf::from(file_path)
        } else {
            context.cwd.join(file_path)
        };

        // Try to actually read the file if it exists
        match tokio::fs::read_to_string(&abs_path).await {
            Ok(content) => FunctionCallOutputPayload {
                content,
                success: Some(true),
            },
            Err(e) => FunctionCallOutputPayload {
                content: format!("Failed to read file {}: {}", abs_path.display(), e),
                success: Some(false),
            },
        }
    }

    async fn execute_write_file(
        &self,
        file_path: String,
        content: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        // For subagents, we simulate file writing
        let abs_path = if file_path.starts_with('/') {
            PathBuf::from(file_path)
        } else {
            context.cwd.join(file_path)
        };

        // Try to actually write the file
        match tokio::fs::write(&abs_path, &content).await {
            Ok(()) => FunctionCallOutputPayload {
                content: format!("File written successfully: {}", abs_path.display()),
                success: Some(true),
            },
            Err(e) => FunctionCallOutputPayload {
                content: format!("Failed to write file {}: {}", abs_path.display(), e),
                success: Some(false),
            },
        }
    }

    async fn execute_store_context(
        &self,
        id: String,
        summary: String,
        content: String,
        context: &FunctionCallContext,
    ) -> FunctionCallOutputPayload {
        // SubagentExecutor cannot directly store contexts as it lacks access to the context repository.
        // Context storage is handled by the LLMSubagentExecutor which has proper access.
        // This method logs the request for debugging purposes.

        tracing::warn!(
            "SubagentExecutor cannot store contexts directly. Context storage request: id='{}', summary='{}', content_length={}",
            id,
            summary,
            content.len()
        );

        FunctionCallOutputPayload {
            content: format!(
                "Note: Context storage '{}' was requested but SubagentExecutor cannot store contexts directly. Use LLMSubagentExecutor for actual context storage.",
                id
            ),
            success: Some(false),
        }
    }
}
