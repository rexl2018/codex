use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use serde::Deserialize;
use serde_json;
use tracing::{debug, error, info, warn};

use crate::exec::ExecParams;
use crate::context_store::IContextRepository;

/// Context information for function call execution
#[derive(Debug, Clone)]
pub struct UniversalFunctionCallContext {
    pub cwd: PathBuf,
    pub sub_id: String,
    pub call_id: String,
    pub agent_type: AgentType,
    pub permissions: FunctionPermissions,
}

/// Agent type for permission checking
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AgentType {
    Main,
    Explorer,
    Coder,
}

/// Function permissions for different agent types
#[derive(Debug, Clone)]
pub struct FunctionPermissions {
    pub can_write_files: bool,
    pub can_execute_shell: bool,
    pub can_create_subagents: bool,
    pub can_store_context: bool,
    pub can_read_files: bool,
}

impl FunctionPermissions {
    pub fn for_agent_type(agent_type: &AgentType) -> Self {
        match agent_type {
            AgentType::Main => Self {
                can_write_files: true,
                can_execute_shell: true,
                can_create_subagents: true,
                can_store_context: true,
                can_read_files: true,
            },
            AgentType::Explorer => Self {
                can_write_files: false,
                can_execute_shell: true,
                can_create_subagents: false,
                can_store_context: true,
                can_read_files: true,
            },
            AgentType::Coder => Self {
                can_write_files: true,
                can_execute_shell: true,
                can_create_subagents: false,
                can_store_context: true,
                can_read_files: true,
            },
        }
    }
}

/// Trait for executing different types of function calls
#[async_trait::async_trait]
pub trait UniversalFunctionExecutor: Send + Sync {
    async fn execute_shell(
        &self,
        command: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_read_file(
        &self,
        file_path: String,
        line_offset: Option<usize>,
        line_num: Option<usize>,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_write_file(
        &self,
        file_path: String,
        content: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_store_context(
        &self,
        id: String,
        summary: String,
        content: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_update_context(
        &self,
        id: String,
        content: String,
        reason: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_create_subagent_task(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_resume_subagent(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_mcp_tool(
        &self,
        tool_name: String,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_apply_patch(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;

    async fn execute_update_plan(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload;
}

/// Universal function call handler that can be used by both main agent and subagents
pub struct UniversalFunctionCallHandler<E: UniversalFunctionExecutor> {
    executor: Arc<E>,
    supported_functions: HashMap<String, bool>,
}

impl<E: UniversalFunctionExecutor> UniversalFunctionCallHandler<E> {
    pub fn new(executor: Arc<E>) -> Self {
        let mut supported_functions = HashMap::new();
        supported_functions.insert("shell".to_string(), true);
        supported_functions.insert("read_file".to_string(), true);
        supported_functions.insert("write_file".to_string(), true);
        supported_functions.insert("store_context".to_string(), true);
        supported_functions.insert("update_context".to_string(), true);
        supported_functions.insert("create_subagent_task".to_string(), true);
        supported_functions.insert("resume_subagent".to_string(), true);
        supported_functions.insert("apply_patch".to_string(), true);
        supported_functions.insert("update_plan".to_string(), true);

        Self {
            executor,
            supported_functions,
        }
    }

    /// Handle a function call with unified logic
    pub async fn handle_function_call(
        &self,
        name: String,
        arguments: String,
        context: UniversalFunctionCallContext,
    ) -> ResponseInputItem {
        debug!(
            "Handling function call: {} for agent type: {:?}",
            name, context.agent_type
        );

        // Check if function is supported
        if !self.supported_functions.contains_key(&name) {
            warn!("Unsupported function call: {}", name);
            return ResponseInputItem::FunctionCallOutput {
                call_id: context.call_id.clone(),
                output: FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Unsupported function call: {}", name),
                    success: Some(false),
                },
            };
        }

        // Check permissions
        let permission_error = self.check_permissions(&name, &context);
        if let Some(error_msg) = permission_error {
            warn!("Permission denied for function call: {} - {}", name, error_msg);
            return ResponseInputItem::FunctionCallOutput {
                call_id: context.call_id.clone(),
                output: FunctionCallOutputPayload {
            content_items: None,
                    content: error_msg,
                    success: Some(false),
                },
            };
        }

        // Execute the function call
        let output = match name.as_str() {
            "shell" => self.handle_shell_call(arguments, &context).await,
            "read_file" => self.handle_read_file_call(arguments, &context).await,
            "write_file" => self.handle_write_file_call(arguments, &context).await,
            "store_context" => self.handle_store_context_call(arguments, &context).await,
            "update_context" => self.handle_update_context_call(arguments, &context).await,
            "create_subagent_task" => {
                self.handle_create_subagent_task_call(arguments, &context)
                    .await
            }
            "resume_subagent" => {
                self.handle_resume_subagent_call(arguments, &context)
                    .await
            }
            "apply_patch" => self.handle_apply_patch_call(arguments, &context).await,
            "update_plan" => self.handle_update_plan_call(arguments, &context).await,
            _ => {
                // Handle MCP tools or other custom tools
                self.executor
                    .execute_mcp_tool(name.clone(), arguments, &context)
                    .await
            }
        };

        ResponseInputItem::FunctionCallOutput {
            call_id: context.call_id,
            output,
        }
    }

    /// Check if the agent has permission to execute the function
    fn check_permissions(
        &self,
        function_name: &str,
        context: &UniversalFunctionCallContext,
    ) -> Option<String> {
        match function_name {
            "write_file" => {
                if !context.permissions.can_write_files {
                    Some(format!(
                        "Error: {:?} agents cannot write files. Only Coder and Main agents can use the write_file function.",
                        context.agent_type
                    ))
                } else {
                    None
                }
            }
            "shell" => {
                if !context.permissions.can_execute_shell {
                    Some(format!(
                        "Error: {:?} agents cannot execute shell commands.",
                        context.agent_type
                    ))
                } else {
                    None
                }
            }
            "create_subagent_task" => {
                if !context.permissions.can_create_subagents {
                    Some(
                        "Error: Subagents cannot create other subagents. Only the main agent can use the create_subagent_task function. Please complete your current task and report your findings instead.".to_string(),
                    )
                } else {
                    None
                }
            }
            "resume_subagent" => {
                if !context.permissions.can_create_subagents {
                    Some(
                        "Error: Only the main agent can use the resume_subagent function.".to_string(),
                    )
                } else {
                    None
                }
            }
            "store_context" => {
                if !context.permissions.can_store_context {
                    Some(format!(
                        "Error: {:?} agents cannot store context.",
                        context.agent_type
                    ))
                } else {
                    None
                }
            }
            "read_file" => {
                if !context.permissions.can_read_files {
                    Some(format!(
                        "Error: {:?} agents cannot read files.",
                        context.agent_type
                    ))
                } else {
                    None
                }
            }
            "apply_patch" => {
                if !context.permissions.can_write_files {
                    Some(format!(
                        "Error: {:?} agents cannot use apply_patch. Only Coder agents can edit files.",
                        context.agent_type
                    ))
                } else {
                    None
                }
            }
            "update_plan" => {
                // update_plan is available to all agent types
                None
            }
            _ => None, // Allow other functions by default
        }
    }

    async fn handle_shell_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        #[derive(Deserialize)]
        struct ShellArgs {
            command: Vec<String>,
        }

        let args = match serde_json::from_str::<ShellArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse shell arguments: {}", e);
                return FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Failed to parse function arguments: {}", e),
                    success: Some(false),
                };
            }
        };

        // Pass the command array as JSON string to preserve shell operators
        let command_json = serde_json::to_string(&args.command).unwrap_or_else(|_| "[]".to_string());
        self.executor.execute_shell(command_json, context).await
    }

    async fn handle_read_file_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        #[derive(Deserialize)]
        struct ReadFileArgs {
            file_path: String,
            #[serde(default)]
            line_offset: Option<usize>,
            #[serde(default)]
            line_num: Option<usize>,
        }

        let args = match serde_json::from_str::<ReadFileArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse read_file arguments: {}", e);
                return FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Failed to parse function arguments: {}", e),
                    success: Some(false),
                };
            }
        };

        debug!("Reading file: {} (offset: {:?}, lines: {:?})", args.file_path, args.line_offset, args.line_num);
        self.executor
            .execute_read_file(args.file_path, args.line_offset, args.line_num, context)
            .await
    }

    async fn handle_write_file_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        #[derive(Deserialize)]
        struct WriteFileArgs {
            file_path: String,
            content: String,
        }

        let args = match serde_json::from_str::<WriteFileArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse write_file arguments: {}", e);
                return FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Failed to parse function arguments: {}", e),
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
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        #[derive(Deserialize)]
        struct StoreContextArgs {
            id: String,
            summary: String,
            content: String,
        }

        let args = match serde_json::from_str::<StoreContextArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse store_context arguments: {}", e);
                return FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Failed to parse function arguments: {}", e),
                    success: Some(false),
                };
            }
        };

        debug!("Storing context: {} - {}", args.id, args.summary);
        self.executor
            .execute_store_context(args.id, args.summary, args.content, context)
            .await
    }

    async fn handle_update_context_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        #[derive(Deserialize)]
        struct UpdateContextArgs {
            id: String,
            content: String,
            reason: String,
        }

        let args = match serde_json::from_str::<UpdateContextArgs>(&arguments) {
            Ok(args) => args,
            Err(e) => {
                error!("Failed to parse update_context arguments: {}", e);
                return FunctionCallOutputPayload {
            content_items: None,
                    content: format!("Failed to parse function arguments: {}", e),
                    success: Some(false),
                };
            }
        };

        debug!("Updating context: {} - {}", args.id, args.reason);
        self.executor
            .execute_update_context(args.id, args.content, args.reason, context)
            .await
    }

    async fn handle_create_subagent_task_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        debug!("Creating subagent task with arguments: {}", arguments);
        self.executor
            .execute_create_subagent_task(arguments, context)
            .await
    }

    async fn handle_resume_subagent_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        debug!("Resuming subagent with arguments: {}", arguments);
        self.executor
            .execute_resume_subagent(arguments, context)
            .await
    }

    async fn handle_apply_patch_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        self.executor
            .execute_apply_patch(arguments, context)
            .await
    }

    async fn handle_update_plan_call(
        &self,
        arguments: String,
        context: &UniversalFunctionCallContext,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        self.executor
            .execute_update_plan(arguments, context)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct MockExecutor;

    #[async_trait::async_trait]
    impl UniversalFunctionExecutor for MockExecutor {
        async fn execute_shell(
            &self,
            _command: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "shell executed".to_string(),
                success: Some(true),
            }
        }

        async fn execute_read_file(
            &self,
            _file_path: String,
            _line_offset: Option<usize>,
            _line_num: Option<usize>,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "file content".to_string(),
                success: Some(true),
            }
        }

        async fn execute_write_file(
            &self,
            _file_path: String,
            _content: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "file written".to_string(),
                success: Some(true),
            }
        }

        async fn execute_store_context(
            &self,
            _id: String,
            _summary: String,
            _content: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "context stored".to_string(),
                success: Some(true),
            }
        }

        async fn execute_update_context(
            &self,
            _id: String,
            _content: String,
            _reason: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "context updated".to_string(),
                success: Some(true),
            }
        }

        async fn execute_create_subagent_task(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "subagent created".to_string(),
                success: Some(true),
            }
        }

        async fn execute_resume_subagent(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "subagent resumed".to_string(),
                success: Some(true),
            }
        }

        async fn execute_mcp_tool(
            &self,
            _tool_name: String,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "mcp tool executed".to_string(),
                success: Some(true),
            }
        }

        async fn execute_apply_patch(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "patch applied".to_string(),
                success: Some(true),
            }
        }

        async fn execute_update_plan(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            content_items: None,
            FunctionCallOutputPayload {
            content_items: None,
                content: "plan updated".to_string(),
                success: Some(true),
            }
        }
    }

    #[tokio::test]
    async fn test_permission_checking() {
        let executor = Arc::new(MockExecutor);
        let handler = UniversalFunctionCallHandler::new(executor);

        // Test Explorer permissions (cannot write files)
        let explorer_context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "test".to_string(),
            call_id: "call1".to_string(),
            agent_type: AgentType::Explorer,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Explorer),
        };

        let result = handler
            .handle_function_call(
                "write_file".to_string(),
                r#"{"file_path": "test.txt", "content": "test"}"#.to_string(),
                explorer_context,
            )
            .await;

        if let ResponseInputItem::FunctionCallOutput { output, .. } = result {
            assert_eq!(output.success, Some(false));
            assert!(output.content.contains("Explorer agents cannot write files"));
        } else {
            panic!("Expected FunctionCallOutput");
        }
    }

    #[tokio::test]
    async fn test_successful_function_call() {
        let executor = Arc::new(MockExecutor);
        let handler = UniversalFunctionCallHandler::new(executor);

        let main_context = UniversalFunctionCallContext {
            cwd: PathBuf::from("/tmp"),
            sub_id: "test".to_string(),
            call_id: "call1".to_string(),
            agent_type: AgentType::Main,
            permissions: FunctionPermissions::for_agent_type(&AgentType::Main),
        };

        let result = handler
            .handle_function_call(
                "read_file".to_string(),
                r#"{"file_path": "test.txt"}"#.to_string(),
                main_context,
            )
            .await;

        if let ResponseInputItem::FunctionCallOutput { output, .. } = result {
            assert_eq!(output.success, Some(true));
            assert_eq!(output.content, "file content");
        } else {
            panic!("Expected FunctionCallOutput");
        }
    }
}