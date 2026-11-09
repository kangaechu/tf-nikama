//! Drift detection mode: plan every drift-enabled project and notify.

use super::exit;
use super::run::{run_parallel, Outcome, RunContext};
use crate::cli::Env;
use crate::config::{matches_patterns, Config};
use crate::engine::{Command, Job};
use crate::github::GithubClient;
use crate::storage::NoopPlanStorage;
use std::sync::Arc;

pub async fn run(
    env: &Env,
    config: Arc<Config>,
    actor: &str,
    namespace: &str,
    github: GithubClient,
) -> i32 {
    let notifier = match crate::report::drift::from_env(&github) {
        Ok(n) => n,
        Err(e) => {
            tracing::error!(error = %e, "drift notification not configured");
            return exit::JOB_FAILED;
        }
    };
    let block_filters: Vec<String> = crate::util::env_opt("INPUT_DIGGER_BLOCK_FILTERS")
        .map(|s| {
            s.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect()
        })
        .unwrap_or_default();

    let mut jobs = Vec::new();
    for project in &config.projects {
        if !project.drift_detection {
            continue;
        }
        if !block_filters.is_empty() {
            let in_block = project
                .block_name
                .as_ref()
                .map(|b| block_filters.contains(b))
                .unwrap_or(false);
            if !in_block {
                continue;
            }
        }
        if !(config.drift_include_patterns.is_empty() && config.drift_exclude_patterns.is_empty())
            && !matches_patterns(
                &project.dir,
                &config.drift_include_patterns,
                &config.drift_exclude_patterns,
            )
        {
            tracing::info!(project = %project.name, "excluded by top-level drift patterns");
            continue;
        }
        if !(project.drift_include_patterns.is_empty() && project.drift_exclude_patterns.is_empty())
            && !matches_patterns(
                &project.dir,
                &project.drift_include_patterns,
                &project.drift_exclude_patterns,
            )
        {
            tracing::info!(project = %project.name, "excluded by project drift patterns");
            continue;
        }
        let workflow = config.workflow_for(project);
        jobs.push(Job::new(
            project,
            workflow,
            Command::Plan,
            None,
            "drift-detect",
            actor,
            namespace,
            "",
            "",
        ));
    }
    if jobs.is_empty() {
        tracing::info!("no projects selected for drift detection");
        return exit::OK;
    }
    tracing::info!(count = jobs.len(), "running drift detection");

    let ctx = Arc::new(RunContext {
        config: config.clone(),
        repo_root: env.repo_root.clone(),
        work_root: env.work_root.clone(),
        init_lock: env.init_lock(),
        storage: Arc::new(NoopPlanStorage),
        locker: None,
        writer: None,
        max_parallel: env.max_parallel.unwrap_or(config.concurrency.max_parallel),
    });
    let mut results = run_parallel(ctx, jobs).await;
    results.sort_by(|a, b| a.project.cmp(&b.project));

    let mut failed = false;
    for r in results {
        match r.outcome {
            Ok(Outcome::Plan(p)) if !p.is_empty() => {
                tracing::warn!(project = %r.project, summary = %p.summary.short(), "drift detected");
                if let Err(e) = notifier.notify(&r.project, namespace, &p.plan_text).await {
                    tracing::error!(project = %r.project, error = %e, "failed to send drift notification");
                    failed = true;
                }
            }
            Ok(Outcome::Plan(_)) => tracing::info!(project = %r.project, "no drift"),
            Ok(_) => {}
            Err(e) => {
                failed = true;
                tracing::error!(project = %r.project, error = %e, "drift plan failed");
                if let Err(e2) = notifier.notify_error(&r.project, namespace, &e).await {
                    tracing::error!(error = %e2, "failed to send drift error notification");
                }
            }
        }
    }
    if failed {
        exit::JOB_FAILED
    } else {
        exit::OK
    }
}
