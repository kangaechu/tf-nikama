//! Parsing of `GITHUB_CONTEXT` (`${{ toJson(github) }}`).

use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct GithubContext {
    pub event_name: String,
    #[serde(default)]
    pub event: Value,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub run_attempt: String,
    #[serde(default = "default_server_url")]
    pub server_url: String,
    #[serde(default)]
    pub sha: String,
}

fn default_server_url() -> String {
    "https://github.com".to_string()
}

impl GithubContext {
    pub fn parse(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn run_url(&self) -> String {
        format!(
            "{}/{}/actions/runs/{}",
            self.server_url.trim_end_matches('/'),
            self.repository,
            self.run_id
        )
    }

    pub fn pull_request_event(&self) -> Result<PullRequestEvent, serde_json::Error> {
        serde_json::from_value(self.event.clone())
    }

    pub fn issue_comment_event(&self) -> Result<IssueCommentEvent, serde_json::Error> {
        serde_json::from_value(self.event.clone())
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Repository {
    #[serde(default = "default_branch")]
    pub default_branch: String,
    pub full_name: String,
}

fn default_branch() -> String {
    "main".to_string()
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub git_ref: String,
    #[serde(default)]
    pub sha: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequest {
    pub number: u64,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub merged: bool,
    pub head: GitRef,
    pub base: GitRef,
    #[serde(default)]
    pub state: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PullRequestEvent {
    pub action: String,
    pub pull_request: PullRequest,
    pub repository: Repository,
    pub sender: User,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Issue {
    pub number: u64,
    /// Present only when the issue is a pull request.
    pub pull_request: Option<Value>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Comment {
    pub id: u64,
    #[serde(default)]
    pub body: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct IssueCommentEvent {
    pub action: String,
    pub issue: Issue,
    pub comment: Comment,
    pub repository: Repository,
    pub sender: User,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pr_context() {
        let ctx = GithubContext::parse(
            r#"{"event_name":"pull_request","actor":"me","repository":"o/r","run_id":"1","run_attempt":"1","server_url":"https://github.com",
                "event":{"action":"opened","pull_request":{"number":7,"draft":false,"merged":false,"head":{"ref":"feat","sha":"abc"},"base":{"ref":"main","sha":"def"}},
                         "repository":{"default_branch":"main","full_name":"o/r"},"sender":{"login":"me"}}}"#,
        )
        .unwrap();
        let ev = ctx.pull_request_event().unwrap();
        assert_eq!(ev.pull_request.number, 7);
        assert_eq!(ctx.run_url(), "https://github.com/o/r/actions/runs/1");
    }
}
