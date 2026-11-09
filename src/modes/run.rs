//! Shared job execution: parallel plans, level-ordered applies.

use crate::aws_auth::populate_job_credentials;
use crate::config::Config;
use crate::engine::{plan_summary_table, ApplyResult, Command, Job, JobExecutor, PlanResult};
use crate::lock::{LockOutcome, PrLocker};
use crate::report::{DetailKind, ReportWriter, RowStatus};
use crate::storage::PlanStorage;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore};

/// Everything a job needs besides itself.
pub struct RunContext {
    pub config: Arc<Config>,
    pub repo_root: PathBuf,
    pub work_root: PathBuf,
    pub init_lock: Option<Arc<Mutex<()>>>,
    pub storage: Arc<dyn PlanStorage>,
    pub locker: Option<Arc<PrLocker>>,
    pub writer: Option<ReportWriter>,
    pub max_parallel: usize,
}

impl RunContext {
    fn executor<'a>(&self, job: &'a Job) -> JobExecutor<'a> {
        let shared_dir = self
            .config
            .projects
            .iter()
            .filter(|p| {
                crate::config::normalize_path(&p.dir)
                    == crate::config::normalize_path(&job.project.dir)
            })
            .count()
            > 1;
        JobExecutor {
            job,
            repo_root: self.repo_root.clone(),
            work_dir: self.work_root.join(sanitize(&job.project.name)),
            init_lock: self.init_lock.clone(),
            isolate_data_dir: shared_dir,
        }
    }

    fn update<F>(&self, f: F)
    where
        F: FnOnce(&mut crate::report::RunReport) + Send + 'static,
    {
        if let Some(w) = &self.writer {
            w.update(f);
        }
    }
}

#[derive(Debug)]
pub enum Outcome {
    Plan(PlanResult),
    Apply(ApplyResult),
    Destroy(ApplyResult),
    Locked,
    Unlocked(bool),
    Skipped(String),
}

#[derive(Debug)]
pub struct JobResult {
    pub project: String,
    pub command: Command,
    pub outcome: Result<Outcome, String>,
}

impl JobResult {
    pub fn is_ok(&self) -> bool {
        self.outcome.is_ok()
    }
}

fn sanitize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Run one job to completion, reporting progress through the writer.
pub async fn run_job(ctx: &RunContext, mut job: Job) -> JobResult {
    let name = job.project.name.clone();
    let command = job.command;
    let n = name.clone();
    ctx.update(move |r| r.row_mut(&n).status = RowStatus::Running);

    let outcome = run_job_inner(ctx, &mut job).await;

    let n = name.clone();
    match &outcome {
        Ok(Outcome::Plan(p)) => {
            let p = p.clone();
            ctx.update(move |r| {
                let row = r.row_mut(&n);
                row.status = if p.is_empty() {
                    RowStatus::NoChanges
                } else {
                    RowStatus::Changes
                };
                row.summary_table = Some(plan_summary_table(&p.summary));
                row.plan_summary = Some(p.summary.clone());
                row.detail_kind = Some(DetailKind::Plan);
                row.detail = Some(p.plan_text.clone());
                row.additional_output = p.additional_output.clone();
            });
        }
        Ok(Outcome::Apply(a)) | Ok(Outcome::Destroy(a)) => {
            let a = a.clone();
            let destroyed = matches!(outcome, Ok(Outcome::Destroy(_)));
            ctx.update(move |r| {
                let row = r.row_mut(&n);
                row.status = if destroyed {
                    RowStatus::Destroyed
                } else {
                    RowStatus::Applied
                };
                row.apply_summary = a.summary;
                row.detail_kind = Some(DetailKind::Apply);
                row.detail = Some(a.output.clone());
                row.additional_output = a.additional_output.clone();
            });
        }
        Ok(Outcome::Locked) => ctx.update(move |r| r.row_mut(&n).status = RowStatus::Locked),
        Ok(Outcome::Unlocked(_)) => ctx.update(move |r| r.row_mut(&n).status = RowStatus::Unlocked),
        Ok(Outcome::Skipped(reason)) => {
            let reason = reason.clone();
            ctx.update(move |r| r.row_mut(&n).status = RowStatus::Skipped(reason));
        }
        Err(e) => {
            let e = e.clone();
            ctx.update(move |r| {
                let row = r.row_mut(&n);
                row.status = RowStatus::Failed;
                row.detail_kind = Some(DetailKind::Error);
                row.detail = Some(e);
            });
        }
    }
    JobResult {
        project: name,
        command,
        outcome,
    }
}

fn format_exec_error(e: &crate::engine::ExecError) -> String {
    let mut s = format!("{}\n", e);
    let stderr = e.stderr.trim();
    let stdout = e.stdout.trim();
    if !stderr.is_empty() {
        s.push_str("\n--- stderr ---\n");
        s.push_str(stderr);
        s.push('\n');
    }
    if !stdout.is_empty() {
        s.push_str("\n--- stdout ---\n");
        s.push_str(&crate::util::truncate_chars(stdout, 6_000, "\n..."));
        s.push('\n');
    }
    s
}

async fn run_job_inner(ctx: &RunContext, job: &mut Job) -> Result<Outcome, String> {
    match job.command {
        Command::Lock => {
            let Some(locker) = &ctx.locker else {
                return Ok(Outcome::Skipped("locking disabled".into()));
            };
            let pr = job.pr_number.ok_or("lock requires a pull request")?;
            match locker
                .lock(pr, &job.lock_id())
                .await
                .map_err(|e| e.to_string())?
            {
                LockOutcome::Acquired | LockOutcome::AlreadyHeld => Ok(Outcome::Locked),
                LockOutcome::HeldByOther(other) => Err(format!("locked by PR #{}", other)),
            }
        }
        Command::Unlock => {
            let mut released = false;
            if let Some(locker) = &ctx.locker {
                released = locker
                    .force_unlock(&job.lock_id())
                    .await
                    .map_err(|e| e.to_string())?;
            }
            if let Err(e) = ctx.storage.delete(&job.plan_file_name()).await {
                tracing::warn!(error = %e, "failed to delete stored plan");
            }
            Ok(Outcome::Unlocked(released))
        }
        Command::Plan => {
            populate_job_credentials(job)
                .await
                .map_err(|e| format!("AWS credentials: {}", e))?;
            let exec = ctx.executor(job);
            let result = exec.plan().await.map_err(|e| format_exec_error(&e))?;
            if !ctx.storage.is_noop() {
                match std::fs::read(&result.plan_file) {
                    Ok(bytes) => {
                        if let Err(e) = ctx.storage.store(&job.plan_file_name(), &bytes).await {
                            return Err(format!(
                                "plan succeeded but storing the plan file failed: {}",
                                e
                            ));
                        }
                    }
                    Err(e) => return Err(format!("could not read plan file: {}", e)),
                }
            }
            Ok(Outcome::Plan(result))
        }
        Command::Apply => {
            populate_job_credentials(job)
                .await
                .map_err(|e| format!("AWS credentials: {}", e))?;
            let exec = ctx.executor(job);
            std::fs::create_dir_all(&exec.work_dir).map_err(|e| e.to_string())?;
            let plan_path = if ctx.storage.is_noop() {
                None
            } else {
                match ctx.storage.retrieve(&job.plan_file_name()).await {
                    Ok(Some(bytes)) => {
                        let p = exec.plan_file();
                        std::fs::write(&p, bytes).map_err(|e| e.to_string())?;
                        Some(p)
                    }
                    Ok(None) => {
                        tracing::warn!(project = %job.project.name, "no stored plan found; applying without a plan file");
                        None
                    }
                    Err(e) => return Err(format!("could not retrieve stored plan: {}", e)),
                }
            };
            let result = exec
                .apply(plan_path.as_deref())
                .await
                .map_err(|e| format_exec_error(&e))?;
            if plan_path.is_some() {
                let _ = ctx.storage.delete(&job.plan_file_name()).await;
            }
            Ok(Outcome::Apply(result))
        }
        Command::Destroy => {
            populate_job_credentials(job)
                .await
                .map_err(|e| format!("AWS credentials: {}", e))?;
            let exec = ctx.executor(job);
            let result = exec.destroy().await.map_err(|e| format_exec_error(&e))?;
            Ok(Outcome::Destroy(result))
        }
    }
}

/// Run jobs with bounded parallelism (plans, locks, unlocks, drift).
pub async fn run_parallel(ctx: Arc<RunContext>, jobs: Vec<Job>) -> Vec<JobResult> {
    let sem = Arc::new(Semaphore::new(ctx.max_parallel.max(1)));
    let mut handles = Vec::new();
    for job in jobs {
        let ctx = ctx.clone();
        let sem = sem.clone();
        handles.push(tokio::spawn(async move {
            let _permit = sem.acquire_owned().await.expect("semaphore");
            run_job(&ctx, job).await
        }));
    }
    let mut results = Vec::new();
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(e) => tracing::error!(error = %e, "job task panicked"),
        }
    }
    results
}

/// Run apply/destroy jobs level by level in dependency order. Projects whose
/// dependencies failed are skipped.
pub async fn run_by_levels(
    ctx: Arc<RunContext>,
    jobs: Vec<Job>,
    parallelism: usize,
) -> Vec<JobResult> {
    let names: Vec<String> = jobs.iter().map(|j| j.project.name.clone()).collect();
    let levels = ctx.config.graph.levels(&names);
    let mut by_name: BTreeMap<String, Job> = jobs
        .into_iter()
        .map(|j| (j.project.name.clone(), j))
        .collect();
    let mut results = Vec::new();
    let mut failed: std::collections::BTreeSet<String> = Default::default();
    for level in levels {
        let mut batch = Vec::new();
        for name in level {
            let Some(job) = by_name.remove(&name) else {
                continue;
            };
            let deps = ctx.config.graph.dependencies_of(&name);
            if let Some(bad) = deps.iter().find(|d| failed.contains(*d)) {
                let reason = format!("dependency {} failed", bad);
                let n = name.clone();
                let r = reason.clone();
                ctx.update(move |rep| rep.row_mut(&n).status = RowStatus::Skipped(r));
                failed.insert(name.clone());
                results.push(JobResult {
                    project: name,
                    command: job.command,
                    outcome: Ok(Outcome::Skipped(reason)),
                });
                continue;
            }
            batch.push(job);
        }
        let sem = Arc::new(Semaphore::new(parallelism.max(1)));
        let mut handles = Vec::new();
        for job in batch {
            let ctx = ctx.clone();
            let sem = sem.clone();
            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire_owned().await.expect("semaphore");
                run_job(&ctx, job).await
            }));
        }
        for h in handles {
            if let Ok(r) = h.await {
                if !r.is_ok() {
                    failed.insert(r.project.clone());
                }
                results.push(r);
            }
        }
    }
    results
}
