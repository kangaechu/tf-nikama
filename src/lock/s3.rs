//! PR locks as objects in an S3 bucket.
//!
//! The object is named after the resource (`<owner>/<repo>#<project>`) and
//! carries the holder in the `LockId` user metadata key, like the Cloud
//! Storage backend. The object is created with `If-None-Match: *`, so two
//! concurrent runs cannot both believe they won.

use super::Lock;
use async_trait::async_trait;
use aws_sdk_s3::error::{ProvideErrorMetadata, SdkError};
use aws_sdk_s3::primitives::ByteStream;
use aws_sdk_s3::Client;
use std::time::Duration;

pub const LOCK_ID_KEY: &str = "lockid";

/// S3 answers a conditional write that races another in-flight write with
/// this code; the request may be retried.
const CONFLICT_CODE: &str = "ConditionalRequestConflict";
const CONFLICT_RETRIES: usize = 3;

pub struct S3Lock {
    client: Client,
    bucket: String,
}

impl S3Lock {
    pub async fn new(bucket: &str) -> anyhow::Result<Self> {
        let config = crate::aws::sdk_config("S3 lock").await?;
        Ok(Self::with_client(Client::new(&config), bucket))
    }

    pub fn with_client(client: Client, bucket: &str) -> Self {
        Self {
            client,
            bucket: bucket.to_string(),
        }
    }

    fn parse_holder(&self, resource: &str, raw: Option<&String>) -> anyhow::Result<u64> {
        let raw = raw.ok_or_else(|| {
            anyhow::anyhow!(
                "lock object s3://{}/{} has no {} metadata",
                self.bucket,
                resource,
                LOCK_ID_KEY
            )
        })?;
        raw.trim().parse::<u64>().map_err(|_| {
            anyhow::anyhow!(
                "lock object s3://{}/{} has an invalid {} '{}'",
                self.bucket,
                resource,
                LOCK_ID_KEY,
                raw
            )
        })
    }
}

#[async_trait]
impl Lock for S3Lock {
    async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<bool> {
        let created_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let mut attempt = 0;
        loop {
            let res = self
                .client
                .put_object()
                .bucket(&self.bucket)
                .key(resource)
                .if_none_match("*")
                .content_type("text/plain")
                .metadata(LOCK_ID_KEY, pr.to_string())
                .metadata("createdat", &created_at)
                .body(ByteStream::from_static(b""))
                .send()
                .await;
            match res {
                Ok(_) => return Ok(true),
                Err(SdkError::ServiceError(e)) => {
                    let status = e.raw().status().as_u16();
                    let code = e.err().code().unwrap_or("");
                    if status == 412 || code == "PreconditionFailed" {
                        return Ok(false);
                    }
                    if code == CONFLICT_CODE && attempt < CONFLICT_RETRIES {
                        attempt += 1;
                        tracing::debug!(resource, attempt, "conditional write conflict; retrying");
                        tokio::time::sleep(Duration::from_millis(200 * attempt as u64)).await;
                        continue;
                    }
                    return Err(anyhow::anyhow!("s3 put_object failed: {}", e.err()));
                }
                Err(e) => return Err(anyhow::anyhow!("s3 put_object failed: {}", e)),
            }
        }
    }

    async fn unlock(&self, resource: &str) -> anyhow::Result<bool> {
        if self.holder(resource).await?.is_none() {
            return Ok(false);
        }
        self.client
            .delete_object()
            .bucket(&self.bucket)
            .key(resource)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("s3 delete_object failed: {}", e))?;
        Ok(true)
    }

    async fn holder(&self, resource: &str) -> anyhow::Result<Option<u64>> {
        let res = self
            .client
            .head_object()
            .bucket(&self.bucket)
            .key(resource)
            .send()
            .await;
        match res {
            Ok(out) => {
                let raw = out.metadata().and_then(|m| m.get(LOCK_ID_KEY));
                self.parse_holder(resource, raw).map(Some)
            }
            Err(SdkError::ServiceError(e)) if e.err().is_not_found() => Ok(None),
            Err(e) => Err(anyhow::anyhow!("s3 head_object failed: {}", e)),
        }
    }

    async fn held_by(&self, pr: u64, prefix: &str) -> anyhow::Result<Vec<String>> {
        // ListObjectsV2 does not return user metadata, so each candidate is
        // inspected with HeadObject.
        let mut keys = Vec::new();
        let mut token = None;
        loop {
            let res = self
                .client
                .list_objects_v2()
                .bucket(&self.bucket)
                .prefix(prefix)
                .set_continuation_token(token)
                .send()
                .await
                .map_err(|e| anyhow::anyhow!("s3 list_objects_v2 failed: {}", e))?;
            keys.extend(
                res.contents()
                    .iter()
                    .filter_map(|o| o.key().map(str::to_string)),
            );
            match res.next_continuation_token() {
                Some(t) if res.is_truncated() == Some(true) => token = Some(t.to_string()),
                _ => break,
            }
        }
        let mut out = Vec::new();
        for key in keys {
            if self.holder(&key).await? == Some(pr) {
                out.push(key);
            }
        }
        out.sort();
        Ok(out)
    }

    fn name(&self) -> &'static str {
        "s3"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aws_credential_types::Credentials;
    use aws_sdk_s3::config::Region;
    use wiremock::matchers::{header, method, path, path_regex, query_param};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn lock(server: &MockServer) -> S3Lock {
        let config = aws_sdk_s3::Config::builder()
            .behavior_version_latest()
            .region(Region::new("us-east-1"))
            .credentials_provider(Credentials::new("ak", "sk", None, None, "test"))
            .endpoint_url(server.uri())
            .force_path_style(true)
            .build();
        S3Lock::with_client(Client::from_conf(config), "locks")
    }

    fn head_ok(pr: u64) -> ResponseTemplate {
        ResponseTemplate::new(200)
            .insert_header("x-amz-meta-lockid", pr.to_string().as_str())
            .insert_header("content-length", "0")
    }

    fn head_missing() -> ResponseTemplate {
        ResponseTemplate::new(404).insert_header("content-length", "0")
    }

    #[tokio::test]
    async fn lock_creates_object_only_when_absent() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/locks/o/r%23a"))
            .and(header("if-none-match", "*"))
            .and(header("x-amz-meta-lockid", "7"))
            .respond_with(ResponseTemplate::new(200).insert_header("etag", "\"x\""))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("PUT"))
            .and(path("/locks/o/r%23b"))
            .and(header("if-none-match", "*"))
            .respond_with(ResponseTemplate::new(412).set_body_string(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?><Error><Code>PreconditionFailed</Code>\
                 <Message>At least one of the pre-conditions you specified did not hold</Message></Error>",
            ))
            .expect(1)
            .mount(&server)
            .await;
        let l = lock(&server);
        assert!(l.lock(7, "o/r#a").await.unwrap());
        assert!(!l.lock(7, "o/r#b").await.unwrap());
    }

    #[tokio::test]
    async fn lock_retries_conditional_conflict_then_fails() {
        let server = MockServer::start().await;
        Mock::given(method("PUT"))
            .and(path("/locks/o/r%23a"))
            .respond_with(ResponseTemplate::new(409).set_body_string(
                "<Error><Code>ConditionalRequestConflict</Code><Message>x</Message></Error>",
            ))
            .expect(1 + CONFLICT_RETRIES as u64)
            .mount(&server)
            .await;
        let err = lock(&server)
            .lock(7, "o/r#a")
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(err.contains("ConditionalRequestConflict"), "{}", err);
    }

    #[tokio::test]
    async fn holder_reads_metadata() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23a"))
            .respond_with(head_ok(42))
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23none"))
            .respond_with(head_missing())
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23bad"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-amz-meta-lockid", "abc")
                    .insert_header("content-length", "0"),
            )
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23nometa"))
            .respond_with(ResponseTemplate::new(200).insert_header("content-length", "0"))
            .mount(&server)
            .await;
        let l = lock(&server);
        assert_eq!(l.holder("o/r#a").await.unwrap(), Some(42));
        assert_eq!(l.holder("o/r#none").await.unwrap(), None);
        let err = l.holder("o/r#bad").await.err().unwrap().to_string();
        assert!(err.contains("invalid lockid"), "{}", err);
        let err = l.holder("o/r#nometa").await.err().unwrap().to_string();
        assert!(err.contains("no lockid metadata"), "{}", err);
    }

    #[tokio::test]
    async fn unlock_deletes_only_existing_object() {
        let server = MockServer::start().await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23a"))
            .respond_with(head_ok(1))
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/locks/o/r%23a"))
            .respond_with(ResponseTemplate::new(204))
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("HEAD"))
            .and(path("/locks/o/r%23none"))
            .respond_with(head_missing())
            .mount(&server)
            .await;
        Mock::given(method("DELETE"))
            .and(path("/locks/o/r%23none"))
            .respond_with(ResponseTemplate::new(204))
            .expect(0)
            .mount(&server)
            .await;
        let l = lock(&server);
        assert!(l.unlock("o/r#a").await.unwrap());
        assert!(!l.unlock("o/r#none").await.unwrap());
    }

    #[tokio::test]
    async fn held_by_lists_prefix_and_filters_by_holder() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path_regex("^/locks/?$"))
            .and(query_param("list-type", "2"))
            .and(query_param("prefix", "o/r#"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
                 <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
                 <Name>locks</Name><Prefix>o/r#</Prefix><KeyCount>3</KeyCount><MaxKeys>1000</MaxKeys>\
                 <IsTruncated>false</IsTruncated>\
                 <Contents><Key>o/r#c</Key></Contents>\
                 <Contents><Key>o/r#a</Key></Contents>\
                 <Contents><Key>o/r#b</Key></Contents>\
                 </ListBucketResult>",
            ))
            .mount(&server)
            .await;
        for (k, pr) in [("a", 1), ("b", 2), ("c", 1)] {
            Mock::given(method("HEAD"))
                .and(path(format!("/locks/o/r%23{}", k)))
                .respond_with(head_ok(pr))
                .mount(&server)
                .await;
        }
        let l = lock(&server);
        assert_eq!(
            l.held_by(1, "o/r#").await.unwrap(),
            vec!["o/r#a".to_string(), "o/r#c".to_string()]
        );
        assert_eq!(l.held_by(3, "o/r#").await.unwrap(), Vec::<String>::new());
    }
}
