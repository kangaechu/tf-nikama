//! Google Cloud Storage plan storage.

use super::{
    decode_args, decode_no_changes, PlanMeta, PlanStorage, StoredPlan, HEAD_SHA_METADATA_KEY,
    PLAN_ARGS_METADATA_KEY, PLAN_NO_CHANGES_METADATA_KEY,
};
use crate::gcs::GcsClient;
use async_trait::async_trait;
use std::collections::BTreeMap;

pub struct GcsPlanStorage {
    client: GcsClient,
    bucket: String,
}

impl GcsPlanStorage {
    pub fn new(client: GcsClient, bucket: &str) -> Self {
        Self {
            client,
            bucket: bucket.to_string(),
        }
    }
}

#[async_trait]
impl PlanStorage for GcsPlanStorage {
    async fn store(&self, key: &str, bytes: &[u8], meta: &PlanMeta) -> anyhow::Result<()> {
        let metadata: BTreeMap<String, String> = meta
            .to_metadata()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        self.client
            .upload(
                &self.bucket,
                key,
                "application/octet-stream",
                &metadata,
                bytes,
                false,
            )
            .await?;
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> anyhow::Result<Option<StoredPlan>> {
        let Some(bytes) = self.client.download(&self.bucket, key).await? else {
            return Ok(None);
        };
        // Custom metadata is not part of the media download; fetch the
        // object resource for it.
        let info = self.client.info(&self.bucket, key).await?;
        let head_sha = info
            .as_ref()
            .and_then(|i| i.metadata.get(HEAD_SHA_METADATA_KEY).cloned())
            .filter(|s| !s.is_empty());
        let extra_args = decode_args(
            info.as_ref()
                .and_then(|i| i.metadata.get(PLAN_ARGS_METADATA_KEY))
                .map(String::as_str),
        );
        let no_changes = decode_no_changes(
            info.as_ref()
                .and_then(|i| i.metadata.get(PLAN_NO_CHANGES_METADATA_KEY))
                .map(String::as_str),
        );
        Ok(Some(StoredPlan {
            bytes,
            head_sha,
            extra_args,
            no_changes,
        }))
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.client.delete(&self.bucket, key).await?;
        Ok(())
    }

    #[cfg(test)]
    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        Ok(self.client.info(&self.bucket, key).await?.is_some())
    }

    fn name(&self) -> &'static str {
        "gcs"
    }
}
