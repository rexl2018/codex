use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, warn, info};
use rand;

/// Enhanced error handling for subagent operations
#[derive(Debug, Clone)]
pub struct SubagentErrorHandler {
    max_retries: u32,
    base_delay: Duration,
    max_delay: Duration,
}

impl Default for SubagentErrorHandler {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
        }
    }
}

impl SubagentErrorHandler {
    pub fn new(max_retries: u32, base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            max_retries,
            base_delay,
            max_delay,
        }
    }

    /// Execute an operation with timeout and retry logic
    pub async fn execute_with_timeout_and_retry<F, T, E>(
        &self,
        operation_name: &str,
        timeout: Duration,
        mut operation: F,
    ) -> Result<T, SubagentError>
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send>>,
        E: std::fmt::Display + std::fmt::Debug + Send + 'static,
    {
        let mut attempt = 0;
        let mut last_error = None;

        while attempt <= self.max_retries {
            let operation_future = operation();
            
            match tokio::time::timeout(timeout, operation_future).await {
                Ok(Ok(result)) => {
                    if attempt > 0 {
                        info!("Operation '{}' succeeded after {} retries", operation_name, attempt);
                    }
                    return Ok(result);
                }
                Ok(Err(e)) => {
                    attempt += 1;
                    last_error = Some(format!("{:?}", e));
                    
                    if attempt <= self.max_retries {
                        let delay = self.calculate_delay(attempt);
                        warn!(
                            "Operation '{}' failed (attempt {}/{}): {}. Retrying in {:?}",
                            operation_name, attempt, self.max_retries + 1, e, delay
                        );
                        sleep(delay).await;
                    } else {
                        error!(
                            "Operation '{}' failed after {} attempts: {}",
                            operation_name, attempt, e
                        );
                    }
                }
                Err(_timeout_error) => {
                    return Err(SubagentError::TaskTimeout {
                        task_id: operation_name.to_string(),
                        duration: timeout,
                    });
                }
            }
        }

        Err(SubagentError::OperationFailed {
            operation: operation_name.to_string(),
            attempts: attempt,
            last_error: last_error.unwrap_or_else(|| "Unknown error".to_string()),
        })
    }

    /// Execute an operation with retry logic and error recovery
    pub async fn execute_with_retry<F, T, E>(&self, operation_name: &str, mut operation: F) -> Result<T, SubagentError>
    where
        F: FnMut() -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, E>> + Send>>,
        E: std::fmt::Display + std::fmt::Debug + Send + 'static,
    {
        let mut attempt = 0;
        let mut last_error = None;

        while attempt <= self.max_retries {
            match operation().await {
                Ok(result) => {
                    if attempt > 0 {
                        info!("Operation '{}' succeeded after {} retries", operation_name, attempt);
                    }
                    return Ok(result);
                }
                Err(e) => {
                    attempt += 1;
                    last_error = Some(e);
                    
                    if attempt <= self.max_retries {
                        let delay = self.calculate_delay(attempt);
                        warn!(
                            "Operation '{}' failed (attempt {}/{}): {}. Retrying in {:?}",
                            operation_name, attempt, self.max_retries + 1, last_error.as_ref().unwrap(), delay
                        );
                        sleep(delay).await;
                    } else {
                        error!(
                            "Operation '{}' failed after {} attempts: {}",
                            operation_name, attempt, last_error.as_ref().unwrap()
                        );
                    }
                }
            }
        }

        Err(SubagentError::OperationFailed {
            operation: operation_name.to_string(),
            attempts: attempt,
            last_error: format!("{:?}", last_error.unwrap()),
        })
    }

    /// Calculate exponential backoff delay with jitter
    fn calculate_delay(&self, attempt: u32) -> Duration {
        let exponential_delay = self.base_delay * 2_u32.pow(attempt.saturating_sub(1));
        let delay_with_jitter = exponential_delay + Duration::from_millis(rand::random::<u64>() % 100);
        std::cmp::min(delay_with_jitter, self.max_delay)
    }

    /// Classify error severity for appropriate handling
    pub fn classify_error(&self, error: &dyn std::error::Error) -> ErrorSeverity {
        let error_str = error.to_string().to_lowercase();
        
        if error_str.contains("timeout") || error_str.contains("connection") {
            ErrorSeverity::Transient
        } else if error_str.contains("not found") || error_str.contains("invalid") {
            ErrorSeverity::Permanent
        } else if error_str.contains("rate limit") || error_str.contains("too many requests") {
            ErrorSeverity::RateLimit
        } else if error_str.contains("unauthorized") || error_str.contains("forbidden") {
            ErrorSeverity::Authentication
        } else {
            ErrorSeverity::Unknown
        }
    }

    /// Determine if an error is retryable based on its classification
    pub fn is_retryable(&self, severity: ErrorSeverity) -> bool {
        matches!(severity, ErrorSeverity::Transient | ErrorSeverity::RateLimit | ErrorSeverity::Unknown)
    }
}

/// Classification of error severity for appropriate handling
#[derive(Debug, Clone, PartialEq)]
pub enum ErrorSeverity {
    /// Temporary errors that may resolve with retry (network issues, timeouts)
    Transient,
    /// Permanent errors that won't resolve with retry (invalid input, not found)
    Permanent,
    /// Rate limiting errors that need longer delays
    RateLimit,
    /// Authentication/authorization errors
    Authentication,
    /// Unknown errors that should be retried cautiously
    Unknown,
}

/// Enhanced error types for subagent operations
#[derive(Debug, thiserror::Error)]
pub enum SubagentError {
    #[error("Operation '{operation}' failed after {attempts} attempts: {last_error}")]
    OperationFailed {
        operation: String,
        attempts: u32,
        last_error: String,
    },

    #[error("Subagent task '{task_id}' timed out after {duration:?}")]
    TaskTimeout {
        task_id: String,
        duration: Duration,
    },

    #[error("Subagent '{task_id}' is in invalid state: {state}")]
    InvalidState {
        task_id: String,
        state: String,
    },

    #[error("Resource exhausted: {resource}")]
    ResourceExhausted {
        resource: String,
    },

    #[error("Configuration error: {message}")]
    Configuration {
        message: String,
    },

    #[error("Dependency error: {dependency} - {message}")]
    Dependency {
        dependency: String,
        message: String,
    },
}

/// Recovery strategies for different types of errors
#[derive(Debug, Clone)]
pub enum RecoveryStrategy {
    /// Retry the operation with exponential backoff
    Retry,
    /// Cancel the current operation and start fresh
    Reset,
    /// Fallback to a simpler or alternative approach
    Fallback,
    /// Escalate to human intervention
    Escalate,
    /// Ignore the error and continue
    Ignore,
}

impl SubagentError {
    /// Get the recommended recovery strategy for this error
    pub fn recovery_strategy(&self) -> RecoveryStrategy {
        match self {
            SubagentError::OperationFailed { attempts, .. } => {
                if *attempts < 3 {
                    RecoveryStrategy::Retry
                } else {
                    RecoveryStrategy::Fallback
                }
            }
            SubagentError::TaskTimeout { .. } => RecoveryStrategy::Reset,
            SubagentError::InvalidState { .. } => RecoveryStrategy::Reset,
            SubagentError::ResourceExhausted { .. } => RecoveryStrategy::Fallback,
            SubagentError::Configuration { .. } => RecoveryStrategy::Escalate,
            SubagentError::Dependency { .. } => RecoveryStrategy::Retry,
        }
    }

    /// Check if this error indicates a critical failure that should stop all operations
    pub fn is_critical(&self) -> bool {
        matches!(self, SubagentError::Configuration { .. })
    }

    /// Get a user-friendly error message
    pub fn user_message(&self) -> String {
        match self {
            SubagentError::OperationFailed { operation, .. } => {
                format!("Failed to execute {}", operation)
            }
            SubagentError::TaskTimeout { task_id, .. } => {
                format!("Task {} took too long to complete", task_id)
            }
            SubagentError::InvalidState { task_id, state } => {
                format!("Task {} is in an unexpected state: {}", task_id, state)
            }
            SubagentError::ResourceExhausted { resource } => {
                format!("System resource exhausted: {}", resource)
            }
            SubagentError::Configuration { .. } => {
                "Configuration error detected. Please check your settings.".to_string()
            }
            SubagentError::Dependency { dependency, .. } => {
                format!("External service {} is unavailable", dependency)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn test_retry_success_after_failure() {
        let handler = SubagentErrorHandler::new(2, Duration::from_millis(10), Duration::from_millis(100));
        let counter = Arc::new(AtomicU32::new(0));
        let counter_clone = counter.clone();

        let result = handler.execute_with_retry("test_operation", move || {
            let counter = counter_clone.clone();
            Box::pin(async move {
                let count = counter.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    Err("Simulated failure")
                } else {
                    Ok("Success")
                }
            })
        }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "Success");
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn test_retry_exhaustion() {
        let handler = SubagentErrorHandler::new(1, Duration::from_millis(10), Duration::from_millis(100));

        let result = handler.execute_with_retry("test_operation", || {
            Box::pin(async { Err::<(), _>("Always fails") })
        }).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            SubagentError::OperationFailed { attempts, .. } => {
                assert_eq!(attempts, 2); // Initial attempt + 1 retry
            }
            _ => panic!("Unexpected error type"),
        }
    }

    #[test]
    fn test_error_classification() {
        let handler = SubagentErrorHandler::default();
        
        struct TestError(String);
        impl std::fmt::Display for TestError {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "{}", self.0)
            }
        }
        impl std::error::Error for TestError {}

        assert_eq!(handler.classify_error(&TestError("Connection timeout".to_string())), ErrorSeverity::Transient);
        assert_eq!(handler.classify_error(&TestError("Rate limit exceeded".to_string())), ErrorSeverity::RateLimit);
        assert_eq!(handler.classify_error(&TestError("Not found".to_string())), ErrorSeverity::Permanent);
        assert_eq!(handler.classify_error(&TestError("Unauthorized access".to_string())), ErrorSeverity::Authentication);
    }
}