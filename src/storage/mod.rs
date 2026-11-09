//! Plan file storage between the plan and apply runs.

mod s3;

pub use s3::S3PlanStorage;

use async_trait::async_trait;
use std::sync::Arc;

#[async_trait]
pub trait PlanStorage: Send + Sync {
    async fn store(&self, key: &str, bytes: &[u8]) -> anyhow::Result<()>;
    async fn retrieve(&self, key: &str) -> anyhow::Result<Option<Vec<u8>>>;
    async fn delete(&self, key: &str) -> anyhow::Result<()>;
    async fn exists(&self, key: &str) -> anyhow::Result<bool>;
    fn name(&self) -> &'static str;
    fn is_noop(&self) -> bool {
        false
    }
}

/// Storage that keeps nothing: `apply` runs without a saved plan.
pub struct NoopPlanStorage;

#[async_trait]
impl PlanStorage for NoopPlanStorage {
    async fn store(&self, _key: &str, _bytes: &[u8]) -> anyhow::Result<()> {
        Ok(())
    }
    async fn retrieve(&self, _key: &str) -> anyhow::Result<Option<Vec<u8>>> {
        Ok(None)
    }
    async fn delete(&self, _key: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn exists(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }
    fn name(&self) -> &'static str {
        "none"
    }
    fn is_noop(&self) -> bool {
        true
    }
}

/// Create the plan storage from `PLAN_UPLOAD_DESTINATION`.
pub async fn from_env() -> anyhow::Result<Arc<dyn PlanStorage>> {
    let dest = crate::util::env_opt("PLAN_UPLOAD_DESTINATION")
        .unwrap_or_default()
        .to_ascii_lowercase();
    match dest.as_str() {
        "" | "none" => Ok(Arc::new(NoopPlanStorage)),
        "aws" | "s3" => {
            let bucket = crate::util::env_opt("AWS_S3_BUCKET").ok_or_else(|| {
                anyhow::anyhow!("AWS_S3_BUCKET is required when PLAN_UPLOAD_DESTINATION=aws")
            })?;
            let encryption = if crate::util::env_is_true("PLAN_UPLOAD_S3_ENCRYPTION_ENABLED") {
                let kind = crate::util::env_opt("PLAN_UPLOAD_S3_ENCRYPTION_TYPE")
                    .unwrap_or_else(|| "AES256".to_string())
                    .to_ascii_uppercase();
                match kind.as_str() {
                    "AES256" => Some(s3::Encryption::Aes256),
                    "KMS" => Some(s3::Encryption::Kms(
                        crate::util::env_opt("PLAN_UPLOAD_S3_ENCRYPTION_KMS_ID").ok_or_else(|| {
                            anyhow::anyhow!("PLAN_UPLOAD_S3_ENCRYPTION_KMS_ID is required for KMS encryption")
                        })?,
                    )),
                    other => anyhow::bail!("unknown PLAN_UPLOAD_S3_ENCRYPTION_TYPE '{}'", other),
                }
            } else {
                None
            };
            Ok(Arc::new(S3PlanStorage::new(&bucket, encryption).await?))
        }
        "github" | "gcp" | "azure" | "gitlab" => anyhow::bail!(
            "PLAN_UPLOAD_DESTINATION={} is not supported by this tool (only aws or empty)",
            dest
        ),
        other => anyhow::bail!("unknown PLAN_UPLOAD_DESTINATION '{}'", other),
    }
}
