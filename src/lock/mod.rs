//! PR-level project locks.

mod dynamodb;

pub use dynamodb::DynamoDbLock;

use crate::github::GithubClient;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

#[async_trait]
pub trait Lock: Send + Sync {
    /// Try to acquire `resource` for PR `pr`. Returns `false` when held by
    /// someone else.
    async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<bool>;
    async fn unlock(&self, resource: &str) -> anyhow::Result<bool>;
    /// PR number currently holding `resource`, if any.
    async fn holder(&self, resource: &str) -> anyhow::Result<Option<u64>>;
    fn name(&self) -> &'static str;
}

/// Lock that always succeeds (used when `pr_locks: false`).
pub struct NoopLock;

#[async_trait]
impl Lock for NoopLock {
    async fn lock(&self, _pr: u64, _resource: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn unlock(&self, _resource: &str) -> anyhow::Result<bool> {
        Ok(true)
    }
    async fn holder(&self, _resource: &str) -> anyhow::Result<Option<u64>> {
        Ok(None)
    }
    fn name(&self) -> &'static str {
        "noop"
    }
}

/// Create the lock backend from the environment (`LOCK_PROVIDER`,
/// `DISABLE_LOCKING`, `NIKAMA_LOCK_TABLE`).
pub async fn from_env(enabled: bool) -> anyhow::Result<Arc<dyn Lock>> {
    if !enabled || crate::util::env_is_true("DISABLE_LOCKING") {
        return Ok(Arc::new(NoopLock));
    }
    let provider = crate::util::env_opt("LOCK_PROVIDER")
        .unwrap_or_else(|| "aws".to_string())
        .to_ascii_lowercase();
    match provider.as_str() {
        "aws" | "dynamodb" => {
            let table = crate::util::env_first(&["NIKAMA_LOCK_TABLE", "DIGGER_LOCK_TABLE"])
                .unwrap_or_else(|| dynamodb::DEFAULT_TABLE.to_string());
            Ok(Arc::new(DynamoDbLock::new(&table).await?))
        }
        "noop" | "none" => Ok(Arc::new(NoopLock)),
        "gcp" | "azure" => anyhow::bail!(
            "LOCK_PROVIDER={} is not supported by this tool (only aws/noop)",
            provider
        ),
        other => anyhow::bail!("unknown LOCK_PROVIDER '{}'", other),
    }
}

/// Outcome of trying to lock a project for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    AlreadyHeld,
    HeldByOther(u64),
}

/// PR-lock semantics on top of a `Lock` backend: a lock held by a closed PR
/// is taken over; a lock held by an open PR blocks.
pub struct PrLocker {
    pub backend: Arc<dyn Lock>,
    pub github: GithubClient,
    closed_cache: Mutex<HashMap<u64, bool>>,
}

impl PrLocker {
    pub fn new(backend: Arc<dyn Lock>, github: GithubClient) -> Self {
        Self {
            backend,
            github,
            closed_cache: Mutex::new(HashMap::new()),
        }
    }

    async fn is_pr_closed(&self, pr: u64) -> anyhow::Result<bool> {
        if let Some(v) = self.closed_cache.lock().await.get(&pr) {
            return Ok(*v);
        }
        let closed = self.github.get_pr(pr).await?.is_closed();
        self.closed_cache.lock().await.insert(pr, closed);
        Ok(closed)
    }

    pub async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<LockOutcome> {
        if let Some(holder) = self.backend.holder(resource).await? {
            if holder == pr {
                return Ok(LockOutcome::AlreadyHeld);
            }
            if self.is_pr_closed(holder).await? {
                tracing::info!(resource, holder, "lock held by a closed PR; releasing it");
                self.backend.unlock(resource).await?;
            } else {
                return Ok(LockOutcome::HeldByOther(holder));
            }
        }
        if self.backend.lock(pr, resource).await? {
            Ok(LockOutcome::Acquired)
        } else {
            // Lost a race; report the current holder.
            match self.backend.holder(resource).await? {
                Some(h) if h != pr => Ok(LockOutcome::HeldByOther(h)),
                Some(_) => Ok(LockOutcome::AlreadyHeld),
                None => Ok(LockOutcome::HeldByOther(0)),
            }
        }
    }

    /// Release only when `pr` is the holder. Returns whether a lock was released.
    pub async fn unlock(&self, pr: u64, resource: &str) -> anyhow::Result<bool> {
        match self.backend.holder(resource).await? {
            Some(h) if h == pr => self.backend.unlock(resource).await,
            _ => Ok(false),
        }
    }

    /// Release regardless of holder.
    pub async fn force_unlock(&self, resource: &str) -> anyhow::Result<bool> {
        if self.backend.holder(resource).await?.is_none() {
            return Ok(false);
        }
        self.backend.unlock(resource).await
    }
}
