//! PR-level project locks.

mod dynamodb;
mod gcs;
mod s3;
#[cfg(test)]
mod tests;

pub use dynamodb::DynamoDbLock;
pub use gcs::GcsLock;
pub use s3::S3Lock;

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
    /// Resources whose name starts with `prefix` (`<owner>/<repo>#`) and
    /// whose holder is `pr`.
    async fn held_by(&self, pr: u64, prefix: &str) -> anyhow::Result<Vec<String>>;
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
    async fn held_by(&self, _pr: u64, _prefix: &str) -> anyhow::Result<Vec<String>> {
        Ok(vec![])
    }
    fn name(&self) -> &'static str {
        "noop"
    }
}

/// Create the lock backend from the environment (`LOCK_PROVIDER`,
/// `DISABLE_LOCKING`, `NIKAMA_LOCK_TABLE`, `AWS_S3_LOCK_BUCKET`,
/// `GOOGLE_STORAGE_LOCK_BUCKET`).
pub async fn from_env(enabled: bool) -> anyhow::Result<Arc<dyn Lock>> {
    if !enabled || crate::util::env_is_true("DISABLE_LOCKING") {
        return Ok(Arc::new(NoopLock));
    }
    let provider = crate::util::env_opt("LOCK_PROVIDER")
        .unwrap_or_else(|| "aws".to_string())
        .to_ascii_lowercase();
    let lock: Arc<dyn Lock> = match provider.as_str() {
        "aws" | "dynamodb" => {
            let table = crate::util::env_first(&["NIKAMA_LOCK_TABLE", "DIGGER_LOCK_TABLE"])
                .unwrap_or_else(|| dynamodb::DEFAULT_TABLE.to_string());
            Arc::new(DynamoDbLock::new(&table).await?)
        }
        "s3" => {
            let bucket = crate::util::env_opt("AWS_S3_LOCK_BUCKET").ok_or_else(|| {
                anyhow::anyhow!("AWS_S3_LOCK_BUCKET is required when LOCK_PROVIDER=s3")
            })?;
            Arc::new(S3Lock::new(bucket.trim()).await?)
        }
        "noop" | "none" => Arc::new(NoopLock),
        "gcp" | "gcs" => {
            let bucket = crate::util::env_opt("GOOGLE_STORAGE_LOCK_BUCKET")
                .map(|b| b.trim().to_ascii_lowercase())
                .ok_or_else(|| {
                    anyhow::anyhow!("GOOGLE_STORAGE_LOCK_BUCKET is required when LOCK_PROVIDER=gcp")
                })?;
            let client = crate::gcs::GcsClient::from_env()?;
            Arc::new(GcsLock::new(client, &bucket))
        }
        "azure" => anyhow::bail!(
            "LOCK_PROVIDER={} is not supported by this tool (only aws, s3, gcp or noop)",
            provider
        ),
        other => anyhow::bail!("unknown LOCK_PROVIDER '{}'", other),
    };
    tracing::info!(provider = lock.name(), "lock backend");
    Ok(lock)
}

/// Outcome of trying to lock a project for a PR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LockOutcome {
    Acquired,
    AlreadyHeld,
    HeldByOther(u64),
}

/// Who holds a resource, from the point of view of one PR, without taking it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HolderStatus {
    /// Nobody, or a closed PR whose lock can be taken over.
    Free,
    /// This PR.
    Ours,
    /// Another open PR.
    Other(u64),
}

/// Whether a pull request is closed. A lock held by a closed PR may be taken
/// over, so this is the one thing the locker needs to know about pull
/// requests; the GitHub client answers it in production.
#[async_trait]
pub trait PrState: Send + Sync {
    async fn is_closed(&self, pr: u64) -> anyhow::Result<bool>;
}

/// PR-lock semantics on top of a `Lock` backend: a lock held by a closed PR
/// is taken over; a lock held by an open PR blocks.
pub struct PrLocker {
    pub backend: Arc<dyn Lock>,
    prs: Arc<dyn PrState>,
    closed_cache: Mutex<HashMap<u64, bool>>,
}

impl PrLocker {
    pub fn new(backend: Arc<dyn Lock>, prs: Arc<dyn PrState>) -> Self {
        Self {
            backend,
            prs,
            closed_cache: Mutex::new(HashMap::new()),
        }
    }

    async fn is_pr_closed(&self, pr: u64) -> anyhow::Result<bool> {
        if let Some(v) = self.closed_cache.lock().await.get(&pr) {
            return Ok(*v);
        }
        let closed = self.prs.is_closed(pr).await?;
        self.closed_cache.lock().await.insert(pr, closed);
        Ok(closed)
    }

    /// Look at the holder of `resource` without acquiring it.
    pub async fn status(&self, pr: u64, resource: &str) -> anyhow::Result<HolderStatus> {
        match self.backend.holder(resource).await? {
            None => Ok(HolderStatus::Free),
            Some(h) if h == pr => Ok(HolderStatus::Ours),
            Some(h) if self.is_pr_closed(h).await? => Ok(HolderStatus::Free),
            Some(h) => Ok(HolderStatus::Other(h)),
        }
    }

    /// Resources under `prefix` held by `pr`.
    pub async fn held_by(&self, pr: u64, prefix: &str) -> anyhow::Result<Vec<String>> {
        self.backend.held_by(pr, prefix).await
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

/// In-memory lock backend shared by the unit tests of this crate.
#[cfg(test)]
pub(crate) mod testing {
    use super::Lock;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use tokio::sync::Mutex;

    #[derive(Default)]
    pub(crate) struct MemLock {
        pub(crate) held: Mutex<HashMap<String, u64>>,
    }

    impl MemLock {
        pub(crate) fn holding(entries: &[(&str, u64)]) -> Self {
            let m = Self::default();
            let mut h = m.held.try_lock().unwrap();
            for (r, pr) in entries {
                h.insert(r.to_string(), *pr);
            }
            drop(h);
            m
        }
    }

    #[async_trait]
    impl Lock for MemLock {
        async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<bool> {
            let mut h = self.held.lock().await;
            match h.get(resource) {
                Some(holder) if *holder != pr => Ok(false),
                _ => {
                    h.insert(resource.to_string(), pr);
                    Ok(true)
                }
            }
        }
        async fn unlock(&self, resource: &str) -> anyhow::Result<bool> {
            Ok(self.held.lock().await.remove(resource).is_some())
        }
        async fn holder(&self, resource: &str) -> anyhow::Result<Option<u64>> {
            Ok(self.held.lock().await.get(resource).copied())
        }
        async fn held_by(&self, pr: u64, prefix: &str) -> anyhow::Result<Vec<String>> {
            let h = self.held.lock().await;
            let mut v: Vec<String> = h
                .iter()
                .filter(|(r, p)| **p == pr && r.starts_with(prefix))
                .map(|(r, _)| r.clone())
                .collect();
            v.sort();
            Ok(v)
        }
        fn name(&self) -> &'static str {
            "mem"
        }
    }
}
