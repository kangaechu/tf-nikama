//! Plan file storage between the plan and apply runs.

mod gcs;
mod s3;

pub use gcs::GcsPlanStorage;
pub use s3::S3PlanStorage;

use async_trait::async_trait;
use std::sync::Arc;

/// Name of the object metadata entry holding the PR head SHA the plan was
/// produced from (`x-amz-meta-head-sha` on S3).
pub const HEAD_SHA_METADATA_KEY: &str = "head-sha";

/// Name of the object metadata entry holding the comment arguments the plan
/// was produced with (`x-amz-meta-plan-args` on S3), as an ASCII-only JSON
/// array.
pub const PLAN_ARGS_METADATA_KEY: &str = "plan-args";

/// Name of the object metadata entry set to `"true"` when the plan has no
/// changes (`x-amz-meta-plan-no-changes` on S3); `apply` then skips the
/// project instead of applying an empty plan.
pub const PLAN_NO_CHANGES_METADATA_KEY: &str = "plan-no-changes";

/// What is recorded next to a plan file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlanMeta {
    /// PR head commit the plan was produced from.
    pub head_sha: Option<String>,
    /// Arguments given after `--` on the comment that produced the plan.
    pub extra_args: Vec<String>,
    /// The plan has no changes.
    pub no_changes: bool,
}

impl PlanMeta {
    /// Object metadata entries for this plan.
    pub fn to_metadata(&self) -> Vec<(&'static str, String)> {
        let mut out = Vec::new();
        if let Some(sha) = &self.head_sha {
            out.push((HEAD_SHA_METADATA_KEY, sha.clone()));
        }
        if !self.extra_args.is_empty() {
            out.push((PLAN_ARGS_METADATA_KEY, encode_args(&self.extra_args)));
        }
        if self.no_changes {
            out.push((PLAN_NO_CHANGES_METADATA_KEY, "true".to_string()));
        }
        out
    }
}

/// Decode the `no-changes` metadata entry; anything but `true` (including
/// a missing entry, as written by older versions) means the plan must be
/// applied.
pub fn decode_no_changes(value: Option<&str>) -> bool {
    value.is_some_and(|v| v.trim().eq_ignore_ascii_case("true"))
}

/// Encode comment arguments for object metadata: JSON with every non-ASCII
/// character escaped, since S3 metadata values must be US-ASCII.
pub fn encode_args(args: &[String]) -> String {
    let json = serde_json::to_string(args).unwrap_or_else(|_| "[]".to_string());
    let mut out = String::with_capacity(json.len());
    for c in json.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for unit in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{:04x}", unit));
            }
        }
    }
    out
}

/// Decode what [`encode_args`] wrote; anything unreadable is treated as no
/// arguments.
pub fn decode_args(value: Option<&str>) -> Vec<String> {
    value
        .and_then(|v| serde_json::from_str::<Vec<String>>(v).ok())
        .unwrap_or_default()
}

/// A stored plan file together with the commit it was produced from.
/// `head_sha` is `None` for plans written without metadata (older versions
/// of this tool, or Digger).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPlan {
    pub bytes: Vec<u8>,
    pub head_sha: Option<String>,
    /// Comment arguments the plan was produced with (empty when none or
    /// when written by an older version).
    pub extra_args: Vec<String>,
    /// The plan has no changes, so there is nothing to apply (`false` when
    /// written by an older version, which then applies as before).
    pub no_changes: bool,
}

#[async_trait]
pub trait PlanStorage: Send + Sync {
    /// Store `bytes` under `key`, recording `meta` with it.
    async fn store(&self, key: &str, bytes: &[u8], meta: &PlanMeta) -> anyhow::Result<()>;
    async fn retrieve(&self, key: &str) -> anyhow::Result<Option<StoredPlan>>;
    async fn delete(&self, key: &str) -> anyhow::Result<()>;
    /// Only the tests look at storage without retrieving the plan.
    #[cfg(test)]
    async fn exists(&self, key: &str) -> anyhow::Result<bool>;
    fn name(&self) -> &'static str;
    fn is_noop(&self) -> bool {
        false
    }
}

/// Storage that keeps nothing: `apply` plans immediately before applying.
pub struct NoopPlanStorage;

#[async_trait]
impl PlanStorage for NoopPlanStorage {
    async fn store(&self, _key: &str, _bytes: &[u8], _meta: &PlanMeta) -> anyhow::Result<()> {
        Ok(())
    }
    async fn retrieve(&self, _key: &str) -> anyhow::Result<Option<StoredPlan>> {
        Ok(None)
    }
    async fn delete(&self, _key: &str) -> anyhow::Result<()> {
        Ok(())
    }
    #[cfg(test)]
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

/// Whether `PLAN_UPLOAD_DESTINATION` names a real storage (not empty or
/// `none`), without creating a client.
pub fn configured() -> bool {
    !matches!(
        crate::util::env_opt("PLAN_UPLOAD_DESTINATION")
            .unwrap_or_default()
            .to_ascii_lowercase()
            .as_str(),
        "" | "none"
    )
}

/// Create the plan storage from `PLAN_UPLOAD_DESTINATION` (`aws`, `gcp` or
/// empty).
pub async fn from_env() -> anyhow::Result<Arc<dyn PlanStorage>> {
    let dest = crate::util::env_opt("PLAN_UPLOAD_DESTINATION")
        .unwrap_or_default()
        .to_ascii_lowercase();
    let storage: Arc<dyn PlanStorage> = match dest.as_str() {
        "" | "none" => Arc::new(NoopPlanStorage),
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
            Arc::new(S3PlanStorage::new(&bucket, encryption).await?)
        }
        "gcp" | "gcs" => {
            let bucket = crate::util::env_opt("GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET")
                .map(|b| b.trim().to_ascii_lowercase())
                .ok_or_else(|| {
                    anyhow::anyhow!(
                        "GOOGLE_STORAGE_PLAN_ARTEFACT_BUCKET is required when PLAN_UPLOAD_DESTINATION=gcp"
                    )
                })?;
            let client = crate::gcs::GcsClient::from_env()?;
            Arc::new(GcsPlanStorage::new(client, &bucket))
        }
        "github" | "azure" | "gitlab" => anyhow::bail!(
            "PLAN_UPLOAD_DESTINATION={} is not supported by this tool (only aws, gcp or empty)",
            dest
        ),
        other => anyhow::bail!("unknown PLAN_UPLOAD_DESTINATION '{}'", other),
    };
    tracing::info!(destination = storage.name(), "plan storage");
    Ok(storage)
}

#[cfg(test)]
mod meta_tests {
    use super::*;

    #[test]
    fn args_round_trip_as_ascii_json() {
        let args = vec![
            "-target=module.x[\"日本\"]".to_string(),
            "-var=emoji=😀".to_string(),
        ];
        let encoded = encode_args(&args);
        assert!(encoded.is_ascii(), "{encoded}");
        assert!(encoded.contains("\\u65e5\\u672c"));
        assert_eq!(decode_args(Some(&encoded)), args);
        assert!(decode_args(None).is_empty());
        assert!(decode_args(Some("not json")).is_empty());
        let meta = PlanMeta {
            head_sha: Some("abc".into()),
            extra_args: vec!["-refresh=false".into()],
            no_changes: false,
        };
        assert_eq!(
            meta.to_metadata(),
            vec![
                (HEAD_SHA_METADATA_KEY, "abc".to_string()),
                (PLAN_ARGS_METADATA_KEY, "[\"-refresh=false\"]".to_string()),
            ]
        );
        assert!(PlanMeta::default().to_metadata().is_empty());
    }

    #[test]
    fn no_changes_round_trip() {
        let meta = PlanMeta {
            no_changes: true,
            ..Default::default()
        };
        assert_eq!(
            meta.to_metadata(),
            vec![(PLAN_NO_CHANGES_METADATA_KEY, "true".to_string())]
        );
        assert!(decode_no_changes(Some("true")));
        assert!(decode_no_changes(Some("TRUE")));
        assert!(!decode_no_changes(Some("false")));
        assert!(!decode_no_changes(Some("")));
        assert!(!decode_no_changes(None));
    }
}
