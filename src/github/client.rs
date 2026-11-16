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
    #[error("GitHub GraphQL query failed: {0}")]
    GraphQl(String),
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
    /// Test merge commit for open PRs; final merge commit after merging.
    pub merge_commit_sha: Option<String>,
    pub base_ref: String,
    pub merged: bool,
    pub state: String,
    pub draft: bool,
    pub mergeable: Option<bool>,
    pub mergeable_state: Option<String>,
    /// Login of the user who opened the PR.
    pub author: String,
    /// Names of the labels on the PR.
    pub labels: Vec<String>,
}

impl PrInfo {
    pub fn is_closed(&self) -> bool {
        self.state == "closed"
    }
}

/// One pull request review (`GET /pulls/{n}/reviews`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewInfo {
    pub user: String,
    /// `APPROVED`, `CHANGES_REQUESTED`, `COMMENTED`, `DISMISSED` or `PENDING`.
    pub state: String,
}

/// Result of `GET /compare/{base}...{head}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareInfo {
    pub ahead_by: u64,
    pub behind_by: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckKind {
    CheckRun,
    Status,
}

/// One check run or commit status on a pull request's head commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckContext {
    /// Check run name or status context.
    pub name: String,
    pub kind: CheckKind,
    /// Required by branch protection / rulesets for this pull request.
    pub required: bool,
    /// Lower-case: `success`, `pending`, `failure`, `neutral`, `skipped`,
    /// `cancelled`, `timed_out`, `action_required`, `error`, `expected`, ...
    pub state: String,
    /// `details_url` of a check run (the Actions run URL for workflow jobs)
    /// or `target_url` of a status.
    pub details_url: String,
}

/// What branch protection evaluates for a pull request.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MergeRequirements {
    /// `APPROVED`, `CHANGES_REQUESTED` or `REVIEW_REQUIRED`; `None` when no
    /// review rule applies to the pull request.
    pub review_decision: Option<String>,
    pub contexts: Vec<CheckContext>,
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
        let text = self
            .request_text(method, path, body, "application/vnd.github+json")
            .await?;
        if text.trim().is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text)?)
    }

    /// Send a request and return the response body as text (empty for 204).
    async fn request_text(
        &self,
        method: Method,
        path: &str,
        body: Option<Value>,
        accept: &str,
    ) -> Result<String> {
        let url = if path.starts_with("http://") || path.starts_with("https://") {
            path.to_string()
        } else {
            format!("{}{}", self.api_base, path)
        };
        let mut attempt = 0u32;
        loop {
            attempt += 1;
            let mut req = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&self.token)
                .header("Accept", accept)
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
                    return Ok(String::new());
                }
                return Ok(resp.text().await?);
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
            merge_commit_sha: v["merge_commit_sha"].as_str().map(str::to_string),
            base_ref: v["base"]["ref"].as_str().unwrap_or_default().to_string(),
            merged: v["merged"].as_bool().unwrap_or(false),
            state: v["state"].as_str().unwrap_or_default().to_string(),
            draft: v["draft"].as_bool().unwrap_or(false),
            mergeable: v["mergeable"].as_bool(),
            mergeable_state: v["mergeable_state"].as_str().map(|s| s.to_string()),
            author: v["user"]["login"].as_str().unwrap_or_default().to_string(),
            labels: v["labels"]
                .as_array()
                .map(|ls| {
                    ls.iter()
                        .filter_map(|l| l["name"].as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
        })
    }

    /// Default branch of the repository (`GET /repos/{owner}/{repo}`).
    pub async fn default_branch(&self) -> Result<String> {
        let v = self.request(Method::GET, &self.repo_path(""), None).await?;
        Ok(v["default_branch"].as_str().unwrap_or("main").to_string())
    }

    /// Raw contents of the file at `path` on `git_ref`
    /// (`GET /contents/{path}?ref=`). `None` when the file does not exist.
    pub async fn get_file(&self, path: &str, git_ref: &str) -> Result<Option<String>> {
        let encoded: Vec<String> = path
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| urlencoding::encode(s).into_owned())
            .collect();
        let api_path = self.repo_path(&format!(
            "/contents/{}?ref={}",
            encoded.join("/"),
            urlencoding::encode(git_ref)
        ));
        match self
            .request_text(
                Method::GET,
                &api_path,
                None,
                "application/vnd.github.raw+json",
            )
            .await
        {
            Ok(text) => Ok(Some(text)),
            Err(GithubError::Status {
                status: StatusCode::NOT_FOUND,
                ..
            }) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Repository role of `login`: `admin`, `maintain`, `write`, `triage`,
    /// `read` or `none` (`GET /collaborators/{login}/permission`). A user
    /// GitHub does not know as a collaborator is reported as `none`.
    pub async fn collaborator_role(&self, login: &str) -> Result<String> {
        let res = self
            .request(
                Method::GET,
                &self.repo_path(&format!(
                    "/collaborators/{}/permission",
                    urlencoding::encode(login)
                )),
                None,
            )
            .await;
        let v = match res {
            Ok(v) => v,
            Err(GithubError::Status {
                status: StatusCode::NOT_FOUND,
                ..
            }) => return Ok("none".to_string()),
            Err(e) => return Err(e),
        };
        // `role_name` carries the fine-grained role (`maintain`, `triage`);
        // `permission` only distinguishes admin / write / read / none.
        let role = v["role_name"]
            .as_str()
            .filter(|s| !s.is_empty())
            .or_else(|| v["permission"].as_str())
            .unwrap_or("none");
        Ok(role.to_string())
    }

    /// Reviews of a PR in submission order (oldest first).
    pub async fn list_reviews(&self, number: u64) -> Result<Vec<ReviewInfo>> {
        let items = self
            .paginate(&self.repo_path(&format!("/pulls/{}/reviews", number)), "")
            .await?;
        Ok(items
            .iter()
            .map(|r| ReviewInfo {
                user: r["user"]["login"].as_str().unwrap_or_default().to_string(),
                state: r["state"].as_str().unwrap_or_default().to_string(),
            })
            .collect())
    }

    /// How far `head` is ahead of / behind `base`.
    pub async fn compare(&self, base: &str, head: &str) -> Result<CompareInfo> {
        let v = self
            .request(
                Method::GET,
                &self.repo_path(&format!("/compare/{}...{}", base, head)),
                None,
            )
            .await?;
        Ok(CompareInfo {
            ahead_by: v["ahead_by"].as_u64().unwrap_or(0),
            behind_by: v["behind_by"].as_u64().unwrap_or(0),
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

    /// GraphQL endpoint: `/graphql` next to the REST base (`/api/v3` →
    /// `/api/graphql` on GitHub Enterprise Server).
    fn graphql_url(&self) -> String {
        match self.api_base.strip_suffix("/api/v3") {
            Some(host) => format!("{}/api/graphql", host),
            None => format!("{}/graphql", self.api_base),
        }
    }

    async fn graphql(&self, query: &str, variables: Value) -> Result<Value> {
        let v = self
            .request(
                Method::POST,
                &self.graphql_url(),
                Some(json!({ "query": query, "variables": variables })),
            )
            .await?;
        if let Some(errors) = v["errors"].as_array().filter(|e| !e.is_empty()) {
            let msgs: Vec<String> = errors
                .iter()
                .map(|e| e["message"].as_str().unwrap_or("unknown error").to_string())
                .collect();
            return Err(GithubError::GraphQl(msgs.join("; ")));
        }
        Ok(v["data"].clone())
    }

    /// What branch protection sees for a pull request: the review decision
    /// and every check run / commit status on the head commit, with whether
    /// branch protection requires it.
    pub async fn merge_requirements(&self, number: u64) -> Result<MergeRequirements> {
        const QUERY: &str = r#"
query($owner: String!, $repo: String!, $number: Int!) {
  repository(owner: $owner, name: $repo) {
    pullRequest(number: $number) {
      reviewDecision
      commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup {
              contexts(first: 100) {
                nodes {
                  __typename
                  ... on CheckRun {
                    name
                    status
                    conclusion
                    detailsUrl
                    isRequired(pullRequestNumber: $number)
                  }
                  ... on StatusContext {
                    context
                    state
                    targetUrl
                    isRequired(pullRequestNumber: $number)
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}"#;
        let data = self
            .graphql(
                QUERY,
                json!({ "owner": self.owner, "repo": self.repo, "number": number }),
            )
            .await?;
        let pr = &data["repository"]["pullRequest"];
        if pr.is_null() {
            return Err(GithubError::GraphQl(format!(
                "pull request #{} not found",
                number
            )));
        }
        let review_decision = pr["reviewDecision"].as_str().map(|s| s.to_string());
        let mut contexts = Vec::new();
        let nodes = &pr["commits"]["nodes"][0]["commit"]["statusCheckRollup"]["contexts"]["nodes"];
        for n in nodes.as_array().into_iter().flatten() {
            let required = n["isRequired"].as_bool().unwrap_or(false);
            match n["__typename"].as_str() {
                Some("CheckRun") => {
                    let status = n["status"].as_str().unwrap_or("").to_ascii_lowercase();
                    let state = if status == "completed" {
                        n["conclusion"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_ascii_lowercase()
                    } else {
                        "pending".to_string()
                    };
                    contexts.push(CheckContext {
                        name: n["name"].as_str().unwrap_or_default().to_string(),
                        kind: CheckKind::CheckRun,
                        required,
                        state,
                        details_url: n["detailsUrl"].as_str().unwrap_or_default().to_string(),
                    });
                }
                Some("StatusContext") => {
                    contexts.push(CheckContext {
                        name: n["context"].as_str().unwrap_or_default().to_string(),
                        kind: CheckKind::Status,
                        required,
                        state: n["state"]
                            .as_str()
                            .unwrap_or("unknown")
                            .to_ascii_lowercase(),
                        details_url: n["targetUrl"].as_str().unwrap_or_default().to_string(),
                    });
                }
                _ => {}
            }
        }
        Ok(MergeRequirements {
            review_decision,
            contexts,
        })
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

    /// Merges the PR. No `commit_title` is sent so GitHub builds the default
    /// message (`Merge pull request #N ...` or `<PR title> (#N)`), honouring
    /// the repository's merge-message settings.
    pub async fn merge_pr(&self, number: u64, method: &str, sha: &str) -> Result<()> {
        self.request(
            Method::PUT,
            &self.repo_path(&format!("/pulls/{}/merge", number)),
            Some(json!({
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
