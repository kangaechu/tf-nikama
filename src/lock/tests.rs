//! PR lock semantics tests with in-memory backend and pull request state.

use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::testing::MemLock;

/// Pull requests known to be closed; every other number is open.
#[derive(Default)]
struct ClosedPrs {
    closed: Vec<u64>,
    lookups: AtomicUsize,
}

#[async_trait]
impl PrState for ClosedPrs {
    async fn is_closed(&self, pr: u64) -> anyhow::Result<bool> {
        self.lookups.fetch_add(1, Ordering::SeqCst);
        Ok(self.closed.contains(&pr))
    }
}

fn locker(closed: &[u64], backend: Arc<dyn Lock>) -> (PrLocker, Arc<ClosedPrs>) {
    let prs = Arc::new(ClosedPrs {
        closed: closed.to_vec(),
        lookups: AtomicUsize::new(0),
    });
    (PrLocker::new(backend, prs.clone()), prs)
}

#[tokio::test]
async fn acquire_then_already_held() {
    let (l, _) = locker(&[], Arc::new(MemLock::default()));
    assert_eq!(l.lock(1, "o/r#a").await.unwrap(), LockOutcome::Acquired);
    assert_eq!(l.lock(1, "o/r#a").await.unwrap(), LockOutcome::AlreadyHeld);
    assert_eq!(l.backend.holder("o/r#a").await.unwrap(), Some(1));
}

#[tokio::test]
async fn open_pr_holder_blocks_and_is_cached() {
    let backend = Arc::new(MemLock::default());
    backend.lock(9, "o/r#a").await.unwrap();
    let (l, prs) = locker(&[], backend);
    assert_eq!(
        l.lock(1, "o/r#a").await.unwrap(),
        LockOutcome::HeldByOther(9)
    );
    assert_eq!(
        l.lock(1, "o/r#a").await.unwrap(),
        LockOutcome::HeldByOther(9)
    );
    // The PR state lookup is cached per holder.
    assert_eq!(prs.lookups.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closed_pr_holder_is_taken_over() {
    let backend = Arc::new(MemLock::default());
    backend.lock(9, "o/r#a").await.unwrap();
    let (l, _) = locker(&[9], backend);
    assert_eq!(l.lock(1, "o/r#a").await.unwrap(), LockOutcome::Acquired);
    assert_eq!(l.backend.holder("o/r#a").await.unwrap(), Some(1));
}

#[tokio::test]
async fn unlock_only_by_holder_and_force() {
    let (l, _) = locker(&[], Arc::new(MemLock::default()));
    l.lock(1, "o/r#a").await.unwrap();
    assert!(!l.unlock(2, "o/r#a").await.unwrap());
    assert_eq!(l.backend.holder("o/r#a").await.unwrap(), Some(1));
    assert!(l.unlock(1, "o/r#a").await.unwrap());
    assert_eq!(l.backend.holder("o/r#a").await.unwrap(), None);
    l.lock(3, "o/r#a").await.unwrap();
    assert!(l.force_unlock("o/r#a").await.unwrap());
    assert!(!l.force_unlock("o/r#a").await.unwrap());
}

#[tokio::test]
async fn noop_lock_behaviour() {
    let n = NoopLock;
    assert!(n.lock(1, "x").await.unwrap());
    assert!(n.unlock("x").await.unwrap());
    assert_eq!(n.holder("x").await.unwrap(), None);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn from_env_selection() {
    let _g = crate::util::test_env_guard();
    for v in ["LOCK_PROVIDER", "DISABLE_LOCKING"] {
        std::env::remove_var(v);
    }
    assert_eq!(from_env(false).await.unwrap().name(), "noop");

    std::env::set_var("DISABLE_LOCKING", "true");
    assert_eq!(from_env(true).await.unwrap().name(), "noop");
    std::env::remove_var("DISABLE_LOCKING");

    std::env::set_var("LOCK_PROVIDER", "noop");
    assert_eq!(from_env(true).await.unwrap().name(), "noop");

    std::env::remove_var("GOOGLE_STORAGE_LOCK_BUCKET");
    std::env::set_var("LOCK_PROVIDER", "gcp");
    let err = from_env(true).await.err().unwrap().to_string();
    assert!(err.contains("GOOGLE_STORAGE_LOCK_BUCKET"), "{}", err);

    std::env::set_var("GOOGLE_STORAGE_LOCK_BUCKET", "Locks");
    std::env::set_var("STORAGE_EMULATOR_HOST", "localhost:1");
    assert_eq!(from_env(true).await.unwrap().name(), "gcs");
    std::env::remove_var("STORAGE_EMULATOR_HOST");
    std::env::remove_var("GOOGLE_STORAGE_LOCK_BUCKET");

    std::env::remove_var("AWS_S3_LOCK_BUCKET");
    std::env::set_var("LOCK_PROVIDER", "s3");
    let err = from_env(true).await.err().unwrap().to_string();
    assert!(err.contains("AWS_S3_LOCK_BUCKET"), "{}", err);

    std::env::set_var("AWS_S3_LOCK_BUCKET", "locks");
    std::env::set_var("AWS_REGION", "us-east-1");
    assert_eq!(from_env(true).await.unwrap().name(), "s3");
    std::env::remove_var("AWS_REGION");
    std::env::remove_var("AWS_S3_LOCK_BUCKET");

    std::env::set_var("LOCK_PROVIDER", "azure");
    let err = from_env(true).await.err().unwrap().to_string();
    assert!(err.contains("not supported"), "{}", err);

    std::env::set_var("LOCK_PROVIDER", "bogus");
    let err = from_env(true).await.err().unwrap().to_string();
    assert!(err.contains("unknown LOCK_PROVIDER"), "{}", err);
    std::env::remove_var("LOCK_PROVIDER");
}

#[tokio::test]
async fn status_and_held_by() {
    let mem = Arc::new(MemLock::holding(&[
        ("o/r#a", 1),
        ("o/r#b", 2),
        ("o/r#c", 3),
        ("o/r#d", 1),
        ("x/y#a", 1),
    ]));
    let (l, _) = locker(&[3], mem.clone());
    assert_eq!(l.status(1, "o/r#a").await.unwrap(), HolderStatus::Ours);
    assert_eq!(l.status(1, "o/r#b").await.unwrap(), HolderStatus::Other(2));
    // Held by a closed PR: free for the taking.
    assert_eq!(l.status(1, "o/r#c").await.unwrap(), HolderStatus::Free);
    assert_eq!(l.status(1, "o/r#zzz").await.unwrap(), HolderStatus::Free);
    // status() never acquires.
    assert_eq!(mem.holder("o/r#c").await.unwrap(), Some(3));
    assert_eq!(
        l.held_by(1, "o/r#").await.unwrap(),
        vec!["o/r#a".to_string(), "o/r#d".to_string()]
    );
}
