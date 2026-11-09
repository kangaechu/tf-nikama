//! PR event mode: `pull_request` and `issue_comment` events.

use super::exit;
use super::run::{run_by_levels, run_parallel, JobResult, Outcome, RunContext};
use crate::cli::Env;
use crate::config::{CommentMode, Config};
use crate::engine::{Command, Job};
use crate::github::comment_cmd::HELP_TEXT;
use crate::github::events::{
    classify_pr_event, jobs_for_comment, jobs_for_pull_request, select_projects, PrSelector,
};
use crate::github::{parse_comment, CommentVerb, GithubClient, GithubContext};
use crate::lock::{LockOutcome, PrLocker};
use crate::report::{ReportWriter, RowStatus, RunReport};
use std::sync::Arc;

struct Trigger {
    pr_number: u64,
    jobs: Vec<Job>,
    /// Does the job set cover every impacted project? (needed for auto-merge)
    covers_all_impacted: bool,
    notes: Vec<String>,
    comment_id: Option<u64>,
}

pub async fn run(env: &Env, config: Arc<Config>, ctx: &GithubContext, github: GithubClient) -> i32 {
    let trigger = match build_trigger(&config, ctx, &github).await {
        Ok(Some(t)) => t,
        Ok(None) => return exit::OK,
        Err(code) => return code,
    };
    if trigger.jobs.is_empty() {
        tracing::info!("no jobs to run");
        if !trigger.notes.is_empty() {
            let body = trigger
                .notes
                .iter()
                .map(|n| format!("- {}", n))
                .collect::<Vec<_>>()
                .join("\n");
            let _ = github
                .create_comment(trigger.pr_number, &format!("**nikama**\n\n{}", body))
                .await;
        }
        return exit::OK;
    }

    if let Some(id) = trigger.comment_id {
        let _ = github.add_reaction(id, "eyes").await;
    }

    // All jobs in one run share a command (PR events may mix e.g. plan+lock;
    // the status context follows the "heaviest" command).
    let main_command = trigger
        .jobs
        .iter()
        .map(|j| j.command)
        .max_by_key(|c| match c {
            Command::Destroy => 4,
            Command::Apply => 3,
            Command::Plan => 2,
            Command::Lock => 1,
            Command::Unlock => 0,
        })
        .unwrap_or(Command::Plan);

    let pr = match github.get_pr(trigger.pr_number).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "could not fetch pull request");
            return exit::INIT;
        }
    };

    // Report + status.
    let marker = match config.comment.mode {
        CommentMode::Latest => "<!-- nikama:latest -->".to_string(),
        _ => format!(
            "<!-- nikama:run={}:attempt={} -->",
            ctx.run_id, ctx.run_attempt
        ),
    };
    let mut report = RunReport::new(
        &marker,
        main_command,
        &ctx.run_url(),
        config.comment.show_empty_plans,
    );
    for j in &trigger.jobs {
        report.row_mut(&j.project.name);
    }
    report.notes = trigger.notes.clone();
    let (writer, writer_task) = ReportWriter::start(
        github.clone(),
        trigger.pr_number,
        report,
        config.comment.mode,
    );

    let status_context = main_command.status_context();
    let _ = github
        .set_status(
            &pr.head_sha,
            "pending",
            status_context,
            &format!("running {} project(s)", trigger.jobs.len()),
            Some(&ctx.run_url()),
        )
        .await;

    // Locking (two-phase).
    let locker = match crate::lock::from_env(config.pr_locks).await {
        Ok(backend) => Arc::new(PrLocker::new(backend, github.clone())),
        Err(e) => {
            tracing::error!(error = %e, "could not initialise lock provider");
            writer.update(move |r| r.notes.push(format!("lock provider error: {}", e)));
            writer.finish().await;
            writer_task.join().await;
            let _ = github
                .set_status(
                    &pr.head_sha,
                    "failure",
                    status_context,
                    "lock provider error",
                    Some(&ctx.run_url()),
                )
                .await;
            return exit::LOCK_INIT;
        }
    };
    let mut runnable = Vec::new();
    let mut lock_failed = false;
    for job in trigger.jobs {
        if matches!(
            job.command,
            Command::Plan | Command::Apply | Command::Destroy
        ) {
            match locker.lock(trigger.pr_number, &job.lock_id()).await {
                Ok(LockOutcome::Acquired) | Ok(LockOutcome::AlreadyHeld) => runnable.push(job),
                Ok(LockOutcome::HeldByOther(other)) => {
                    lock_failed = true;
                    let n = job.project.name.clone();
                    writer.update(move |r| r.row_mut(&n).status = RowStatus::LockedByOther(other));
                    tracing::warn!(project = %job.project.name, other, "project locked by another PR");
                }
                Err(e) => {
                    lock_failed = true;
                    let n = job.project.name.clone();
                    let msg = format!("lock error: {}", e);
                    writer.update(move |r| {
                        let row = r.row_mut(&n);
                        row.status = RowStatus::Failed;
                        row.detail_kind = Some(crate::report::DetailKind::Error);
                        row.detail = Some(msg);
                    });
                }
            }
        } else {
            runnable.push(job);
        }
    }

    // Apply preconditions.
    let has_apply = runnable.iter().any(|j| matches!(j.command, Command::Apply));
    if has_apply {
        if let Err(reason) = apply_allowed(&github, &pr, &runnable, status_context).await {
            tracing::error!(%reason, "apply not allowed");
            let r2 = reason.clone();
            writer.update(move |r| {
                r.notes.push(format!("apply not allowed: {}", r2));
                for row in r.rows.values_mut() {
                    if row.status == RowStatus::Pending {
                        row.status = RowStatus::Skipped("apply not allowed".into());
                    }
                }
            });
            writer.finish().await;
            writer_task.join().await;
            let _ = github
                .set_status(
                    &pr.head_sha,
                    "failure",
                    status_context,
                    &reason,
                    Some(&ctx.run_url()),
                )
                .await;
            return exit::JOB_FAILED;
        }
    }

    let storage = match crate::storage::from_env().await {
        Ok(s) => s,
        Err(e) => {
            tracing::error!(error = %e, "could not initialise plan storage");
            writer.update(move |r| r.notes.push(format!("plan storage error: {}", e)));
            writer.finish().await;
            writer_task.join().await;
            let _ = github
                .set_status(
                    &pr.head_sha,
                    "failure",
                    status_context,
                    "plan storage error",
                    Some(&ctx.run_url()),
                )
                .await;
            return exit::INIT;
        }
    };

    let run_ctx = Arc::new(RunContext {
        config: config.clone(),
        repo_root: env.repo_root.clone(),
        work_root: env.work_root.clone(),
        init_lock: env.init_lock(),
        storage,
        locker: Some(locker.clone()),
        writer: Some(writer.clone()),
        max_parallel: env.max_parallel.unwrap_or(config.concurrency.max_parallel),
    });

    let (serial, parallel): (Vec<Job>, Vec<Job>) = runnable
        .into_iter()
        .partition(|j| matches!(j.command, Command::Apply | Command::Destroy));
    let mut results: Vec<JobResult> = run_parallel(run_ctx.clone(), parallel).await;
    if !serial.is_empty() {
        results.extend(
            run_by_levels(
                run_ctx.clone(),
                serial,
                config.concurrency.apply_parallelism,
            )
            .await,
        );
    }

    writer.finish().await;
    writer_task.join().await;

    let failed = lock_failed || results.iter().any(|r| !r.is_ok());
    let applied = results
        .iter()
        .filter(|r| matches!(r.outcome, Ok(Outcome::Apply(_))))
        .count();
    let description = summarize(&results, lock_failed);
    let _ = github
        .set_status(
            &pr.head_sha,
            if failed { "failure" } else { "success" },
            status_context,
            &description,
            Some(&ctx.run_url()),
        )
        .await;

    write_outputs(trigger.pr_number);

    if !failed && applied > 0 && config.auto_merge && trigger.covers_all_impacted {
        auto_merge(&github, &config, trigger.pr_number).await;
    }

    if failed {
        exit::JOB_FAILED
    } else {
        exit::OK
    }
}

pub(super) fn summarize(results: &[JobResult], lock_failed: bool) -> String {
    let ok = results.iter().filter(|r| r.is_ok()).count();
    let failed = results.iter().filter(|r| !r.is_ok()).count();
    let changes = results
        .iter()
        .filter(|r| matches!(&r.outcome, Ok(Outcome::Plan(p)) if !p.is_empty()))
        .count();
    let mut parts = vec![format!("{} succeeded", ok)];
    if changes > 0 {
        parts.push(format!("{} with changes", changes));
    }
    if failed > 0 {
        parts.push(format!("{} failed", failed));
    }
    if lock_failed {
        parts.push("some projects locked by other PRs".into());
    }
    parts.join(", ")
}

async fn build_trigger(
    config: &Config,
    ctx: &GithubContext,
    github: &GithubClient,
) -> Result<Option<Trigger>, i32> {
    match ctx.event_name.as_str() {
        "pull_request" | "pull_request_target" => {
            let event = ctx.pull_request_event().map_err(|e| {
                tracing::error!(error = %e, "could not parse pull_request event");
                exit::CONTEXT
            })?;
            let selector = classify_pr_event(&event, config);
            if let PrSelector::Ignore(reason) = &selector {
                tracing::info!(%reason, "ignoring event");
                return Ok(None);
            }
            let pr_number = event.pull_request.number;
            let files = github.changed_files(pr_number).await.map_err(|e| {
                tracing::error!(error = %e, "could not list changed files");
                exit::EVENT
            })?;
            let impacted = crate::config::impacted_projects(config, &files);
            log_impacted(&impacted);
            let jobs = jobs_for_pull_request(&event, config, &impacted, &selector);
            Ok(Some(Trigger {
                pr_number,
                jobs,
                covers_all_impacted: true,
                notes: vec![],
                comment_id: None,
            }))
        }
        "issue_comment" => {
            let event = ctx.issue_comment_event().map_err(|e| {
                tracing::error!(error = %e, "could not parse issue_comment event");
                exit::CONTEXT
            })?;
            if event.action != "created" {
                tracing::info!(action = %event.action, "ignoring issue_comment action");
                return Ok(None);
            }
            if event.issue.pull_request.is_none() {
                tracing::info!("comment is on an issue, not a pull request; ignoring");
                return Ok(None);
            }
            let Some(parsed) = parse_comment(&event.comment.body) else {
                tracing::info!("comment is not a nikama command; ignoring");
                return Ok(None);
            };
            let pr_number = event.issue.number;
            let cmd = match parsed {
                Ok(c) => c,
                Err(e) => {
                    let _ = github
                        .create_comment(pr_number, &format!("**nikama**: {}\n\n{}", e, HELP_TEXT))
                        .await;
                    return Err(exit::COMMAND);
                }
            };
            let command = match cmd.verb {
                CommentVerb::Help => {
                    let _ = github.create_comment(pr_number, HELP_TEXT).await;
                    return Ok(None);
                }
                CommentVerb::Run(Command::Destroy) => {
                    let _ = github
                        .create_comment(
                            pr_number,
                            "**nikama**: `destroy` cannot be run from a PR comment. Use a manually dispatched workflow with `mode: manual` and `command: digger destroy`.",
                        )
                        .await;
                    return Err(exit::COMMAND);
                }
                CommentVerb::Run(c) => c,
            };
            let pr = github.get_pr(pr_number).await.map_err(|e| {
                tracing::error!(error = %e, "could not fetch pull request");
                exit::EVENT
            })?;
            let files = github.changed_files(pr_number).await.map_err(|e| {
                tracing::error!(error = %e, "could not list changed files");
                exit::EVENT
            })?;
            let impacted = crate::config::impacted_projects(config, &files);
            log_impacted(&impacted);
            let (selected, unmatched) = select_projects(&impacted, &cmd.projects, &cmd.dirs);
            let mut notes = Vec::new();
            if !unmatched.is_empty() {
                notes.push(format!(
                    "selector(s) matched no impacted project: {}",
                    unmatched.join(", ")
                ));
            }
            if impacted.is_empty() {
                notes.push("no projects are impacted by this pull request".into());
            }
            let covers_all = selected.len() == impacted.len();
            let jobs = jobs_for_comment(&event, config, &selected, command, &pr.head_ref);
            Ok(Some(Trigger {
                pr_number,
                jobs,
                covers_all_impacted: covers_all,
                notes,
                comment_id: Some(event.comment.id),
            }))
        }
        "merge_group" => {
            tracing::info!("ignoring merge_group event");
            Ok(None)
        }
        other => {
            tracing::error!(event = other, "unsupported event for PR mode (use mode: manual or drift-detection for workflow_dispatch/schedule)");
            Err(exit::EVENT)
        }
    }
}

fn log_impacted(impacted: &[crate::config::Project]) {
    if impacted.is_empty() {
        tracing::info!("no impacted projects");
    } else {
        tracing::info!(
            projects = %impacted.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", "),
            "impacted projects"
        );
    }
}

/// Result of the network-free part of the apply precondition check.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum ApplyGate {
    Allowed,
    Denied(String),
    /// Branch protection reports `blocked`; the commit statuses decide.
    CheckStatuses,
}

/// Apply is allowed when the PR is merged, mergeable, or every apply job has
/// `skip_merge_check`.
pub(super) fn apply_gate(pr: &crate::github::PrInfo, jobs: &[Job]) -> ApplyGate {
    if jobs
        .iter()
        .filter(|j| j.command == Command::Apply)
        .all(|j| j.skip_merge_check)
    {
        return ApplyGate::Allowed;
    }
    if pr.merged {
        return ApplyGate::Allowed;
    }
    if pr.state == "closed" {
        return ApplyGate::Denied("pull request is closed".into());
    }
    match (pr.mergeable, pr.mergeable_state.as_deref()) {
        (Some(true), Some("clean"))
        | (Some(true), Some("unstable"))
        | (Some(true), Some("has_hooks")) => ApplyGate::Allowed,
        (_, Some("blocked")) => ApplyGate::CheckStatuses,
        (Some(false), state) => ApplyGate::Denied(format!(
            "pull request is not mergeable (state: {})",
            state.unwrap_or("unknown")
        )),
        (None, state) => ApplyGate::Denied(format!(
            "GitHub has not computed mergeability yet (state: {}); retry in a moment",
            state.unwrap_or("unknown")
        )),
        (Some(true), state) => ApplyGate::Denied(format!(
            "pull request is not in a mergeable state ({})",
            state.unwrap_or("unknown")
        )),
    }
}

/// When branch protection blocks the PR, accept if every *other* commit
/// status is green (our own contexts are ignored).
pub(super) fn check_required_statuses(
    combined: &crate::github::client::CombinedStatus,
    own_context: &str,
) -> Result<(), String> {
    let bad: Vec<String> = combined
        .statuses
        .iter()
        .filter(|s| {
            s.context != own_context
                && !s.context.starts_with("digger/")
                && !s.context.starts_with("nikama/")
        })
        .filter(|s| s.state != "success")
        .map(|s| format!("{} ({})", s.context, s.state))
        .collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "pull request is blocked by required checks: {}",
            bad.join(", ")
        ))
    }
}

async fn apply_allowed(
    github: &GithubClient,
    pr: &crate::github::PrInfo,
    jobs: &[Job],
    own_context: &str,
) -> Result<(), String> {
    match apply_gate(pr, jobs) {
        ApplyGate::Allowed => Ok(()),
        ApplyGate::Denied(reason) => Err(reason),
        ApplyGate::CheckStatuses => {
            let combined = github
                .combined_status(&pr.head_sha)
                .await
                .map_err(|e| format!("could not read commit status: {}", e))?;
            check_required_statuses(&combined, own_context)
        }
    }
}

async fn auto_merge(github: &GithubClient, config: &Config, pr_number: u64) {
    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    let pr = match github.get_pr(pr_number).await {
        Ok(p) => p,
        Err(e) => {
            tracing::error!(error = %e, "auto-merge: could not fetch PR");
            return;
        }
    };
    if pr.merged {
        tracing::info!("auto-merge: PR already merged");
        return;
    }
    match github.combined_status(&pr.head_sha).await {
        Ok(c) if c.state == "success" => {}
        Ok(c) => {
            tracing::warn!(state = %c.state, "auto-merge: combined status is not success; skipping");
            return;
        }
        Err(e) => {
            tracing::error!(error = %e, "auto-merge: could not read combined status");
            return;
        }
    }
    if pr.mergeable != Some(true) {
        tracing::warn!("auto-merge: PR is not mergeable; skipping");
        return;
    }
    match github
        .merge_pr(
            pr_number,
            config.auto_merge_strategy.as_github_method(),
            &pr.head_sha,
            "auto-merge",
        )
        .await
    {
        Ok(()) => tracing::info!("auto-merge: PR merged"),
        Err(e) => tracing::error!(error = %e, "auto-merge: merge failed"),
    }
}

fn write_outputs(pr_number: u64) {
    use std::io::Write;
    if let Some(path) = crate::util::env_opt("GITHUB_OUTPUT") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
        {
            let _ = writeln!(f, "pr_number={}", pr_number);
        }
    }
    // Digger compatibility: it exported DIGGER_PR_NUMBER via GITHUB_ENV.
    if let Some(path) = crate::util::env_opt("GITHUB_ENV") {
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
        {
            let _ = writeln!(f, "DIGGER_PR_NUMBER={}", pr_number);
        }
    }
}
