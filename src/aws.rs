//! AWS SDK configuration shared by the DynamoDB and S3 backends.
//!
//! Credentials and region come from the environment of the workflow step
//! (typically `aws-actions/configure-aws-credentials`); per-job credentials
//! for Terraform itself are handled separately in `aws_auth`.

/// Load the SDK configuration from the environment. The region is required:
/// without it every request would fail later with a less helpful error.
pub async fn sdk_config(purpose: &str) -> anyhow::Result<aws_config::SdkConfig> {
    let config = aws_config::load_from_env().await;
    if config.region().is_none() {
        anyhow::bail!("AWS_REGION is not set; it is required for the {}", purpose);
    }
    Ok(config)
}
