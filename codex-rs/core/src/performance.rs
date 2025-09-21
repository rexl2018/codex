use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock, Semaphore};
use tokio::time::interval;
use tracing::{debug, warn, info};
use codex_protocol::protocol::{Event, EventMsg};
use crate::session::Session;

/// Performance optimization utilities for agent communication and coordination
pub struct PerformanceOptimizer {
    /// Batch event sender for reducing individual event overhead
    event_batcher: EventBatcher,
    /// Connection pool for managing subagent manager connections
    connection_pool: ConnectionPool,
    /// Cache for frequently accessed data
    cache: Arc<RwLock<PerformanceCache>>,
    /// Rate limiter for preventing overwhelming the system
    rate_limiter: Arc<Semaphore>,
}

impl PerformanceOptimizer {
    pub fn new(max_concurrent_operations: usize) -> Self {
        Self {
            event_batcher: EventBatcher::new(),
            connection_pool: ConnectionPool::new(),
            cache: Arc::new(RwLock::new(PerformanceCache::new())),
            rate_limiter: Arc::new(Semaphore::new(max_concurrent_operations)),
        }
    }

    /// Execute an operation with rate limiting
    pub async fn execute_with_rate_limit<F, T>(&self, operation: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let _permit = self.rate_limiter.acquire().await.unwrap();
        operation.await
    }

    /// Get cached data or compute it if not available
    pub async fn get_or_compute<T, F>(&self, key: &str, compute_fn: F) -> T
    where
        T: Clone + Send + Sync + 'static,
        F: std::future::Future<Output = T>,
    {
        // Try to get from cache first
        {
            let cache = self.cache.read().await;
            if let Some(cached_value) = cache.get::<T>(key) {
                return cached_value;
            }
        }

        // Compute the value
        let value = compute_fn.await;

        // Store in cache
        {
            let mut cache = self.cache.write().await;
            cache.insert(key.to_string(), value.clone());
        }

        value
    }

    /// Send event through the batcher for improved performance
    pub async fn send_event_batched(&self, session: Arc<Session>, event: Event) {
        self.event_batcher.queue_event(session, event).await;
    }

    /// Get performance metrics
    pub async fn get_metrics(&self) -> PerformanceMetrics {
        let cache = self.cache.read().await;
        PerformanceMetrics {
            cache_hit_rate: cache.hit_rate(),
            cache_size: cache.size(),
            available_permits: self.rate_limiter.available_permits(),
            batched_events_count: self.event_batcher.queued_count().await,
        }
    }
}

/// Batches events to reduce the overhead of individual event sending
pub struct EventBatcher {
    sender: mpsc::UnboundedSender<BatchedEvent>,
    queued_count: Arc<std::sync::atomic::AtomicUsize>,
}

struct BatchedEvent {
    session: Arc<Session>,
    event: Event,
}

impl EventBatcher {
    pub fn new() -> Self {
        let (sender, mut receiver) = mpsc::unbounded_channel::<BatchedEvent>();
        let queued_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let queued_count_clone = queued_count.clone();

        // Spawn background task to process batched events
        tokio::spawn(async move {
            let mut interval = interval(Duration::from_millis(10)); // Batch every 10ms
            let mut batch = Vec::new();
            let mut session_batches: HashMap<String, Vec<Event>> = HashMap::new();

            loop {
                tokio::select! {
                    // Collect events into batches
                    event = receiver.recv() => {
                        match event {
                            Some(batched_event) => {
                                let session_id = format!("session_{:p}", batched_event.session.as_ref());
                                session_batches.entry(session_id).or_default().push(batched_event.event.clone());
                                batch.push(batched_event);
                                queued_count_clone.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            }
                            None => break, // Channel closed
                        }
                    }
                    // Process batches periodically
                    _ = interval.tick() => {
                        if !batch.is_empty() {
                            Self::process_batch(batch.drain(..).collect()).await;
                            session_batches.clear();
                            queued_count_clone.store(0, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }
        });

        Self {
            sender,
            queued_count,
        }
    }

    pub async fn queue_event(&self, session: Arc<Session>, event: Event) {
        let batched_event = BatchedEvent { session, event };
        if let Err(e) = self.sender.send(batched_event) {
            warn!("Failed to queue event for batching: {}", e);
        }
    }

    pub async fn queued_count(&self) -> usize {
        self.queued_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn process_batch(batch: Vec<BatchedEvent>) {
        debug!("Processing batch of {} events", batch.len());
        
        // Group events by session for efficient processing
        let mut session_events: HashMap<String, (Arc<Session>, Vec<Event>)> = HashMap::new();
        
        for batched_event in batch {
            let session_id = format!("session_{:p}", batched_event.session.as_ref());
            session_events
                .entry(session_id)
                .or_insert_with(|| (batched_event.session.clone(), Vec::new()))
                .1
                .push(batched_event.event);
        }

        // Send events in batches per session
        for (session_id, (session, events)) in session_events {
            debug!("Sending {} events for session {}", events.len(), session_id);
            for event in events {
                session.send_event(event).await;
            }
        }
    }
}

/// Connection pool for managing expensive resources
pub struct ConnectionPool {
    // In a real implementation, this would manage actual connections
    // For now, it's a placeholder for the concept
    _placeholder: (),
}

impl ConnectionPool {
    pub fn new() -> Self {
        Self {
            _placeholder: (),
        }
    }
}

/// Cache for frequently accessed data with TTL support
pub struct PerformanceCache {
    data: HashMap<String, CacheEntry>,
    hits: AtomicU64,
    misses: AtomicU64,
}

struct CacheEntry {
    value: Box<dyn std::any::Any + Send + Sync>,
    created_at: Instant,
    ttl: Duration,
}

impl PerformanceCache {
    pub fn new() -> Self {
        Self {
            data: HashMap::new(),
            hits: AtomicU64::new(0),
            misses: AtomicU64::new(0),
        }
    }

    pub fn insert<T: Clone + Send + Sync + 'static>(&mut self, key: String, value: T) {
        let entry = CacheEntry {
            value: Box::new(value),
            created_at: Instant::now(),
            ttl: Duration::from_secs(300), // 5 minutes default TTL
        };
        self.data.insert(key, entry);
    }

    pub fn get<T: Clone + Send + Sync + 'static>(&self, key: &str) -> Option<T> {
        if let Some(entry) = self.data.get(key) {
            // Check if entry has expired
            if entry.created_at.elapsed() > entry.ttl {
                self.misses.fetch_add(1, Ordering::Relaxed);
                return None;
            }

            // Try to downcast the value
            if let Some(value) = entry.value.downcast_ref::<T>() {
                self.hits.fetch_add(1, Ordering::Relaxed);
                return Some(value.clone());
            }
        }
        
        self.misses.fetch_add(1, Ordering::Relaxed);
        None
    }

    pub fn hit_rate(&self) -> f64 {
        let hits = self.hits.load(Ordering::Relaxed);
        let misses = self.misses.load(Ordering::Relaxed);
        if hits + misses == 0 {
            0.0
        } else {
            hits as f64 / (hits + misses) as f64
        }
    }

    pub fn size(&self) -> usize {
        self.data.len()
    }

    pub fn cleanup_expired(&mut self) {
        let now = Instant::now();
        self.data.retain(|_, entry| now.duration_since(entry.created_at) <= entry.ttl);
    }
}

/// Performance metrics for monitoring
#[derive(Debug, Clone)]
pub struct PerformanceMetrics {
    pub cache_hit_rate: f64,
    pub cache_size: usize,
    pub available_permits: usize,
    pub batched_events_count: usize,
}

/// Optimized event sender that reduces allocation overhead
#[derive(Clone)]
pub struct OptimizedEventSender {
    session: Arc<Session>,
    performance_optimizer: Arc<PerformanceOptimizer>,
}

impl OptimizedEventSender {
    pub fn new(session: Arc<Session>, performance_optimizer: Arc<PerformanceOptimizer>) -> Self {
        Self {
            session,
            performance_optimizer,
        }
    }

    /// Send an event with performance optimizations
    pub async fn send_event(&self, event: Event) {
        self.performance_optimizer
            .send_event_batched(self.session.clone(), event)
            .await;
    }

    /// Send an error event with standardized formatting
    pub async fn send_error(&self, sub_id: String, message: String) {
        let event = Event {
            id: sub_id,
            msg: EventMsg::Error(codex_protocol::protocol::ErrorEvent { message }),
        };
        self.send_event(event).await;
    }

    /// Send a background event with standardized formatting
    pub async fn send_background_event(&self, sub_id: String, message: String) {
        let event = Event {
            id: sub_id,
            msg: EventMsg::BackgroundEvent(codex_protocol::protocol::BackgroundEventEvent { message }),
        };
        self.send_event(event).await;
    }
}

/// Task pool for managing concurrent operations efficiently
pub struct TaskPool {
    semaphore: Arc<Semaphore>,
    active_tasks: Arc<std::sync::atomic::AtomicUsize>,
}

impl TaskPool {
    pub fn new(max_concurrent_tasks: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent_tasks)),
            active_tasks: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    /// Execute a task with concurrency control
    pub async fn execute<F, T>(&self, task: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        let _permit = self.semaphore.acquire().await.unwrap();
        self.active_tasks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        
        let result = task.await;
        
        self.active_tasks.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        result
    }

    /// Get the number of currently active tasks
    pub fn active_count(&self) -> usize {
        self.active_tasks.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Get the number of available task slots
    pub fn available_slots(&self) -> usize {
        self.semaphore.available_permits()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::sleep;

    #[tokio::test]
    async fn test_performance_cache() {
        let mut cache = PerformanceCache::new();
        
        // Test insertion and retrieval
        cache.insert("test_key".to_string(), "test_value".to_string());
        let value: Option<String> = cache.get("test_key");
        assert_eq!(value, Some("test_value".to_string()));
        
        // Test cache hit rate
        let _: Option<String> = cache.get("nonexistent_key");
        assert!(cache.hit_rate() > 0.0);
    }

    #[tokio::test]
    async fn test_task_pool() {
        let pool = TaskPool::new(2);
        
        let start = std::time::Instant::now();
        
        // Start 3 tasks that each take 100ms
        let tasks = (0..3).map(|i| {
            pool.execute(async move {
                sleep(Duration::from_millis(100)).await;
                i
            })
        });
        
        let results: Vec<_> = futures::future::join_all(tasks).await;
        let elapsed = start.elapsed();
        
        // Should take at least 200ms due to concurrency limit of 2
        assert!(elapsed >= Duration::from_millis(200));
        assert_eq!(results, vec![0, 1, 2]);
    }

    #[tokio::test]
    async fn test_rate_limiter() {
        let optimizer = PerformanceOptimizer::new(1);
        
        let start = std::time::Instant::now();
        
        // Execute 2 operations that should be rate limited
        let task1 = optimizer.execute_with_rate_limit(async {
            sleep(Duration::from_millis(50)).await;
            1
        });
        
        let task2 = optimizer.execute_with_rate_limit(async {
            sleep(Duration::from_millis(50)).await;
            2
        });
        
        let (result1, result2) = tokio::join!(task1, task2);
        let elapsed = start.elapsed();
        
        // Should take at least 100ms due to rate limiting
        assert!(elapsed >= Duration::from_millis(100));
        assert_eq!(result1, 1);
        assert_eq!(result2, 2);
    }
}