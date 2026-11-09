//! DynamoDB lock backend, schema-compatible with Digger's
//! `DiggerDynamoDBLockTable`.

use super::Lock;
use async_trait::async_trait;
use aws_sdk_dynamodb::error::SdkError;
use aws_sdk_dynamodb::operation::update_item::UpdateItemError;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, KeySchemaElement, KeyType,
    ScalarAttributeType, TableStatus,
};
use aws_sdk_dynamodb::Client;
use std::collections::HashMap;
use std::time::Duration;

pub const DEFAULT_TABLE: &str = "DiggerDynamoDBLockTable";
const PK: &str = "LOCK";
const TTL_DAYS: i64 = 90;

pub struct DynamoDbLock {
    client: Client,
    table: String,
    ensured: tokio::sync::OnceCell<()>,
}

impl DynamoDbLock {
    pub async fn new(table: &str) -> anyhow::Result<Self> {
        let config = aws_config::load_from_env().await;
        if config.region().is_none() {
            anyhow::bail!("AWS_REGION is not set; it is required for the DynamoDB lock");
        }
        Ok(Self {
            client: Client::new(&config),
            table: table.to_string(),
            ensured: tokio::sync::OnceCell::new(),
        })
    }

    fn sk(resource: &str) -> String {
        format!("RES#{}", resource)
    }

    fn key(resource: &str) -> HashMap<String, AttributeValue> {
        HashMap::from([
            ("PK".to_string(), AttributeValue::S(PK.to_string())),
            ("SK".to_string(), AttributeValue::S(Self::sk(resource))),
        ])
    }

    async fn ensure_table(&self) -> anyhow::Result<()> {
        self.ensured
            .get_or_try_init(|| async {
                match self
                    .client
                    .describe_table()
                    .table_name(&self.table)
                    .send()
                    .await
                {
                    Ok(_) => return Ok(()),
                    Err(SdkError::ServiceError(e)) if e.err().is_resource_not_found_exception() => {
                    }
                    Err(e) => return Err(anyhow::anyhow!("describe_table failed: {}", e)),
                }
                tracing::info!(table = %self.table, "creating DynamoDB lock table");
                self.client
                    .create_table()
                    .table_name(&self.table)
                    .billing_mode(BillingMode::PayPerRequest)
                    .attribute_definitions(
                        AttributeDefinition::builder()
                            .attribute_name("PK")
                            .attribute_type(ScalarAttributeType::S)
                            .build()?,
                    )
                    .attribute_definitions(
                        AttributeDefinition::builder()
                            .attribute_name("SK")
                            .attribute_type(ScalarAttributeType::S)
                            .build()?,
                    )
                    .key_schema(
                        KeySchemaElement::builder()
                            .attribute_name("PK")
                            .key_type(KeyType::Hash)
                            .build()?,
                    )
                    .key_schema(
                        KeySchemaElement::builder()
                            .attribute_name("SK")
                            .key_type(KeyType::Range)
                            .build()?,
                    )
                    .send()
                    .await
                    .map_err(|e| anyhow::anyhow!("create_table failed: {}", e))?;
                for _ in 0..30 {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    let d = self
                        .client
                        .describe_table()
                        .table_name(&self.table)
                        .send()
                        .await;
                    if let Ok(d) = d {
                        if d.table().and_then(|t| t.table_status()) == Some(&TableStatus::Active) {
                            return Ok(());
                        }
                    }
                }
                Err(anyhow::anyhow!(
                    "timed out waiting for lock table to become ACTIVE"
                ))
            })
            .await
            .map(|_| ())
    }
}

#[async_trait]
impl Lock for DynamoDbLock {
    async fn lock(&self, pr: u64, resource: &str) -> anyhow::Result<bool> {
        self.ensure_table().await?;
        let now = chrono::Utc::now();
        let timeout = (now + chrono::Duration::days(TTL_DAYS)).to_rfc3339();
        let res = self
            .client
            .update_item()
            .table_name(&self.table)
            .set_key(Some(Self::key(resource)))
            .update_expression("SET transaction_id = :tid, #t = :timeout")
            .condition_expression("attribute_not_exists(SK) OR #t < :now")
            .expression_attribute_names("#t", "timeout")
            .expression_attribute_values(":tid", AttributeValue::N(pr.to_string()))
            .expression_attribute_values(":timeout", AttributeValue::S(timeout))
            .expression_attribute_values(":now", AttributeValue::S(now.to_rfc3339()))
            .send()
            .await;
        match res {
            Ok(_) => Ok(true),
            Err(SdkError::ServiceError(e))
                if matches!(e.err(), UpdateItemError::ConditionalCheckFailedException(_)) =>
            {
                Ok(false)
            }
            Err(e) => Err(anyhow::anyhow!("dynamodb update_item failed: {}", e)),
        }
    }

    async fn unlock(&self, resource: &str) -> anyhow::Result<bool> {
        self.ensure_table().await?;
        self.client
            .delete_item()
            .table_name(&self.table)
            .set_key(Some(Self::key(resource)))
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("dynamodb delete_item failed: {}", e))?;
        Ok(true)
    }

    async fn holder(&self, resource: &str) -> anyhow::Result<Option<u64>> {
        self.ensure_table().await?;
        let out = self
            .client
            .get_item()
            .table_name(&self.table)
            .set_key(Some(Self::key(resource)))
            .consistent_read(true)
            .send()
            .await
            .map_err(|e| anyhow::anyhow!("dynamodb get_item failed: {}", e))?;
        let Some(item) = out.item() else {
            return Ok(None);
        };
        let tid = item
            .get("transaction_id")
            .and_then(|v| v.as_n().ok())
            .and_then(|n| n.parse::<u64>().ok())
            .unwrap_or(0);
        if tid == 0 {
            return Ok(None);
        }
        // Respect the TTL the same way Digger does (string comparison of RFC3339).
        if let Some(t) = item.get("timeout").and_then(|v| v.as_s().ok()) {
            if t.as_str() < chrono::Utc::now().to_rfc3339().as_str() {
                return Ok(None);
            }
        }
        Ok(Some(tid))
    }

    fn name(&self) -> &'static str {
        "dynamodb"
    }
}
