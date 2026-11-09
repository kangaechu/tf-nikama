//! Per-job AWS credentials: GitHub OIDC → STS / Cognito (DESIGN.md §11).

use crate::config::{AwsCognitoOidc, AwsRoleToAssume};
use crate::engine::Job;
use aws_credential_types::Credentials;
use std::collections::BTreeMap;

const COGNITO_IDP: &str = "token.actions.githubusercontent.com";

#[derive(Debug, Clone)]
pub struct StaticCreds {
    pub access_key_id: String,
    pub secret_access_key: String,
    pub session_token: Option<String>,
}

impl StaticCreds {
    fn env_map(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("AWS_ACCESS_KEY_ID".to_string(), self.access_key_id.clone());
        m.insert(
            "AWS_SECRET_ACCESS_KEY".to_string(),
            self.secret_access_key.clone(),
        );
        if let Some(t) = &self.session_token {
            m.insert("AWS_SESSION_TOKEN".to_string(), t.clone());
        }
        m
    }

    fn backend_config_args(&self) -> Vec<String> {
        let mut v = vec![
            format!("-backend-config=access_key={}", self.access_key_id),
            format!("-backend-config=secret_key={}", self.secret_access_key),
        ];
        if let Some(t) = &self.session_token {
            v.push(format!("-backend-config=token={}", t));
        }
        v
    }

    fn to_sdk(&self) -> Credentials {
        Credentials::new(
            self.access_key_id.clone(),
            self.secret_access_key.clone(),
            self.session_token.clone(),
            None,
            "nikama",
        )
    }
}

/// Fetch a GitHub Actions OIDC token for `audience`.
pub async fn github_oidc_token(audience: &str) -> anyhow::Result<String> {
    let url = crate::util::env_opt("ACTIONS_ID_TOKEN_REQUEST_URL").ok_or_else(|| {
        anyhow::anyhow!(
            "ACTIONS_ID_TOKEN_REQUEST_URL is not set; grant `id-token: write` to the workflow"
        )
    })?;
    let token = crate::util::env_opt("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
        .ok_or_else(|| anyhow::anyhow!("ACTIONS_ID_TOKEN_REQUEST_TOKEN is not set"))?;
    let sep = if url.contains('?') { '&' } else { '?' };
    let full = format!("{}{}audience={}", url, sep, urlencoding::encode(audience));
    let resp = reqwest::Client::new()
        .get(&full)
        .bearer_auth(token)
        .header("Accept", "application/json; api-version=2.0")
        .header("User-Agent", "actions/oidc-client")
        .send()
        .await?
        .error_for_status()?;
    let v: serde_json::Value = resp.json().await?;
    v["value"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("OIDC token response had no 'value'"))
}

async fn assume_role_with_web_identity(
    role_arn: &str,
    region: &str,
    session_name: &str,
    token: &str,
) -> anyhow::Result<StaticCreds> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region.to_string()))
        .no_credentials()
        .load()
        .await;
    let sts = aws_sdk_sts::Client::new(&config);
    let out = sts
        .assume_role_with_web_identity()
        .role_arn(role_arn)
        .role_session_name(sanitize_session_name(session_name))
        .web_identity_token(token)
        .send()
        .await
        .map_err(|e| {
            anyhow::anyhow!(
                "sts assume_role_with_web_identity({}) failed: {}",
                role_arn,
                e
            )
        })?;
    let c = out
        .credentials()
        .ok_or_else(|| anyhow::anyhow!("STS returned no credentials"))?;
    Ok(StaticCreds {
        access_key_id: c.access_key_id().to_string(),
        secret_access_key: c.secret_access_key().to_string(),
        session_token: Some(c.session_token().to_string()),
    })
}

async fn assume_role_with_creds(
    base: &StaticCreds,
    role_arn: &str,
    region: &str,
    session_name: &str,
    duration: Option<u32>,
) -> anyhow::Result<StaticCreds> {
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(region.to_string()))
        .credentials_provider(base.to_sdk())
        .load()
        .await;
    let sts = aws_sdk_sts::Client::new(&config);
    let mut req = sts
        .assume_role()
        .role_arn(role_arn)
        .role_session_name(sanitize_session_name(session_name));
    if let Some(d) = duration {
        req = req.duration_seconds(d as i32);
    }
    let out = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("sts assume_role({}) failed: {}", role_arn, e))?;
    let c = out
        .credentials()
        .ok_or_else(|| anyhow::anyhow!("STS returned no credentials"))?;
    Ok(StaticCreds {
        access_key_id: c.access_key_id().to_string(),
        secret_access_key: c.secret_access_key().to_string(),
        session_token: Some(c.session_token().to_string()),
    })
}

async fn cognito_credentials(cfg: &AwsCognitoOidc) -> anyhow::Result<StaticCreds> {
    let token = github_oidc_token("cognito-identity.amazonaws.com").await?;
    let config = aws_config::defaults(aws_config::BehaviorVersion::latest())
        .region(aws_config::Region::new(cfg.aws_region.clone()))
        .no_credentials()
        .load()
        .await;
    let client = aws_sdk_cognitoidentity::Client::new(&config);
    let id = client
        .get_id()
        .identity_pool_id(&cfg.cognito_identity_pool_id)
        .account_id(&cfg.aws_account_id)
        .logins(COGNITO_IDP, &token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("cognito get_id failed: {}", e))?;
    let identity_id = id
        .identity_id()
        .ok_or_else(|| anyhow::anyhow!("cognito get_id returned no identity id"))?;
    let creds = client
        .get_credentials_for_identity()
        .identity_id(identity_id)
        .logins(COGNITO_IDP, &token)
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("cognito get_credentials_for_identity failed: {}", e))?;
    let c = creds
        .credentials()
        .ok_or_else(|| anyhow::anyhow!("cognito returned no credentials"))?;
    Ok(StaticCreds {
        access_key_id: c.access_key_id().unwrap_or_default().to_string(),
        secret_access_key: c.secret_key().unwrap_or_default().to_string(),
        session_token: c.session_token().map(|s| s.to_string()),
    })
}

fn sanitize_session_name(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "=,.@-_".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect();
    let mut out = cleaned;
    if out.len() > 64 {
        out.truncate(64);
    }
    if out.len() < 2 {
        out = "nikama".to_string();
    }
    out
}

/// Populate `job.backend_config_args` / `job.command_env` / `job.state_env`
/// according to the project's AWS settings. No-op when none are configured.
pub async fn populate_job_credentials(job: &mut Job) -> anyhow::Result<()> {
    let project = job.project.clone();
    match (&project.aws_cognito_oidc, &project.aws_role_to_assume) {
        (Some(cognito), role) => {
            let base = cognito_credentials(cognito).await?;
            match role {
                Some(role) => {
                    let (state, command) =
                        assume_pair(role, &project.name, |arn, session| {
                            let base = base.clone();
                            let region = cognito.aws_region.clone();
                            let dur = cognito.session_duration;
                            async move {
                                assume_role_with_creds(&base, &arn, &region, &session, dur).await
                            }
                        })
                        .await?;
                    apply_env_strategy(job, state, command);
                }
                None => {
                    job.state_env.extend(base.env_map());
                    job.command_env.extend(base.env_map());
                }
            }
        }
        (None, Some(role)) => {
            let token = github_oidc_token("sts.amazonaws.com").await?;
            let (state, command) = assume_pair(role, &project.name, |arn, session| {
                let token = token.clone();
                let region = role.region.clone();
                async move { assume_role_with_web_identity(&arn, &region, &session, &token).await }
            })
            .await?;
            // "backend-config" strategy: state creds go to init args, command
            // creds to the environment.
            if let Some(s) = state {
                job.backend_config_args = s.backend_config_args();
            }
            if let Some(c) = command {
                job.command_env.extend(c.env_map());
            }
        }
        (None, None) => {}
    }
    Ok(())
}

async fn assume_pair<F, Fut>(
    role: &AwsRoleToAssume,
    project: &str,
    assume: F,
) -> anyhow::Result<(Option<StaticCreds>, Option<StaticCreds>)>
where
    F: Fn(String, String) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<StaticCreds>>,
{
    let state = match &role.state {
        Some(arn) => Some(assume(arn.clone(), format!("{}-state", project)).await?),
        None => None,
    };
    let command = match &role.command {
        Some(arn) if role.state.as_deref() == Some(arn.as_str()) && state.is_some() => {
            state.clone()
        }
        Some(arn) => Some(assume(arn.clone(), format!("{}-command", project)).await?),
        None => None,
    };
    Ok((state, command))
}

fn apply_env_strategy(job: &mut Job, state: Option<StaticCreds>, command: Option<StaticCreds>) {
    if let Some(s) = &state {
        job.state_env.extend(s.env_map());
    }
    if let Some(c) = &command {
        job.command_env.extend(c.env_map());
    }
    // Mirror Digger: fill a missing side from the other.
    if state.is_none() {
        if let Some(c) = &command {
            job.state_env.extend(c.env_map());
        }
    }
    if command.is_none() {
        if let Some(s) = &state {
            job.command_env.extend(s.env_map());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_names() {
        assert_eq!(sanitize_session_name("my proj/x-state"), "my-proj-x-state");
        assert_eq!(sanitize_session_name(""), "nikama");
    }

    #[test]
    fn backend_args() {
        let c = StaticCreds {
            access_key_id: "A".into(),
            secret_access_key: "S".into(),
            session_token: Some("T".into()),
        };
        assert_eq!(
            c.backend_config_args(),
            vec![
                "-backend-config=access_key=A",
                "-backend-config=secret_key=S",
                "-backend-config=token=T"
            ]
        );
    }
}
