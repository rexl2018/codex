use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::SystemTime;
use tokio::sync::RwLock;

/// Context storage abstract interface
#[async_trait]
pub trait IContextRepository: Send + Sync {
    /// Store context
    async fn store_context(&self, context: Context) -> Result<(), ContextError>;

    /// Get context by ID
    async fn get_context(&self, id: &str) -> Result<Option<Context>, ContextError>;

    /// Batch get contexts
    async fn get_contexts(&self, ids: &[String]) -> Result<Vec<Context>, ContextError>;

    /// Query contexts
    async fn query_contexts(&self, query: &ContextQuery) -> Result<Vec<Context>, ContextError>;

    /// Update context
    async fn update_context(
        &self,
        id: &str,
        content: String,
        reason: String,
    ) -> Result<(), ContextError>;

    /// Delete context
    async fn delete_context(&self, id: &str) -> Result<(), ContextError>;

    /// Get statistics
    async fn get_stats(&self) -> Result<ContextStats, ContextError>;
}

/// Context data model (complete object stored in context store)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Context {
    /// Unique identifier
    pub id: String,
    /// Context summary (describes the nature and purpose of the content)
    pub summary: String,
    /// Context content
    pub content: String,
    /// Creator (task ID)
    pub created_by: String,
    /// Associated task ID
    pub task_id: Option<String>,
    /// Creation time
    pub created_at: SystemTime,
    /// Last update time
    pub updated_at: SystemTime,
    /// Tags
    pub tags: Vec<String>,
    /// Metadata
    pub metadata: HashMap<String, String>,
}

/// Context query conditions
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextQuery {
    /// Query by ID
    pub ids: Option<Vec<String>>,
    /// Query by tags
    pub tags: Option<Vec<String>>,
    /// Query by creator
    pub created_by: Option<String>,
    /// Maximum return count
    pub limit: Option<usize>,
}

/// Context statistics
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextStats {
    pub total_contexts: usize,
    pub total_size_bytes: usize,
    pub contexts_by_creator: HashMap<String, usize>,
    pub contexts_by_task: HashMap<String, usize>,
}

/// Context error types
#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("Context not found: {id}")]
    NotFound { id: String },
    #[error("Context already exists: {id}")]
    AlreadyExists { id: String },
    #[error("Invalid context ID: {id}")]
    InvalidId { id: String },
    #[error("Storage error: {message}")]
    StorageError { message: String },
    #[error("Serialization error: {0}")]
    SerializationError(#[from] serde_json::Error),
}

/// In-memory context storage implementation
pub struct InMemoryContextRepository {
    contexts: Arc<RwLock<HashMap<String, Context>>>,
}

impl InMemoryContextRepository {
    pub fn new() -> Self {
        Self {
            contexts: Arc::new(RwLock::new(HashMap::new())),
        }
    }
}

impl Default for InMemoryContextRepository {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl IContextRepository for InMemoryContextRepository {
    async fn store_context(&self, context: Context) -> Result<(), ContextError> {
        let mut contexts = self.contexts.write().await;

        if contexts.contains_key(&context.id) {
            return Err(ContextError::AlreadyExists { id: context.id });
        }

        contexts.insert(context.id.clone(), context);
        Ok(())
    }

    async fn get_context(&self, id: &str) -> Result<Option<Context>, ContextError> {
        let contexts = self.contexts.read().await;
        Ok(contexts.get(id).cloned())
    }

    async fn get_contexts(&self, ids: &[String]) -> Result<Vec<Context>, ContextError> {
        let contexts = self.contexts.read().await;
        let mut result = Vec::new();

        for id in ids {
            if let Some(context) = contexts.get(id) {
                result.push(context.clone());
            }
        }

        Ok(result)
    }

    async fn query_contexts(&self, query: &ContextQuery) -> Result<Vec<Context>, ContextError> {
        let contexts = self.contexts.read().await;
        let mut result: Vec<Context> = contexts.values().cloned().collect();

        // Apply filter conditions
        if let Some(ref ids) = query.ids {
            result.retain(|ctx| ids.contains(&ctx.id));
        }

        if let Some(ref tags) = query.tags {
            result.retain(|ctx| tags.iter().any(|tag| ctx.tags.contains(tag)));
        }

        if let Some(ref created_by) = query.created_by {
            result.retain(|ctx| &ctx.created_by == created_by);
        }

        // Apply limit
        if let Some(limit) = query.limit {
            result.truncate(limit);
        }

        Ok(result)
    }

    async fn update_context(
        &self,
        id: &str,
        content: String,
        reason: String,
    ) -> Result<(), ContextError> {
        let mut contexts = self.contexts.write().await;

        let context = contexts
            .get_mut(id)
            .ok_or_else(|| ContextError::NotFound { id: id.to_string() })?;

        context.content = content;
        context.updated_at = SystemTime::now();
        context
            .metadata
            .insert("last_update_reason".to_string(), reason);

        Ok(())
    }

    async fn delete_context(&self, id: &str) -> Result<(), ContextError> {
        let mut contexts = self.contexts.write().await;

        contexts
            .remove(id)
            .ok_or_else(|| ContextError::NotFound { id: id.to_string() })?;

        Ok(())
    }

    async fn get_stats(&self) -> Result<ContextStats, ContextError> {
        let contexts = self.contexts.read().await;

        let mut contexts_by_creator = HashMap::new();
        let mut contexts_by_task = HashMap::new();
        let mut total_size_bytes = 0;

        for context in contexts.values() {
            // Count by creator
            *contexts_by_creator
                .entry(context.created_by.clone())
                .or_insert(0) += 1;

            // Count by task
            if let Some(ref task_id) = context.task_id {
                *contexts_by_task.entry(task_id.clone()).or_insert(0) += 1;
            }

            // Calculate size
            total_size_bytes += context.content.len() + context.summary.len();
        }

        Ok(ContextStats {
            total_contexts: contexts.len(),
            total_size_bytes,
            contexts_by_creator,
            contexts_by_task,
        })
    }
}

/// Helper functions for context management
impl Context {
    /// Create a new context
    pub fn new(
        id: String,
        summary: String,
        content: String,
        created_by: String,
        task_id: Option<String>,
    ) -> Self {
        let now = SystemTime::now();
        Self {
            id,
            summary,
            content,
            created_by,
            task_id,
            created_at: now,
            updated_at: now,
            tags: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Add a tag to the context
    pub fn add_tag(&mut self, tag: String) {
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
    }

    /// Add metadata to the context
    pub fn add_metadata(&mut self, key: String, value: String) {
        self.metadata.insert(key, value);
    }

    /// Get the size of the context in bytes
    pub fn size_bytes(&self) -> usize {
        self.content.len() + self.summary.len()
    }
}

impl From<Context> for codex_protocol::protocol::ContextSummary {
    fn from(context: Context) -> Self {
        let size_bytes = context.size_bytes();
        Self {
            id: context.id,
            summary: context.summary,
            created_by: context.created_by,
            created_at: context
                .created_at
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string(),
            size_bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_store_and_get_context() {
        let repo = InMemoryContextRepository::new();
        let context = Context::new(
            "test-1".to_string(),
            "Test context".to_string(),
            "This is test content".to_string(),
            "orchestrator".to_string(),
            None,
        );

        // Store context
        repo.store_context(context.clone()).await.unwrap();

        // Get context
        let retrieved = repo.get_context("test-1").await.unwrap();
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().content, "This is test content");
    }

    #[tokio::test]
    async fn test_query_contexts() {
        let repo = InMemoryContextRepository::new();

        // Store multiple contexts
        let context1 = Context::new(
            "test-1".to_string(),
            "Test context 1".to_string(),
            "Content 1".to_string(),
            "task-1".to_string(),
            Some("task-1".to_string()),
        );

        let context2 = Context::new(
            "test-2".to_string(),
            "Test context 2".to_string(),
            "Content 2".to_string(),
            "task-2".to_string(),
            Some("task-2".to_string()),
        );

        repo.store_context(context1).await.unwrap();
        repo.store_context(context2).await.unwrap();

        // Query by creator
        let query = ContextQuery {
            ids: None,
            tags: None,
            created_by: Some("task-1".to_string()),
            limit: None,
        };

        let results = repo.query_contexts(&query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "test-1");
    }

    #[tokio::test]
    async fn test_update_context() {
        let repo = InMemoryContextRepository::new();
        let context = Context::new(
            "test-1".to_string(),
            "Test context".to_string(),
            "Original content".to_string(),
            "orchestrator".to_string(),
            None,
        );

        repo.store_context(context).await.unwrap();

        // Update context
        repo.update_context(
            "test-1",
            "Updated content".to_string(),
            "Test update".to_string(),
        )
        .await
        .unwrap();

        // Verify update
        let updated = repo.get_context("test-1").await.unwrap().unwrap();
        assert_eq!(updated.content, "Updated content");
        assert!(updated.metadata.contains_key("last_update_reason"));
    }

    #[tokio::test]
    async fn test_get_stats() {
        let repo = InMemoryContextRepository::new();

        let context1 = Context::new(
            "test-1".to_string(),
            "Test 1".to_string(),
            "Content 1".to_string(),
            "task-1".to_string(),
            Some("task-1".to_string()),
        );

        let context2 = Context::new(
            "test-2".to_string(),
            "Test 2".to_string(),
            "Content 2".to_string(),
            "task-1".to_string(),
            Some("task-1".to_string()),
        );

        repo.store_context(context1).await.unwrap();
        repo.store_context(context2).await.unwrap();

        let stats = repo.get_stats().await.unwrap();
        assert_eq!(stats.total_contexts, 2);
        assert_eq!(stats.contexts_by_creator.get("task-1"), Some(&2));
        assert_eq!(stats.contexts_by_task.get("task-1"), Some(&2));
    }
}
