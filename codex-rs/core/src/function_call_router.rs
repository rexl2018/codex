use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseInputItem;
use serde_json;
use tracing::debug;
use tracing::error;
use tracing::info;
use tracing::warn;

use crate::context_store::IContextRepository;
use crate::mcp_connection_manager::McpConnectionManager;
use crate::unified_function_handler::AgentType;
use crate::unified_function_handler::FunctionPermissions;
use crate::unified_function_handler::UniversalFunctionCallContext;
use crate::unified_function_handler::UniversalFunctionCallHandler;
use crate::unified_function_handler::UniversalFunctionExecutor;

/// Configuration for the function call router
#[derive(Debug, Clone)]
pub struct FunctionCallRouterConfig {
    pub cwd: PathBuf,
    pub agent_type: AgentType,
    pub enable_mcp_tools: bool,
    pub max_execution_time_ms: Option<u64>,
}

/// Router that manages function call execution across different contexts
pub struct FunctionCallRouter<E: UniversalFunctionExecutor> {
    handler: UniversalFunctionCallHandler<E>,
    config: FunctionCallRouterConfig,
    mcp_connection_manager: Option<Arc<McpConnectionManager>>,
    context_repository: Option<Arc<dyn IContextRepository>>,
}

impl<E: UniversalFunctionExecutor> FunctionCallRouter<E> {
    pub fn new(
        executor: Arc<E>,
        config: FunctionCallRouterConfig,
        mcp_connection_manager: Option<Arc<McpConnectionManager>>,
        context_repository: Option<Arc<dyn IContextRepository>>,
    ) -> Self {
        let handler = UniversalFunctionCallHandler::new(executor);

        Self {
            handler,
            config,
            mcp_connection_manager,
            context_repository,
        }
    }

    /// Route a function call to the appropriate handler
    pub async fn route_function_call(
        &self,
        name: String,
        arguments: String,
        sub_id: String,
        call_id: String,
    ) -> ResponseInputItem {
        debug!(
            "Routing function call: {} for agent type: {:?}",
            name, self.config.agent_type
        );

        // Create execution context
        let context = UniversalFunctionCallContext {
            cwd: self.config.cwd.clone(),
            sub_id: sub_id.clone(),
            call_id: call_id.clone(),
            agent_type: self.config.agent_type.clone(),
            permissions: FunctionPermissions::for_agent_type(&self.config.agent_type),
        };

        // Check if this is an MCP tool call
        if self.is_mcp_tool(&name) {
            return self.handle_mcp_tool_call(name, arguments, context).await;
        }

        // Route to the universal handler
        self.handler
            .handle_function_call(name, arguments, context)
            .await
    }

    /// Check if a function name corresponds to an MCP tool
    fn is_mcp_tool(&self, function_name: &str) -> bool {
        if !self.config.enable_mcp_tools {
            return false;
        }

        // Check for MCP tools with different delimiter patterns:
        // 1. "server/tool" format
        // 2. "server__tool" format (used by claude-tools)
        // 3. Query the MCP connection manager to see if the tool exists
        if function_name.contains('/') || function_name.contains("__") {
            return true;
        }

        // Check if this is a known MCP tool (hardcoded list)
        if self.is_known_mcp_tool(function_name) {
            return true;
        }

        // Query the MCP connection manager to see if this tool exists
        if let Some(mcp_manager) = &self.mcp_connection_manager {
            // Use the parse_tool_name method which handles various formats
            if mcp_manager.parse_tool_name(function_name).is_some() {
                return true;
            }
        }

        false
    }

    /// Check if this is a known MCP tool (without server prefix)
    fn is_known_mcp_tool(&self, function_name: &str) -> bool {
        // List of common MCP tool names that might not have server prefixes
        let known_mcp_tools = [
            "filesystem_read",
            "filesystem_write",
            "filesystem_list",
            "web_search",
            "browser_action",
            "database_query",
        ];

        known_mcp_tools.contains(&function_name)
    }

    /// Handle MCP tool calls
    async fn handle_mcp_tool_call(
        &self,
        name: String,
        arguments: String,
        context: UniversalFunctionCallContext,
    ) -> ResponseInputItem {
        debug!("Handling MCP tool call: {}", name);

        if let Some(mcp_manager) = &self.mcp_connection_manager {
            // Parse server name and tool name
            let (server_name, tool_name) = self.parse_mcp_tool_name(&name);

            // Execute MCP tool through the connection manager
            match self
                .execute_mcp_tool_via_manager(mcp_manager, &server_name, &tool_name, &arguments)
                .await
            {
                Ok(result) => ResponseInputItem::FunctionCallOutput {
                    call_id: context.call_id,
                    output: FunctionCallOutputPayload {
                        content: result,
                        success: Some(true),
                    },
                },
                Err(error) => {
                    error!("MCP tool execution failed: {}", error);
                    ResponseInputItem::FunctionCallOutput {
                        call_id: context.call_id,
                        output: FunctionCallOutputPayload {
                            content: format!("MCP tool execution failed: {}", error),
                            success: Some(false),
                        },
                    }
                }
            }
        } else {
            warn!("MCP tool call attempted but no MCP connection manager available");
            ResponseInputItem::FunctionCallOutput {
                call_id: context.call_id,
                output: FunctionCallOutputPayload {
                    content: "MCP tools are not available in this context".to_string(),
                    success: Some(false),
                },
            }
        }
    }

    /// Parse MCP tool name into server and tool components
    fn parse_mcp_tool_name(&self, tool_name: &str) -> (String, String) {
        // First try to use the MCP connection manager's sophisticated parsing
        if let Some(mcp_manager) = &self.mcp_connection_manager {
            if let Some((server_name, actual_tool_name)) = mcp_manager.parse_tool_name(tool_name) {
                return (server_name, actual_tool_name);
            }
        }

        // Fallback to manual parsing for different delimiter patterns
        if let Some(slash_pos) = tool_name.find('/') {
            // Handle "server/tool" format
            let server_name = tool_name[..slash_pos].to_string();
            let tool_name = tool_name[slash_pos + 1..].to_string();
            (server_name, tool_name)
        } else if let Some(double_underscore_pos) = tool_name.find("__") {
            // Handle "server__tool" format (used by claude-tools)
            let server_name = tool_name[..double_underscore_pos].to_string();
            let tool_name = tool_name[double_underscore_pos + 2..].to_string();
            (server_name, tool_name)
        } else {
            // Default server name if not specified
            ("default".to_string(), tool_name.to_string())
        }
    }

    /// Execute MCP tool via the connection manager
    async fn execute_mcp_tool_via_manager(
        &self,
        mcp_manager: &McpConnectionManager,
        server_name: &str,
        tool_name: &str,
        arguments: &str,
    ) -> Result<String, String> {
        // Parse arguments as JSON
        let args_value: serde_json::Value = serde_json::from_str(arguments)
            .map_err(|e| format!("Failed to parse MCP tool arguments: {}", e))?;

        // Execute the MCP tool
        match mcp_manager
            .call_tool(server_name, tool_name, Some(args_value), None)
            .await
        {
            Ok(result) => {
                // Format the result for display
                self.format_mcp_result(&result)
            }
            Err(e) => Err(format!("MCP tool call failed: {}", e)),
        }
    }

    /// Format MCP tool result for display
    fn format_mcp_result(&self, result: &mcp_types::CallToolResult) -> Result<String, String> {
        if result.is_error.unwrap_or(false) {
            return Err(format!("MCP tool returned error: {:?}", result.content));
        }

        // Extract content from the result
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

        Ok(output.trim().to_string())
    }

    /// Get available function names for this router configuration
    pub fn get_available_functions(&self) -> Vec<String> {
        let mut functions = vec![
            "shell".to_string(),
            "read_file".to_string(),
            "store_context".to_string(),
        ];

        // Add functions based on agent permissions
        match self.config.agent_type {
            AgentType::Main => {
                functions.push("write_file".to_string());
                functions.push("create_subagent_task".to_string());
            }
            AgentType::Coder => {
                functions.push("write_file".to_string());
            }
            AgentType::Explorer => {
                // Explorer has the base functions only
            }
        }

        // Add MCP tools if enabled
        if self.config.enable_mcp_tools {
            if let Some(mcp_manager) = &self.mcp_connection_manager {
                // Get available MCP tools from the connection manager
                // This would require extending the McpConnectionManager interface
                // For now, we'll add some common MCP tool names
                functions.extend([
                    "filesystem_read".to_string(),
                    "filesystem_write".to_string(),
                    "web_search".to_string(),
                ]);
            }
        }

        functions
    }

    /// Update router configuration
    pub fn update_config(&mut self, config: FunctionCallRouterConfig) {
        self.config = config;
    }

    /// Check if a function is available for the current agent type
    pub fn is_function_available(&self, function_name: &str) -> bool {
        let available_functions = self.get_available_functions();
        available_functions.contains(&function_name.to_string())
    }
}

/// Builder for creating function call routers with different configurations
pub struct FunctionCallRouterBuilder<E: UniversalFunctionExecutor> {
    executor: Option<Arc<E>>,
    config: Option<FunctionCallRouterConfig>,
    mcp_connection_manager: Option<Arc<McpConnectionManager>>,
    context_repository: Option<Arc<dyn IContextRepository>>,
}

impl<E: UniversalFunctionExecutor> FunctionCallRouterBuilder<E> {
    pub fn new() -> Self {
        Self {
            executor: None,
            config: None,
            mcp_connection_manager: None,
            context_repository: None,
        }
    }

    pub fn with_executor(mut self, executor: Arc<E>) -> Self {
        self.executor = Some(executor);
        self
    }

    pub fn with_config(mut self, config: FunctionCallRouterConfig) -> Self {
        self.config = Some(config);
        self
    }

    pub fn with_mcp_connection_manager(
        mut self,
        mcp_connection_manager: Arc<McpConnectionManager>,
    ) -> Self {
        self.mcp_connection_manager = Some(mcp_connection_manager);
        self
    }

    pub fn with_context_repository(
        mut self,
        context_repository: Arc<dyn IContextRepository>,
    ) -> Self {
        self.context_repository = Some(context_repository);
        self
    }

    pub fn build(self) -> Result<FunctionCallRouter<E>, String> {
        let executor = self.executor.ok_or("Executor is required")?;
        let config = self.config.ok_or("Config is required")?;

        Ok(FunctionCallRouter::new(
            executor,
            config,
            self.mcp_connection_manager,
            self.context_repository,
        ))
    }
}

impl<E: UniversalFunctionExecutor> Default for FunctionCallRouterBuilder<E> {
    fn default() -> Self {
        Self::new()
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
            FunctionCallOutputPayload {
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
            FunctionCallOutputPayload {
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
            FunctionCallOutputPayload {
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
            FunctionCallOutputPayload {
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
            FunctionCallOutputPayload {
                content: "context updated".to_string(),
                success: Some(true),
            }
        }

        async fn execute_create_subagent_task(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            FunctionCallOutputPayload {
                content: "subagent created".to_string(),
                success: Some(true),
            }
        }

        async fn execute_resume_subagent(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            FunctionCallOutputPayload {
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
            FunctionCallOutputPayload {
                content: "mcp tool executed".to_string(),
                success: Some(true),
            }
        }

        async fn execute_apply_patch(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            FunctionCallOutputPayload {
                content: "patch applied".to_string(),
                success: Some(true),
            }
        }

        async fn execute_update_plan(
            &self,
            _arguments: String,
            _context: &UniversalFunctionCallContext,
        ) -> FunctionCallOutputPayload {
            FunctionCallOutputPayload {
                content: "plan updated".to_string(),
                success: Some(true),
            }
        }
    }

    #[tokio::test]
    async fn test_router_builder() {
        let executor = Arc::new(MockExecutor);
        let config = FunctionCallRouterConfig {
            cwd: PathBuf::from("/tmp"),
            agent_type: AgentType::Main,
            enable_mcp_tools: false,
            max_execution_time_ms: Some(30000),
        };

        let router = FunctionCallRouterBuilder::new()
            .with_executor(executor)
            .with_config(config)
            .build()
            .expect("Failed to build router");

        let available_functions = router.get_available_functions();
        assert!(available_functions.contains(&"shell".to_string()));
        assert!(available_functions.contains(&"read_file".to_string()));
        assert!(available_functions.contains(&"write_file".to_string()));
        assert!(available_functions.contains(&"create_subagent_task".to_string()));
    }

    #[tokio::test]
    async fn test_function_routing() {
        let executor = Arc::new(MockExecutor);
        let config = FunctionCallRouterConfig {
            cwd: PathBuf::from("/test"),
            agent_type: AgentType::Explorer,
            enable_mcp_tools: true,
            max_execution_time_ms: Some(5000),
        };

        let router = FunctionCallRouter::new(executor, config, None, None);

        // Test shell function routing
        let result = router
            .route_function_call(
                "shell".to_string(),
                r#"{"command": ["ls"]}"#.to_string(),
                "test_sub".to_string(),
                "test_call".to_string(),
            )
            .await;

        match result {
            ResponseInputItem::FunctionCallOutput { call_id, output } => {
                assert_eq!(call_id, "test_call");
                assert_eq!(output.content, "shell executed");
                assert_eq!(output.success, Some(true));
            }
            _ => panic!("Expected FunctionCallOutput"),
        }
    }

    #[test]
    fn test_mcp_tool_detection() {
        let executor = Arc::new(MockExecutor);
        let config = FunctionCallRouterConfig {
            cwd: PathBuf::from("/test"),
            agent_type: AgentType::Explorer,
            enable_mcp_tools: true,
            max_execution_time_ms: Some(5000),
        };

        let router = FunctionCallRouter::new(executor, config, None, None);

        // Test different MCP tool patterns
        assert!(router.is_mcp_tool("server/tool")); // slash delimiter
        assert!(router.is_mcp_tool("claude-tools__Bash")); // double underscore delimiter
        assert!(router.is_mcp_tool("server__tool")); // double underscore delimiter
        assert!(router.is_mcp_tool("filesystem_read")); // known MCP tool

        // Test non-MCP tools
        assert!(!router.is_mcp_tool("shell"));
        assert!(!router.is_mcp_tool("read_file"));
        assert!(!router.is_mcp_tool("write_file"));
        assert!(!router.is_mcp_tool("unknown_tool"));
    }

    #[test]
    fn test_mcp_tool_detection_disabled() {
        let executor = Arc::new(MockExecutor);
        let config = FunctionCallRouterConfig {
            cwd: PathBuf::from("/test"),
            agent_type: AgentType::Explorer,
            enable_mcp_tools: false, // MCP tools disabled
            max_execution_time_ms: Some(5000),
        };

        let router = FunctionCallRouter::new(executor, config, None, None);

        // All MCP tool patterns should return false when disabled
        assert!(!router.is_mcp_tool("server/tool"));
        assert!(!router.is_mcp_tool("claude-tools__Bash"));
        assert!(!router.is_mcp_tool("server__tool"));
        assert!(!router.is_mcp_tool("filesystem_read"));
    }

    #[test]
    fn test_parse_mcp_tool_name() {
        let executor = Arc::new(MockExecutor);
        let config = FunctionCallRouterConfig {
            cwd: PathBuf::from("/test"),
            agent_type: AgentType::Explorer,
            enable_mcp_tools: true,
            max_execution_time_ms: Some(5000),
        };

        let router = FunctionCallRouter::new(executor, config, None, None);

        // Test slash delimiter
        let (server, tool) = router.parse_mcp_tool_name("server/tool");
        assert_eq!(server, "server");
        assert_eq!(tool, "tool");

        // Test double underscore delimiter (claude-tools pattern)
        let (server, tool) = router.parse_mcp_tool_name("claude-tools__Bash");
        assert_eq!(server, "claude-tools");
        assert_eq!(tool, "Bash");

        // Test no delimiter (should use default server)
        let (server, tool) = router.parse_mcp_tool_name("standalone_tool");
        assert_eq!(server, "default");
        assert_eq!(tool, "standalone_tool");
    }
}
