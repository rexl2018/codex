use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tracing::{debug, error, info, warn};
use codex_protocol::models::FunctionCallOutputPayload;

use crate::unified_error_types::{
    UnifiedError, UnifiedResult, ErrorContext, ErrorSeverity,
    FunctionCallErrorCode, FileSystemErrorCode, ShellErrorCode,
    ContextErrorCode, ValidationErrorCode, McpErrorCode,
};

/// Error handling strategy
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorHandlingStrategy {
    /// Log and return error
    LogAndReturn,
    /// Log, retry, and return error if retry fails
    LogRetryAndReturn { max_retries: u32, delay_ms: u64 },
    /// Log and convert to user-friendly message
    LogAndConvertToUserMessage,
    /// Log and ignore (for non-critical errors)
    LogAndIgnore,
    /// Custom handling with callback
    Custom(String), // Strategy name for lookup
}

/// Error handler configuration
#[derive(Debug, Clone)]
pub struct ErrorHandlerConfig {
    /// Default strategy for different error types
    pub default_strategies: HashMap<String, ErrorHandlingStrategy>,
    /// Whether to log stack traces
    pub log_stack_traces: bool,
    /// Maximum error message length for user display
    pub max_user_message_length: usize,
    /// Whether to include error codes in user messages
    pub include_error_codes: bool,
    /// Whether to enable error metrics collection
    pub enable_metrics: bool,
}

impl Default for ErrorHandlerConfig {
    fn default() -> Self {
        let mut default_strategies = HashMap::new();
        
        // Function call errors
        default_strategies.insert(
            "FunctionCall".to_string(),
            ErrorHandlingStrategy::LogAndConvertToUserMessage,
        );
        
        // File system errors
        default_strategies.insert(
            "FileSystem".to_string(),
            ErrorHandlingStrategy::LogRetryAndReturn {
                max_retries: 2,
                delay_ms: 1000,
            },
        );
        
        // Shell execution errors
        default_strategies.insert(
            "ShellExecution".to_string(),
            ErrorHandlingStrategy::LogAndConvertToUserMessage,
        );
        
        // Context errors
        default_strategies.insert(
            "Context".to_string(),
            ErrorHandlingStrategy::LogRetryAndReturn {
                max_retries: 1,
                delay_ms: 500,
            },
        );
        
        // Permission errors
        default_strategies.insert(
            "Permission".to_string(),
            ErrorHandlingStrategy::LogAndConvertToUserMessage,
        );
        
        // Validation errors
        default_strategies.insert(
            "Validation".to_string(),
            ErrorHandlingStrategy::LogAndConvertToUserMessage,
        );
        
        // MCP errors
        default_strategies.insert(
            "Mcp".to_string(),
            ErrorHandlingStrategy::LogRetryAndReturn {
                max_retries: 2,
                delay_ms: 2000,
            },
        );
        
        // Network errors
        default_strategies.insert(
            "Network".to_string(),
            ErrorHandlingStrategy::LogRetryAndReturn {
                max_retries: 3,
                delay_ms: 1000,
            },
        );
        
        // Configuration errors
        default_strategies.insert(
            "Configuration".to_string(),
            ErrorHandlingStrategy::LogAndReturn,
        );
        
        // Timeout errors
        default_strategies.insert(
            "Timeout".to_string(),
            ErrorHandlingStrategy::LogAndConvertToUserMessage,
        );
        
        // Resource errors
        default_strategies.insert(
            "Resource".to_string(),
            ErrorHandlingStrategy::LogAndReturn,
        );
        
        // Internal errors
        default_strategies.insert(
            "Internal".to_string(),
            ErrorHandlingStrategy::LogAndReturn,
        );

        Self {
            default_strategies,
            log_stack_traces: true,
            max_user_message_length: 500,
            include_error_codes: false,
            enable_metrics: true,
        }
    }
}

/// Error metrics for monitoring and debugging
#[derive(Debug, Clone, Default)]
pub struct ErrorMetrics {
    /// Total error count by type
    pub error_counts: HashMap<String, u64>,
    /// Error count by severity
    pub severity_counts: HashMap<ErrorSeverity, u64>,
    /// Recent errors (limited to last N errors)
    pub recent_errors: Vec<(chrono::DateTime<chrono::Utc>, UnifiedError)>,
    /// Error rate (errors per minute)
    pub error_rate: f64,
}

/// Custom error handler callback
pub type CustomErrorHandler = Box<dyn Fn(&UnifiedError, &ErrorContext) -> UnifiedResult<FunctionCallOutputPayload> + Send + Sync>;

/// Unified error handler that provides consistent error handling across the codebase
pub struct UnifiedErrorHandler {
    config: RwLock<ErrorHandlerConfig>,
    metrics: RwLock<ErrorMetrics>,
    custom_handlers: RwLock<HashMap<String, CustomErrorHandler>>,
}

impl UnifiedErrorHandler {
    /// Create a new error handler with default configuration
    pub fn new() -> Self {
        Self {
            config: RwLock::new(ErrorHandlerConfig::default()),
            metrics: RwLock::new(ErrorMetrics::default()),
            custom_handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Create a new error handler with custom configuration
    pub fn with_config(config: ErrorHandlerConfig) -> Self {
        Self {
            config: RwLock::new(config),
            metrics: RwLock::new(ErrorMetrics::default()),
            custom_handlers: RwLock::new(HashMap::new()),
        }
    }

    /// Handle an error and return appropriate response
    pub async fn handle_error(
        &self,
        error: UnifiedError,
        context: Option<ErrorContext>,
        call_id: Option<String>,
    ) -> FunctionCallOutputPayload {
        let error_type = self.get_error_type(&error);
        let context = context.unwrap_or_else(|| ErrorContext::new("unknown"));
        
        // Update metrics
        self.update_metrics(&error);
        
        // Log the error
        self.log_error(&error, &context);
        
        // Get handling strategy
        let strategy = self.get_strategy(&error_type);
        
        // Handle based on strategy
        match strategy {
            ErrorHandlingStrategy::LogAndReturn => {
                self.create_error_response(&error, call_id, false)
            }
            ErrorHandlingStrategy::LogRetryAndReturn { max_retries, delay_ms } => {
                // For now, we don't implement retry logic here as it would require
                // the original operation context. This would be handled at a higher level.
                warn!("Retry strategy not implemented at this level for error: {}", error);
                self.create_error_response(&error, call_id, false)
            }
            ErrorHandlingStrategy::LogAndConvertToUserMessage => {
                self.create_error_response(&error, call_id, true)
            }
            ErrorHandlingStrategy::LogAndIgnore => {
                info!("Ignoring non-critical error: {}", error);
                FunctionCallOutputPayload {
            content_items: None,
                    content: "Operation completed with warnings".to_string(),
                    success: Some(true),
                }
            }
            ErrorHandlingStrategy::Custom(strategy_name) => {
                self.handle_custom_error(&error, &context, &strategy_name, call_id)
            }
        }
    }

    /// Handle error and convert to standard result format
    pub async fn handle_error_result<T>(
        &self,
        result: UnifiedResult<T>,
        context: Option<ErrorContext>,
    ) -> UnifiedResult<T> {
        match result {
            Ok(value) => Ok(value),
            Err(error) => {
                let context = context.unwrap_or_else(|| ErrorContext::new("unknown"));
                
                // Update metrics and log
                self.update_metrics(&error);
                self.log_error(&error, &context);
                
                // Return the error (don't convert to response here)
                Err(error)
            }
        }
    }

    /// Register a custom error handler
    pub fn register_custom_handler(
        &self,
        strategy_name: String,
        handler: CustomErrorHandler,
    ) -> Result<(), String> {
        let mut handlers = self.custom_handlers.write().unwrap();
        
        if handlers.contains_key(&strategy_name) {
            return Err(format!("Custom handler '{}' already registered", strategy_name));
        }
        
        handlers.insert(strategy_name, handler);
        Ok(())
    }

    /// Update error handling configuration
    pub fn update_config(&self, config: ErrorHandlerConfig) {
        let mut current_config = self.config.write().unwrap();
        *current_config = config;
    }

    /// Get error metrics
    pub fn get_metrics(&self) -> ErrorMetrics {
        let metrics = self.metrics.read().unwrap();
        metrics.clone()
    }

    /// Clear error metrics
    pub fn clear_metrics(&self) {
        let mut metrics = self.metrics.write().unwrap();
        *metrics = ErrorMetrics::default();
    }

    /// Convert common error types to UnifiedError
    pub fn convert_error(&self, error: &dyn std::error::Error) -> UnifiedError {
        let error_string = error.to_string();
        
        // Try to identify error type from message
        if error_string.contains("No such file or directory") {
            UnifiedError::file_system(
                crate::unified_error_types::FileSystemOperation::Read,
                "unknown",
                error_string,
                FileSystemErrorCode::FileNotFound,
            )
        } else if error_string.contains("Permission denied") {
            UnifiedError::file_system(
                crate::unified_error_types::FileSystemOperation::Read,
                "unknown",
                error_string,
                FileSystemErrorCode::PermissionDenied,
            )
        } else if error_string.contains("timeout") || error_string.contains("Timeout") {
            UnifiedError::timeout("unknown_operation", 30000, error_string)
        } else {
            UnifiedError::internal("unknown", error_string, None)
        }
    }

    /// Create a function call output payload from an error
    fn create_error_response(
        &self,
        error: &UnifiedError,
        call_id: Option<String>,
        use_user_message: bool,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        let config = self.config.read().unwrap();
        
        let mut content = if use_user_message {
            error.user_message()
        } else {
            error.to_string()
        };
        
        // Include error code if configured
        if config.include_error_codes {
            content = format!("[{}] {}", error.error_code(), content);
        }
        
        // Truncate if too long
        if content.len() > config.max_user_message_length {
            content.truncate(config.max_user_message_length - 3);
            content.push_str("...");
        }
        
        FunctionCallOutputPayload {
            content,
            content_items: None,
            success: Some(false),
        }
    }

    /// Handle custom error with registered handler
    fn handle_custom_error(
        &self,
        error: &UnifiedError,
        context: &ErrorContext,
        strategy_name: &str,
        call_id: Option<String>,
    ) -> FunctionCallOutputPayload {
            content_items: None,
        let handlers = self.custom_handlers.read().unwrap();
        
        if let Some(handler) = handlers.get(strategy_name) {
            match handler(error, context) {
                Ok(response) => response,
                Err(handler_error) => {
                    error!("Custom error handler '{}' failed: {}", strategy_name, handler_error);
                    self.create_error_response(error, call_id, true)
                }
            }
        } else {
            warn!("Custom error handler '{}' not found, using default", strategy_name);
            self.create_error_response(error, call_id, true)
        }
    }

    /// Get error type string for strategy lookup
    fn get_error_type(&self, error: &UnifiedError) -> String {
        match error {
            UnifiedError::FunctionCall { .. } => "FunctionCall".to_string(),
            UnifiedError::FileSystem { .. } => "FileSystem".to_string(),
            UnifiedError::ShellExecution { .. } => "ShellExecution".to_string(),
            UnifiedError::Context { .. } => "Context".to_string(),
            UnifiedError::Permission { .. } => "Permission".to_string(),
            UnifiedError::Validation { .. } => "Validation".to_string(),
            UnifiedError::Mcp { .. } => "Mcp".to_string(),
            UnifiedError::Network { .. } => "Network".to_string(),
            UnifiedError::Configuration { .. } => "Configuration".to_string(),
            UnifiedError::Timeout { .. } => "Timeout".to_string(),
            UnifiedError::Resource { .. } => "Resource".to_string(),
            UnifiedError::Internal { .. } => "Internal".to_string(),
        }
    }

    /// Get handling strategy for error type
    fn get_strategy(&self, error_type: &str) -> ErrorHandlingStrategy {
        let config = self.config.read().unwrap();
        config.default_strategies
            .get(error_type)
            .cloned()
            .unwrap_or(ErrorHandlingStrategy::LogAndReturn)
    }

    /// Log error with appropriate level
    fn log_error(&self, error: &UnifiedError, context: &ErrorContext) {
        let config = self.config.read().unwrap();
        
        match error.severity() {
            ErrorSeverity::Low => {
                debug!("Error in {}: {}", context.component, error);
            }
            ErrorSeverity::Medium => {
                warn!("Error in {}: {}", context.component, error);
            }
            ErrorSeverity::High => {
                error!("Error in {}: {}", context.component, error);
            }
            ErrorSeverity::Critical => {
                error!("CRITICAL ERROR in {}: {}", context.component, error);
                if config.log_stack_traces {
                    error!("Error context: {:?}", context);
                }
            }
        }
    }

    /// Update error metrics
    fn update_metrics(&self, error: &UnifiedError) {
        let mut metrics = self.metrics.write().unwrap();
        
        // Update error count by type
        let error_type = self.get_error_type(error);
        *metrics.error_counts.entry(error_type).or_insert(0) += 1;
        
        // Update severity count
        *metrics.severity_counts.entry(error.severity()).or_insert(0) += 1;
        
        // Add to recent errors (keep last 100)
        metrics.recent_errors.push((chrono::Utc::now(), error.clone()));
        if metrics.recent_errors.len() > 100 {
            metrics.recent_errors.remove(0);
        }
        
        // Calculate error rate (simplified - errors in last minute)
        let one_minute_ago = chrono::Utc::now() - chrono::Duration::minutes(1);
        let recent_count = metrics.recent_errors
            .iter()
            .filter(|(timestamp, _)| *timestamp > one_minute_ago)
            .count();
        metrics.error_rate = recent_count as f64;
    }
}

impl Default for UnifiedErrorHandler {
    fn default() -> Self {
        Self::new()
    }
}

/// Global error handler instance
lazy_static::lazy_static! {
    pub static ref GLOBAL_ERROR_HANDLER: Arc<UnifiedErrorHandler> = Arc::new(UnifiedErrorHandler::new());
}

/// Convenience function to handle errors using the global handler
pub async fn handle_error(
    error: UnifiedError,
    context: Option<ErrorContext>,
    call_id: Option<String>,
) -> FunctionCallOutputPayload {
    GLOBAL_ERROR_HANDLER.handle_error(error, context, call_id).await
}

/// Convenience function to handle error results using the global handler
pub async fn handle_error_result<T>(
    result: UnifiedResult<T>,
    context: Option<ErrorContext>,
) -> UnifiedResult<T> {
    GLOBAL_ERROR_HANDLER.handle_error_result(result, context).await
}

/// Macro for creating error context with file and line information
#[macro_export]
macro_rules! error_context {
    ($component:expr) => {
        $crate::unified_error_types::ErrorContext::new($component)
            .with_function(module_path!())
            .with_line(line!())
    };
    ($component:expr, $function:expr) => {
        $crate::unified_error_types::ErrorContext::new($component)
            .with_function($function)
            .with_line(line!())
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_error_handler() {
        let handler = UnifiedErrorHandler::new();
        
        let error = UnifiedError::function_call(
            "test_function",
            "Test error",
            FunctionCallErrorCode::InvalidArguments,
        );
        
        let response = handler.handle_error(error, None, Some("call_123".to_string())).await;
        
        assert_eq!(response.success, Some(false));
        assert!(response.content.contains("test_function"));
    }

    #[tokio::test]
    async fn test_custom_handler() {
        let handler = UnifiedErrorHandler::new();
        
        // Register custom handler
        let custom_handler: CustomErrorHandler = Box::new(|error, _context| {
            Ok(FunctionCallOutputPayload {
            content_items: None,
                content: format!("Custom handling: {}", error.user_message()),
                success: Some(false),
            })
        });
        
        handler.register_custom_handler("test_custom".to_string(), custom_handler).unwrap();
        
        // Update config to use custom handler
        let mut config = ErrorHandlerConfig::default();
        config.default_strategies.insert(
            "FunctionCall".to_string(),
            ErrorHandlingStrategy::Custom("test_custom".to_string()),
        );
        handler.update_config(config);
        
        let error = UnifiedError::function_call(
            "test_function",
            "Test error",
            FunctionCallErrorCode::InvalidArguments,
        );
        
        let response = handler.handle_error(error, None, Some("call_123".to_string())).await;
        
        assert_eq!(response.success, Some(false));
        assert!(response.content.contains("Custom handling"));
    }

    #[test]
    fn test_error_metrics() {
        let handler = UnifiedErrorHandler::new();
        
        let error1 = UnifiedError::function_call(
            "test1",
            "Error 1",
            FunctionCallErrorCode::InvalidArguments,
        );
        let error2 = UnifiedError::file_system(
            crate::unified_error_types::FileSystemOperation::Read,
            "test.txt",
            "Error 2",
            FileSystemErrorCode::FileNotFound,
        );
        
        handler.update_metrics(&error1);
        handler.update_metrics(&error2);
        handler.update_metrics(&error1);
        
        let metrics = handler.get_metrics();
        
        assert_eq!(metrics.error_counts.get("FunctionCall"), Some(&2));
        assert_eq!(metrics.error_counts.get("FileSystem"), Some(&1));
        assert_eq!(metrics.recent_errors.len(), 3);
    }
}