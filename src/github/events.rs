//! Conversion of GitHub events into jobs.

use super::context::{IssueCommentEvent, PullRequestEvent};
use crate::config::{Config, Project};
use crate::engine::{Command, Job};

/// What a `pull_request` event asks us to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrAction {
    Run {
        commands: Vec<String>,
        event_name: &'static str,
    },
    Ignore(String),
}

/// Decide which workflow commands a PR event maps to (DESIGN.md §6.2).
/// `commands` are resolved per project because each workflow may differ; this
/// returns the *selector* and the caller looks up the workflow.
pub fn classify_pr_event(event: &PullRequestEvent, config: &Config) -> PrSelector {
    let pr = &event.pull_request;
    match event.action.as_str() {
        "closed" => {
            if pr.merged && pr.base.git_ref == event.repository.default_branch {
                PrSelector::CommitToDefault
            } else {
                PrSelector::Closed
            }
        }
        "opened" | "reopened" | "synchronize" | "ready_for_review" => {
            if pr.draft && !config.allow_draft_prs {
                PrSelector::Ignore("draft pull request (allow_draft_prs is false)".into())
            } else {
                PrSelector::Pushed
            }
        }
        "converted_to_draft" => PrSelector::ConvertedToDraft,
        other => PrSelector::Ignore(format!("pull_request action '{}' is not handled", other)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrSelector {
    Pushed,
    Closed,
    CommitToDefault,
    ConvertedToDraft,
    Ignore(String),
}

impl PrSelector {
    pub fn event_name(&self) -> &'static str {
        match self {
            PrSelector::ConvertedToDraft => "pull_request_converted_to_draft",
            _ => "pull_request",
        }
    }
}

/// Build jobs for a PR event over the impacted projects.
pub fn jobs_for_pull_request(
    event: &PullRequestEvent,
    config: &Config,
    impacted: &[Project],
    selector: &PrSelector,
) -> Vec<Job> {
    let pr = &event.pull_request;
    let mut jobs = Vec::new();
    for project in impacted {
        let workflow = config.workflow_for(project);
        let cfg = &workflow.configuration;
        let commands: Vec<String> = match selector {
            PrSelector::Pushed => cfg.on_pull_request_pushed.clone(),
            PrSelector::Closed => cfg.on_pull_request_closed.clone(),
            PrSelector::CommitToDefault => cfg.on_commit_to_default.clone(),
            PrSelector::ConvertedToDraft => {
                if cfg.on_pull_request_to_draft.is_empty() && !config.allow_draft_prs {
                    vec!["digger unlock".to_string()]
                } else {
                    cfg.on_pull_request_to_draft.clone()
                }
            }
            PrSelector::Ignore(_) => vec![],
        };
        for c in commands {
            let Some(command) = Command::parse(&c) else {
                tracing::warn!(command = %c, project = %project.name, "unknown workflow command; skipping");
                continue;
            };
            jobs.push(Job::new(
                project,
                workflow,
                command,
                Some(pr.number),
                selector.event_name(),
                &event.sender.login,
                &event.repository.full_name,
                &pr.head.git_ref,
                &event.repository.default_branch,
            ));
        }
    }
    jobs
}

/// Build jobs for a comment command over the selected projects.
pub fn jobs_for_comment(
    event: &IssueCommentEvent,
    config: &Config,
    selected: &[Project],
    command: Command,
    pr_branch: &str,
) -> Vec<Job> {
    selected
        .iter()
        .map(|project| {
            Job::new(
                project,
                config.workflow_for(project),
                command,
                Some(event.issue.number),
                "issue_comment",
                &event.sender.login,
                &event.repository.full_name,
                pr_branch,
                &event.repository.default_branch,
            )
        })
        .collect()
}

/// Apply `-p` / `-d` selectors to the impacted projects. Returns the selection
/// and the selectors that matched nothing.
pub fn select_projects(
    impacted: &[Project],
    projects: &[String],
    dirs: &[String],
) -> (Vec<Project>, Vec<String>) {
    if projects.is_empty() && dirs.is_empty() {
        return (impacted.to_vec(), vec![]);
    }
    let mut selected: Vec<Project> = Vec::new();
    let mut unmatched = Vec::new();
    for name in projects {
        match impacted
            .iter()
            .find(|p| &p.name == name || p.alias.as_deref() == Some(name.as_str()))
        {
            Some(p) => {
                if !selected.iter().any(|s| s.name == p.name) {
                    selected.push(p.clone());
                }
            }
            None => unmatched.push(format!("-p {}", name)),
        }
    }
    for dir in dirs {
        let norm = crate::config::normalize_path(dir);
        let matches: Vec<&Project> = impacted
            .iter()
            .filter(|p| crate::config::normalize_path(&p.dir) == norm)
            .collect();
        if matches.is_empty() {
            unmatched.push(format!("-d {}", dir));
        }
        for p in matches {
            if !selected.iter().any(|s| s.name == p.name) {
                selected.push(p.clone());
            }
        }
    }
    (selected, unmatched)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::load_from_str;

    fn pr_event(action: &str, merged: bool, base: &str, draft: bool) -> PullRequestEvent {
        serde_json::from_value(serde_json::json!({
            "action": action,
            "pull_request": {"number": 5, "draft": draft, "merged": merged,
                "head": {"ref": "feat", "sha": "a"}, "base": {"ref": base, "sha": "b"}},
            "repository": {"default_branch": "main", "full_name": "o/r"},
            "sender": {"login": "me"}
        }))
        .unwrap()
    }

    fn cfg() -> Config {
        let dir = tempfile::tempdir().unwrap();
        load_from_str(
            "projects:\n  - name: a\n    dir: a\n  - name: b\n    dir: b\n    workflow: custom\nworkflows:\n  custom:\n    workflow_configuration:\n      on_pull_request_pushed: [\"digger plan\", \"digger lock\"]\n",
            dir.path(),
        )
        .unwrap()
    }

    #[test]
    fn classifies() {
        let c = cfg();
        assert_eq!(
            classify_pr_event(&pr_event("opened", false, "main", false), &c),
            PrSelector::Pushed
        );
        assert_eq!(
            classify_pr_event(&pr_event("closed", true, "main", false), &c),
            PrSelector::CommitToDefault
        );
        assert_eq!(
            classify_pr_event(&pr_event("closed", false, "main", false), &c),
            PrSelector::Closed
        );
        assert_eq!(
            classify_pr_event(&pr_event("closed", true, "dev", false), &c),
            PrSelector::Closed
        );
        assert!(matches!(
            classify_pr_event(&pr_event("synchronize", false, "main", true), &c),
            PrSelector::Ignore(_)
        ));
        assert!(matches!(
            classify_pr_event(&pr_event("labeled", false, "main", false), &c),
            PrSelector::Ignore(_)
        ));
    }

    #[test]
    fn builds_jobs() {
        let c = cfg();
        let ev = pr_event("opened", false, "main", false);
        let jobs = jobs_for_pull_request(&ev, &c, &c.projects, &PrSelector::Pushed);
        let cmds: Vec<(String, Command)> = jobs
            .iter()
            .map(|j| (j.project.name.clone(), j.command))
            .collect();
        assert_eq!(
            cmds,
            vec![
                ("a".into(), Command::Plan),
                ("b".into(), Command::Plan),
                ("b".into(), Command::Lock)
            ]
        );
        assert_eq!(jobs[0].run_env["PR_BRANCH"], "feat");
        assert_eq!(jobs[0].pr_number, Some(5));
    }

    #[test]
    fn selects() {
        let c = cfg();
        let (sel, un) = select_projects(&c.projects, &["b".into(), "zzz".into()], &["a/".into()]);
        assert_eq!(
            sel.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            vec!["b", "a"]
        );
        assert_eq!(un, vec!["-p zzz"]);
    }
}
