use thiserror::Error;
use serde::{Deserialize, Serialize};

/// Unified error types for the entire codex system
#[derive(Error, Debug, Clone, Serialize, Deserialize)]
pub enum UnifiedError {
    /// Function call related errors
    #[error("Function call error: {message}")]
    FunctionCall {
        function_name: String,
        message: String,
        error_code: FunctionCallErrorCode,
    },

    /// File system operation errors
    #[error("File system error: {message}")]
    FileSystem {
        operation: FileSystemOperation,
        path: String,
        message: String,
        error_code: FileSystemErrorCode,
    },

    /// Shell execution errors
    #[error("Shell execution error: {message}")]
    ShellExecution {
        command: String,
        message: String,
        exit_code: Option<i32>,
        error_code: ShellErrorCode,
    },

    /// Context management errors
    #[error("Context error: {message}")]
    Context {
        operation: ContextOperation,
        context_id: Option<String>,
        message: String,
        error_code: ContextErrorCode,
    },

    /// Permission and authorization errors
    #[error("Permission denied: {message}")]
    Permission {
        agent_type: String,
        requested_operation: String,
        message: String,
    },

    /// Validation errors
    #[error("Validation error: {message}")]
    Validation {
        field: String,
        value: String,
        message: String,
        error_code: ValidationErrorCode,
    },

    /// MCP (Model Context Protocol) errors
    #[error("MCP error: {message}")]
    Mcp {
        server_name: String,
        tool_name: String,
        message: String,
        error_code: McpErrorCode,
    },

    /// Network and communication errors
    #[error("Network error: {message}")]
    Network {
        operation: NetworkOperation,
        endpoint: String,
        message: String,
        status_code: Option<u16>,
    },

    /// Configuration errors
    #[error("Configuration error: {message}")]
    Configuration {
        component: String,
        message: String,
        error_code: ConfigurationErrorCode,
    },

    /// Timeout errors
    #[error("Timeout error: {message}")]
    Timeout {
        operation: String,
        timeout_ms: u64,
        message: String,
    },

    /// Resource errors (memory, disk space, etc.)
    #[error("Resource error: {message}")]
    Resource {
        resource_type: ResourceType,
        message: String,
        current_usage: Option<u64>,
        limit: Option<u64>,
    },

    /// Internal system errors
    #[error("Internal error: {message}")]
    Internal {
        component: String,
        message: String,
        error_code: Option<String>,
    },
}

/// Function call error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FunctionCallErrorCode {
    InvalidArguments,
    MissingArguments,
    UnsupportedFunction,
    ExecutionFailed,
    PermissionDenied,
    Timeout,
    InternalError,
}

/// File system operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileSystemOperation {
    Read,
    Write,
    Create,
    Delete,
    List,
    Move,
    Copy,
    Permissions,
}

/// File system error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileSystemErrorCode {
    FileNotFound,
    PermissionDenied,
    FileAlreadyExists,
    DirectoryNotFound,
    NotAFile,
    NotADirectory,
    FileTooLarge,
    DiskFull,
    InvalidPath,
    IoError,
}

/// Shell execution error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ShellErrorCode {
    CommandNotFound,
    PermissionDenied,
    ExecutionFailed,
    Timeout,
    InvalidCommand,
    ShellNotAvailable,
    EnvironmentError,
}

/// Context operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContextOperation {
    Store,
    Retrieve,
    List,
    Delete,
    Search,
    Update,
}

/// Context error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ContextErrorCode {
    ContextNotFound,
    ContextAlreadyExists,
    InvalidContextId,
    ContentTooLarge,
    StorageFull,
    SerializationError,
    DeserializationError,
}

/// Validation error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ValidationErrorCode {
    Required,
    InvalidFormat,
    OutOfRange,
    TooLong,
    TooShort,
    InvalidCharacters,
    InvalidType,
}

/// MCP error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum McpErrorCode {
    ServerNotFound,
    ToolNotFound,
    ConnectionFailed,
    RequestFailed,
    ResponseInvalid,
    Timeout,
    AuthenticationFailed,
}

/// Network operations
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum NetworkOperation {
    HttpRequest,
    WebSocketConnection,
    FileDownload,
    FileUpload,
    ApiCall,
}

/// Configuration error codes
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ConfigurationErrorCode {
    MissingConfiguration,
    InvalidConfiguration,
    ConfigurationNotFound,
    ParseError,
    ValidationFailed,
}

/// Resource types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ResourceType {
    Memory,
    DiskSpace,
    FileHandles,
    NetworkConnections,
    CpuTime,
    ProcessCount,
}

/// Error severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, PartialOrd, Eq, Hash)]
pub enum ErrorSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Error context for better debugging and logging
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorContext {
    /// Timestamp when the error occurred
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Component or module where the error occurred
    pub component: String,
    /// Function or method where the error occurred
    pub function: Option<String>,
    /// Line number where the error occurred
    pub line: Option<u32>,
    /// Additional context information
    pub additional_info: std::collections::HashMap<String, String>,
    /// Error severity
    pub severity: ErrorSeverity,
    /// Whether the error is recoverable
    pub recoverable: bool,
}

impl UnifiedError {
    /// Create a function call error
    pub fn function_call(
        function_name: impl Into<String>,
        message: impl Into<String>,
        error_code: FunctionCallErrorCode,
    ) -> Self {
        Self::FunctionCall {
            function_name: function_name.into(),
            message: message.into(),
            error_code,
        }
    }

    /// Create a file system error
    pub fn file_system(
        operation: FileSystemOperation,
        path: impl Into<String>,
        message: impl Into<String>,
        error_code: FileSystemErrorCode,
    ) -> Self {
        Self::FileSystem {
            operation,
            path: path.into(),
            message: message.into(),
            error_code,
        }
    }

    /// Create a shell execution error
    pub fn shell_execution(
        command: impl Into<String>,
        message: impl Into<String>,
        exit_code: Option<i32>,
        error_code: ShellErrorCode,
    ) -> Self {
        Self::ShellExecution {
            command: command.into(),
            message: message.into(),
            exit_code,
            error_code,
        }
    }

    /// Create a context error
    pub fn context(
        operation: ContextOperation,
        context_id: Option<String>,
        message: impl Into<String>,
        error_code: ContextErrorCode,
    ) -> Self {
        Self::Context {
            operation,
            context_id,
            message: message.into(),
            error_code,
        }
    }

    /// Create a permission error
    pub fn permission(
        agent_type: impl Into<String>,
        requested_operation: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self::Permission {
            agent_type: agent_type.into(),
            requested_operation: requested_operation.into(),
            message: message.into(),
        }
    }

    /// Create a validation error
    pub fn validation(
        field: impl Into<String>,
        value: impl Into<String>,
        message: impl Into<String>,
        error_code: ValidationErrorCode,
    ) -> Self {
        Self::Validation {
            field: field.into(),
            value: value.into(),
            message: message.into(),
            error_code,
        }
    }

    /// Create an MCP error
    pub fn mcp(
        server_name: impl Into<String>,
        tool_name: impl Into<String>,
        message: impl Into<String>,
        error_code: McpErrorCode,
    ) -> Self {
        Self::Mcp {
            server_name: server_name.into(),
            tool_name: tool_name.into(),
            message: message.into(),
            error_code,
        }
    }

    /// Create a timeout error
    pub fn timeout(
        operation: impl Into<String>,
        timeout_ms: u64,
        message: impl Into<String>,
    ) -> Self {
        Self::Timeout {
            operation: operation.into(),
            timeout_ms,
            message: message.into(),
        }
    }

    /// Create an internal error
    pub fn internal(
        component: impl Into<String>,
        message: impl Into<String>,
        error_code: Option<String>,
    ) -> Self {
        Self::Internal {
            component: component.into(),
            message: message.into(),
            error_code,
        }
    }

    /// Get the error severity
    pub fn severity(&self) -> ErrorSeverity {
        match self {
            Self::Internal { .. } => ErrorSeverity::Critical,
            Self::Permission { .. } => ErrorSeverity::High,
            Self::Configuration { .. } => ErrorSeverity::High,
            Self::FileSystem { error_code, .. } => match error_code {
                FileSystemErrorCode::PermissionDenied => ErrorSeverity::High,
                FileSystemErrorCode::DiskFull => ErrorSeverity::Critical,
                _ => ErrorSeverity::Medium,
            },
            Self::ShellExecution { error_code, .. } => match error_code {
                ShellErrorCode::PermissionDenied => ErrorSeverity::High,
                ShellErrorCode::Timeout => ErrorSeverity::Medium,
                _ => ErrorSeverity::Medium,
            },
            Self::Network { .. } => ErrorSeverity::Medium,
            Self::Timeout { .. } => ErrorSeverity::Medium,
            Self::Resource { resource_type, .. } => match resource_type {
                ResourceType::Memory | ResourceType::DiskSpace => ErrorSeverity::High,
                _ => ErrorSeverity::Medium,
            },
            _ => ErrorSeverity::Low,
        }
    }

    /// Check if the error is recoverable
    pub fn is_recoverable(&self) -> bool {
        match self {
            Self::Internal { .. } => false,
            Self::Configuration { .. } => false,
            Self::Permission { .. } => false,
            Self::FileSystem { error_code, .. } => match error_code {
                FileSystemErrorCode::DiskFull => false,
                FileSystemErrorCode::PermissionDenied => false,
                _ => true,
            },
            Self::Resource { resource_type, .. } => match resource_type {
                ResourceType::Memory | ResourceType::DiskSpace => false,
                _ => true,
            },
            _ => true,
        }
    }

    /// Get error code as string
    pub fn error_code(&self) -> String {
        match self {
            Self::FunctionCall { error_code, .. } => format!("FUNC_{:?}", error_code),
            Self::FileSystem { error_code, .. } => format!("FS_{:?}", error_code),
            Self::ShellExecution { error_code, .. } => format!("SHELL_{:?}", error_code),
            Self::Context { error_code, .. } => format!("CTX_{:?}", error_code),
            Self::Permission { .. } => "PERM_DENIED".to_string(),
            Self::Validation { error_code, .. } => format!("VAL_{:?}", error_code),
            Self::Mcp { error_code, .. } => format!("MCP_{:?}", error_code),
            Self::Network { .. } => "NET_ERROR".to_string(),
            Self::Configuration { error_code, .. } => format!("CFG_{:?}", error_code),
            Self::Timeout { .. } => "TIMEOUT".to_string(),
            Self::Resource { resource_type, .. } => format!("RES_{:?}", resource_type),
            Self::Internal { error_code, .. } => {
                error_code.clone().unwrap_or_else(|| "INT_ERROR".to_string())
            }
        }
    }

    /// Convert to user-friendly message
    pub fn user_message(&self) -> String {
        match self {
            Self::FunctionCall { function_name, message, .. } => {
                format!("Failed to execute function '{}': {}", function_name, message)
            }
            Self::FileSystem { operation, path, message, .. } => {
                format!("Failed to {:?} file '{}': {}", operation, path, message)
            }
            Self::ShellExecution { command, message, .. } => {
                format!("Failed to execute command '{}': {}", command, message)
            }
            Self::Context { operation, context_id, message, .. } => {
                let id_str = context_id.as_deref().unwrap_or("unknown");
                format!("Failed to {:?} context '{}': {}", operation, id_str, message)
            }
            Self::Permission { agent_type, requested_operation, message } => {
                format!("Permission denied for {} agent to {}: {}", agent_type, requested_operation, message)
            }
            Self::Validation { field, message, .. } => {
                format!("Invalid value for '{}': {}", field, message)
            }
            Self::Mcp { server_name, tool_name, message, .. } => {
                format!("MCP tool '{}' on server '{}' failed: {}", tool_name, server_name, message)
            }
            Self::Network { operation, endpoint, message, .. } => {
                format!("Network {:?} to '{}' failed: {}", operation, endpoint, message)
            }
            Self::Configuration { component, message, .. } => {
                format!("Configuration error in '{}': {}", component, message)
            }
            Self::Timeout { operation, timeout_ms, .. } => {
                format!("Operation '{}' timed out after {}ms", operation, timeout_ms)
            }
            Self::Resource { resource_type, message, .. } => {
                format!("{:?} resource error: {}", resource_type, message)
            }
            Self::Internal { component, message, .. } => {
                format!("Internal error in '{}': {}", component, message)
            }
        }
    }
}

impl ErrorContext {
    /// Create a new error context
    pub fn new(component: impl Into<String>) -> Self {
        Self {
            timestamp: chrono::Utc::now(),
            component: component.into(),
            function: None,
            line: None,
            additional_info: std::collections::HashMap::new(),
            severity: ErrorSeverity::Medium,
            recoverable: true,
        }
    }

    /// Set the function name
    pub fn with_function(mut self, function: impl Into<String>) -> Self {
        self.function = Some(function.into());
        self
    }

    /// Set the line number
    pub fn with_line(mut self, line: u32) -> Self {
        self.line = Some(line);
        self
    }

    /// Add additional information
    pub fn with_info(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.additional_info.insert(key.into(), value.into());
        self
    }

    /// Set the severity
    pub fn with_severity(mut self, severity: ErrorSeverity) -> Self {
        self.severity = severity;
        self
    }

    /// Set whether the error is recoverable
    pub fn with_recoverable(mut self, recoverable: bool) -> Self {
        self.recoverable = recoverable;
        self
    }
}

/// Result type using UnifiedError
pub type UnifiedResult<T> = Result<T, UnifiedError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let error = UnifiedError::function_call(
            "test_function",
            "Test error message",
            FunctionCallErrorCode::InvalidArguments,
        );

        assert_eq!(error.error_code(), "FUNC_InvalidArguments");
        assert_eq!(error.severity(), ErrorSeverity::Low);
        assert!(error.is_recoverable());
    }

    #[test]
    fn test_error_context() {
        let context = ErrorContext::new("test_component")
            .with_function("test_function")
            .with_line(42)
            .with_info("key", "value")
            .with_severity(ErrorSeverity::High)
            .with_recoverable(false);

        assert_eq!(context.component, "test_component");
        assert_eq!(context.function, Some("test_function".to_string()));
        assert_eq!(context.line, Some(42));
        assert_eq!(context.severity, ErrorSeverity::High);
        assert!(!context.recoverable);
    }

    #[test]
    fn test_user_message() {
        let error = UnifiedError::file_system(
            FileSystemOperation::Read,
            "/path/to/file.txt",
            "File not found",
            FileSystemErrorCode::FileNotFound,
        );

        let message = error.user_message();
        assert!(message.contains("Failed to Read file"));
        assert!(message.contains("/path/to/file.txt"));
        assert!(message.contains("File not found"));
    }

    #[test]
    fn test_error_severity() {
        let low_error = UnifiedError::validation(
            "field",
            "value",
            "Invalid format",
            ValidationErrorCode::InvalidFormat,
        );
        assert_eq!(low_error.severity(), ErrorSeverity::Low);

        let high_error = UnifiedError::permission(
            "Explorer",
            "write_file",
            "Permission denied",
        );
        assert_eq!(high_error.severity(), ErrorSeverity::High);

        let critical_error = UnifiedError::internal(
            "core",
            "Panic occurred",
            Some("PANIC".to_string()),
        );
        assert_eq!(critical_error.severity(), ErrorSeverity::Critical);
    }
}