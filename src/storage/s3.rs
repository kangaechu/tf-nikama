//! S3 plan storage.

use super::{
    decode_args, decode_no_changes, PlanMeta, PlanStorage, StoredPlan, HEAD_SHA_METADATA_KEY,
    PLAN_ARGS_METADATA_KEY, PLAN_NO_CHANGES_METADATA_KEY,
};
use async_trait::async_trait;
use aws_sdk_s3::error::SdkError;
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::types::ServerSideEncryption;
use aws_sdk_s3::Client;

#[derive(Debug, Clone)]
pub enum Encryption {
    Aes256,
    Kms(String),
}

pub struct S3PlanStorage {
    client: Client,
    bucket: String,
    encryption: Option<Encryption>,
}

impl S3PlanStorage {
    pub async fn new(bucket: &str, encryption: Option<Encryption>) -> anyhow::Result<Self> {
        let config = aws_config::load_from_env().await;
        Ok(Self {
            client: Client::new(&config),
            bucket: bucket.to_string(),
            encryption,
        })
    }
}

#[async_trait]
impl PlanStorage for S3PlanStorage {
    async fn store(&self, key: &str, bytes: &[u8], meta: &PlanMeta) -> anyhow::Result<()> {
        let mut req = self
            .client
            .put_object()
            .bucket(&self.bucket)
            .key(key)
            .body(ByteStream::from(bytes.to_vec()));
        for (k, v) in meta.to_metadata() {
            req = req.metadata(k, v);
        }
        match &self.encryption {
            Some(Encryption::Aes256) => {
                req = req.server_side_encryption(ServerSideEncryption::Aes256);
            }
            Some(Encryption::Kms(id)) => {
                req = req
                    .server_side_encryption(ServerSideEncryption::AwsKms)
                    .ssekms_key_id(id);
            }
            None => {}
        }
        req.send()
            .await
            .map_err(|e| anyhow::anyhow!("s3 put_object failed: {}", e))?;
        Ok(())
    }

    async fn retrieve(&self, key: &str) -> anyhow::Result<Option<StoredPlan>> {
        let res = self
            .client
            .get_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        match res {
            Ok(out) => {
                let head_sha = out
                    .metadata()
                    .and_then(|m| m.get(HEAD_SHA_METADATA_KEY))
                    .filter(|s| !s.is_empty())
                    .cloned();
                let extra_args = decode_args(
                    out.metadata()
                        .and_then(|m| m.get(PLAN_ARGS_METADATA_KEY))
                        .map(String::as_str),
                );
                let no_changes = decode_no_changes(
                    out.metadata()
                        .and_then(|m| m.get(PLAN_NO_CHANGES_METADATA_KEY))
                        .map(String::as_str),
                );
                let data = out
                    .body
                    .collect()
                    .await
                    .map_err(|e| anyhow::anyhow!("s3 read failed: {}", e))?;
                Ok(Some(StoredPlan {
                    bytes: data.into_bytes().to_vec(),
                    head_sha,
                    extra_args,
                    no_changes,
                }))
            }
            Err(SdkError::ServiceError(e)) if e.err().is_no_such_key() => Ok(None),
            Err(e) => Err(anyhow::anyhow!("s3 get_object failed: {}", e)),
        }
    }

    async fn delete(&self, key: &str) -> anyhow::Result<()> {
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("s3 delete_object failed: {}", e))?;
        Ok(())
    }

    #[cfg(test)]
    async fn exists(&self, key: &str) -> anyhow::Result<bool> {
        let res = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(key)
            .send()
            .await;
        match res {
            Ok(_) => Ok(true),
            Err(SdkError::ServiceError(e)) if e.err().is_not_found() => Ok(false),
            Err(e) => Err(anyhow::anyhow!("s3 head_object failed: {}", e)),
        }
    }

    fn name(&self) -> &'static str {
        "s3"
    }
}
