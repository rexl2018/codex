use std::collections::HashMap;
use serde::{Deserialize, Serialize};

use crate::unified_function_handler::AgentType;


/// Unified tool configuration that can be used across different agent types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnifiedToolConfig {
    /// Base tools available to all agents
    pub base_tools: Vec<ToolDefinition>,
    /// Tools specific to main agents
    pub main_agent_tools: Vec<ToolDefinition>,
    /// Tools specific to explorer agents
    pub explorer_agent_tools: Vec<ToolDefinition>,
    /// Tools specific to coder agents
    pub coder_agent_tools: Vec<ToolDefinition>,
    /// MCP tools configuration
    pub mcp_tools: McpToolsConfig,
    /// Execution configuration
    pub execution_config: ExecutionConfig,
}

/// Definition of a single tool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    /// Tool name
    pub name: String,
    /// Tool description
    pub description: String,
    /// Tool type
    pub tool_type: ToolType,
    /// Whether the tool is enabled
    pub enabled: bool,
    /// Tool-specific configuration
    pub config: ToolSpecificConfig,
    /// Required permissions
    pub required_permissions: Vec<Permission>,
}

/// Types of tools available
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ToolType {
    /// Shell command execution
    Shell,
    /// File system operations
    FileSystem,
    /// Context management
    Context,
    /// Subagent management
    SubagentManagement,
    /// Web search
    WebSearch,
    /// Image processing
    ImageProcessing,
    /// MCP tool
    Mcp,
    /// Custom tool
    Custom(String),
}

/// Tool-specific configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpecificConfig {
    /// Shell tool configuration
    pub shell: Option<ShellToolConfig>,
    /// File system tool configuration
    pub filesystem: Option<FileSystemToolConfig>,
    /// Context tool configuration
    pub context: Option<ContextToolConfig>,
    /// MCP tool configuration
    pub mcp: Option<McpToolConfig>,
    /// Custom configuration as JSON
    pub custom: Option<serde_json::Value>,
}

/// Shell tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShellToolConfig {
    /// Maximum execution time in milliseconds
    pub max_execution_time_ms: Option<u64>,
    /// Whether to use streamable shell tool
    pub use_streamable: bool,
    /// Allowed shell types
    pub allowed_shells: Vec<String>,
    /// Environment variables to set
    pub environment_vars: HashMap<String, String>,
}

/// File system tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileSystemToolConfig {
    /// Maximum file size for reading (in bytes)
    pub max_read_size_bytes: Option<u64>,
    /// Maximum file size for writing (in bytes)
    pub max_write_size_bytes: Option<u64>,
    /// Allowed file extensions for reading
    pub allowed_read_extensions: Vec<String>,
    /// Allowed file extensions for writing
    pub allowed_write_extensions: Vec<String>,
    /// Restricted paths (cannot access)
    pub restricted_paths: Vec<String>,
}

/// Context tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextToolConfig {
    /// Maximum number of contexts to store
    pub max_contexts: Option<u32>,
    /// Maximum context content size
    pub max_content_size_bytes: Option<u64>,
    /// Whether to enable context search
    pub enable_search: bool,
}

/// MCP tool configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolConfig {
    /// Server name for MCP tool
    pub server_name: String,
    /// Tool name on the server
    pub tool_name: String,
    /// Whether the tool is enabled
    pub enabled: bool,
    /// Tool-specific parameters
    pub parameters: HashMap<String, serde_json::Value>,
}

/// MCP tools configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpToolsConfig {
    /// Whether MCP tools are enabled
    pub enabled: bool,
    /// Available MCP servers
    pub servers: HashMap<String, McpServerConfig>,
    /// Default timeout for MCP calls
    pub default_timeout_ms: u64,
}

/// MCP server configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerConfig {
    /// Server name
    pub name: String,
    /// Server endpoint or command
    pub endpoint: String,
    /// Whether the server is enabled
    pub enabled: bool,
    /// Available tools on this server
    pub tools: Vec<String>,
}

/// Execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Default timeout for tool execution
    pub default_timeout_ms: u64,
    /// Maximum concurrent tool executions
    pub max_concurrent_executions: u32,
    /// Whether to enable tool execution logging
    pub enable_logging: bool,
    /// Whether to enable tool execution metrics
    pub enable_metrics: bool,
}

/// Permissions required for tool execution
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Permission {
    /// Can read files
    ReadFiles,
    /// Can write files
    WriteFiles,
    /// Can execute shell commands
    ExecuteShell,
    /// Can store context
    StoreContext,
    /// Can create subagents
    CreateSubagents,
    /// Can access network
    NetworkAccess,
    /// Can access MCP tools
    McpAccess,
}

impl UnifiedToolConfig {
    /// Create a default configuration
    pub fn default() -> Self {
        Self {
            base_tools: Self::create_base_tools(),
            main_agent_tools: Self::create_main_agent_tools(),
            explorer_agent_tools: Self::create_explorer_agent_tools(),
            coder_agent_tools: Self::create_coder_agent_tools(),
            mcp_tools: McpToolsConfig {
                enabled: false,
                servers: HashMap::new(),
                default_timeout_ms: 30000,
            },
            execution_config: ExecutionConfig {
                default_timeout_ms: 30000,
                max_concurrent_executions: 5,
                enable_logging: true,
                enable_metrics: false,
            },
        }
    }

    /// Get tools available for a specific agent type
    pub fn get_tools_for_agent(&self, agent_type: &AgentType) -> Vec<ToolDefinition> {
        let mut tools = Vec::new();

        match agent_type {
            AgentType::Main => {
                // Main agent only gets its specific tools, no base tools
                tools.extend(self.main_agent_tools.clone());
            }
            AgentType::Explorer => {
                // Subagents get base tools + their specific tools
                tools.extend(self.base_tools.clone());
                tools.extend(self.explorer_agent_tools.clone());
            }
            AgentType::Coder => {
                // Subagents get base tools + their specific tools
                tools.extend(self.base_tools.clone());
                tools.extend(self.coder_agent_tools.clone());
            }
        }

        // Filter enabled tools
        tools.into_iter().filter(|tool| tool.enabled).collect()
    }

    /// Check if an agent has permission to use a tool
    pub fn has_permission(&self, agent_type: &AgentType, tool_name: &str) -> bool {
        let tools = self.get_tools_for_agent(agent_type);
        
        if let Some(tool) = tools.iter().find(|t| t.name == tool_name) {
            let agent_permissions = self.get_agent_permissions(agent_type);
            tool.required_permissions
                .iter()
                .all(|perm| agent_permissions.contains(perm))
        } else {
            false
        }
    }

    /// Get permissions for an agent type
    pub fn get_agent_permissions(&self, agent_type: &AgentType) -> Vec<Permission> {
        match agent_type {
            AgentType::Main => vec![
                // Main agent should primarily be a coordinator
                // Only allow context management and subagent creation
                Permission::StoreContext,
                Permission::CreateSubagents,
                // Allow limited read access for context gathering
                Permission::ReadFiles,
            ],
            AgentType::Explorer => vec![
                Permission::ReadFiles,
                Permission::ExecuteShell,
                Permission::StoreContext,
                Permission::NetworkAccess,
                Permission::McpAccess,
                // Note: Explorer should NOT have WriteFiles permission
            ],
            AgentType::Coder => vec![
                Permission::ReadFiles,
                Permission::WriteFiles,
                Permission::ExecuteShell,
                Permission::StoreContext,
                Permission::McpAccess,
            ],
        }
    }

    /// Create base tools available to all agents
    fn create_base_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "read_file".to_string(),
                description: "Read the contents of a file".to_string(),
                tool_type: ToolType::FileSystem,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: Some(FileSystemToolConfig {
                        max_read_size_bytes: Some(10 * 1024 * 1024), // 10MB
                        max_write_size_bytes: None,
                        allowed_read_extensions: vec![], // Allow all
                        allowed_write_extensions: vec![],
                        restricted_paths: vec![
                            "/etc/passwd".to_string(),
                            "/etc/shadow".to_string(),
                        ],
                    }),
                    context: None,
                    mcp: None,
                    custom: None,
                },
                required_permissions: vec![Permission::ReadFiles],
            },
            ToolDefinition {
                name: "shell".to_string(),
                description: "Execute shell commands".to_string(),
                tool_type: ToolType::Shell,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: Some(ShellToolConfig {
                        max_execution_time_ms: Some(300000), // 5 minutes
                        use_streamable: false,
                        allowed_shells: vec!["bash".to_string(), "sh".to_string()],
                        environment_vars: HashMap::new(),
                    }),
                    filesystem: None,
                    context: None,
                    mcp: None,
                    custom: None,
                },
                required_permissions: vec![Permission::ExecuteShell],
            },
            ToolDefinition {
                name: "store_context".to_string(),
                description: "Store context information for later retrieval".to_string(),
                tool_type: ToolType::Context,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: None,
                    context: Some(ContextToolConfig {
                        max_contexts: Some(100),
                        max_content_size_bytes: Some(1024 * 1024), // 1MB
                        enable_search: true,
                    }),
                    mcp: None,
                    custom: None,
                },
                required_permissions: vec![Permission::StoreContext],
            },
            ToolDefinition {
                name: "update_context".to_string(),
                description: "Update the content of an existing context item".to_string(),
                tool_type: ToolType::Context,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: None,
                    context: Some(ContextToolConfig {
                        max_contexts: Some(100),
                        max_content_size_bytes: Some(1024 * 1024), // 1MB
                        enable_search: true,
                    }),
                    mcp: None,
                    custom: None,
                },
                required_permissions: vec![Permission::StoreContext],
            },
            ToolDefinition {
                name: "update_plan".to_string(),
                description: "Update the task plan with steps and status".to_string(),
                tool_type: ToolType::Custom("plan".to_string()),
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: None,
                    context: None,
                    mcp: None,
                    custom: Some(serde_json::json!({
                        "max_steps": 20,
                        "allow_parallel_steps": false
                    })),
                },
                required_permissions: vec![], // No special permissions needed
            },
        ]
    }

    /// Create tools specific to main agents
    fn create_main_agent_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "list_contexts".to_string(),
                description: "List available contexts created in this session".to_string(),
                tool_type: ToolType::Context,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: Some(ContextToolConfig { max_contexts: None, max_content_size_bytes: None, enable_search: true }), mcp: None, custom: None },
                required_permissions: vec![Permission::StoreContext, Permission::ReadFiles],
            },
            ToolDefinition {
                name: "multi_retrieve_contexts".to_string(),
                description: "Retrieve multiple contexts by ID".to_string(),
                tool_type: ToolType::Context,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: Some(ContextToolConfig { max_contexts: Some(50), max_content_size_bytes: None, enable_search: true }), mcp: None, custom: None },
                required_permissions: vec![Permission::StoreContext, Permission::ReadFiles],
            },
            ToolDefinition {
                name: "create_subagent_task".to_string(),
                description: "Create a new subagent task".to_string(),
                tool_type: ToolType::SubagentManagement,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: None, mcp: None, custom: None },
                required_permissions: vec![Permission::CreateSubagents],
            },
            ToolDefinition {
                name: "resume_subagent".to_string(),
                description: "Resume a previously completed/failed/cancelled subagent task using the same task_id".to_string(),
                tool_type: ToolType::SubagentManagement,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: None, mcp: None, custom: None },
                required_permissions: vec![Permission::CreateSubagents],
            },
            ToolDefinition {
                name: "list_recently_completed_subagents".to_string(),
                description: "List recently completed/failed/cancelled subagent tasks".to_string(),
                tool_type: ToolType::SubagentManagement,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: None, mcp: None, custom: None },
                required_permissions: vec![Permission::CreateSubagents],
            },
            ToolDefinition {
                name: "multi_get_subagent_report".to_string(),
                description: "Retrieve the reports for multiple subagent tasks".to_string(),
                tool_type: ToolType::SubagentManagement,
                enabled: true,
                config: ToolSpecificConfig { shell: None, filesystem: None, context: None, mcp: None, custom: None },
                required_permissions: vec![Permission::CreateSubagents],
            },
        ]
    }

    /// Create tools specific to explorer agents
    fn create_explorer_agent_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "web_search".to_string(),
                description: "Search the web for information".to_string(),
                tool_type: ToolType::WebSearch,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: None,
                    context: None,
                    mcp: None,
                    custom: Some(serde_json::json!({
                        "max_results": 10,
                        "timeout_ms": 30000
                    })),
                },
                required_permissions: vec![Permission::NetworkAccess],
            },
        ]
    }

    /// Create tools specific to coder agents
    fn create_coder_agent_tools() -> Vec<ToolDefinition> {
        vec![
            ToolDefinition {
                name: "write_file".to_string(),
                description: "Write content to a file".to_string(),
                tool_type: ToolType::FileSystem,
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: Some(FileSystemToolConfig {
                        max_read_size_bytes: None,
                        max_write_size_bytes: Some(5 * 1024 * 1024), // 5MB for coder
                        allowed_read_extensions: vec![],
                        allowed_write_extensions: vec![
                            "rs".to_string(),
                            "py".to_string(),
                            "js".to_string(),
                            "ts".to_string(),
                            "md".to_string(),
                            "txt".to_string(),
                        ],
                        restricted_paths: vec![
                            "/etc/".to_string(),
                            "/usr/".to_string(),
                            "/sys/".to_string(),
                        ],
                    }),
                    context: None,
                    mcp: None,
                    custom: None,
                },
                required_permissions: vec![Permission::WriteFiles],
            },
            ToolDefinition {
                name: "apply_patch".to_string(),
                description: "Apply structured patches to edit files".to_string(),
                tool_type: ToolType::Custom("apply_patch".to_string()),
                enabled: true,
                config: ToolSpecificConfig {
                    shell: None,
                    filesystem: None,
                    context: None,
                    mcp: None,
                    custom: Some(serde_json::json!({
                        "max_patch_size_bytes": 1024 * 1024, // 1MB
                        "allow_file_creation": true,
                        "allow_file_deletion": true
                    })),
                },
                required_permissions: vec![Permission::WriteFiles],
            },
        ]
    }


}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = UnifiedToolConfig::default();
        
        // Check that base tools are available
        assert!(!config.base_tools.is_empty());
        
        // Check that different agent types have different tools
        let main_tools = config.get_tools_for_agent(&AgentType::Main);
        let explorer_tools = config.get_tools_for_agent(&AgentType::Explorer);
        let coder_tools = config.get_tools_for_agent(&AgentType::Coder);
        
        // Main agent should have the most tools
        assert!(main_tools.len() >= explorer_tools.len());
        assert!(main_tools.len() >= coder_tools.len());
        
        // Check that subagents have basic tools, but main agent doesn't
        assert!(!main_tools.iter().any(|t| t.name == "read_file")); // Main agent should NOT have read_file
        assert!(explorer_tools.iter().any(|t| t.name == "read_file"));
        assert!(coder_tools.iter().any(|t| t.name == "read_file"));
    }

    #[test]
    fn test_permission_checking() {
        let config = UnifiedToolConfig::default();
        
        // Main agent should only have coordinator permissions (no base tools)
        assert!(!config.has_permission(&AgentType::Main, "read_file")); // Main agent should NOT have read_file
        assert!(!config.has_permission(&AgentType::Main, "shell")); // No shell access
        assert!(!config.has_permission(&AgentType::Main, "store_context")); // No store_context access
        assert!(config.has_permission(&AgentType::Main, "create_subagent_task"));
        assert!(config.has_permission(&AgentType::Main, "list_contexts"));
        assert!(config.has_permission(&AgentType::Main, "multi_retrieve_contexts"));
        
        // Explorer should not have write permissions
        assert!(config.has_permission(&AgentType::Explorer, "read_file"));
        assert!(!config.has_permission(&AgentType::Explorer, "write_file")); // Explorer should NOT have write_file
        assert!(config.has_permission(&AgentType::Explorer, "shell")); // Explorer can use shell
        assert!(!config.has_permission(&AgentType::Explorer, "create_subagent_task"));
        
        // Coder should have write but not subagent creation
        assert!(config.has_permission(&AgentType::Coder, "read_file"));
        assert!(config.has_permission(&AgentType::Coder, "write_file")); // Coder should have write_file
        assert!(config.has_permission(&AgentType::Coder, "shell")); // Coder can use shell
        assert!(!config.has_permission(&AgentType::Coder, "create_subagent_task"));
    }
}