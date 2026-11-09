//! Drift notifications: Slack webhook and GitHub Issues.

use super::COMMENT_MAX_CHARS;
use crate::github::GithubClient;
use crate::util::truncate_chars;
use async_trait::async_trait;

#[async_trait]
pub trait DriftNotifier: Send + Sync {
    async fn notify(&self, project: &str, repo: &str, plan: &str) -> anyhow::Result<()>;
    async fn notify_error(&self, project: &str, repo: &str, error: &str) -> anyhow::Result<()>;
    fn name(&self) -> &'static str;
}

pub struct SlackWebhook {
    url: String,
    http: reqwest::Client,
}

impl SlackWebhook {
    pub fn new(url: &str) -> Self {
        Self {
            url: url.to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn send(&self, text: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(&self.url)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await?;
        if !resp.status().is_success() {
            anyhow::bail!("slack webhook returned {}", resp.status());
        }
        Ok(())
    }
}

/// Split `text` into chunks of at most `max` characters on line boundaries,
/// wrapping each chunk in a code fence.
pub fn split_code_blocks(text: &str, max: usize) -> Vec<String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        if current.chars().count() + line.chars().count() + 1 > max && !current.is_empty() {
            parts.push(format!("```\n{}\n```", current.trim_end()));
            current.clear();
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.trim().is_empty() {
        parts.push(format!("```\n{}\n```", current.trim_end()));
    }
    parts
}

#[async_trait]
impl DriftNotifier for SlackWebhook {
    async fn notify(&self, project: &str, repo: &str, plan: &str) -> anyhow::Result<()> {
        let header = format!(
            ":warning: *Infrastructure Drift Detected*\n*Repository:* {}\n*Project:* {}\n",
            repo, project
        );
        let blocks = split_code_blocks(plan, 3_800);
        if blocks.is_empty() {
            return self.send(&header).await;
        }
        for (i, b) in blocks.iter().enumerate() {
            let text = if i == 0 {
                format!("{}{}", header, b)
            } else {
                format!(
                    "*Project:* {} (continued {}/{})\n{}",
                    project,
                    i + 1,
                    blocks.len(),
                    b
                )
            };
            self.send(&text).await?;
        }
        Ok(())
    }

    async fn notify_error(&self, project: &str, repo: &str, error: &str) -> anyhow::Result<()> {
        self.send(&format!(
            ":x: *Drift detection failed*\n*Repository:* {}\n*Project:* {}\n```\n{}\n```",
            repo,
            project,
            truncate_chars(error, 3_000, "\n...")
        ))
        .await
    }

    fn name(&self) -> &'static str {
        "slack"
    }
}

pub struct GithubIssueNotifier {
    github: GithubClient,
}

impl GithubIssueNotifier {
    pub fn new(github: GithubClient) -> Self {
        Self { github }
    }

    fn title(project: &str) -> String {
        format!("Drift detected in project: {}", project)
    }
}

#[async_trait]
impl DriftNotifier for GithubIssueNotifier {
    async fn notify(&self, project: &str, repo: &str, plan: &str) -> anyhow::Result<()> {
        let title = Self::title(project);
        let body_head = format!(
            "Drift was detected in project `{}` of `{}`.\n\nComment `digger apply -p {}` on a PR that touches this project, or fix the drift manually.\n\n```terraform\n",
            project, repo, project
        );
        let body_tail = "\n```\n";
        let budget = COMMENT_MAX_CHARS.saturating_sub(body_head.len() + body_tail.len() + 200);
        let body = format!(
            "{}{}{}",
            body_head,
            truncate_chars(plan, budget, "\n... (truncated)"),
            body_tail
        );
        let existing = self.github.list_open_issues(Some("digger")).await?;
        if let Some(issue) = existing.iter().find(|i| i.title == title) {
            self.github.update_issue_body(issue.number, &body).await?;
            tracing::info!(issue = issue.number, project, "updated drift issue");
        } else {
            let created = self.github.create_issue(&title, &body, &["digger"]).await?;
            tracing::info!(issue = created.number, project, "created drift issue");
        }
        Ok(())
    }

    async fn notify_error(&self, project: &str, _repo: &str, error: &str) -> anyhow::Result<()> {
        tracing::error!(
            project,
            error,
            "drift detection failed (no issue created for errors)"
        );
        Ok(())
    }

    fn name(&self) -> &'static str {
        "github-issue"
    }
}

/// Pick the notifier from the environment.
pub fn from_env(github: &GithubClient) -> anyhow::Result<Box<dyn DriftNotifier>> {
    if let Some(url) = crate::util::env_first(&[
        "INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL",
        "NIKAMA_DRIFT_SLACK_WEBHOOK_URL",
    ]) {
        return Ok(Box::new(SlackWebhook::new(&url)));
    }
    if crate::util::env_opt("INPUT_DRIFT_GITHUB_ISSUES").is_some()
        || crate::util::env_is_true("NIKAMA_DRIFT_GITHUB_ISSUES")
    {
        return Ok(Box::new(GithubIssueNotifier::new(github.clone())));
    }
    anyhow::bail!(
        "no drift notification configured: set INPUT_DRIFT_DETECTION_SLACK_NOTIFICATION_URL or INPUT_DRIFT_GITHUB_ISSUES=true"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        let text = (0..100)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let parts = split_code_blocks(&text, 200);
        assert!(parts.len() > 1);
        for p in &parts {
            assert!(p.starts_with("```\n") && p.ends_with("\n```"));
            assert!(p.chars().count() <= 210);
        }
    }
}
