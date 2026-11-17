//! Single-writer task that upserts the run comment(s) with debouncing.

use super::comment::{RowStatus, RunReport};
use crate::config::CommentMode;
use crate::engine::Command;
use crate::github::{client::CommentInfo, GithubClient};
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type Update = Box<dyn FnOnce(&mut RunReport) + Send + 'static>;

enum Msg {
    Update(Update),
    Flush(oneshot::Sender<()>),
    /// Publish any pending changes and stop the writer task.
    Close(oneshot::Sender<()>),
}

#[derive(Clone)]
pub struct ReportWriter {
    tx: mpsc::UnboundedSender<Msg>,
}

pub struct ReportWriterTask {
    handle: tokio::task::JoinHandle<()>,
}

impl ReportWriterTask {
    pub async fn join(self) {
        let _ = self.handle.await;
    }
}

const DEBOUNCE: Duration = Duration::from_secs(2);

impl ReportWriter {
    /// Start the writer. The first publish happens immediately so the PR
    /// shows a running report.
    pub fn start(
        github: GithubClient,
        pr_number: u64,
        report: RunReport,
        mode: CommentMode,
    ) -> (ReportWriter, ReportWriterTask) {
        let (tx, rx) = mpsc::unbounded_channel();
        let handle = tokio::spawn(run(github, pr_number, report, mode, rx));
        (ReportWriter { tx }, ReportWriterTask { handle })
    }

    /// Queue a mutation of the report.
    pub fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut RunReport) + Send + 'static,
    {
        let _ = self.tx.send(Msg::Update(Box::new(f)));
    }

    /// Wait until every queued update has been published.
    pub async fn flush(&self) {
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Flush(tx)).is_ok() {
            let _ = rx.await;
        }
    }

    /// Mark the report finished, publish it and stop the writer task.
    ///
    /// The task must be stopped explicitly: it would otherwise only exit once
    /// every `ReportWriter` clone is dropped, and callers (e.g. `RunContext`)
    /// still hold clones when they `join()` the task, which would deadlock.
    /// Updates queued after `finish` are dropped.
    pub async fn finish(&self) {
        self.update(|r| r.finished = true);
        let (tx, rx) = oneshot::channel();
        if self.tx.send(Msg::Close(tx)).is_ok() {
            let _ = rx.await;
        }
    }
}

struct Publisher {
    github: GithubClient,
    pr_number: u64,
    mode: CommentMode,
    /// Comment id per part key ("1", "2", ... or project name).
    ids: HashMap<String, u64>,
    /// Last body published per key, to skip no-op edits.
    last: HashMap<String, String>,
    listed: bool,
    previous: Vec<CommentInfo>,
    author_id: Option<u64>,
}

impl Publisher {
    async fn discover_existing(&mut self, report: &RunReport) {
        if self.listed {
            return;
        }
        self.listed = true;
        let marker_prefix = report.marker.trim_end_matches("-->").trim_end().to_string();
        match self.github.list_comments(self.pr_number).await {
            Ok(comments) => {
                for c in comments {
                    let Some(body) = &c.body else { continue };
                    let Some(first) = body.lines().next() else {
                        continue;
                    };
                    if first != report.marker && !first.starts_with(&format!("{} ", marker_prefix))
                    {
                        self.previous.push(c);
                        continue;
                    }
                    self.author_id = c.user.as_ref().map(|u| u.id);
                    let key = part_key(first);
                    self.ids.insert(key, c.id);
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not list PR comments"),
        }
    }

    async fn publish(&mut self, report: &RunReport) {
        self.discover_existing(report).await;
        let bodies: Vec<(String, String)> = match self.mode {
            CommentMode::Single | CommentMode::Latest => report
                .render()
                .into_iter()
                .enumerate()
                .map(|(i, b)| ((i + 1).to_string(), b))
                .collect(),
            CommentMode::PerProject => report.render_per_project(),
        };
        let mut published = true;
        for (key, body) in bodies {
            if self.last.get(&key) == Some(&body) {
                continue;
            }
            let result = match self.ids.get(&key) {
                Some(id) => self.github.edit_comment(*id, &body).await.map(|_| None),
                None => self
                    .github
                    .create_comment(self.pr_number, &body)
                    .await
                    .map(|c| {
                        self.author_id = c.user.as_ref().map(|u| u.id);
                        Some(c.id)
                    }),
            };
            match result {
                Ok(Some(id)) => {
                    self.ids.insert(key.clone(), id);
                    self.last.insert(key, body);
                }
                Ok(None) => {
                    self.last.insert(key, body);
                }
                Err(e) => {
                    published = false;
                    tracing::error!(error = %e, "failed to publish PR comment");
                }
            }
        }
        if published
            && report.finished
            && report.command == Command::Plan
            && self.mode != CommentMode::Latest
        {
            self.hide_superseded(report).await;
        }
    }

    async fn hide_superseded(&mut self, report: &RunReport) {
        let Some(author) = self.author_id else { return };
        let Some(first_id) = self.ids.values().min().copied() else {
            return;
        };
        for c in &self.previous {
            if c.id >= first_id
                || c.node_id.is_empty()
                || c.user.as_ref().map(|u| u.id) != Some(author)
                || !superseded(c.body.as_deref().unwrap_or(""), report)
            {
                continue;
            }
            if let Err(e) = self.github.minimize_outdated_comment(&c.node_id).await {
                // Reporting housekeeping must never fail Terraform work.
                tracing::warn!(comment_id = c.id, error = %e, "could not hide outdated plan comment");
            }
        }
    }
}

/// Only machine-readable nikama plan reports with fully replaced scopes
/// qualify. Older reports without scope metadata are retained conservatively.
fn superseded(body: &str, report: &RunReport) -> bool {
    let mut lines = body.lines();
    let Some(marker) = lines.next() else {
        return false;
    };
    if !(marker.starts_with("<!-- nikama:run=") || marker == "<!-- nikama:latest -->") {
        return false;
    }
    let Some(header) = lines.next() else {
        return false;
    };
    if !header.starts_with("### nikama plan · run ") || header.contains(" · running") {
        return false;
    }
    let Some(scope) = lines
        .find(|line| !line.is_empty())
        .and_then(|line| line.strip_prefix("<!-- nikama:scope "))
        .and_then(|s| s.strip_suffix(" -->"))
    else {
        return false;
    };
    #[derive(serde::Deserialize)]
    struct Scope {
        command: String,
        projects: Vec<String>,
    }
    let Ok(scope) = serde_json::from_str::<Scope>(scope) else {
        return false;
    };
    scope.command == "plan"
        && !scope.projects.is_empty()
        && scope.projects.iter().all(|name| {
            report
                .rows
                .get(name)
                .is_some_and(|row| matches!(row.status, RowStatus::Changes | RowStatus::NoChanges))
        })
}

/// Extract a part key from a marker line: `... part=2 -->` → "2",
/// `... project=x -->` → "x", otherwise "1".
fn part_key(marker_line: &str) -> String {
    for tok in marker_line.split_whitespace() {
        if let Some(v) = tok.strip_prefix("part=") {
            return v.trim_end_matches("-->").to_string();
        }
        if let Some(v) = tok.strip_prefix("project=") {
            return v.trim_end_matches("-->").to_string();
        }
    }
    "1".to_string()
}

async fn run(
    github: GithubClient,
    pr_number: u64,
    mut report: RunReport,
    mode: CommentMode,
    mut rx: mpsc::UnboundedReceiver<Msg>,
) {
    let mut publisher = Publisher {
        github,
        pr_number,
        mode,
        ids: HashMap::new(),
        last: HashMap::new(),
        listed: false,
        previous: Vec::new(),
        author_id: None,
    };
    publisher.publish(&report).await;
    let mut dirty = false;
    let mut pending_flush: Vec<oneshot::Sender<()>> = Vec::new();
    loop {
        let msg = if dirty {
            match tokio::time::timeout(DEBOUNCE, rx.recv()).await {
                Ok(m) => m,
                Err(_) => {
                    publisher.publish(&report).await;
                    dirty = false;
                    for tx in pending_flush.drain(..) {
                        let _ = tx.send(());
                    }
                    continue;
                }
            }
        } else {
            rx.recv().await
        };
        match msg {
            Some(Msg::Update(f)) => {
                f(&mut report);
                dirty = true;
            }
            Some(Msg::Flush(tx)) => {
                if dirty {
                    publisher.publish(&report).await;
                    dirty = false;
                }
                for p in pending_flush.drain(..) {
                    let _ = p.send(());
                }
                let _ = tx.send(());
            }
            Some(Msg::Close(tx)) => {
                if dirty {
                    publisher.publish(&report).await;
                }
                for p in pending_flush.drain(..) {
                    let _ = p.send(());
                }
                let _ = tx.send(());
                break;
            }
            None => {
                if dirty {
                    publisher.publish(&report).await;
                }
                for p in pending_flush.drain(..) {
                    let _ = p.send(());
                }
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_keys() {
        assert_eq!(part_key("<!-- nikama:run=1:attempt=1 -->"), "1");
        assert_eq!(part_key("<!-- nikama:run=1:attempt=1 part=3 -->"), "3");
        assert_eq!(
            part_key("<!-- nikama:run=1:attempt=1 project=app -->"),
            "app"
        );
    }
}
