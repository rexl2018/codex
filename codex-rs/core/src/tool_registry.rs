use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use std::collections::BTreeMap;

use crate::client_common::tools::{ToolSpec, ResponsesApiTool};
use crate::tools::spec::JsonSchema;
use crate::tool_config::{UnifiedToolConfig, ToolDefinition, ToolType, Permission};
use crate::unified_function_handler::AgentType;

/// Central registry for managing tools across the application
pub struct ToolRegistry {
    /// Tool configurations indexed by agent type
    configs: RwLock<HashMap<AgentType, UnifiedToolConfig>>,
    /// Registered tool factories
    tool_factories: RwLock<HashMap<String, Box<dyn ToolFactory + Send + Sync>>>,
    /// MCP tools cache
    mcp_tools_cache: RwLock<Option<HashMap<String, mcp_types::Tool>>>,
}

/// Factory trait for creating tools
pub trait ToolFactory: Send + Sync {
    /// Create a tool spec from the tool definition
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String>;
    
    /// Get the tool type this factory handles
    fn tool_type(&self) -> ToolType;
    
    /// Validate tool configuration
    fn validate_config(&self, definition: &ToolDefinition) -> Result<(), String>;
}

/// Factory for shell tools
pub struct ShellToolFactory;

impl ToolFactory for ShellToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::Shell {
            return Err("Invalid tool type for ShellToolFactory".to_string());
        }

        // Create shell tool based on configuration
        let shell_config = definition.config.shell.as_ref()
            .ok_or("Shell tool requires shell configuration")?;

        // Create shell tool
        let mut properties = BTreeMap::new();
        properties.insert(
            "command".to_string(),
            JsonSchema::String {
                description: Some("The shell command to execute".to_string()),
            },
        );

        Ok(ToolSpec::Function(ResponsesApiTool {
            name: "shell".to_string(),
            description: "Runs a shell command and returns its output.".to_string(),
            strict: false,
            parameters: JsonSchema::Object {
                properties,
                required: Some(vec!["command".to_string()]),
                additional_properties: Some(false.into()),
            },
        }))
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Shell
    }

    fn validate_config(&self, definition: &ToolDefinition) -> Result<(), String> {
        if definition.config.shell.is_none() {
            return Err("Shell tool requires shell configuration".to_string());
        }
        Ok(())
    }
}

/// Factory for file system tools
pub struct FileSystemToolFactory;

impl ToolFactory for FileSystemToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::FileSystem {
            return Err("Invalid tool type for FileSystemToolFactory".to_string());
        }

        match definition.name.as_str() {
            "read_file" => {
                let mut properties = BTreeMap::new();
                properties.insert("file_path".to_string(), JsonSchema::String { description: Some("Path to the file to read".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "read_file".to_string(),
                    description: "Read the contents of a file".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["file_path".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            "write_file" => {
                let mut properties = BTreeMap::new();
                properties.insert("file_path".to_string(), JsonSchema::String { description: Some("Path to the file to write".to_string()) });
                properties.insert("content".to_string(), JsonSchema::String { description: Some("Content to write to the file".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "write_file".to_string(),
                    description: "Write content to a file".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["file_path".to_string(), "content".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            _ => Err(format!("Unknown file system tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::FileSystem
    }

    fn validate_config(&self, definition: &ToolDefinition) -> Result<(), String> {
        if definition.config.filesystem.is_none() {
            return Err("File system tool requires filesystem configuration".to_string());
        }
        Ok(())
    }
}

/// Factory for context tools
pub struct ContextToolFactory;

impl ToolFactory for ContextToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::Context {
            return Err("Invalid tool type for ContextToolFactory".to_string());
        }

        match definition.name.as_str() {
            "store_context" => {
                let mut properties = BTreeMap::new();
                properties.insert("id".to_string(), JsonSchema::String { description: Some("Context ID".to_string()) });
                properties.insert("content".to_string(), JsonSchema::String { description: Some("Context content".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "store_context".to_string(),
                    description: "Store context information".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["id".to_string(), "content".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            "list_contexts" => {
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "list_contexts".to_string(),
                    description: "List available contexts".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties: BTreeMap::new(),
                        required: None,
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            _ => Err(format!("Unknown context tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Context
    }

    fn validate_config(&self, definition: &ToolDefinition) -> Result<(), String> {
        if definition.config.context.is_none() {
            return Err("Context tool requires context configuration".to_string());
        }
        Ok(())
    }
}

/// Factory for subagent management tools
pub struct SubagentToolFactory;

impl ToolFactory for SubagentToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::SubagentManagement {
            return Err("Invalid tool type for SubagentToolFactory".to_string());
        }

        match definition.name.as_str() {
            "create_subagent_task" => {
                let mut properties = BTreeMap::new();
                properties.insert("task_description".to_string(), JsonSchema::String { description: Some("Description of the subagent task".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "create_subagent_task".to_string(),
                    description: "Create a new subagent task".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["task_description".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            _ => Err(format!("Unknown subagent tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::SubagentManagement
    }

    fn validate_config(&self, _definition: &ToolDefinition) -> Result<(), String> {
        // Subagent tools don't require specific configuration validation for now
        Ok(())
    }
}

/// Factory for web search tools
pub struct WebSearchToolFactory;

impl ToolFactory for WebSearchToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::WebSearch {
            return Err("Invalid tool type for WebSearchToolFactory".to_string());
        }

        match definition.name.as_str() {
            "web_search" => Ok(ToolSpec::WebSearch {}),
            _ => Err(format!("Unknown web search tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::WebSearch
    }

    fn validate_config(&self, _definition: &ToolDefinition) -> Result<(), String> {
        // Web search tools don't require specific configuration validation for now
        Ok(())
    }
}

/// Factory for apply patch tools
pub struct ApplyPatchToolFactory;

impl ToolFactory for ApplyPatchToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if !matches!(definition.tool_type, ToolType::Custom(ref name) if name == "apply_patch") {
            return Err("Invalid tool type for ApplyPatchToolFactory".to_string());
        }

        match definition.name.as_str() {
            "apply_patch" => {
                // Create a simple apply_patch tool
                let mut properties = BTreeMap::new();
                properties.insert("patch".to_string(), JsonSchema::String { description: Some("The patch to apply".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "apply_patch".to_string(),
                    description: "Apply a patch to files".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["patch".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            _ => Err(format!("Unknown apply patch tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Custom("apply_patch".to_string())
    }

    fn validate_config(&self, _definition: &ToolDefinition) -> Result<(), String> {
        // Apply patch tools don't require specific configuration validation for now
        Ok(())
    }
}

/// Factory for plan tools
pub struct PlanToolFactory;

impl ToolFactory for PlanToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if !matches!(definition.tool_type, ToolType::Custom(ref name) if name == "plan") {
            return Err("Invalid tool type for PlanToolFactory".to_string());
        }

        match definition.name.as_str() {
            "update_plan" => {
                let mut properties = BTreeMap::new();
                properties.insert("plan".to_string(), JsonSchema::String { description: Some("The plan to update".to_string()) });
                Ok(ToolSpec::Function(ResponsesApiTool {
                    name: "update_plan".to_string(),
                    description: "Update the current plan".to_string(),
                    strict: false,
                    parameters: JsonSchema::Object {
                        properties,
                        required: Some(vec!["plan".to_string()]),
                        additional_properties: Some(false.into()),
                    },
                }))
            },
            _ => Err(format!("Unknown plan tool: {}", definition.name)),
        }
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Custom("plan".to_string())
    }

    fn validate_config(&self, _definition: &ToolDefinition) -> Result<(), String> {
        // Plan tools don't require specific configuration validation for now
        Ok(())
    }
}

/// Factory for MCP tools
pub struct McpToolFactory;

impl ToolFactory for McpToolFactory {
    fn create_tool_spec(&self, definition: &ToolDefinition) -> Result<ToolSpec, String> {
        if definition.tool_type != ToolType::Mcp {
            return Err("Invalid tool type for McpToolFactory".to_string());
        }

        // MCP tools are handled dynamically, so we create a placeholder
        // The actual MCP tool conversion happens in get_legacy_openai_tools
        Err("MCP tools are handled dynamically".to_string())
    }

    fn tool_type(&self) -> ToolType {
        ToolType::Mcp
    }

    fn validate_config(&self, _definition: &ToolDefinition) -> Result<(), String> {
        // MCP tools are validated by the MCP connection manager
        Ok(())
    }
}

impl ToolRegistry {
    /// Create a new tool registry with default configurations
    pub fn new() -> Self {
        let mut registry = Self {
            configs: RwLock::new(HashMap::new()),
            tool_factories: RwLock::new(HashMap::new()),
            mcp_tools_cache: RwLock::new(None),
        };

        // Register default tool factories
        registry.register_default_factories();

        // Initialize default configurations
        registry.initialize_default_configs();

        registry
    }

    /// Register default tool factories
    fn register_default_factories(&self) {
        let mut factories = self.tool_factories.write().unwrap();
        
        factories.insert("shell".to_string(), Box::new(ShellToolFactory));
        factories.insert("read_file".to_string(), Box::new(FileSystemToolFactory));
        factories.insert("write_file".to_string(), Box::new(FileSystemToolFactory));
        factories.insert("store_context".to_string(), Box::new(ContextToolFactory));
        factories.insert("update_context".to_string(), Box::new(ContextToolFactory));
        factories.insert("list_contexts".to_string(), Box::new(ContextToolFactory));
        factories.insert("multi_retrieve_contexts".to_string(), Box::new(ContextToolFactory));
        factories.insert("create_subagent_task".to_string(), Box::new(SubagentToolFactory));
        factories.insert("resume_subagent".to_string(), Box::new(SubagentToolFactory));
        factories.insert("list_recently_completed_subagents".to_string(), Box::new(SubagentToolFactory));
        factories.insert("multi_get_subagent_report".to_string(), Box::new(SubagentToolFactory));
        factories.insert("web_search".to_string(), Box::new(WebSearchToolFactory));
        factories.insert("update_plan".to_string(), Box::new(PlanToolFactory));
        factories.insert("apply_patch".to_string(), Box::new(ApplyPatchToolFactory));
    }

    /// Initialize default configurations for all agent types
    fn initialize_default_configs(&self) {
        let mut configs = self.configs.write().unwrap();
        
        configs.insert(AgentType::Main, UnifiedToolConfig::default());
        configs.insert(AgentType::Explorer, UnifiedToolConfig::default());
        configs.insert(AgentType::Coder, UnifiedToolConfig::default());
    }

    /// Register a custom tool factory
    pub fn register_tool_factory(
        &self,
        tool_name: String,
        factory: Box<dyn ToolFactory + Send + Sync>,
    ) -> Result<(), String> {
        let mut factories = self.tool_factories.write().unwrap();
        
        if factories.contains_key(&tool_name) {
            return Err(format!("Tool factory for '{}' already registered", tool_name));
        }
        
        factories.insert(tool_name, factory);
        Ok(())
    }

    /// Get tools for a specific agent type, including MCP tools if available
    pub fn get_tools_for_agent(&self, agent_type: &AgentType) -> Result<Vec<ToolSpec>, String> {
        let configs = self.configs.read().unwrap();
        let config = configs.get(agent_type)
            .ok_or_else(|| format!("No configuration found for agent type: {:?}", agent_type))?;

        let tool_definitions = config.get_tools_for_agent(agent_type);
        let mut tools = Vec::new();

        let factories = self.tool_factories.read().unwrap();

        for definition in tool_definitions {
            if let Some(factory) = factories.get(&definition.name) {
                // Validate configuration
                factory.validate_config(&definition)
                    .map_err(|e| format!("Tool '{}' validation failed: {}", definition.name, e))?;

                // Create the tool
                let tool = factory.create_tool_spec(&definition)
                    .map_err(|e| format!("Failed to create tool '{}': {}", definition.name, e))?;

                tools.push(tool);
            } else {
                return Err(format!("No factory registered for tool: {}", definition.name));
            }
        }

        // TODO: Add MCP tools support
        // MCP tools conversion will be implemented later

        Ok(tools)
    }

    /// Update configuration for an agent type
    pub fn update_config(&self, agent_type: AgentType, config: UnifiedToolConfig) {
        let mut configs = self.configs.write().unwrap();
        configs.insert(agent_type, config);
    }

    /// Get configuration for an agent type
    pub fn get_config(&self, agent_type: &AgentType) -> Option<UnifiedToolConfig> {
        let configs = self.configs.read().unwrap();
        configs.get(agent_type).cloned()
    }

    /// Check if an agent has permission to use a tool
    pub fn has_permission(&self, agent_type: &AgentType, tool_name: &str) -> bool {
        let configs = self.configs.read().unwrap();
        if let Some(config) = configs.get(agent_type) {
            config.has_permission(agent_type, tool_name)
        } else {
            false
        }
    }

    /// Get available tool names for an agent type
    pub fn get_available_tool_names(&self, agent_type: &AgentType) -> Vec<String> {
        let configs = self.configs.read().unwrap();
        if let Some(config) = configs.get(agent_type) {
            config.get_tools_for_agent(agent_type)
                .into_iter()
                .map(|tool| tool.name)
                .collect()
        } else {
            Vec::new()
        }
    }

    /// Set MCP tools cache
    pub fn set_mcp_tools(&self, mcp_tools: HashMap<String, mcp_types::Tool>) {
        let mut cache = self.mcp_tools_cache.write().unwrap();
        *cache = Some(mcp_tools);
    }

    /// Get MCP tools
    pub fn get_mcp_tools(&self) -> Option<HashMap<String, mcp_types::Tool>> {
        let cache = self.mcp_tools_cache.read().unwrap();
        cache.clone()
    }



    /// Validate all registered tools
    pub fn validate_all_tools(&self) -> Result<(), String> {
        let configs = self.configs.read().unwrap();
        let factories = self.tool_factories.read().unwrap();

        for (agent_type, config) in configs.iter() {
            let tool_definitions = config.get_tools_for_agent(agent_type);
            
            for definition in tool_definitions {
                if let Some(factory) = factories.get(&definition.name) {
                    factory.validate_config(&definition)
                        .map_err(|e| format!("Tool '{}' for agent {:?} validation failed: {}", 
                                            definition.name, agent_type, e))?;
                } else {
                    return Err(format!("No factory registered for tool: {}", definition.name));
                }
            }
        }

        Ok(())
    }
}

impl Default for ToolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Global tool registry instance
lazy_static::lazy_static! {
    pub static ref GLOBAL_TOOL_REGISTRY: Arc<ToolRegistry> = Arc::new(ToolRegistry::new());
}

/// Convenience function to get tools for an agent type using the global registry
pub fn get_tools_for_agent(agent_type: &AgentType) -> Result<Vec<ToolSpec>, String> {
    GLOBAL_TOOL_REGISTRY.get_tools_for_agent(agent_type)
}

/// Convenience function to check permissions using the global registry
pub fn has_permission(agent_type: &AgentType, tool_name: &str) -> bool {
    GLOBAL_TOOL_REGISTRY.has_permission(agent_type, tool_name)
}

/// Convenience function to get available tool names using the global registry
pub fn get_available_tool_names(agent_type: &AgentType) -> Vec<String> {
    GLOBAL_TOOL_REGISTRY.get_available_tool_names(agent_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tool_registry_creation() {
        let registry = ToolRegistry::new();
        
        // Test that we can get tools for different agent types
        let main_tools = registry.get_tools_for_agent(&AgentType::Main);
        let explorer_tools = registry.get_tools_for_agent(&AgentType::Explorer);
        let coder_tools = registry.get_tools_for_agent(&AgentType::Coder);
        
        assert!(main_tools.is_ok());
        assert!(explorer_tools.is_ok());
        assert!(coder_tools.is_ok());
        
        // Main agent should have more tools than others
        assert!(main_tools.unwrap().len() >= explorer_tools.unwrap().len());
    }

    #[test]
    fn test_permission_checking() {
        let registry = ToolRegistry::new();
        
        // Test permission checking - Main agent should only have coordinator permissions (no base tools)
        assert!(!registry.has_permission(&AgentType::Main, "read_file")); // Main agent should NOT have read_file
        assert!(!registry.has_permission(&AgentType::Main, "write_file")); // Main agent no longer has write permission
        assert!(!registry.has_permission(&AgentType::Main, "shell")); // Main agent no longer has shell permission
        assert!(!registry.has_permission(&AgentType::Main, "store_context")); // Main agent should NOT have store_context
        assert!(registry.has_permission(&AgentType::Main, "create_subagent_task"));
        assert!(registry.has_permission(&AgentType::Main, "list_contexts"));
        assert!(registry.has_permission(&AgentType::Main, "multi_retrieve_contexts"));
        
        assert!(registry.has_permission(&AgentType::Explorer, "read_file"));
        assert!(!registry.has_permission(&AgentType::Explorer, "write_file"));
        assert!(!registry.has_permission(&AgentType::Explorer, "create_subagent_task"));
    }

    #[test]
    fn test_tool_validation() {
        let registry = ToolRegistry::new();
        
        // Test that all tools validate correctly
        let result = registry.validate_all_tools();
        assert!(result.is_ok(), "Tool validation failed: {:?}", result);
    }

    #[test]
    fn test_global_registry() {
        // Test global registry functions
        let main_tools = get_tools_for_agent(&AgentType::Main);
        assert!(main_tools.is_ok());
        
        assert!(has_permission(&AgentType::Main, "read_file"));
        assert!(!has_permission(&AgentType::Explorer, "write_file"));
        
        let tool_names = get_available_tool_names(&AgentType::Main);
        assert!(!tool_names.is_empty());
        assert!(tool_names.contains(&"read_file".to_string()));
    }
}