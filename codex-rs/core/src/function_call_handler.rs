/// Universal Function Call Handler
/// 
/// This module provides a unified interface for handling function calls
/// that can be used by both the main agent and subagents.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::{FunctionCallOutputPayload, ResponseInputItem};
use serde::Deserialize;
use serde_json;
use tracing::{debug, error, info, warn};

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
    async fn execute_shell(&self, command: String, context: &FunctionCallContext) -> FunctionCallOutputPayload;
    
    /// Read a file
    async fn execute_read_file(&self, file_path: String, context: &FunctionCallContext) -> FunctionCallOutputPayload;
    
    /// Write a file
    async fn execute_write_file(&self, file_path: String, content: String, context: &FunctionCallContext) -> FunctionCallOutputPayload;
    
    /// Store a context item
    async fn execute_store_context(&self, id: String, summary: String, content: String, context: &FunctionCallContext) -> FunctionCallOutputPayload;
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
            command: String,
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

        debug!("Executing shell command: {}", args.command);
        self.executor.execute_shell(args.command, context).await
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
        self.executor.execute_read_file(args.file_path, context).await
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

        debug!("Writing file: {} (content length: {})", args.file_path, args.content.len());
        self.executor.execute_write_file(args.file_path, args.content, context).await
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

        debug!("Storing context: id='{}', summary='{}'", args.id, args.summary);
        self.executor.execute_store_context(args.id, args.summary, args.content, context).await
    }
}

/// Simple executor for subagents (simulated execution)
pub struct SubagentExecutor;

#[async_trait::async_trait]
impl FunctionExecutor for SubagentExecutor {
    async fn execute_shell(&self, command: String, _context: &FunctionCallContext) -> FunctionCallOutputPayload {
        // For subagents, we simulate shell execution
        let output = format!("Command executed: {command}\nOutput: [simulated shell output]");
        FunctionCallOutputPayload {
            content: output,
            success: Some(true),
        }
    }

    async fn execute_read_file(&self, file_path: String, context: &FunctionCallContext) -> FunctionCallOutputPayload {
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

    async fn execute_write_file(&self, file_path: String, content: String, context: &FunctionCallContext) -> FunctionCallOutputPayload {
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

    async fn execute_store_context(&self, id: String, summary: String, content: String, context: &FunctionCallContext) -> FunctionCallOutputPayload {
        // This is a placeholder implementation for the SubagentExecutor
        // The actual context storage will be handled by the LLMSubagentExecutor
        // which has access to the context repository
        
        tracing::info!(
            "Context storage requested: id='{}', summary='{}', content_length={}",
            id, summary, content.len()
        );
        
        FunctionCallOutputPayload {
            content: format!("Context '{}' stored successfully", id),
            success: Some(true),
        }
    }
}