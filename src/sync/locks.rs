use crate::error::AppError;
use dashmap::DashMap;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

/// Named async mutex manager with optional timeout.
/// Mirrors the `modules/locks.js` behavior from the Node.js codebase.
#[derive(Debug, Clone)]
pub struct NamedLockManager {
    locks: Arc<DashMap<String, Arc<Mutex<()>>>>,
}

impl NamedLockManager {
    pub fn new() -> Self {
        Self {
            locks: Arc::new(DashMap::new()),
        }
    }

    /// Get or create a named mutex.
    fn get_lock(&self, name: &str) -> Arc<Mutex<()>> {
        self.locks
            .entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Run a future exclusively under the named lock with a timeout.
    /// Mirrors `with_lock(name, fn, { timeout_ms })` from locks.js.
    pub async fn with_lock<F, Fut, T>(
        &self,
        name: &str,
        timeout: Duration,
        f: F,
    ) -> Result<T, AppError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, AppError>>,
    {
        let mutex = self.get_lock(name);
        let guard = tokio::time::timeout(timeout, mutex.lock())
            .await
            .map_err(|_| {
                AppError::Internal(format!(
                    "Lock acquisition timeout after {:?}: {}",
                    timeout, name
                ))
            })?;
        let result = f().await;
        drop(guard);
        result
    }

    /// Non-blocking lock attempt. Returns true if lock was acquired.
    /// Mirrors `try_acquire_lock(name)` from locks.js.
    pub fn is_locked(&self, name: &str) -> bool {
        if let Some(mutex) = self.locks.get(name) {
            mutex.try_lock().is_err()
        } else {
            false
        }
    }
}

impl Default for NamedLockManager {
    fn default() -> Self {
        Self::new()
    }
}
