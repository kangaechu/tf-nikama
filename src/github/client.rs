//! Minimal GitHub REST client (reqwest) with retry.

use reqwest::{Method, StatusCode};
use serde::Deserialize;
use serde_json::{json, Value};
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("GitHub API {method} {path} failed with {status}: {body}")]
    Status {
        method: String,
        path: String,
        status: StatusCode,
        body: String,
    },
    #[error("GitHub API request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("GitHub API response could not be decoded: {0}")]
    Decode(#[from] serde_json::Error),
}

pub type Result<T> = std::result::Result<T, GithubError>;

#[derive(Debug, Clone)]
pub struct GithubClient {
    http: reqwest::Client,
    api_base: String,
    token: String,
    pub owner: String,
    pub repo: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CommentInfo {
    pub id: u64,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub html_url: String,
}

#[derive(Debug, Clone)]
pub struct PrInfo {
    pub number: u64,
    pub head_ref: String,
    pub head_sha: String,
    pub base_ref: String,
    pub merged: bool,
    pub state: String,
    pub draft: bool,
    pub mergeable: Option<bool>,
    pub mergeable_state: Option<String>,
}

impl PrInfo {
    pub fn is_closed(&self) -> bool {
        self.state == "closed"
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct StatusEntry {
    pub context: String,
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CombinedStatus {
    pub state: String,
    #[serde(default)]
    pub statuses: Vec<StatusEntry>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueInfo {
    pub number: u64,
    pub title: String,
    #[serde(default)]
    pub html_url: String,
}

impl GithubClient {
    pub fn new(token: &str, owner: &str, repo: &str, api_base: Option<&str>) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(concat!("nikama/", env!("CARGO_PKG_VERSION")))
            .timeout(Duration::from_secs(60))
            .build()
            .expect("reqwest client");
        Self {
            http,
            api_base: api_base
                .unwrap_or("https://api.github.com")
                .trim_end_matches('/')
                .to_string(),
            token: token.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
        }
    }

    fn repo_path(&self, suffix: &str) -> String {
        format!("/repos/{}/{}{}", self.owner, self.repo, suffix)
    }

    async fn request(&self, method: Method, path: &str, body: Option<Value>) -> Result<Value> {
        let url = format!("{}{}", self.api_base, path);
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&self.token)
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28");
            if let Some(b) = &body {
                req = req.json(b);
            }
            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) if attempt < 5 => {
                    tracing::warn!(error = %e, attempt, "GitHub request error; retrying");
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            let status = resp.status();
            if status.is_success() {
                if status == StatusCode::NO_CONTENT {
                    return Ok(Value::Null);
                }
                let text = resp.text().await?;
                if text.trim().is_empty() {
                    return Ok(Value::Null);
                }
                return Ok(serde_json::from_str(&text)?);
            }
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let remaining = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            let text = resp.text().await.unwrap_or_default();
            let retryable = status == StatusCode::TOO_MANY_REQUESTS
                || status.is_server_error()
                || (status == StatusCode::FORBIDDEN
                    && (remaining == Some(0) || text.contains("rate limit")));
            if retryable && attempt < 5 {
                let wait = retry_after
                    .map(|s| Duration::from_secs(s.min(120)))
                    .unwrap_or_else(|| backoff(attempt));
                tracing::warn!(%status, attempt, ?wait, "GitHub request throttled or failed; retrying");
                tokio::time::sleep(wait).await;
                continue;
            }
            return Err(GithubError::Status {
                method: method.to_string(),
                path: path.to_string(),
                status,
                body: crate::util::truncate_chars(&text, 500, "…"),
            });
        }
    }

    async fn paginate(&self, path: &str, extra_query: &str) -> Result<Vec<Value>> {
        let mut out = Vec::new();
        for page in 1..=50u32 {
            let sep = if path.contains('?') { '&' } else { '?' };
            let p = format!("{}{}per_page=100&page={}{}", path, sep, page, extra_query);
            let v = self.request(Method::GET, &p, None).await?;
            let arr = match v {
                Value::Array(a) => a,
                _ => break,
            };
            let n = arr.len();
            out.extend(arr);
            if n < 100 {
                break;
            }
        }
        Ok(out)
    }

    pub async fn get_pr(&self, number: u64) -> Result<PrInfo> {
        let v = self
            .request(
                Method::GET,
                &self.repo_path(&format!("/pulls/{}", number)),
                None,
            )
            .await?;
        Ok(PrInfo {
            number,
            head_ref: v["head"]["ref"].as_str().unwrap_or_default().to_string(),
            head_sha: v["head"]["sha"].as_str().unwrap_or_default().to_string(),
            base_ref: v["base"]["ref"].as_str().unwrap_or_default().to_string(),
            merged: v["merged"].as_bool().unwrap_or(false),
            state: v["state"].as_str().unwrap_or_default().to_string(),
            draft: v["draft"].as_bool().unwrap_or(false),
            mergeable: v["mergeable"].as_bool(),
            mergeable_state: v["mergeable_state"].as_str().map(|s| s.to_string()),
        })
    }

    /// Changed file paths of a PR (renamed files contribute both names).
    pub async fn changed_files(&self, number: u64) -> Result<Vec<String>> {
        let items = self
            .paginate(&self.repo_path(&format!("/pulls/{}/files", number)), "")
            .await?;
        let mut files = Vec::new();
        for it in items {
            if let Some(f) = it["filename"].as_str() {
                files.push(f.to_string());
            }
            if let Some(f) = it["previous_filename"].as_str() {
                files.push(f.to_string());
            }
        }
        Ok(files)
    }

    pub async fn list_comments(&self, number: u64) -> Result<Vec<CommentInfo>> {
        let items = self
            .paginate(&self.repo_path(&format!("/issues/{}/comments", number)), "")
            .await?;
        Ok(items
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    pub async fn create_comment(&self, number: u64, body: &str) -> Result<CommentInfo> {
        let v = self
            .request(
                Method::POST,
                &self.repo_path(&format!("/issues/{}/comments", number)),
                Some(json!({ "body": body })),
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn edit_comment(&self, comment_id: u64, body: &str) -> Result<()> {
        self.request(
            Method::PATCH,
            &self.repo_path(&format!("/issues/comments/{}", comment_id)),
            Some(json!({ "body": body })),
        )
        .await?;
        Ok(())
    }

    pub async fn add_reaction(&self, comment_id: u64, content: &str) -> Result<()> {
        self.request(
            Method::POST,
            &self.repo_path(&format!("/issues/comments/{}/reactions", comment_id)),
            Some(json!({ "content": content })),
        )
        .await?;
        Ok(())
    }

    pub async fn set_status(
        &self,
        sha: &str,
        state: &str,
        context: &str,
        description: &str,
        target_url: Option<&str>,
    ) -> Result<()> {
        let mut body = json!({
            "state": state,
            "context": context,
            "description": crate::util::truncate_chars(description, 140, "…"),
        });
        if let Some(u) = target_url {
            body["target_url"] = json!(u);
        }
        self.request(
            Method::POST,
            &self.repo_path(&format!("/statuses/{}", sha)),
            Some(body),
        )
        .await?;
        Ok(())
    }

    pub async fn combined_status(&self, sha: &str) -> Result<CombinedStatus> {
        let v = self
            .request(
                Method::GET,
                &self.repo_path(&format!("/commits/{}/status", sha)),
                None,
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn merge_pr(
        &self,
        number: u64,
        method: &str,
        sha: &str,
        message: &str,
    ) -> Result<()> {
        self.request(
            Method::PUT,
            &self.repo_path(&format!("/pulls/{}/merge", number)),
            Some(json!({
                "commit_title": message,
                "merge_method": method,
                "sha": sha,
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn list_open_issues(&self, label: Option<&str>) -> Result<Vec<IssueInfo>> {
        let extra = match label {
            Some(l) => format!("&labels={}", urlencoding::encode(l)),
            None => String::new(),
        };
        let items = self
            .paginate(&self.repo_path("/issues?state=open"), &extra)
            .await?;
        Ok(items
            .into_iter()
            .filter(|v| v.get("pull_request").is_none())
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }

    pub async fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<IssueInfo> {
        let v = self
            .request(
                Method::POST,
                &self.repo_path("/issues"),
                Some(json!({ "title": title, "body": body, "labels": labels })),
            )
            .await?;
        Ok(serde_json::from_value(v)?)
    }

    pub async fn update_issue_body(&self, number: u64, body: &str) -> Result<()> {
        self.request(
            Method::PATCH,
            &self.repo_path(&format!("/issues/{}", number)),
            Some(json!({ "body": body })),
        )
        .await?;
        Ok(())
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt.min(6)))
}
