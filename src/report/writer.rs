//! Single-writer task that upserts the run comment(s) with debouncing.

use super::comment::RunReport;
use crate::config::CommentMode;
use crate::github::GithubClient;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};

type Update = Box<dyn FnOnce(&mut RunReport) + Send + 'static>;

enum Msg {
    Update(Update),
    Flush(oneshot::Sender<()>),
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

    /// Mark the report finished and publish it.
    pub async fn finish(&self) {
        self.update(|r| r.finished = true);
        self.flush().await;
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
                    if !first.starts_with(&marker_prefix) {
                        continue;
                    }
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
                    .map(|c| Some(c.id)),
            };
            match result {
                Ok(Some(id)) => {
                    self.ids.insert(key.clone(), id);
                    self.last.insert(key, body);
                }
                Ok(None) => {
                    self.last.insert(key, body);
                }
                Err(e) => tracing::error!(error = %e, "failed to publish PR comment"),
            }
        }
    }
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
